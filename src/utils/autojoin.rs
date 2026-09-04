use std::time::{Duration, SystemTime, UNIX_EPOCH};

use matrix_sdk::Client;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::{Room, RoomState};
use tracing::{debug, error, info, warn};

use crate::utils::matrix_util::{Retryable, classify_error};

/// Smallest wait between two join attempts.
const MIN_JOIN_DELAY: Duration = Duration::from_secs(2);
/// Largest wait between two join attempts.
const MAX_JOIN_DELAY: Duration = Duration::from_secs(300);
/// Total time spent retrying a single invitation before giving up.
const JOIN_RETRY_BUDGET: Duration = Duration::from_secs(3600);

/// Accept invitations automatically.
pub async fn on_stripped_state_member(event: StrippedRoomMemberEvent, client: Client, room: Room) {
    let Some(own_user_id) = client.user_id() else {
        return;
    };
    if event.state_key != own_user_id {
        return;
    }

    match room.state() {
        RoomState::Joined => {
            debug!(room_id = %room.room_id(), "Already joined room");
        }
        RoomState::Invited | RoomState::Knocked => {
            // The room name is only used for logging. Requiring one here meant rooms without
            // an m.room.name -- direct messages, freshly created rooms, many bridged rooms --
            // were never joined at all.
            let room_name = room.name().unwrap_or_else(|| "<unnamed>".to_owned());
            info!(room_name, room_id = %room.room_id(), "Invited into room");
            tokio::spawn(join_with_retry(client, room, room_name));
        }
        RoomState::Left | RoomState::Banned => {
            debug!(
                room_name = room.name().unwrap_or_else(|| "<unnamed>".to_owned()),
                room_id = %room.room_id(),
                state = ?room.state(),
                "No longer a member of the room"
            );
        }
    }
}

/// Keep trying to join until it works, the error turns out to be permanent, or the budget is
/// used up.
///
/// Synapse can send the invite before the invited user is allowed to act on it, see
/// <https://github.com/matrix-org/synapse/issues/4345>.
async fn join_with_retry(client: Client, room: Room, room_name: String) {
    let deadline = SystemTime::now() + JOIN_RETRY_BUDGET;
    let mut delay = MIN_JOIN_DELAY;

    loop {
        let error = match client.join_room_by_id(room.room_id()).await {
            Ok(_) => {
                info!(room_name, room_id = %room.room_id(), "Successfully joined room");
                return;
            }
            Err(error) => error,
        };

        // A 403 ("not invited", "banned from this room") will keep failing no matter how long
        // we wait. Previously every error was retried the same way for up to an hour.
        let wait = match classify_error(&error) {
            Retryable::No => {
                error!(room_name, room_id = %room.room_id(), %error, "Cannot join room, giving up");
                return;
            }
            Retryable::Yes { retry_after } => retry_after.unwrap_or(delay).min(MAX_JOIN_DELAY),
        };

        if SystemTime::now() + wait > deadline {
            error!(
                room_name,
                room_id = %room.room_id(),
                %error,
                budget_secs = JOIN_RETRY_BUDGET.as_secs(),
                "Still cannot join room after the retry budget, giving up"
            );
            return;
        }

        warn!(
            room_name,
            room_id = %room.room_id(),
            wait_secs = wait.as_secs(),
            %error,
            "Failed to join room, retrying"
        );
        tokio::time::sleep(wait).await;

        // Jitter keeps several invitations arriving at once from retrying in lockstep.
        delay = (delay * 2 + jitter()).min(MAX_JOIN_DELAY);
    }
}

fn jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    Duration::from_millis(u64::from(nanos % 1_000))
}
