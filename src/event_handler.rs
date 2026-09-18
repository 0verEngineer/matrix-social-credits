use crate::data::activity::{ActivityKind, record_activity};
use crate::data::emoji::{Emoji, delete_emoji, find_emoji_in_db, insert_emoji};
use crate::data::event::{Event, find_event_in_db, insert_event};
use crate::data::room::{is_room_active, set_room_active};
use crate::data::user::{HtmlAndTextAnswer, User, UserType, find_user_in_db};
use crate::data::user_room_data::{
    add_social_credit, set_social_credit, set_social_credit_for_users, users_with_room_data,
};
use crate::utils::emoji_util::{get_emoji_list_answer, normalize_emoji};
use crate::utils::matrix_util::send_message;
use crate::utils::message::{escape_html, heading, notice_html, notice_plain};
use crate::utils::user_util::{
    compare_user, current_room_members, get_user_list_answer, resolve_configured_user_id,
    setup_user, split_user_id,
};
use matrix_sdk::ruma::events;
use matrix_sdk::ruma::events::room::message::{MessageType, Relation};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use matrix_sdk::ruma::{EventId, OwnedUserId, UserId};
use matrix_sdk::{Room, RoomState};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, trace};

pub struct EventHandler {
    conn: Arc<Mutex<Connection>>,
    /// The bot's own user id, as reported by the homeserver after login.
    ///
    /// This replaces the previous name plus homeserver-URL pair. Deriving the domain from
    /// `MATRIX_HOMESERVER_URL` is wrong whenever `.well-known` delegation is in play, because
    /// then the URL host and the server name in the user id are different hosts.
    own_user_id: OwnedUserId,
    initial_social_credit: i32,
    reaction_period_minutes: i32,
    reaction_limit: i32,
    /// Whether messages and images are counted towards the weekly payout.
    activity_enabled: bool,
}

