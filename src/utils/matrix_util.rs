use std::time::{Duration, SystemTime, UNIX_EPOCH};

use matrix_sdk::ruma::api::error::{ErrorBody, ErrorKind, RetryAfter};
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::{Client, Error, Room};
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

/// Restore a stored session, or log in and store the resulting one.
///
/// Restoring is tried first so a restart does not create yet another device and does not hit
/// the `/login` rate limit at all. If the homeserver rejects the stored session (revoked
/// token, account logged out elsewhere) the file is dropped and a fresh login is performed.
pub async fn authenticate(
    client: &Client,
    store: &SessionStore,
    username: &str,
    password: &str,
    device_display_name: &str,
    budget: Duration,
) -> anyhow::Result<()> {
    if let Some(session) = store.load() {
        match client.restore_session(session).await {
            Ok(()) => {
                // restore_session only loads the tokens; ask the homeserver whether they are
                // still valid before we rely on them.
                match client.whoami().await {
                    Ok(_) => {
                        info!("Reused the stored session");
                        return Ok(());
                    }
                    Err(error) => {
                        warn!(%error, "The stored session was rejected, logging in again");
                        store.clear();
                    }
                }
            }
            Err(error) => {
                warn!(%error, "Unable to restore the stored session, logging in again");
                store.clear();
            }
        }
    }

    login_with_retry(client, username, password, device_display_name, budget).await?;

    match client.matrix_auth().session() {
        Some(session) => {
            if let Err(error) = store.save(&session) {
                // Not fatal, it just means the next start logs in again.
                warn!(%error, "Unable to save the session");
            }
        }
        None => warn!("Logged in but no session to save"),
    }

    Ok(())
}

/// Log in, retrying while the homeserver is unreachable or rate limiting us.
///
/// Without this the bot dies on startup whenever the Matrix stack is restarted: `/login` is
/// one of the endpoints Synapse rate limits most aggressively, and with
/// `restart: unless-stopped` a crash on 429 turns into a restart loop that makes the rate
/// limiting worse.
async fn login_with_retry(
    client: &Client,
    username: &str,
    password: &str,
    device_display_name: &str,
    budget: Duration,
) -> anyhow::Result<()> {
    let deadline = SystemTime::now() + budget;
    let mut attempt: u32 = 0;

    loop {
        let result = client
            .matrix_auth()
            .login_username(username, password)
            .initial_device_display_name(device_display_name)
            .send()
            .await;

        let error = match result {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };

        let wait = match classify_error(&error) {
            Retryable::No => {
                return Err(anyhow::anyhow!("Login rejected by the homeserver: {error}"));
            }
            Retryable::Yes { retry_after } => retry_after
                .unwrap_or_else(|| backoff_for(attempt))
                .clamp(MIN_BACKOFF, MAX_BACKOFF),
        };

        let now = SystemTime::now();
        if now + wait > deadline {
            return Err(anyhow::anyhow!(
                "Login still failing after {}s, giving up: {error}",
                budget.as_secs()
            ));
        }

        attempt += 1;
        warn!(
            attempt,
            wait_secs = wait.as_secs(),
            error = %error,
            "Login failed, retrying"
        );
        tokio::time::sleep(wait).await;
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
