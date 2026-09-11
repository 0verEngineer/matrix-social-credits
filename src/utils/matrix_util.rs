use std::time::{Duration, SystemTime, UNIX_EPOCH};

use matrix_sdk::ruma::DeviceId;
use matrix_sdk::ruma::api::error::{ErrorBody, ErrorKind, RetryAfter};
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::{Client, ClientBuildError, Error, Room};
use tracing::{error, info, warn};

use crate::utils::session::SessionStore;

/// Smallest delay used by our own retry loops.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
/// Upper bound for a single wait, so we keep polling a homeserver that is still booting.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How a failed request should be treated by our own retry loops.
///
/// This mirrors what matrix-sdk does internally for the requests it retries itself, but we
/// need the same decision for the calls the SDK cannot retry for us -- most importantly the
/// login, which happens before any `RequestConfig` retry budget applies to a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryable {
    /// Worth trying again. `retry_after` is what the homeserver asked us to wait, if it said
    /// anything at all.
    Yes { retry_after: Option<Duration> },
    /// Trying again would fail exactly the same way.
    No,
}

/// Decide whether `error` is worth retrying.
pub fn classify_error(error: &Error) -> Retryable {
    match error.client_api_error_kind() {
        // 429 M_LIMIT_EXCEEDED. Synapse returns this a lot right after a restart, and it
        // tells us how long to wait -- respecting that is the whole point.
        Some(ErrorKind::LimitExceeded(limit)) => {
            let retry_after = match limit.retry_after.as_ref() {
                Some(RetryAfter::Delay(delay)) => Some(*delay),
                Some(RetryAfter::DateTime(when)) => when.duration_since(SystemTime::now()).ok(),
                None => None,
            };
            Retryable::Yes { retry_after }
        }

        // Nothing we can fix by waiting: wrong credentials, revoked token, gone account.
        Some(
            ErrorKind::Forbidden
            | ErrorKind::UnknownToken(_)
            | ErrorKind::UserDeactivated
            | ErrorKind::UserSuspended
            | ErrorKind::UserLocked
            | ErrorKind::Unrecognized
            | ErrorKind::InvalidUsername
            | ErrorKind::MissingToken,
        ) => Retryable::No,

        // Some other Matrix error, or a proxy answering without a Matrix body. Go by the
        // status code: 429 and 5xx are transient, the rest is not.
        _ => match error.as_client_api_error() {
            Some(api_error) => {
                let from_homeserver = !matches!(api_error.body, ErrorBody::NotJson { .. });
                if retry_by_status(api_error.status_code.as_u16(), from_homeserver) {
                    Retryable::Yes { retry_after: None }
                } else {
                    Retryable::No
                }
            }
            // No Matrix error at all, so this failed below the Matrix layer: connection
            // refused, DNS not up yet, TLS handshake aborted. Exactly what happens while the
            // Matrix stack is restarting, and always worth retrying.
            None => Retryable::Yes { retry_after: None },
        },
    }
}

/// Whether an HTTP failure without a recognised Matrix error kind is worth another attempt.
///
/// `from_homeserver` is false when the response body was not a Matrix error at all. That is
/// the tell that something in front of the homeserver answered: a reverse proxy that is still
/// starting replies `404 page not found` in plain text, which clears up by itself. Treating
/// that as permanent turns a few seconds of proxy startup into a crash loop -- seen in
/// production, three restarts before the proxy was ready.
fn retry_by_status(status: u16, from_homeserver: bool) -> bool {
    if !from_homeserver {
        return true;
    }

    status == 429 || (500..600).contains(&status)
}

/// Exponential backoff with jitter, capped at [`MAX_BACKOFF`].
///
/// The jitter keeps several bots (or several rooms) from hammering the homeserver in
/// lockstep after it comes back up.
fn backoff_for(attempt: u32) -> Duration {
    let base = MIN_BACKOFF
        .saturating_mul(2u32.saturating_pow(attempt.min(6)))
        .min(MAX_BACKOFF);

    // A dedicated RNG would be overkill here; the clock is random enough for jitter.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter = Duration::from_millis(u64::from(nanos % 1_000));

    (base + jitter).min(MAX_BACKOFF + Duration::from_secs(1))
}

