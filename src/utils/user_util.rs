use std::sync::{Arc, Mutex};
use regex::Regex;
use rusqlite::Connection;
use crate::data::{get_user, insert_user, User};

pub fn extract_userdata_from_string(body: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"@(?P<username>[^:]+):(?P<domain>[^">]+)"#).unwrap();

    if let Some(captures) = re.captures(body) {
        let username = captures.name("username").unwrap().as_str().to_string();
        let domain = captures.name("domain").unwrap().as_str().to_string();
        return Some((username, domain));
    }
    None
}

pub fn construct_and_register_user(conn: &Arc<Mutex<Connection>>, sender: &String) -> Option<User> {
    if let Some((username, domain)) = extract_userdata_from_string(sender) {
        let user_opt = get_user(conn, &username, &domain);
        if user_opt.is_some() {
            return user_opt;
        }

        let user = User {
            id: 0,
            name: username,
            url: domain,
            social_credit: 0,
            user_type: crate::data::UserType::Default,
        };

        if insert_user(conn, &user).is_ok() {
            return Some(user);
        }
    }
    None
}

