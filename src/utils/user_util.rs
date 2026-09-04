use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use matrix_sdk::{Room, RoomMemberships};
use matrix_sdk::ruma::{OwnedUserId, ServerName, UserId};
use rusqlite::Connection;
use crate::data::user::{find_all_users_with_room_data_in_db, find_user_in_db, insert_user, update_user, User, HtmlAndTextAnswer, UserType};
use crate::data::user_room_data::{find_user_room_data_by_user_id_and_room_id, insert_user_room_data, UserRoomData};
use tracing::{debug, error, info, warn};

pub fn compare_user(user1: &User, user2: &User) -> bool {
    user1.name == user2.name && user1.url == user2.url
}

/// Split a Matrix user id into the two columns the `user` table stores.
///
/// The database predates this and keeps localpart and server name in separate columns, so
/// this is the single place that maps between the two representations.
pub fn split_user_id(user_id: &UserId) -> (String, String) {
    (user_id.localpart().to_owned(), user_id.server_name().to_string())
}

/// Build a user id from an `ADMIN_USERNAME` value.
///
/// Accepts both a bare localpart (`alice`) and a full user id (`@alice:example.org`). A bare
/// localpart is resolved against `server_name`, which must be the server name from the bot's
/// own user id -- *not* the host of `MATRIX_HOMESERVER_URL`. With `.well-known` delegation
/// those two differ (`matrix.example.org` vs `example.org`), and deriving the domain from the
/// URL used to hand out an admin id that never matched a real user.
pub fn resolve_configured_user_id(configured: &str, server_name: &ServerName) -> Option<OwnedUserId> {
    let configured = configured.trim();

    if configured.starts_with('@') {
        return UserId::parse(configured).ok();
    }

    UserId::parse(format!("@{configured}:{server_name}")).ok()
}

pub fn setup_user(conn: &Arc<Mutex<Connection>>, room: Option<Room>, user_id: &UserId, user_type: UserType, initial_social_credit: i32) -> Option<User> {
    let (username, domain) = split_user_id(user_id);

    let user_opt = find_user_in_db(conn, &username, &domain);
    if let Some(mut actual_user) = user_opt {
        setup_user_room_data_for_room(conn, room, &mut actual_user, initial_social_credit);
        return Some(actual_user);
    }

    debug!(user = %user_id, "User not found in db, creating new one");

    let user = User {
        id: -1,
        name: username.clone(),
        url: domain.clone(),
        user_type,
        room_data: None,
    };

    if insert_user(conn, &user).is_ok() {
        let Some(mut inserted_user) = find_user_in_db(conn, &username, &domain) else {
            error!(user = %user_id, "Failed to find user in db after inserting");
            return None;
        };
        setup_user_room_data_for_room(conn, room, &mut inserted_user, initial_social_credit);
        return Some(inserted_user);
    }

    None
}

fn setup_user_room_data_for_room(conn: &Arc<Mutex<Connection>>, room: Option<Room>, user: &mut User, initial_social_credit: i32) {
    if room.is_some() {
        let room = room.unwrap();
        let room_data = find_user_room_data_by_user_id_and_room_id(conn, user.id, &room.room_id().to_string());
        if room_data.is_ok() {
            let room_data = room_data.unwrap();
            user.room_data = Some(room_data);
            return;
        }

        let room_id = room.room_id().to_string();
        debug!(user = %user.name, %room_id, "Room data for user not found in db, creating");

        let room_data = UserRoomData {
            id: -1,
            user_id: user.id,
            room_id,
            social_credit: initial_social_credit,
        };

        if insert_user_room_data(conn, &room_data).is_err() {
            error!(user = %user.name, "Failed to insert room data for user");
        }

        user.room_data = Some(room_data);
    }
}

pub fn initial_admin_user_setup(conn: &Arc<Mutex<Connection>>, admin_user_id: &UserId) {
    let (username, domain) = split_user_id(admin_user_id);

    match find_user_in_db(conn, &username, &domain) {
        Some(mut admin_user) => {
            if !matches!(admin_user.user_type, UserType::Admin) {
                admin_user.user_type = UserType::Admin;
                update_user(conn, &admin_user).expect("Failed to update admin user");
            }
        }
        None => {
            // No room, so no room data is created here and the initial score is irrelevant.
            setup_user(conn, None, admin_user_id, UserType::Admin, 0)
                .expect("Failed to construct or register admin user");
        }
    }

    info!(admin = %admin_user_id, "Admin user configured");
}

/// Build the `!list` answer for `room`.
///
/// The scores come from the database, but who is shown comes from the room's current member
/// list. Previously this was a pure database query, and nothing ever removed a
/// `user_room_data` row: once somebody had written a single message in a room they stayed in
/// the list forever, including after leaving, being kicked or being banned.
///
/// Scores of users who left are kept in the database on purpose, so they are still there if
/// somebody rejoins -- they are only hidden from the listing.
pub async fn get_user_list_answer(conn: &Arc<Mutex<Connection>>, room: &Room, own_user_id: &UserId) -> HtmlAndTextAnswer {
    let (own_localpart, own_server) = split_user_id(own_user_id);
    let users_opt = find_all_users_with_room_data_in_db(
        conn,
        &room.room_id().to_string(),
        &own_localpart,
        &own_server,
    );
    let empty_answer = HtmlAndTextAnswer {
        html: String::from("No scores"),
        text: String::from("No Scores"),
    };

    let Some(mut users) = users_opt else {
        return empty_answer;
    };

    if users.is_empty() {
        return empty_answer;
    }

    let mut note = "";
    match current_room_members(room).await {
        Some(members) => {
            users.retain(|user| members.contains(&(user.name.clone(), user.url.clone())));
        }
        None => {
            // The homeserver did not hand out the member list (rate limited, or down). Show
            // the stored scores rather than nothing, but say that the list may be stale.
            note = " (member list unavailable, this may include users who left)";
        }
    }

    if users.is_empty() {
        return empty_answer;
    }

    // Sort users by social credit
    users.sort_by(|a, b| {
        let a_credit = a.room_data.as_ref().map_or(0, |sc| sc.social_credit);
        let b_credit = b.room_data.as_ref().map_or(0, |sc| sc.social_credit);
        b_credit.cmp(&a_credit)
    });

    let mut text_entries: Vec<String> = Vec::with_capacity(users.len());
    let mut html_entries: Vec<String> = Vec::with_capacity(users.len());

    for user in users {
        let Some(room_data) = user.room_data else {
            continue;
        };
        text_entries.push(format!("{}: {}", user.name, room_data.social_credit));
        html_entries.push(format!("{}: <b>{}</b>", user.name, room_data.social_credit));
    }

    HtmlAndTextAnswer {
        text: format!("Social Credit Scores{}: {}", note, text_entries.join(", ")),
        html: format!(
            "<h3>Social Credit Scores{}:</h3><br>{}",
            note,
            html_entries.join("<br>")
        ),
    }
}

/// The users currently joined to `room`, in the `(localpart, server name)` shape the `user`
/// table stores.
///
/// Returns `None` when the member list could not be obtained, so callers can tell "nobody is
/// in the room" apart from "we do not know who is in the room".
async fn current_room_members(room: &Room) -> Option<HashSet<(String, String)>> {
    match room.members(RoomMemberships::JOIN).await {
        Ok(members) => Some(members.iter().map(|member| split_user_id(member.user_id())).collect()),
        Err(error) => {
            warn!(room_id = %room.room_id(), %error, "Unable to load the room member list");
            None
        }
    }
}
