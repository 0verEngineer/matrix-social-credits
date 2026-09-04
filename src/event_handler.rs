use crate::data::emoji::{Emoji, delete_emoji, find_emoji_in_db, insert_emoji};
use crate::data::event::{Event, find_event_in_db, insert_event};
use crate::data::user::{User, UserType};
use crate::data::user_room_data::add_social_credit;
use crate::utils::emoji_util::{get_emoji_list_answer, normalize_emoji};
use crate::utils::matrix_util::send_message;
use crate::utils::message::{escape_html, notice_html, notice_plain};
use crate::utils::user_util::{compare_user, get_user_list_answer, setup_user};
use matrix_sdk::ruma::events;
use matrix_sdk::ruma::events::room::message::{MessageType, Relation};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use matrix_sdk::ruma::{OwnedUserId, UserId};
use matrix_sdk::{Room, RoomState};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, trace};

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
}

impl EventHandler {
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        own_user_id: OwnedUserId,
        initial_social_credit: i32,
        reaction_period_minutes: i32,
        reaction_limit: i32,
    ) -> Self {
        EventHandler {
            conn,
            own_user_id,
            initial_social_credit,
            reaction_period_minutes,
            reaction_limit,
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

        let sender = setup_user(
            &self.conn,
            Some(room.clone()),
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
                if let AnySyncTimelineEvent::MessageLike(message_like_event) = deserialized_event {
                    trace!(?message_like_event, "Annotated message like event");
                    trace!(sender = %message_like_event.sender(), "Recipient of the reaction");

                    // The sender here is the user where the social credit score should be changed, so it is the recipient of the reaction
                    let recipient_user_id = message_like_event.sender();
                    if self.is_user_the_bot(recipient_user_id) {
                        debug!("Recipient of reaction is the bot itself");
                        return;
                    }

                    let recipient_opt = setup_user(
                        &self.conn,
                        Some(room.clone()),
                        recipient_user_id,
                        UserType::Default,
                        self.initial_social_credit,
                    );
                    let Some(mut recipient) = recipient_opt else {
                        debug!(user = %recipient_user_id, "Unable to resolve the recipient of the reaction");
                        return;
                    };

                    let annotated_message_id = message_like_event.event_id().to_string();

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
                match content.msgtype {
                    MessageType::Text(..) => {}
                    _ => {
                        return;
                    }
                }

                // An edit carries the new text prefixed with "* " in its fallback body. The
                // old code stripped that prefix and then ran the command again, so editing a
                // "!list" message re-triggered it -- and a plain message starting with "* "
                // (a markdown bullet) was parsed as a command. Edits are ignored instead; the
                // original event was already handled when it arrived.
                if matches!(content.relates_to, Some(Relation::Replacement(_))) {
                    trace!(event_id = %event.event_id(), "Ignoring an edit");
                    return;
                }

                self.handle_command(&room, &sender, content.body().trim())
                    .await;
            }
        }
    }

    /// Dispatch a chat command.
    ///
    /// Each handler used to be called in sequence behind `if handler(..).await { return; }`,
    /// but every one of them ended in `true;` -- a statement, not a return value -- so they
    /// all returned `false` and none of those early returns ever fired.
    async fn handle_command(&self, room: &Room, sender: &User, body: &str) {
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
            _ => {}
        }
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
        // The placeholders have to be escaped. As literal <emoji> and <social_credit> they
        // were swallowed by every client as unknown HTML tags, so the help text read
        // "!register_emoji  : Register an emoji ...".
        let commands = [
            (
                "!list",
                "List all users and their social credit score for the current room",
            ),
            (
                "!list_emoji",
                "List all registered emojis and their social credit score for the current room",
            ),
            (
                "!register_emoji <emoji> <social_credit>",
                "Register an emoji with a social credit score for the current room. Example: !register_emoji 😑 -25",
            ),
            (
                "!unregister_emoji <emoji>",
                "Remove a registered emoji from the current room. Example: !unregister_emoji 😑",
            ),
        ];

        let plain = std::iter::once("Commands:".to_owned())
            .chain(
                commands
                    .iter()
                    .map(|(usage, description)| format!("- {usage}: {description}")),
            )
            .collect::<Vec<_>>()
            .join("\n");

        let html = format!(
            "<h3>Commands:</h3>{}",
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

        send_message(room, notice_html(plain, html)).await;
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

    fn is_user_the_bot(&self, user_id: &UserId) -> bool {
        user_id == self.own_user_id
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
    use super::split_command;

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
