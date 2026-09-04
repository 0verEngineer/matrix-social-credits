use std::time::Duration;
use matrix_sdk::Client;
use matrix_sdk::{Room, RoomState};
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use tracing::{debug, error, info, warn};

/// Autojoin // todo check if it works if kicked once and reinvited
pub async fn on_stripped_state_member(event: StrippedRoomMemberEvent,
                                      client: Client,
                                      room: Room,
) {
    let Some(own_user_id) = client.user_id() else { return; };
    if event.state_key != own_user_id { return; }

    match room.state() {
        RoomState::Joined => {
            debug!(room_id = %room.room_id(), "Already joined room");
        },
        RoomState::Invited | RoomState::Knocked => {
            if room.name().is_none() { return; }
            let room_name = room.name().unwrap();
            info!(room_name, room_id = %room.room_id(), "Invited into room");
            tokio::spawn(async move {
                let mut delay = 2;

                while let Err(err) = client.join_room_by_id(room.room_id()).await {
                    // retry autojoin due to synapse sending invites, before the
                    // invited user can join for more information see
                    // https://github.com/matrix-org/synapse/issues/4345
                    warn!(room_name, room_id = %room.room_id(), delay_secs = delay, error = ?err, "Failed to join room, retrying");

                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    delay *= 2;

                    if delay > 3600 {
                        error!(room_name, room_id = %room.room_id(), error = ?err, "Giving up joining room");
                        break;
                    }
                }
                info!(room_name, room_id = %room.room_id(), "Successfully joined room");
            });
        },
        RoomState::Left | RoomState::Banned => {
            if room.name().is_none() { return; }
            info!(room_name = room.name().unwrap(), room_id = %room.room_id(), "Left room");
        },
    }
}
