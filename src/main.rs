mod event_handler;
mod data;
mod utils;

use std::env;
use std::time::Duration;
use matrix_sdk::{
    Client, LoopCtrl, config::SyncSettings,
};
use matrix_sdk::Room;
use matrix_sdk::config::RequestConfig;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use std::sync::{Arc, Mutex};
use rusqlite::{Connection};
use crate::data::emoji::create_table_emoji;
use crate::data::event::create_table_event;
use crate::data::user::create_table_user;
use crate::data::user_room_data::create_table_user_room_data;
use crate::data::user_reaction::{create_table_user_reaction};
use crate::event_handler::EventHandler;
use crate::utils::autojoin::on_stripped_state_member;
use crate::utils::matrix_util::{Retryable, classify_error, log_retry_configuration, login_with_retry};
use crate::utils::user_util::{initial_admin_user_setup, resolve_configured_user_id};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;


// todo session preservation and emoji verification
// todo query all room users on initial setup and create user_room_data for every user, also handle user joining

/// Log level defaults. Overridable via `RUST_LOG`, e.g.
/// `RUST_LOG=matrix_social_credits=debug,matrix_sdk=info`.
///
/// The SDK is kept at `warn` on purpose: its `info` output is very chatty, but its warnings
/// carry the rate limit and retry diagnostics we care about.
const DEFAULT_LOG_FILTER: &str = "matrix_social_credits=info,matrix_sdk=warn";

/// How many times the SDK may retry a single HTTP request.
///
/// This matters for more than just the number: matrix-sdk only retries plain network
/// failures (connection refused, DNS, TLS) when a retry limit is configured at all. Without
/// it, every request issued while the Matrix stack is restarting fails immediately.
/// 429 and 5xx responses are retried either way.
const DEFAULT_HTTP_RETRY_LIMIT: usize = 10;

/// Upper bound for the wait between two attempts of the same request.
const DEFAULT_HTTP_MAX_RETRY_TIME_SECS: u64 = 60;

/// How long the initial login may keep retrying before we give up and exit.
const DEFAULT_LOGIN_RETRY_BUDGET_SECS: u64 = 900;

/// Long polling timeout for the sync loop.
const SYNC_TIMEOUT_SECS: u64 = 30;