impl EventHandler {
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        own_user_id: OwnedUserId,
        initial_social_credit: i32,
        reaction_period_minutes: i32,
        reaction_limit: i32,
        activity_enabled: bool,
    ) -> Self {
        EventHandler {
            conn,
            own_user_id,
            initial_social_credit,
            reaction_period_minutes,
            reaction_limit,
            activity_enabled,
        }
    }

    pub async fn on_message_like_event(&self, event: AnySyncMessageLikeEvent, room: Room) {
        // matrix-sdk 0.7 replaced the Room::Joined / Room::Invited / Room::Left enum with a
        // single Room type that carries its membership state.
        if room.state() != RoomState::Joined {
            return;
        }

        if self.check_and_handle_event_already_handled(&event) {
            return;
        }
        if self.handle_sender_is_the_bot(&event) {
            return;
        }

        // Nothing is counted, scored or answered in a room until the admin has switched it
        // on. The bot joins every room it is invited into, and before this check it went to
        // work in all of them -- old test rooms, the welcome room it cannot be kicked out of.
        // This comes before setup_user on purpose: an inactive room must not leave user rows
        // behind either.
        if !self.is_room_active(room.room_id().as_str()) {
            self.on_event_in_inactive_room(&event, &room).await;
            return;
        }

        let sender = setup_user(
            &self.conn,
            Some(room.room_id().as_str()),
            event.sender(),
            UserType::Default,
            self.initial_social_credit,
        );
        if sender.is_none() {
            debug!(sender = %event.sender(), "Unable to resolve the sender of the event");
            return;
        }

        // Matrix does not support stickers in tagged messages so we cannot use stickers at the moment

        if event.event_type().to_string() == "m.reaction" {
            let sender = sender.clone().unwrap();
            if event.original_content().is_none() {
                debug!(event_id = %event.event_id(), "Received a m.reaction event without original_content");
                return;
            }

            if let events::AnyMessageLikeEventContent::Reaction(content) =
                event.original_content().unwrap()
            {
                trace!(?content, "Reaction content");
                let emoji_text = normalize_emoji(&content.relates_to.key);

                let emoji = find_emoji_in_db(&self.conn, &emoji_text, &room.room_id().to_string());
                if emoji.is_none() {
                    debug!(emoji = %content.relates_to.key, "Emoji is not registered");
                    return;
                }
                let emoji = emoji.unwrap();

                if sender.room_data.is_none() {
                    error!(sender = %sender.name, "Sender of reaction does not have room data");
                    return;
                }

                let sender_user_room_data = sender.clone().room_data.unwrap();

                // The reaction points at the message it annotates. In ruma 0.16 the annotation is
                // reachable directly via `relates_to`, the Relation enum detour is gone.
                let annotated_event_id = content.relates_to.event_id.clone();
                let message_event = room.event(&annotated_event_id, None).await;
                if message_event.is_err() {
                    error!(event_id = %annotated_event_id, "Unable to fetch the message event this reaction relates to");
                    return;
                }

                let message_event = message_event.unwrap();
                let deserialized_event = match message_event.raw().deserialize() {
                    Ok(event) => event,
                    Err(e) => {
                        error!(event_id = %annotated_event_id, error = %e, "Unable to deserialize message event");
                        return;
                    }
                };
                trace!(?deserialized_event, "Annotated event");

                // The author of the annotated event is the user whose score changes, so the
                // recipient of the reaction.
                if let Some((recipient_user_id, annotated_message_id)) =
                    annotated_event_author(&deserialized_event)
                {
                    trace!(sender = %recipient_user_id, "Recipient of the reaction");

                    if self.is_user_the_bot(recipient_user_id) {
                        debug!("Recipient of reaction is the bot itself");
                        return;
                    }

                    let recipient_opt = setup_user(
                        &self.conn,
                        Some(room.room_id().as_str()),
                        recipient_user_id,
                        UserType::Default,
                        self.initial_social_credit,
                    );
                    let Some(mut recipient) = recipient_opt else {
                        debug!(user = %recipient_user_id, "Unable to resolve the recipient of the reaction");
                        return;
                    };

                    let annotated_message_id = annotated_message_id.to_string();

                    if sender_user_room_data.has_user_already_reacted_to_message_event_id(
                        &self.conn,
                        &annotated_message_id,
                    ) {
                        debug!(sender = %format_args!("@{}:{}", sender.name, sender.url), event_id = %event.event_id(), "Sender already reacted to this message event");
                        return;
                    }

                    if compare_user(&recipient, &sender) {
                        debug!("Sender and recipient of reaction are the same user");
                        return;
                    }

                    if recipient.room_data.is_none() {
                        error!(user = %recipient.name, "Recipient of reaction does not have room data");
                        return;
                    }

                    // The cooldown is checked last, once it is clear that this reaction would
                    // actually count. Checking it up front meant telling a user to wait for a
                    // reaction that was going to be dropped anyway -- a duplicate, or a
                    // reaction to their own message.
                    let time_till_user_can_react = sender_user_room_data
                        .get_time_till_user_can_react(
                            &self.conn,
                            self.reaction_period_minutes,
                            self.reaction_limit,
                        );
                    if time_till_user_can_react > 0 {
                        let minutes = time_till_user_can_react / 60;
                        let seconds = time_till_user_can_react % 60;
                        let text = format!(
                            "{}, you are still on cooldown, remaining time: {}m {}s",
                            sender.name, minutes, seconds
                        );
                        let html = escape_html(&text);
                        send_message(&room, notice_html(text, html)).await;
                        return;
                    }

                    let recipient_room_data = recipient.room_data.take().unwrap();
                    let old_social_credit = recipient_room_data.social_credit;

                    let new_social_credit = match add_social_credit(
                        &self.conn,
                        recipient_room_data.user_id,
                        &recipient_room_data.room_id,
                        emoji.social_credit,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            error!(user = %recipient.name, %error, "Unable to update the social credit score");
                            return;
                        }
                    };

                    sender
                        .room_data
                        .unwrap()
                        .add_reaction(&self.conn, &annotated_message_id);

                    // The plaintext body used to be the HTML string, so clients without HTML
                    // rendering and push notifications showed the raw <b> tags.
                    let plain = format!(
                        "{} changed {}'s Social Credit Score using {} from {} to {}",
                        sender.name,
                        recipient.name,
                        emoji.emoji,
                        old_social_credit,
                        new_social_credit
                    );
                    let html = format!(
                        "<b>{}</b> changed <b>{}'s</b> Social Credit Score using {} from <b>{}</b> to <b>{}</b>",
                        escape_html(&sender.name),
                        escape_html(&recipient.name),
                        escape_html(&emoji.emoji),
                        old_social_credit,
                        new_social_credit
                    );
                    send_message(&room, notice_html(plain, html)).await;
                }
            }
        }

        if event.event_type().to_string() == "m.room.message" {
            if event.original_content().is_none() {
                debug!(event_id = %event.event_id(), "Received a m.room.message event without original_content");
                return;
            }

            let sender = sender.unwrap();

            if let events::AnyMessageLikeEventContent::RoomMessage(content) =
                event.original_content().unwrap()
            {
                // Text and emotes count as messages, images as images. Everything else --
                // video, audio, files, locations, notices -- is left alone rather than
                // guessed at; reactions never reach this branch at all.
                let activity = match content.msgtype {
                    MessageType::Text(..) | MessageType::Emote(..) => Some(ActivityKind::Message),
                    MessageType::Image(..) => Some(ActivityKind::Image),
                    _ => None,
                };
                if activity.is_none() {
                    return;
                }
                let is_text = matches!(content.msgtype, MessageType::Text(..));

                // An edit carries the new text prefixed with "* " in its fallback body. The
                // old code stripped that prefix and then ran the command again, so editing a
                // "!list" message re-triggered it -- and a plain message starting with "* "
                // (a markdown bullet) was parsed as a command. Edits are ignored instead; the
                // original event was already handled when it arrived. They must not count
                // towards the payout either, or editing a message ten times would pay ten
                // times.
                if matches!(content.relates_to, Some(Relation::Replacement(_))) {
                    trace!(event_id = %event.event_id(), "Ignoring an edit");
                    return;
                }

                let was_a_command = if is_text {
                    self.handle_command(&room, &sender, content.body().trim())
                        .await
                } else {
                    false
                };

                // Talking to the bot is not an achievement.
                if was_a_command {
                    return;
                }

                if let Some(kind) = activity {
                    self.count_activity(&sender, kind);
                }
            }
        }
    }

    /// Dispatch a chat command, reporting whether the message was one.
    ///
    /// Each handler used to be called in sequence behind `if handler(..).await { return; }`,
    /// but every one of them ended in `true;` -- a statement, not a return value -- so they
    /// all returned `false` and none of those early returns ever fired.
    ///
    /// The return value is what keeps commands out of the activity count, without a second
    /// list of command names that could drift away from this one.
    async fn handle_command(&self, room: &Room, sender: &User, body: &str) -> bool {
        let (command, arguments) = split_command(body);

        match command {
            "!help" => self.handle_help(room).await,
            "!list" => self.handle_list(room).await,
            "!list_emoji" | "!list-emoji" | "!list_emojis" | "!list-emojis" => {
                self.handle_list_emojis(room).await
            }
            "!register_emoji" | "!register-emoji" => {
                self.handle_register_emoji(room, sender, arguments).await
            }
            "!unregister_emoji" | "!unregister-emoji" => {
                self.handle_unregister_emoji(room, sender, arguments).await
            }
            "!set_score" | "!set-score" => self.handle_set_score(room, sender, arguments).await,
            "!set_score_all" | "!set-score-all" => {
                self.handle_set_score_all(room, sender, arguments).await
            }
            "!activate" => {
                if self.require_admin(room, sender).await {
                    self.handle_activate(room).await
                }
            }
            "!deactivate" => {
                if self.require_admin(room, sender).await {
                    self.handle_deactivate(room).await
                }
            }
            _ => return false,
        }

        true
    }

    /// What still gets through in a room that is not active.
    ///
    /// `!help` is answered for everybody, so that somebody who finds the bot in a room can
    /// find out what it is and why it is quiet. `!activate` and `!deactivate` only for the
    /// admin; everybody else is ignored without a word there. An answer -- even "you are not
    /// allowed" -- would be the bot making noise in a room it has been told to stay out of.
    async fn on_event_in_inactive_room(&self, event: &AnySyncMessageLikeEvent, room: &Room) {
        let Some(body) = plain_text_body(event) else {
            return;
        };
        let (command, _) = split_command(&body);

        match command {
            "!help" => {
                let answer = help_answer(false);
                send_message(room, notice_html(answer.text, answer.html)).await;
            }
            "!activate" | "!deactivate" => {
                if !self.is_admin(event.sender()) {
                    trace!(sender = %event.sender(), room_id = %room.room_id(), "Ignoring a command in an inactive room");
                    return;
                }
                if command == "!activate" {
                    self.handle_activate(room).await
                } else {
                    self.handle_deactivate(room).await
                }
            }
            _ => {}
        }
    }

    fn is_room_active(&self, room_id: &str) -> bool {
        let connection = match self.conn.lock() {
            Ok(connection) => connection,
            Err(_) => {
                error!("Database mutex is poisoned");
                return false;
            }
        };
        match is_room_active(&connection, room_id) {
            Ok(active) => active,
            Err(error) => {
                error!(room_id, %error, "Unable to read whether the room is active");
                false
            }
        }
    }

    fn set_room_active(&self, room_id: &str, active: bool) -> bool {
        let connection = match self.conn.lock() {
            Ok(connection) => connection,
            Err(_) => {
                error!("Database mutex is poisoned");
                return false;
            }
        };
        match set_room_active(&connection, room_id, active) {
            Ok(()) => true,
            Err(error) => {
                error!(room_id, active, %error, "Unable to store whether the room is active");
                false
            }
        }
    }

    /// Whether `user_id` is the admin, without creating anything.
    ///
    /// `setup_user` would do the lookup too, but it inserts the user when it does not find
    /// one. The admin row always exists, it is written at startup.
    fn is_admin(&self, user_id: &UserId) -> bool {
        let (name, url) = split_user_id(user_id);
        find_user_in_db(&self.conn, &name, &url)
            .is_some_and(|user| matches!(user.user_type, UserType::Admin))
    }

    /// Switch the bot on in this room. Nothing is reset: whatever scores, emojis and
    /// counters the room already has carry on from where they were.
    async fn handle_activate(&self, room: &Room) {
        let room_id = room.room_id().as_str();

        if self.is_room_active(room_id) {
            send_message(room, notice_plain("This room is already active")).await;
            return;
        }
        if !self.set_room_active(room_id, true) {
            send_message(room, notice_plain("Failed to activate this room")).await;
            return;
        }

        info!(room_id, "Room activated");
        send_message(
            room,
            notice_plain("The social credit system is now active in this room"),
        )
        .await;
    }

    /// Switch the bot off in this room. The data stays, so `!activate` picks up again where
    /// this left off.
    async fn handle_deactivate(&self, room: &Room) {
        let room_id = room.room_id().as_str();

        if !self.is_room_active(room_id) {
            send_message(room, notice_plain("This room is not active")).await;
            return;
        }
        if !self.set_room_active(room_id, false) {
            send_message(room, notice_plain("Failed to deactivate this room")).await;
            return;
        }

        info!(room_id, "Room deactivated");
        send_message(
            room,
            notice_plain(
                "The social credit system is now inactive in this room. Scores and emojis are kept; !activate switches it back on",
            ),
        )
        .await;
    }

    /// Count one message or image towards the weekly payout.
    fn count_activity(&self, sender: &User, kind: ActivityKind) {
        if !self.activity_enabled {
            return;
        }

        let Some(room_data) = sender.room_data.as_ref() else {
            debug!(user = %sender.name, "No room data, not counting the activity");
            return;
        };

        record_activity(&self.conn, room_data.id, kind);
    }

    /// Answer with the usage hint unless the sender is an admin.
    ///
    /// Returns `true` when the sender may go ahead.
    async fn require_admin(&self, room: &Room, sender: &User) -> bool {
        if matches!(sender.user_type, UserType::Admin) {
            return true;
        }

        send_message(
            room,
            notice_plain("You are not allowed to use this command"),
        )
        .await;
        false
    }

    fn check_and_handle_event_already_handled(&self, event: &AnySyncMessageLikeEvent) -> bool {
        let handled_event = find_event_in_db(&self.conn, &event.event_id().to_string());
        if let Some(handled_event) = handled_event {
            debug!(event_id = %handled_event.id, "Event already handled");
            return true;
        }

        let new_handled_event = Event {
            id: event.event_id().to_string(),
            event_type: event.event_type().to_string(),
            handled: true,
        };
        if insert_event(&self.conn, &new_handled_event).is_err() {
            error!(event_id = %new_handled_event.id, "Unable to insert event into db");
            return true;
        }
        false
    }

    fn handle_sender_is_the_bot(&self, event: &AnySyncMessageLikeEvent) -> bool {
        if self.is_user_the_bot(event.sender()) {
            trace!(event_id = %event.event_id(), "Received a message from the bot itself");
            return true;
        }
        false
    }

    async fn handle_list(&self, room: &Room) {
        let answer = get_user_list_answer(&self.conn, room, &self.own_user_id).await;
        send_message(room, notice_html(answer.text, answer.html)).await;
    }

    async fn handle_list_emojis(&self, room: &Room) {
        let answer = get_emoji_list_answer(&self.conn, room);
        send_message(room, notice_html(answer.text, answer.html)).await;
    }

    async fn handle_help(&self, room: &Room) {
        let answer = help_answer(true);
        send_message(room, notice_html(answer.text, answer.html)).await;
    }

    async fn handle_register_emoji(&self, room: &Room, sender: &User, arguments: &str) {
        if !self.require_admin(room, sender).await {
            return;
        }

        let error_message = "Invalid command usage! Example: !register-emoji 😑 -25";

        // split_whitespace also copes with several spaces between the arguments, which the
        // previous split(" ") plus "drop a leading empty part" special case did not.
        let parts = arguments.split_whitespace().collect::<Vec<&str>>();
        if parts.len() != 2 {
            send_message(room, notice_plain(error_message)).await;
            return;
        }

        let emoji_text = normalize_emoji(parts[0]);
        let Ok(social_credit) = parts[1].parse::<i32>() else {
            send_message(room, notice_plain(error_message)).await;
            return;
        };

        if emoji_text.is_empty() {
            send_message(room, notice_plain(error_message)).await;
            return;
        }

        let room_id = &room.room_id().to_string();

        if find_emoji_in_db(&self.conn, &emoji_text, room_id).is_some() {
            send_message(room, notice_plain("Emoji already registered")).await;
            return;
        }

        let emoji = Emoji {
            id: -1,
            room_id: room_id.to_string(),
            emoji: emoji_text,
            social_credit,
        };

        if insert_emoji(&self.conn, &emoji).is_err() {
            error!(emoji = %emoji.emoji, "Unable to insert emoji into db");
            send_message(room, notice_plain("Failed to register the emoji")).await;
            return;
        }

        send_message(
            room,
            notice_plain(format!(
                "Emoji registered: {} with social credit score: {}",
                emoji.emoji, emoji.social_credit
            )),
        )
        .await;
    }

    /// Remove a registered emoji again.
    ///
    /// Without this a typo in `!register_emoji` was permanent -- there was no way to correct
    /// or drop an entry short of editing the database by hand.
    async fn handle_unregister_emoji(&self, room: &Room, sender: &User, arguments: &str) {
        if !self.require_admin(room, sender).await {
            return;
        }

        let error_message = "Invalid command usage! Example: !unregister-emoji 😑";

        let parts = arguments.split_whitespace().collect::<Vec<&str>>();
        if parts.len() != 1 {
            send_message(room, notice_plain(error_message)).await;
            return;
        }

        let emoji_text = normalize_emoji(parts[0]);
        let room_id = room.room_id().to_string();

        if find_emoji_in_db(&self.conn, &emoji_text, &room_id).is_none() {
            send_message(room, notice_plain("Emoji is not registered")).await;
            return;
        }

        if delete_emoji(&self.conn, &emoji_text, &room_id).is_err() {
            error!(emoji = %emoji_text, "Unable to delete emoji from db");
            send_message(room, notice_plain("Failed to remove the emoji")).await;
            return;
        }

        send_message(room, notice_plain(format!("Emoji removed: {emoji_text}"))).await;
    }

    /// Put one user's score in this room at exactly `value`.
    ///
    /// The user has to be in the room. That is what keeps a typo in the name from creating a
    /// row for somebody who does not exist; for a member without a score yet the row is
    /// created, the admin clearly wants them to have this one.
    async fn handle_set_score(&self, room: &Room, sender: &User, arguments: &str) {
        if !self.require_admin(room, sender).await {
            return;
        }

        let error_message = "Invalid command usage! Example: !set_score alice 250";

        let parts = arguments.split_whitespace().collect::<Vec<&str>>();
        if parts.len() != 2 {
            send_message(room, notice_plain(error_message)).await;
            return;
        }
        let Ok(value) = parts[1].parse::<i32>() else {
            send_message(room, notice_plain(error_message)).await;
            return;
        };

        // A bare localpart is resolved against the bot's own server, the same way
        // ADMIN_USERNAME is.
        let Some(user_id) = resolve_configured_user_id(parts[0], self.own_user_id.server_name())
        else {
            send_message(room, notice_plain(error_message)).await;
            return;
        };
        if self.is_user_the_bot(&user_id) {
            send_message(room, notice_plain("The bot does not have a score")).await;
            return;
        }

        let Some(members) = current_room_members(room).await else {
            send_message(room, notice_plain("Unable to load the room member list")).await;
            return;
        };
        if !members.contains(&split_user_id(&user_id)) {
            send_message(
                room,
                notice_plain(format!("{user_id} is not a member of this room")),
            )
            .await;
            return;
        }

        let room_id = room.room_id().as_str();
        let Some(user) = setup_user(
            &self.conn,
            Some(room_id),
            &user_id,
            UserType::Default,
            self.initial_social_credit,
        ) else {
            send_message(room, notice_plain("Failed to look up the user")).await;
            return;
        };

        let previous = match set_social_credit(&self.conn, user.id, room_id, value) {
            Ok(previous) => previous,
            Err(error) => {
                error!(user = %user_id, %error, "Unable to set the social credit score");
                send_message(room, notice_plain("Failed to set the score")).await;
                return;
            }
        };

        info!(room_id, user = %user_id, previous, value, admin = %sender.name, "Score set by the admin");

        let plain = format!(
            "{} set {}'s Social Credit Score from {} to {}",
            sender.name, user.name, previous, value
        );
        let html = format!(
            "<b>{}</b> set <b>{}'s</b> Social Credit Score from <b>{}</b> to <b>{}</b>",
            escape_html(&sender.name),
            escape_html(&user.name),
            previous,
            value
        );
        send_message(room, notice_html(plain, html)).await;
    }

    /// Put everybody's score in this room at exactly `value`.
    ///
    /// "Everybody" is everyone who is in the room and already has a score here -- the same
    /// people `!list` shows. Members who never sent anything have no score and get none;
    /// people who left keep theirs, so that a return does not start from a value set while
    /// they were away.
    async fn handle_set_score_all(&self, room: &Room, sender: &User, arguments: &str) {
        if !self.require_admin(room, sender).await {
            return;
        }

        let error_message = "Invalid command usage! Example: !set_score_all 250";

        let parts = arguments.split_whitespace().collect::<Vec<&str>>();
        if parts.len() != 1 {
            send_message(room, notice_plain(error_message)).await;
            return;
        }
        let Ok(value) = parts[0].parse::<i32>() else {
            send_message(room, notice_plain(error_message)).await;
            return;
        };

        let Some(members) = current_room_members(room).await else {
            send_message(room, notice_plain("Unable to load the room member list")).await;
            return;
        };

        let room_id = room.room_id().as_str();
        // The lock is released before anything is awaited; a guard held across an await
        // makes the handler future !Send.
        let users = {
            let connection = match self.conn.lock() {
                Ok(connection) => connection,
                Err(_) => {
                    error!("Database mutex is poisoned");
                    return;
                }
            };
            users_with_room_data(&connection, room_id)
        };
        let present: Vec<i32> = match users {
            Ok(users) => users
                .into_iter()
                .filter(|user| members.contains(&(user.name.clone(), user.url.clone())))
                .map(|user| user.user_id)
                .collect(),
            Err(error) => {
                error!(room_id, %error, "Unable to load the users of the room");
                send_message(room, notice_plain("Failed to set the scores")).await;
                return;
            }
        };

        let changed = match set_social_credit_for_users(&self.conn, room_id, &present, value) {
            Ok(changed) => changed,
            Err(error) => {
                error!(room_id, %error, "Unable to set the social credit scores");
                send_message(room, notice_plain("Failed to set the scores")).await;
                return;
            }
        };

        info!(room_id, changed, value, admin = %sender.name, "Scores set by the admin");

        let who = if changed == 1 {
            "1 user".to_owned()
        } else {
            format!("{changed} users")
        };
        let plain = format!(
            "{} set the Social Credit Score of {} to {}",
            sender.name, who, value
        );
        let html = format!(
            "<b>{}</b> set the Social Credit Score of <b>{}</b> to <b>{}</b>",
            escape_html(&sender.name),
            who,
            value
        );
        send_message(room, notice_html(plain, html)).await;
    }

    fn is_user_the_bot(&self, user_id: &UserId) -> bool {
        user_id == self.own_user_id
    }
}

