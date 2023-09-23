use std::sync::{Arc, Mutex};
use matrix_sdk::Client;
use matrix_sdk::room::{Joined, Room};
use matrix_sdk::ruma::events;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, RoomMessageEventContent};
use rusqlite::Connection;
use crate::data::emoji::{Emoji, find_all_emoji_in_db, find_emoji_in_db, insert_emoji};
use crate::data::event::{Event, find_event_in_db, insert_event};
use crate::data::user::{find_all_users_in_db, User, UserType};
use crate::utils::emoji_util::get_emoji_list_answer;
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
                            let emoji = self.find_emoji_in_cache(&content.relates_to.key, &room.room_id().to_string());
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

                            let mut stripped_body: String = body.to_string();
                            if body.starts_with("* ") {
                                stripped_body = body.strip_prefix("* ").unwrap().to_string();
                            }

                            if self.handle_list(&room, &mut stripped_body).await { return; };
                            if self.handle_list_emojis(&room, &mut stripped_body).await { return; };
                            if self.handle_register_emoji(room, &mut sender, &mut stripped_body).await { return; }

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

    async fn handle_list(&self, room: &Joined, stripped_body: &mut String) -> bool {
        if stripped_body == "!list" {
            let answer = get_user_list_answer(&self.conn, &room);
            let content = RoomMessageEventContent::text_html(answer.text, answer.html);
            room.send(content, None).await.unwrap();
            true;
        }
        false
    }

    async fn handle_list_emojis(&self, room: &Joined, stripped_body: &mut String) -> bool {
        if stripped_body == "!list_emoji" || stripped_body == "!list-emoji" || stripped_body == "!list_emojis" || stripped_body == "!list-emojis" {
            let answer = get_emoji_list_answer(&self.conn, &room);
            let content = RoomMessageEventContent::text_html(answer.text, answer.html);
            room.send(content, None).await.unwrap();
            true;
        }
        false
    }

    async fn handle_register_emoji(&self, room: Joined, sender: &mut Option<User>, body: &mut String) -> bool {
        if body.starts_with("!register_emoji") || body.starts_with("!register-emoji") {
            match sender.clone().unwrap().user_type {
                UserType::Admin => {},
                _ => {
                    room.send(RoomMessageEventContent::text_plain("You are not allowed to use this command"), None).await.unwrap();
                    return true;
                }
            }

            let error_message = "Invalid command usage! Example: !register-emoji 😑 -25";
            let mut text_opt = body.strip_prefix("!register_emoji");
            if text_opt.is_none() {
                text_opt = body.strip_prefix("!register-emoji");
                if text_opt.is_none() {
                    room.send(RoomMessageEventContent::text_plain(error_message), None).await.unwrap();
                    return true;
                }
            }
            let mut parts = text_opt.unwrap().split(" ").collect::<Vec<&str>>();
            if parts.len() == 3 && parts[0] == "" {
                parts.remove(0);
            }

            if parts.len() != 2 {
                room.send(RoomMessageEventContent::text_plain(error_message), None).await.unwrap();
                return true;
            }

            let emoji = parts[0];
            let social_credit_opt = parts[1].parse::<i32>();
            if social_credit_opt.is_err() || emoji.len() == 0 || emoji == " " {
                room.send(RoomMessageEventContent::text_plain(error_message), None).await.unwrap();
                return true;
            }
            let social_credit = social_credit_opt.unwrap();

            let room_id = &room.room_id().to_string();

            if self.find_emoji_in_cache(emoji, room_id).is_some() || find_emoji_in_db(&self.conn, &emoji.to_string(), room_id).is_some() {
                room.send(RoomMessageEventContent::text_plain("Emoji already registered"), None).await.unwrap();
                return true;
            }

            let emoji = Emoji {
                id: -1,
                room_id: room_id.to_string(),
                emoji: emoji.to_string(),
                social_credit,
            };

            insert_emoji(&self.conn, &emoji).unwrap();
            self.cached_emojis.lock().unwrap().push(emoji.clone());
            room.send(RoomMessageEventContent::text_plain(format!("Emoji registered: {} with social credit score: {}", emoji.emoji, emoji.social_credit)), None).await.unwrap();
            true;
        }
        false
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

    fn find_emoji_in_cache(&self, emoji_text: &str, room_id: &String) -> Option<Emoji> {
        let emojis_guard = self.cached_emojis.lock().unwrap();
        for emoji in emojis_guard.iter() {
            if emoji.emoji == emoji_text && &emoji.room_id == room_id {
                return Some(emoji.clone());
            }
        }
        None
    }
}

