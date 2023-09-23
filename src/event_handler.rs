use std::sync::{Arc, Mutex};
use matrix_sdk::Client;
use matrix_sdk::room::{Joined, Room};
use matrix_sdk::ruma::{events};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnyTimelineEvent};
use matrix_sdk::ruma::events::room::encrypted::Relation;
use matrix_sdk::ruma::events::room::message::{MessageType, RoomMessageEventContent};
use rusqlite::Connection;
use crate::data::emoji::{Emoji, find_all_emoji_in_db, find_emoji_in_db, insert_emoji};
use crate::data::event::{Event, find_event_in_db, insert_event};
use crate::data::user::{find_all_users_with_social_credit_in_db, update_user, User, UserType};
use crate::data::user_social_credit::update_user_social_credit;
use crate::utils::emoji_util::get_emoji_list_answer;
use crate::utils::user_util::{extract_userdata_from_string, get_user_list_answer, setup_user};


pub struct EventHandler {
    conn: Arc<Mutex<Connection>>,
    client: Client,
    cached_emojis: Arc<Mutex<Vec<Emoji>>>,
    cached_users: Arc<Mutex<Vec<User>>>,
    bot_username: String,
    homeserver_url: String,
    initial_social_credit: i32,
}

impl EventHandler {
    pub fn new(conn: Arc<Mutex<Connection>>, client: Client, bot_username: String, homeserver_url: String, initial_social_credit: i32) -> Self {
        let users: Vec<User> = find_all_users_with_social_credit_in_db(&conn).unwrap_or(Vec::new());
        let emojis: Vec<Emoji> = find_all_emoji_in_db(&conn).unwrap_or(Vec::new());
        EventHandler {
            conn,
            client,
            cached_emojis: Arc::new(Mutex::new(emojis)),
            cached_users: Arc::new(Mutex::new(users)),
            bot_username,
            homeserver_url,
            initial_social_credit,
        }
    }

