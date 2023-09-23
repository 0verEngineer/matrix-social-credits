use std::time::Duration;
use matrix_sdk::Client;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;

/// Autojoin // todo check if it works if kicked once and reinvited
pub async fn on_stripped_state_member(event: StrippedRoomMemberEvent,
                                      client: Client,
                                      room: Room,
) {
    if client.user_id().is_none() { return; }
    if event.state_key != client.user_id().unwrap() { return; }

    match room {
        Room::Joined(_) => {
            println!("Already joined room {}", room.room_id());
        },
        Room::Invited(_) => {
            if room.name().is_none() { return; }
            let room_name = room.name().unwrap();
            println!("Invited into room {}, id: {}", room_name, room.room_id());
            tokio::spawn(async move {
                let mut delay = 2;

                while let Err(err) = client.join_room_by_id(room.room_id()).await {
                    // retry autojoin due to synapse sending invites, before the
                    // invited user can join for more information see
                    // https://github.com/matrix-org/synapse/issues/4345
                    eprintln!("Failed to join room {}, id: {} ({err:?}), retrying in {delay}s", room_name, room.room_id());

                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    delay *= 2;

                    if delay > 3600 {
                        eprintln!("Can't join room {}, id: {} ({err:?})", room_name, room.room_id());
                        break;
                    }
                }
                println!("Successfully joined room {}, id: {}", room_name, room.room_id());
            });
        },
        Room::Left(_) => {
            if room.name().is_none() { return; }
            println!("Left room {}, id: {}", room.name().unwrap(), room.room_id());
        },
    }
}