/// Commands everybody may use, and the ones only the admin may.
///
/// Two lists rather than one with a "(admin)" marker: somebody who is not the admin can stop
/// reading at the first heading.
const COMMANDS: [(&str, &str); 3] = [
    ("!help", "Show this list"),
    (
        "!list",
        "List all users and their social credit score for the current room",
    ),
    (
        "!list_emoji",
        "List all registered emojis and their social credit score for the current room",
    ),
];

const ADMIN_COMMANDS: [(&str, &str); 6] = [
    (
        "!register_emoji <emoji> <social_credit>",
        "Register an emoji with a social credit score for the current room. Example: !register_emoji 😑 -25",
    ),
    (
        "!unregister_emoji <emoji>",
        "Remove a registered emoji from the current room. Example: !unregister_emoji 😑",
    ),
    (
        "!set_score <user> <social_credit>",
        "Set one user's social credit score in the current room. Example: !set_score alice 250",
    ),
    (
        "!set_score_all <social_credit>",
        "Set the social credit score of everybody in the current room who has one. Example: !set_score_all 250",
    ),
    (
        "!activate",
        "Switch the bot on in the current room. Nothing is counted, scored or answered before that",
    ),
    (
        "!deactivate",
        "Switch the bot off in the current room again. Scores and emojis are kept",
    ),
];