    pub async fn on_message_like_event(&self, event: AnySyncMessageLikeEvent, room: Room) {
        match room {
            Room::Joined(room) => {
                //println!("Received a AnySyncMessageLikeEvent, type: {:?}, event {:?}", event.event_type().to_string(), event); // debug level

                // Check if we already handled this event
                let handled_event = find_event_in_db(&self.conn, &event.event_id().to_string());
                if handled_event.is_some() {
                    println!("Event {} already handled", handled_event.unwrap().id); // debug level
                    return;
                }

                let new_handled_event = Event {
                    id: event.event_id().to_string(),
                    event_type: event.event_type().to_string(),
                    handled: true,
                };
                if insert_event(&self.conn, &new_handled_event).is_err() {
                    println!("Unable to insert event {} into db", new_handled_event.id); // debug level
                    return;
                }

                // Check if the sender is the bot itself
                if self.handle_user_tag_is_the_bot(&event) { return; }

                let mut sender = self.find_user_in_cache_by_tag(&event.sender().to_string());
                if sender.is_none() {
                    sender = setup_user(&self.conn, Some(room.clone()), &event.sender().to_string(), UserType::Default, self.initial_social_credit);
                    if sender.is_none() {
                        println!("Sender is none"); // debug level
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
                    if event.original_content().is_none() {
                        println!("Recieved a m.reaction event without original_content. event: {:?}", event); // debug level
                        return;
                    }

                    match event.original_content().unwrap() {
                        events::AnyMessageLikeEventContent::Reaction(content) => {
                            println!("Reaction content {:?}", content);
                            let emoji = self.find_emoji_in_cache(&content.relates_to.key, &room.room_id().to_string());
                            if emoji.is_none() {
                                println!("Emoji {} is not registered", content.relates_to.key); // debug level
                                return;
                            }

                            // todo unwrap usage
                            let relation = &event.original_content().unwrap().relation();
                            if relation.is_none() {
                                println!("Relation is none");
                                return;
                            }

                            // todo unwrap usage
                            match relation.clone().unwrap().clone() {
                                Relation::Annotation(annotation) => {
                                    println!("Event id {:?}", annotation.event_id);
                                    let message_event = room.event(&*annotation.event_id).await;
                                    if message_event.is_err() {
                                        println!("Unable to get the message event that relates to this reaction event");
                                        return;
                                    }

                                    let message_event = message_event.unwrap().event;
                                    match message_event.deserialize().unwrap() { // todo unwrap usage
                                        AnyTimelineEvent::MessageLike(message_like_event) => {
                                            println!("Message like event {:?}", message_like_event);
                                            println!("Sender: {}", message_like_event.sender().to_string());

                                            // The sender here is the user where the social credit score should be changed, so it is the recipient of the reaction
                                            let recipient_user_tag = message_like_event.sender().to_string();
                                            let mut recipient_opt = self.find_user_in_cache_by_tag(&recipient_user_tag);
                                            if recipient_opt.is_none() {
                                                recipient_opt = setup_user(&self.conn, Some(room.clone()), &recipient_user_tag, UserType::Default, self.initial_social_credit);
                                                if recipient_opt.is_none() {
                                                    println!("Recipient of reaction is none");
                                                    return;
                                                }
                                                // We do not need to add the user to the cache here, the update_user_in_cache_and_db below does it
                                            }

                                            let mut recipient = recipient_opt.clone().unwrap();

                                            if self.is_user_the_bot(&recipient.name, &recipient.url) {
                                                println!("Recipient of reaction is the bot itself"); // debug level
                                                return;
                                            }

                                            if recipient.social_credit.is_none() {
                                                println!("Recipient of reaction does not have a social credit score"); // error level
                                                return;
                                            }

                                            let mut recipient_social_credit = recipient.social_credit.unwrap();
                                            recipient_social_credit.social_credit += emoji.clone().unwrap().social_credit;
                                            recipient.social_credit = Some(recipient_social_credit);
                                            self.update_user_in_cache_and_db(&recipient);
                                            let text = format!("<b>{}'s</b> Social Credit Score: <b>{}</b>", recipient.name, recipient.social_credit.unwrap().social_credit);
                                            room.send(RoomMessageEventContent::text_html(
                                                text.clone(),
                                                text
                                            ), None).await.unwrap();
                                        },
                                        _ => {}
                                    }
                                },
                                _ => {}
                            }
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

                            // commands
                            let mut stripped_body: String = body.to_string();
                            if body.starts_with("* ") {
                                stripped_body = body.strip_prefix("* ").unwrap().to_string();
                            }

                            if self.handle_help(&room, &mut stripped_body).await { return; };
                            if self.handle_list(&room, &mut stripped_body).await { return; };
                            if self.handle_list_emojis(&room, &mut stripped_body).await { return; };
                            if self.handle_register_emoji(room, &mut sender, &mut stripped_body).await { return; }
                        }
                        _ => {}
                    }
                }
            },
            _ => { return }
        }
    }

    fn handle_user_tag_is_the_bot(&self, event: &AnySyncMessageLikeEvent) -> bool {
        let sender_userdata = extract_userdata_from_string(event.sender().to_string().as_str());
        if sender_userdata.is_some() {
            let sender_userdata = sender_userdata.unwrap();
            if self.is_user_the_bot(&*sender_userdata.0, &*sender_userdata.1) {
                println!("Received a message from the bot itself, event: {:?}", event); // debug level
                return true;
            }
        }
        false
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

    async fn handle_help(&self, room: &Joined, stripped_body: &mut String) -> bool {
        if stripped_body == "!help" {
            let help_body = "<h3>Commands:</h3>
                - <b>!list</b>: List all users and their social credit score for the current room<br>
                - <b>!list_emoji</b>: List all registered emojis and their social credit score for the current room<br>
                - <b>!register_emoji</b> <emoji> <social_credit>: Register an emoji with a social credit score for the current room. Example: !register_emoji 😑 -25
            ".to_string();
            let content = RoomMessageEventContent::text_html(help_body.clone(), help_body);
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

    /// Update the user in the cache and the database, also updates the social credit score in the database
    /// if the user has a social credit score
    fn update_user_in_cache_and_db(&self, user: &User) {
        let mut found = false;
        let mut users_guard = self.cached_users.lock().unwrap();
        for cached_user in users_guard.iter_mut() {
            if cached_user.id == user.id {
                found = true;
                *cached_user = user.clone();
                break;
            }
        }

        // Add user to cache if not found
        if !found {
            users_guard.push(user.clone());
        }

        if user.social_credit.is_some() {
            if update_user_social_credit(&self.conn, &user.clone().social_credit.unwrap()).is_err() {
                println!("Unable to update user social credit in db"); // error level
            }
        }

        if update_user(&self.conn, &user).is_err() {
            println!("Unable to update user in db"); // error level
        }
    }

    fn find_user_in_cache_by_tag(&self, tag: &str) -> Option<User> {
        let userdata_opt = extract_userdata_from_string(tag);
        if userdata_opt.is_none() {
            println!("Invalid user tag: {}", tag); // debug level
            return None;
        }
        let userdata = userdata_opt.unwrap();
        self.find_user_in_cache(&*userdata.0, &*userdata.1)
    }

    fn find_user_in_cache(&self, name: &str, url: &str) -> Option<User> {
        let users_guard = self.cached_users.lock().unwrap();
        for user in users_guard.iter() {
            if user.name == name && user.url == url {
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

    fn is_user_the_bot(&self, name: &str, url: &str) -> bool {
        name == self.bot_username && url == self.homeserver_url
    }
}

