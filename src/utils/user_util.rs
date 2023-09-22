use std::sync::{Arc, Mutex};
use matrix_sdk::room::{Joined};
use regex::Regex;
use rusqlite::Connection;
use crate::data::user::{find_all_users_with_social_credit_for_room_in_db, find_user_in_db, insert_user, update_user, User, UserListAnswer, UserType};
use crate::data::user_social_credit::{find_user_social_credit_by_user_id_and_room_id, insert_user_social_credit, UserSocialCredit};

pub fn extract_userdata_from_string(body: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"@(?P<username>[^:]+):(?P<domain>[^">]+)"#).unwrap();

    if let Some(captures) = re.captures(body) {
        let username = captures.name("username").unwrap().as_str().to_string();
        let domain = captures.name("domain").unwrap().as_str().to_string();
        return Some((username, domain));
    }
    None
}

pub fn setup_user(conn: &Arc<Mutex<Connection>>, room: Option<Joined>, sender: &String, user_type: UserType) -> Option<User> {
    if let Some((username, domain)) = extract_userdata_from_string(sender) {
        let user_opt = find_user_in_db(conn, &username, &domain);
        if user_opt.is_some() {
            setup_user_social_credit_for_room(conn, room, &user_opt.clone().unwrap());
            return user_opt;
        }

        let user = User {
            id: -1,
            name: username.clone(),
            url: domain.clone(),
            user_type,
            social_credit: None,
        };

        if insert_user(conn, &user).is_ok() {
            let user_opt = find_user_in_db(conn, &username, &domain);
            if user_opt.is_none() {
                println!("Failed to find user in db after inserting");
                return None;
            }
            let user = user_opt.unwrap();

            setup_user_social_credit_for_room(conn, room, &user);

            return Some(user);
        }
    }
    None
}

fn setup_user_social_credit_for_room(conn: &Arc<Mutex<Connection>>, room: Option<Joined>, user: &User) {
    if room.is_some() {
        let room = room.unwrap();

        // todo this can be done better, query the user and the social_credit directly in one query in the setup_user method where this method is called the first time
        let social_credit_opt = find_user_social_credit_by_user_id_and_room_id(conn, user.id, &room.room_id().to_string());
        if social_credit_opt.is_err() || social_credit_opt.is_ok() && social_credit_opt.unwrap().is_some() {
            return;
        }

        let social_credits = UserSocialCredit {
            id: -1,
            user_id: user.id,
            room_id: room.room_id().to_string(),
            social_credit: 50,
        };

        if insert_user_social_credit(conn, &social_credits).is_err() {
            println!("Failed to insert social credit for user {}", user.name);
        }
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
        setup_user(&conn, None, &format!("@{}:{}", username, homeserver_url_relative), UserType::Admin).expect("Failed to construct or register admin user");
    }
}

pub fn get_user_list_answer(conn: &Arc<Mutex<Connection>>, room: &Joined) -> UserListAnswer {
    let users_opt = find_all_users_with_social_credit_for_room_in_db(&conn, &room.room_id().to_string());
    let empty_answer = UserListAnswer {
        html: String::from("No scores"),
        text: String::from("No Scores"),
    };

    if users_opt.is_none() {
        return empty_answer;
    }

    let mut text_body = String::from("Social Credit Scores: ");
    let mut html_body = String::from("<h3>Social Credit Scores:</h3>");

    let mut users = users_opt.unwrap();

    if users.len() == 0 {
        return empty_answer;
    }

    // Sort users by social credit
    users.sort_by(|a, b| {
        let a_credit = a.social_credit.as_ref().map_or(0, |sc| sc.social_credit);
        let b_credit = b.social_credit.as_ref().map_or(0, |sc| sc.social_credit);
        b_credit.cmp(&a_credit)
    });

    for user in users {
        let social_credit_opt = user.social_credit;
        if social_credit_opt.is_none() {
            continue;
        }
        let social_credit = social_credit_opt.unwrap();
        text_body.push_str(&format!("{}: {},", user.name, social_credit.social_credit));
        html_body.push_str(&format!("{}: <b>{}</b><br>", user.name, social_credit.social_credit));
    }

    // Remove the last comma
    if text_body.len() >= 1 {
        text_body.remove(text_body.len() - 1);
    }
    // Remove the last <br>
    if html_body.len() >= 4 {
        html_body.truncate(html_body.len() - 4);
    }

    UserListAnswer {
        html: html_body.to_string(),
        text: text_body.to_string(),
    }
}