/// Why [`retry_within_budget`] stopped.
enum RetryError {
    /// Trying again would fail the same way.
    Permanent(Error),
    /// The last failure was transient, but the budget is used up.
    BudgetExhausted(Error),
}

/// Run `op` until it succeeds, `classify` calls its error permanent, or `deadline` passes.
///
/// The wait between attempts is what the homeserver asked for, or our own backoff.
async fn retry_within_budget<T>(
    what: &str,
    deadline: SystemTime,
    mut op: impl AsyncFnMut() -> Result<T, Error>,
    classify: impl Fn(&Error) -> Retryable,
) -> Result<T, RetryError> {
    let mut attempt: u32 = 0;

    loop {
        let error = match op().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };

        let wait = match classify(&error) {
            Retryable::No => return Err(RetryError::Permanent(error)),
            Retryable::Yes { retry_after } => retry_after
                .unwrap_or_else(|| backoff_for(attempt))
                .clamp(MIN_BACKOFF, MAX_BACKOFF),
        };

        if SystemTime::now() + wait > deadline {
            return Err(RetryError::BudgetExhausted(error));
        }

        attempt += 1;
        warn!(
            attempt,
            wait_secs = wait.as_secs(),
            error = %error,
            "{what} failed, retrying"
        );
        tokio::time::sleep(wait).await;
    }
}

/// Build a client and restore the stored session into it, or log in and store the resulting
/// session. Hands back the client that ended up logged in.
///
/// Restoring is tried first so a restart does not create yet another device and does not hit
/// the `/login` rate limit at all. If the homeserver rejects the stored session (revoked
/// token, account logged out elsewhere) the file is dropped and a fresh login is performed.
///
/// The login happens on a *new* client. matrix-sdk allows exactly one session per client and
/// panics on the second, so a client that has had the rejected session restored into it is
/// of no use for logging in.
///
/// The login also asks for the device the crypto store belongs to, if there is one. Without
/// that the homeserver hands out a new device, the SDK then refuses to pair it with the
/// existing crypto store, and the client is left half initialised -- retrying that panics
/// too. See [`SessionStore::stored_device_id`].
pub async fn authenticate(
    build_client: impl AsyncFn() -> Result<Client, ClientBuildError>,
    store: &SessionStore,
    username: &str,
    password: &str,
    device_display_name: &str,
    budget: Duration,
) -> anyhow::Result<Client> {
    let deadline = SystemTime::now() + budget;

    if let Some(session) = store.load() {
        let client = build_client().await?;
        match client.restore_session(session).await {
            Ok(()) => {
                // restore_session only loads the tokens; ask the homeserver whether they are
                // still valid before we rely on them. A homeserver that is not up yet is not
                // a rejection, so this is retried like the login is.
                let whoami = retry_within_budget(
                    "whoami",
                    deadline,
                    async || client.whoami().await.map_err(Error::from),
                    classify_error,
                )
                .await;

                match whoami {
                    Ok(_) => {
                        info!("Reused the stored session");
                        return Ok(client);
                    }
                    Err(RetryError::Permanent(error)) => {
                        warn!(%error, "The stored session was rejected, logging in again");
                        store.clear();
                    }
                    Err(RetryError::BudgetExhausted(error)) => {
                        return Err(anyhow::anyhow!(
                            "Homeserver still unreachable after {}s, giving up: {error}",
                            budget.as_secs()
                        ));
                    }
                }
            }
            Err(error) => {
                warn!(%error, "Unable to restore the stored session, logging in again");
                store.clear();
            }
        }

        // Closes the store with it, before the device id is read and a new client opens it.
        drop(client);
    }

    let device_id = store.stored_device_id().await;
    let client = build_client().await?;
    login_with_retry(
        &client,
        username,
        password,
        device_id.as_deref(),
        device_display_name,
        deadline,
    )
    .await?;

    match client.matrix_auth().session() {
        Some(session) => {
            if let Err(error) = store.save(&session) {
                // Not fatal, it just means the next start logs in again.
                warn!(%error, "Unable to save the session");
            }
        }
        None => warn!("Logged in but no session to save"),
    }

    Ok(client)
}

