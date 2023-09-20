use std::sync::{Arc, Mutex};
use std::time::Duration;
use matrix_sdk::Client;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, RoomMessageEventContent};
use rusqlite::Connection;
use tokio::time::sleep;
use crate::utils::user_util::{construct_and_register_user};


pub async fn on_message_like_event(conn: Arc<Mutex<Connection>>, event: AnySyncMessageLikeEvent, room: Room) {
    match room {
        Room::Joined(room) => {
            println!("Received a AnySyncMessageLikeEvent, type: {:?}, event {:?}", event.event_type().to_string(), event);

            let sender = construct_and_register_user(&conn, &event.sender().to_string());
            if sender.is_none() {
                println!("Sender is none"); // todo debug level
                return;
            }

            // Matrix does not support stickers in tagged messages so we cannot use stickers at the moment
            /*if event.event_type().to_string() == "m.sticker" {
                println!("Received a sticker event {:?}", event);
                match event.original_content().unwrap() {
                    events::AnyMessageLikeEventContent::Sticker(StickerEventContent { body, info, url, ..}) => {}
                    _ => {}
                }
            }*/

            if event.event_type().to_string() == "m.reaction" {
                // todo
                println!("Received a reaction event {:?}", event);
            }

            if event.event_type().to_string() == "m.room.message" {
                match event.original_content().unwrap() {
                    events::AnyMessageLikeEventContent::RoomMessage(content) => {
                        match content.msgtype {
                            MessageType::Text(..) => {},
                            _ => { return; }
                        }

                        let body = content.body();
                        let recipient = construct_and_register_user(&conn, &body.to_string());
                        if recipient.is_none() {
                            println!("Recipient is none"); // todo debug level
                            return;
                        }

                        if body == "!party" {
                            println!("Party time!");
                            let content = RoomMessageEventContent::text_plain("🎉🎊🥳 let's PARTY!! 🥳🎊🎉");
                            room.send(content, None).await.unwrap();
                        }

                        if body == "😑" {
                            println!("Emoji detected!");
                        }
                    }
                    _ => {}
                }
            }
        },
        _ => { return }
    }
}

/// Autojoin // todo check if it works if kicked once and reinvited
pub async fn on_stripped_state_member(room_member: StrippedRoomMemberEvent,
                                   client: Client,
                                   room: Room,
) {
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    match room {
        Room::Joined(_) => {
            println!("Already joined room {}", room.room_id());
        },
        Room::Invited(_) => {
            println!("Invited into room {}, id: {}", room.name().unwrap(), room.room_id());
            tokio::spawn(async move {
                let mut delay = 2;

                while let Err(err) = client.join_room_by_id(room.room_id()).await {
                    // retry autojoin due to synapse sending invites, before the
                    // invited user can join for more information see
                    // https://github.com/matrix-org/synapse/issues/4345
                    eprintln!("Failed to join room {}, id: {} ({err:?}), retrying in {delay}s", room.name().unwrap(), room.room_id());

                    sleep(Duration::from_secs(delay)).await;
                    delay *= 2;

                    if delay > 3600 {
                        eprintln!("Can't join room {}, id: {} ({err:?})", room.name().unwrap(), room.room_id());
                        break;
                    }
                }
                println!("Successfully joined room {}, id: {}", room.name().unwrap(), room.room_id());
            });
        },
        Room::Left(_) => {
            println!("Left room {}, id: {}", room.name().unwrap(), room.room_id());
        },
    }
}
