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
use crate::data::user_room_data::create_table_user_room_data;
use crate::data::user_reaction::{create_table_user_reaction};
use crate::event_handler::EventHandler;
use crate::utils::autojoin::on_stripped_state_member;
use crate::utils::user_util::{initial_admin_user_setup};


// todo session preservation and emoji verification
// todo logging + log levels + file logging
// todo test setup in empty room with only admin user, no messages
//  -> query all room users on initial setup and create user_room_data for every user, also handle user joining

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = env::var("DB_PATH").expect("DB_PATH not set");
    let initial_social_credit = get_env_var_as_i32("INITIAL_SOCIAL_CREDIT");
    let reaction_timespan = get_env_var_as_i32("REACTION_TIMESPAN");
    let reaction_limit = get_env_var_as_i32("REACTION_LIMIT");
    let admin_username = env::var("ADMIN_USERNAME").expect("ADMIN_USERNAME not set");
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

    // Database setup
    let conn = Connection::open(db_path)?;
    conn.execute("PRAGMA foreign_keys = ON", []).expect("Failed to enable foreign key support");
    create_table_user(&conn);
    create_table_user_room_data(&conn);
    create_table_user_reaction(&conn);
    create_table_emoji(&conn);
    create_table_event(&conn);

    let client = Client::builder().homeserver_url(homeserver_url.clone()).build().await?;
    client.login_username(username.as_str(), &*password).initial_device_display_name("Social Credit System").send().await?;
    client.add_event_handler(on_stripped_state_member);

    let shared_conn = Arc::new(Mutex::new(conn));
    let event_handler = Arc::new(EventHandler::new(
        shared_conn.clone(),
        username,
        homeserver_url.clone(),
        initial_social_credit,
        reaction_timespan,
        reaction_limit,
    ));

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

fn get_env_var_as_i32(var_name: &str) -> i32 {
    env::var(var_name)
        .map_err(|e| format!("Couldn't read {}: {}", var_name, e))
        .and_then(|value| {
            value.parse::<i32>().map_err(|e| format!("Failed to parse {}: {}", var_name, e))
        })
        .expect(&format!("Failed to parse {}", var_name))
}