/// Log in, retrying while the homeserver is unreachable or rate limiting us.
///
/// Without this the bot dies on startup whenever the Matrix stack is restarted: `/login` is
/// one of the endpoints Synapse rate limits most aggressively, and with
/// `restart: unless-stopped` a crash on 429 turns into a restart loop that makes the rate
/// limiting worse.
///
/// `device_id` is the device the crypto store belongs to. Passing it makes the homeserver
/// reuse that device instead of creating a new one, which is the only way a fresh login can
/// work with an existing crypto store.
async fn login_with_retry(
    client: &Client,
    username: &str,
    password: &str,
    device_id: Option<&DeviceId>,
    device_display_name: &str,
    deadline: SystemTime,
) -> anyhow::Result<()> {
    // Set as soon as the homeserver has answered the login, before the local stores are
    // opened for the new session. A failure after that point is not the homeserver's and
    // does not go away by asking again -- the SDK panics on a second login attempt.
    let login_accepted = || client.auth_api().is_some();

    let result = retry_within_budget(
        "Login",
        deadline,
        async || {
            let mut login = client
                .matrix_auth()
                .login_username(username, password)
                .initial_device_display_name(device_display_name);
            if let Some(device_id) = device_id {
                login = login.device_id(device_id.as_str());
            }
            login.send().await.map(|_| ())
        },
        |error| {
            if login_accepted() {
                Retryable::No
            } else {
                classify_error(error)
            }
        },
    )
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(RetryError::Permanent(error)) if login_accepted() => Err(anyhow::anyhow!(
            "Logged in, but the client state in STORE_PATH cannot be used with this login: \
             {error}. If the stored device is gone for good, delete STORE_PATH; the bot then \
             starts as a new device without its old room keys"
        )),
        Err(RetryError::Permanent(error)) => {
            Err(anyhow::anyhow!("Login rejected by the homeserver: {error}"))
        }
        Err(RetryError::BudgetExhausted(error)) => Err(anyhow::anyhow!(
            "Login still failing at the end of the retry budget, giving up: {error}"
        )),
    }
}

/// Send a message into a room, logging instead of panicking when it fails.
///
/// The SDK already retries transient failures according to the client's `RequestConfig`, so
/// by the time we see an error here it is either permanent or the retry budget is used up.
/// Either way, a failed status message must not take the bot down.
pub async fn send_message(room: &Room, content: RoomMessageEventContent) {
    if let Err(error) = room.send(content).await {
        error!(room_id = %room.room_id(), %error, "Failed to send message");
    }
}

/// Log a summary of how the client is configured to retry, so the reason for long stalls is
/// visible in the log.
pub fn log_retry_configuration(retry_limit: usize, max_retry_time: Duration) {
    info!(
        retry_limit,
        max_retry_time_secs = max_retry_time.as_secs(),
        "HTTP retry configuration"
    );
}

#[cfg(test)]
mod tests {
    use super::retry_by_status;

    #[test]
    fn rate_limits_and_server_errors_are_retried() {
        assert!(retry_by_status(429, true));
        assert!(retry_by_status(500, true));
        assert!(retry_by_status(502, true));
        assert!(retry_by_status(503, true));
    }

    #[test]
    fn a_real_matrix_error_is_taken_at_face_value() {
        assert!(!retry_by_status(400, true));
        assert!(!retry_by_status(401, true));
        assert!(!retry_by_status(403, true));
        // The homeserver saying "no such endpoint" will keep saying it.
        assert!(!retry_by_status(404, true));
    }

    /// A body that is not a Matrix error means the homeserver never saw the request. Seen in
    /// production as `404 page not found` in plain text from a reverse proxy that was still
    /// coming up -- three crash-restarts before it was ready.
    #[test]
    fn anything_that_did_not_come_from_the_homeserver_is_retried() {
        assert!(retry_by_status(404, false));
        assert!(retry_by_status(400, false));
        assert!(retry_by_status(502, false));
    }
}