/// The `!help` answer.
///
/// In a room that is not active it ends with a line saying so; otherwise the list of
/// commands reads as if they worked here, and none of them do.
///
/// The placeholders have to be escaped. As literal <emoji> and <social_credit> they were
/// swallowed by every client as unknown HTML tags, so the help text read
/// "!register_emoji  : Register an emoji ...".
fn help_answer(room_is_active: bool) -> HtmlAndTextAnswer {
    fn section(title: &str, commands: &[(&str, &str)]) -> (String, String) {
        let plain = std::iter::once(title.to_owned())
            .chain(
                commands
                    .iter()
                    .map(|(usage, description)| format!("- {usage}: {description}")),
            )
            .collect::<Vec<_>>()
            .join("\n");

        let html = format!(
            "{}{}",
            heading(title),
            commands
                .iter()
                .map(|(usage, description)| format!(
                    "<b>{}</b>: {}",
                    escape_html(usage),
                    escape_html(description)
                ))
                .collect::<Vec<_>>()
                .join("<br>")
        );

        (plain, html)
    }

    let (plain, html) = section("Commands:", &COMMANDS);
    let (admin_plain, admin_html) = section("Admin commands:", &ADMIN_COMMANDS);

    let mut text = format!("{plain}\n\n{admin_plain}");
    let mut html = format!("{html}<br><br>{admin_html}");

    if !room_is_active {
        let note = "This room is not active. Nothing is counted or scored here until the admin sends !activate.";
        text.push_str(&format!("\n\n{note}"));
        html.push_str(&format!("<br><br><i>{}</i>", escape_html(note)));
    }

    HtmlAndTextAnswer { text, html }
}