/// Wait applied after a failed sync iteration, so a hard-down homeserver is not polled in a
/// tight loop.
const SYNC_ERROR_BACKOFF: Duration = Duration::from_secs(5);

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let db_path = env::var("DB_PATH").expect("DB_PATH not set");
    let initial_social_credit = get_env_var_as_i32("INITIAL_SOCIAL_CREDIT");
    let reaction_timespan = get_env_var_as_i32("REACTION_TIMESPAN");
    let reaction_limit = get_env_var_as_i32("REACTION_LIMIT");
    let admin_username = env::var("ADMIN_USERNAME").expect("ADMIN_USERNAME not set");
    let username = env::var("MATRIX_USERNAME").expect("MATRIX_USERNAME not set");
    let homeserver_url = env::var("MATRIX_HOMESERVER_URL").expect("MATRIX_HOMESERVER_URL not set");
    if !homeserver_url.starts_with("https://") && !homeserver_url.starts_with("http://") {
        panic!("MATRIX_HOMESERVER_URL must start with http:// or https://");
    }
    let password = env::var("MATRIX_PASSWORD").expect("MATRIX_PASSWORD not set");

    let http_retry_limit = optional_env_var("HTTP_RETRY_LIMIT", DEFAULT_HTTP_RETRY_LIMIT);
    let http_max_retry_time = Duration::from_secs(optional_env_var(
        "HTTP_MAX_RETRY_TIME_SECS",
        DEFAULT_HTTP_MAX_RETRY_TIME_SECS,
    ));
    let login_retry_budget = Duration::from_secs(optional_env_var(
        "LOGIN_RETRY_BUDGET_SECS",
        DEFAULT_LOGIN_RETRY_BUDGET_SECS,
    ));

    // Database setup
    let conn = Connection::open(db_path)?;
    conn.execute("PRAGMA foreign_keys = ON", []).expect("Failed to enable foreign key support");
    create_table_user(&conn);
    create_table_user_room_data(&conn);
    create_table_user_reaction(&conn);
    create_table_emoji(&conn);
    create_table_event(&conn);

    log_retry_configuration(http_retry_limit, http_max_retry_time);
    let client = Client::builder()
        .homeserver_url(homeserver_url.clone())
        .request_config(
            RequestConfig::new()
                .retry_limit(http_retry_limit)
                .max_retry_time(http_max_retry_time),
        )
        .build()
        .await?;

    login_with_retry(
        &client,
        username.as_str(),
        password.as_str(),
        "Social Credit System",
        login_retry_budget,
    )
    .await?;

    // Everything that needs to know who the bot is takes it from here. The homeserver is the
    // authority on that; the host part of MATRIX_HOMESERVER_URL is not the server name when
    // .well-known delegation is used.
    let own_user_id = client
        .user_id()
        .expect("Logged in but the client has no user id")
        .to_owned();
    let server_name = own_user_id.server_name().to_owned();
    info!(user_id = %own_user_id, "Logged in");

    let admin_user_id = resolve_configured_user_id(&admin_username, &server_name)
        .unwrap_or_else(|| panic!("ADMIN_USERNAME '{admin_username}' is not a valid Matrix user"));

    client.add_event_handler(on_stripped_state_member);

    let shared_conn = Arc::new(Mutex::new(conn));
    let event_handler = Arc::new(EventHandler::new(
        shared_conn.clone(),
        own_user_id,
        initial_social_credit,
        reaction_timespan,
        reaction_limit,
    ));

    initial_admin_user_setup(&shared_conn, &admin_user_id);

    client.add_event_handler({
        let event_handler = event_handler.clone();
        move |event: AnySyncMessageLikeEvent, room: Room| {
            let handler = event_handler.clone();
            async move {
                handler.on_message_like_event(event, room).await;
            }
        }
    });

    info!("Starting sync loop");
    let sync_settings = SyncSettings::default().timeout(Duration::from_secs(SYNC_TIMEOUT_SECS));

    tokio::select! {
        result = run_sync_loop(&client, sync_settings) => result?,
        _ = shutdown_signal() => info!("Shutdown signal received, stopping"),
    }

    Ok(())
}

/// Run the sync loop, surviving transient failures.
///
/// `Client::sync` returns on the first error, which previously took the whole process down
/// with it: a single 429 or a 502 from the reverse proxy while Synapse restarts was enough.
/// `sync_with_result_callback` hands us every iteration's result instead, so we can decide
/// whether to keep going.
async fn run_sync_loop(client: &Client, sync_settings: SyncSettings) -> anyhow::Result<()> {
    client
        .sync_with_result_callback(sync_settings, |result| async move {
            match result {
                Ok(_) => Ok(LoopCtrl::Continue),
                Err(error) => match classify_error(&error) {
                    Retryable::Yes { retry_after } => {
                        let wait = retry_after.unwrap_or(SYNC_ERROR_BACKOFF);
                        warn!(
                            %error,
                            wait_secs = wait.as_secs(),
                            "Sync failed, retrying"
                        );
                        tokio::time::sleep(wait).await;
                        Ok(LoopCtrl::Continue)
                    }
                    Retryable::No => {
                        error!(%error, "Sync failed permanently");
                        Err(error)
                    }
                },
            }
        })
        .await?;

    Ok(())
}

/// Resolve on SIGTERM (what `docker stop` sends) or Ctrl-C.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                error!(%error, "Unable to listen for SIGTERM");
                return;
            }
        };

        tokio::select! {
            _ = terminate.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn get_env_var_as_i32(var_name: &str) -> i32 {
    env::var(var_name)
        .map_err(|e| format!("Couldn't read {}: {}", var_name, e))
        .and_then(|value| {
            value.parse::<i32>().map_err(|e| format!("Failed to parse {}: {}", var_name, e))
        })
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", var_name, e))
}

/// Read an optional numeric environment variable, falling back to `default` when it is unset
/// or unparsable.
fn optional_env_var<T: std::str::FromStr>(var_name: &str, default: T) -> T {
    match env::var(var_name) {
        Err(_) => default,
        Ok(value) => match value.parse::<T>() {
            Ok(parsed) => parsed,
            Err(_) => {
                warn!(var_name, value, "Unparsable value, using the default");
                default
            }
        },
    }
}
