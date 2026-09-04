use std::sync::{Arc, Mutex};
use matrix_sdk::Room;
use matrix_sdk::ruma::{OwnedUserId, ServerName, UserId};
use rusqlite::Connection;
use crate::data::user::{find_all_users_with_room_data_in_db, find_user_in_db, insert_user, update_user, User, HtmlAndTextAnswer, UserType};
use crate::data::user_room_data::{find_user_room_data_by_user_id_and_room_id, insert_user_room_data, UserRoomData};
use tracing::{debug, error, info};

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
            reactions: Vec::new(),
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

pub fn get_user_list_answer(conn: &Arc<Mutex<Connection>>, room: &Room, own_user_id: &UserId) -> HtmlAndTextAnswer {
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

    if users_opt.is_none() {
        return empty_answer;
    }

    let mut text_body = String::from("Social Credit Scores: ");
    let mut html_body = String::from("<h3>Social Credit Scores:</h3><br>");

    let mut users = users_opt.unwrap();

    if users.len() == 0 {
        return empty_answer;
    }

    // Sort users by social credit
    users.sort_by(|a, b| {
        let a_credit = a.room_data.as_ref().map_or(0, |sc| sc.social_credit);
        let b_credit = b.room_data.as_ref().map_or(0, |sc| sc.social_credit);
        b_credit.cmp(&a_credit)
    });

    for user in users {
        let room_data_opt = user.room_data;
        if room_data_opt.is_none() {
            continue;
        }
        let room_data = room_data_opt.unwrap();
        text_body.push_str(&format!("{}: {},", user.name, room_data.social_credit));
        html_body.push_str(&format!("{}: <b>{}</b><br>", user.name, room_data.social_credit));
    }

    // Remove the last comma
    if text_body.len() >= 1 {
        text_body.remove(text_body.len() - 1);
    }
    // Remove the last <br>
    if html_body.len() >= 4 {
        html_body.truncate(html_body.len() - 4);
    }

    HtmlAndTextAnswer {
        html: html_body.to_string(),
        text: text_body.to_string(),
    }
}