/// The body of a plain text message, or `None` for anything that is not one.
///
/// Edits are not messages here either -- the original was already handled when it arrived,
/// and its edited fallback body starts with "* ".
fn plain_text_body(event: &AnySyncMessageLikeEvent) -> Option<String> {
    let events::AnyMessageLikeEventContent::RoomMessage(content) = event.original_content()? else {
        return None;
    };
    if !matches!(content.msgtype, MessageType::Text(..)) {
        return None;
    }
    if matches!(content.relates_to, Some(Relation::Replacement(_))) {
        return None;
    }
    Some(content.body().trim().to_owned())
}

/// The author of the event a reaction points at, together with that event's id.
///
/// The annotated event is frequently one the bot cannot read: in an encrypted room it arrives
/// as `m.room.encrypted` whenever the bot has no key for it, and matrix-sdk hands the still
/// encrypted event back rather than failing. That is fine here -- sender and event id of an
/// encrypted event are in the clear, and those two are all that scoring needs. It is why a
/// reaction still counts for a message the bot could not decrypt.
fn annotated_event_author(event: &AnySyncTimelineEvent) -> Option<(&UserId, &EventId)> {
    match event {
        AnySyncTimelineEvent::MessageLike(event) => Some((event.sender(), event.event_id())),
        // A reaction to a state event; there is nobody to score for it.
        AnySyncTimelineEvent::State(_) => None,
    }
}

