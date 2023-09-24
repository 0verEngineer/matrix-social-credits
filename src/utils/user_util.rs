use std::sync::{Arc, Mutex};
use matrix_sdk::room::{Joined};
use regex::Regex;
use rusqlite::Connection;
use crate::data::user::{find_all_users_with_room_data_in_db, find_user_in_db, insert_user, update_user, User, HtmlAndTextAnswer, UserType};
use crate::data::user_room_data::{find_user_room_data_by_user_id_and_room_id, insert_user_room_data, UserRoomData};

pub fn compare_user(user1: &User, user2: &User) -> bool {
    user1.name == user2.name && user1.url == user2.url
}

pub fn extract_userdata_from_string(body: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"@(?P<username>[^:]+):(?P<domain>[^">]+)"#).unwrap();

    if let Some(captures) = re.captures(body) {
        let username = captures.name("username").unwrap().as_str().to_string();
        let domain = captures.name("domain").unwrap().as_str().to_string();
        return Some((username, domain));
    }
    None
}

pub fn setup_user(conn: &Arc<Mutex<Connection>>, room: Option<Joined>, user_tag: &String, user_type: UserType, initial_social_credit: i32) -> Option<User> {
    if let Some((username, domain)) = extract_userdata_from_string(user_tag) {
        let user_opt = find_user_in_db(conn, &username, &domain);
        let mut mut_user_opt = user_opt.clone().take();
        if let Some(ref mut actual_user) = mut_user_opt {
            setup_user_room_data_for_room(conn, room, actual_user, initial_social_credit);
            return Some(actual_user.clone());
        }

        println!("User {} not found in db, creating new one", user_tag); // debug level

        let user = User {
            id: -1,
            name: username.clone(),
            url: domain.clone(),
            user_type,
            room_data: None,
        };

        if insert_user(conn, &user).is_ok() {
            let user_opt = find_user_in_db(conn, &username, &domain);
            if user_opt.is_none() {
                println!("Failed to find user in db after inserting");
                return None;
            }
            let mut mut_user = user_opt.unwrap();
            setup_user_room_data_for_room(conn, room, &mut mut_user, initial_social_credit);
            return Some(mut_user.clone());
        }
    }
    None
}

fn setup_user_room_data_for_room(conn: &Arc<Mutex<Connection>>, room: Option<Joined>, user: &mut User, initial_social_credit: i32) {
    if room.is_some() {
        let room = room.unwrap();
        let room_data = find_user_room_data_by_user_id_and_room_id(conn, user.id, &room.room_id().to_string());
        if room_data.is_ok() {
            let room_data = room_data.unwrap();
            user.room_data = Some(room_data);
            return;
        }

        let room_id = room.room_id().to_string();
        println!("Room data for user {} and room {} not found in db, creating", user.name, room_id); // debug level

        let room_data = UserRoomData {
            id: -1,
            user_id: user.id,
            room_id,
            social_credit: initial_social_credit,
            last_reactions: Vec::new(),
        };

        if insert_user_room_data(conn, &room_data).is_err() {
            println!("Failed to insert room data for user {}", user.name);
        }

        user.room_data = Some(room_data);
    }
}

pub fn initial_admin_user_setup(conn: &Arc<Mutex<Connection>>, username: &String, homeserver_url_relative: &str) {
    let admin_user = find_user_in_db(&conn, &username, &homeserver_url_relative.to_string());
    if admin_user.is_some() {
        let mut admin_user = admin_user.unwrap();
        if !matches!(admin_user.user_type, UserType::Admin) {
            admin_user.user_type = UserType::Admin;
            update_user(&conn, &admin_user).expect("Failed to update admin user");
        }
    }
    else if admin_user.is_none() {
        setup_user(&conn, None, &format!("@{}:{}", username, homeserver_url_relative), UserType::Admin, -1).expect("Failed to construct or register admin user");
    }
}

pub fn get_user_list_answer(conn: &Arc<Mutex<Connection>>, room: &Joined) -> HtmlAndTextAnswer {
    let users_opt = find_all_users_with_room_data_in_db(&conn, &room.room_id().to_string());
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