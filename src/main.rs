mod event_handler;
mod data;
mod utils;

use std::env;
use matrix_sdk::{
    Client, config::SyncSettings,
};
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use std::sync::{Arc, Mutex};
use rusqlite::{Connection};
use crate::data::emoji::create_table_emoji;
use crate::data::event::create_table_event;
use crate::data::user::create_table_user;
use crate::data::user_social_credit::create_table_user_social_credit;
use crate::event_handler::EventHandler;
use crate::utils::autojoin::on_stripped_state_member;
use crate::utils::user_util::{initial_admin_user_setup};


// todo session preservation and emoji verification
// todo limit unwrap usage
// todo initial social_credit score per env variable
// todo logging + log levels + file logging
// todo user needs a timestamp per room to prevent spamming, env variable for cooldown, refactor user_social_credit to user_room_data
// todo prevent user from changing their own social_credit score
// todo test setup in empty room with only admin user, no messages
//  -> query all room users on initial setup and create user_social_credit for every user, also handle user joining
// todo if we setup in a room with messages and users the !list command gives nothing back
//  -> remove the user cache because we cannot cache the user with the user_social_credit because there are mutliple social_credit scores per user per room


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let conn = Connection::open("social_credit.db")?;

    let initial_social_credit = env::var("INITIAL_SOCIAL_CREDIT")
        .map_err(|e| format!("Couldn't read INITIAL_SOCIAL_CREDIT: {}", e))
        .and_then(|value| {
            value.parse::<i32>().map_err(|e| format!("Failed to parse INITIAL_SOCIAL_CREDIT: {}", e))
        }).expect("Failed to parse INITIAL_SOCIAL_CREDIT");
    let admin_username = env::var("MATRIX_ADMIN_USERNAME").expect("MATRIX_ADMIN_USERNAME not set");
    let username = env::var("MATRIX_USERNAME").expect("MATRIX_USERNAME not set");
    let homeserver_url = env::var("MATRIX_HOMESERVER_URL").expect("MATRIX_HOMESERVER_URL not set");
    let homeserver_url_relative : &str;
    if homeserver_url.starts_with("https://") {
        homeserver_url_relative = homeserver_url.strip_prefix("https://").expect("Failed to strip https:// from homeserver url");
    }
    else if homeserver_url.starts_with("http://") {
        homeserver_url_relative = homeserver_url.strip_prefix("http://").expect("Failed to strip http:// from homeserver url");
    }
    else {
        panic!("Invalid homeserver url");
    }
    let password = env::var("MATRIX_PASSWORD").expect("MATRIX_PASSWORD not set");

    create_table_user(&conn);
    create_table_user_social_credit(&conn);
    create_table_emoji(&conn);
    create_table_event(&conn);

    let client = Client::builder().homeserver_url(homeserver_url.clone()).build().await?;
    client.login_username(username.as_str(), &*password).initial_device_display_name("Social Credit System").send().await?;
    client.add_event_handler(on_stripped_state_member);

    let shared_conn = Arc::new(Mutex::new(conn));
    let event_handler = Arc::new(EventHandler::new(shared_conn.clone(), client.clone(), username, homeserver_url.clone(), initial_social_credit));

    initial_admin_user_setup(&shared_conn, &admin_username, &homeserver_url_relative);

    client.add_event_handler({
        let event_handler = event_handler.clone();
        move |event: AnySyncMessageLikeEvent, room: Room| {
            let handler = event_handler.clone();
            async move {
                handler.on_message_like_event(event, room).await;
            }
        }
    });

    client.sync(SyncSettings::default()).await.expect("Sync loop fail");

    Ok(())
}