/// Split a message body into the command word and everything after it.
fn split_command(body: &str) -> (&str, &str) {
    let body = body.trim();
    let command = body.split_whitespace().next().unwrap_or_default();
    (command, body[command.len()..].trim())
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::events::AnySyncTimelineEvent;
    use matrix_sdk::ruma::serde::Raw;

    use super::{annotated_event_author, help_answer, split_command};

    /// A reaction points at a message. In an encrypted room the bot often cannot read that
    /// message -- it was sent before the bot's device existed, or its author does not share
    /// keys with unverified sessions. matrix-sdk then hands back the undecrypted
    /// `m.room.encrypted` event instead of failing, and scoring has to keep working, because
    /// clients send reactions themselves in the clear.
    ///
    /// This pins the property the reaction handler depends on: sender and event id of an
    /// encrypted event are readable without any key.
    #[test]
    fn the_author_of_an_undecryptable_event_is_still_readable() {
        let raw = Raw::<AnySyncTimelineEvent>::from_json_string(
            r#"{
                "type": "m.room.encrypted",
                "sender": "@alice:example.org",
                "event_id": "$secret",
                "origin_server_ts": 1700000000000,
                "content": {
                    "algorithm": "m.megolm.v1.aes-sha2",
                    "ciphertext": "AwgAEnB+not+real+ciphertext",
                    "sender_key": "somesenderkey",
                    "device_id": "SOMEDEVICE",
                    "session_id": "somesession"
                }
            }"#
            .to_owned(),
        )
        .unwrap();

        let event = raw
            .deserialize()
            .expect("an encrypted event still deserializes");
        let (sender, event_id) =
            annotated_event_author(&event).expect("an encrypted event has an author");

        assert_eq!(sender.as_str(), "@alice:example.org");
        assert_eq!(event_id.as_str(), "$secret");
    }

    #[test]
    fn a_state_event_has_nobody_to_score() {
        let raw = Raw::<AnySyncTimelineEvent>::from_json_string(
            r#"{
                "type": "m.room.topic",
                "state_key": "",
                "sender": "@alice:example.org",
                "event_id": "$topic",
                "origin_server_ts": 1700000000000,
                "content": { "topic": "hello" }
            }"#
            .to_owned(),
        )
        .unwrap();

        let event = raw.deserialize().unwrap();

        assert!(annotated_event_author(&event).is_none());
    }

    /// The admin's commands are listed under their own heading, so somebody who is not the
    /// admin can stop reading at the first one.
    #[test]
    fn help_lists_the_admin_commands_separately() {
        let answer = help_answer(true);

        let commands_at = answer.text.find("Commands:").unwrap();
        let admin_at = answer.text.find("Admin commands:").unwrap();
        assert!(commands_at < admin_at);

        let (everyone, admin) = answer.text.split_at(admin_at);
        assert!(everyone.contains("- !list:"));
        assert!(!everyone.contains("!register_emoji"));
        assert!(admin.contains("- !register_emoji <emoji> <social_credit>:"));
        assert!(admin.contains("- !set_score <user> <social_credit>:"));
        assert!(admin.contains("- !set_score_all <social_credit>:"));
        assert!(admin.contains("- !activate:"));
        assert!(admin.contains("- !deactivate:"));

        // The two sections are separated by a blank line in both bodies.
        assert!(answer.text.contains("\n\nAdmin commands:"));
        assert!(answer.html.contains("<br><br><b>Admin commands:</b><br>"));
    }

    /// Somebody who finds the bot in a room it has not been switched on in should learn
    /// that from `!help`, not from the silence.
    #[test]
    fn help_in_an_inactive_room_says_so() {
        let inactive = help_answer(false);
        let active = help_answer(true);

        assert!(inactive.text.ends_with(
            "This room is not active. Nothing is counted or scored here until the admin sends !activate."
        ));
        assert!(inactive.html.ends_with("!activate.</i>"));
        assert!(!active.text.contains("not active"));
    }

    /// The placeholders used to be swallowed by clients as unknown HTML tags.
    #[test]
    fn help_escapes_the_placeholders() {
        let answer = help_answer(true);

        assert!(answer.html.contains("&lt;emoji&gt; &lt;social_credit&gt;"));
        assert!(!answer.html.contains("<emoji>"));
    }

    #[test]
    fn splits_a_command_without_arguments() {
        assert_eq!(split_command("!list"), ("!list", ""));
    }

    /// Mobile clients like to append a space; the previous `stripped_body == "!list"`
    /// comparison did not match then.
    #[test]
    fn tolerates_surrounding_whitespace() {
        assert_eq!(split_command("  !list  "), ("!list", ""));
    }

    #[test]
    fn keeps_the_arguments() {
        assert_eq!(
            split_command("!register_emoji 😑 -25"),
            ("!register_emoji", "😑 -25")
        );
    }

    /// split(" ") plus a "drop a leading empty part" special case broke on this.
    #[test]
    fn tolerates_several_spaces_between_the_arguments() {
        let (_, arguments) = split_command("!register_emoji   😑    -25");
        let parts: Vec<&str> = arguments.split_whitespace().collect();
        assert_eq!(parts, vec!["😑", "-25"]);
    }

    #[test]
    fn an_empty_body_yields_an_empty_command() {
        assert_eq!(split_command("   "), ("", ""));
    }
}
