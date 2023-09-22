use std::sync::{Arc, Mutex};
use matrix_sdk::Client;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, RoomMessageEventContent};
use rusqlite::Connection;
use crate::data::emoji::{Emoji, find_all_emoji_in_db};
use crate::data::event::{Event, find_event_in_db, insert_event};
use crate::data::user::{find_all_users_in_db, User, UserType};
use crate::utils::user_util::{get_user_list_answer, setup_user};


pub struct EventHandler {
    conn: Arc<Mutex<Connection>>,
    client: Client,
    cached_emojis: Arc<Mutex<Vec<Emoji>>>,
    cached_users: Arc<Mutex<Vec<User>>>,
}

impl EventHandler {
    pub fn new(conn: Arc<Mutex<Connection>>, client: Client) -> Self {
        let users: Vec<User> = find_all_users_in_db(&conn).unwrap_or(Vec::new());
        let emojis: Vec<Emoji> = find_all_emoji_in_db(&conn).unwrap_or(Vec::new());
        EventHandler {
            conn,
            client,
            cached_emojis: Arc::new(Mutex::new(emojis)),
            cached_users: Arc::new(Mutex::new(users)),
        }
    }

    pub async fn on_message_like_event(&self, event: AnySyncMessageLikeEvent, room: Room) {
        match room {
            Room::Joined(room) => {
                //println!("Received a AnySyncMessageLikeEvent, type: {:?}, event {:?}", event.event_type().to_string(), event); // todo debug level

                // Check if we already handled this event
                let handled_event = find_event_in_db(&self.conn, &event.event_id().to_string());
                if handled_event.is_some() {
                    println!("Event {} already handled", handled_event.unwrap().id); // todo debug level
                    return;
                }

                let new_handled_event = Event {
                    id: event.event_id().to_string(),
                    event_type: event.event_type().to_string(),
                    handled: true,
                };
                if insert_event(&self.conn, &new_handled_event).is_err() {
                    println!("Unable to insert event {} into db", new_handled_event.id); // todo debug level
                    return;
                }

                let mut sender = self.find_user_in_cache(&event.sender().to_string());
                if sender.is_none() {
                    sender = setup_user(&self.conn, Some(room.clone()), &event.sender().to_string(), UserType::Default);
                    if sender.is_none() {
                        println!("Sender is none"); // todo debug level
                        return;
                    }
                    // Add sender to user cache
                    let mut users_guard = self.cached_users.lock().unwrap();
                    users_guard.push(sender.clone().unwrap());
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
                    match event.original_content().unwrap() {
                        events::AnyMessageLikeEventContent::Reaction(content) => {
                            println!("Reaction content {:?}", content);
                            let emoji = self.find_emoji_in_cache(&content.relates_to.key);
                            if emoji.is_none() {
                                println!("Emoji is not registered"); // todo debug level
                                return;
                            }

                            let message_event = room.event(&event.event_id()).await;
                            if message_event.is_err() {
                                println!("Unable to get the message event that relates to this reaction event"); // todo debug level
                                return;
                            }
                            println!("MessageEvent loaded: {:?}", message_event);
                        }
                        _ => {}
                    }
                }

                if event.event_type().to_string() == "m.room.message" {
                    match event.original_content().unwrap() {
                        events::AnyMessageLikeEventContent::RoomMessage(content) => {
                            match content.msgtype {
                                MessageType::Text(..) => {},
                                _ => { return; }
                            }

                            let body = content.body();

                            let mut recipient = self.find_user_in_cache(&body.to_string());
                            if recipient.is_none() {
                                recipient = setup_user(&self.conn, Some(room.clone()), &body.to_string(), UserType::Default);
                                if recipient.is_some() {
                                    // Add recipient to user cache
                                    let mut users_guard = self.cached_users.lock().unwrap();
                                    users_guard.push(recipient.clone().unwrap());
                                }
                            }

                            // commands

                            if body == "!list" {
                                let answer = get_user_list_answer(&self.conn, &room);
                                let content = RoomMessageEventContent::text_html(answer.text, answer.html);
                                room.send(content, None).await.unwrap();
                            }

                            // reactions

                            if recipient.is_none() {
                                println!("Recipient is none"); // todo debug level
                                return;
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

    fn find_user_in_cache(&self, name: &str) -> Option<User> {
        let users_guard = self.cached_users.lock().unwrap();
        for user in users_guard.iter() {
            if user.name == name {
                return Some(user.clone());
            }
        }
        None
    }

    fn find_emoji_in_cache(&self, name: &str) -> Option<Emoji> {
        let emojis_guard = self.cached_emojis.lock().unwrap();
        for emoji in emojis_guard.iter() {
            if emoji.emoji == name {
                return Some(emoji.clone());
            }
        }
        None
    }
}

