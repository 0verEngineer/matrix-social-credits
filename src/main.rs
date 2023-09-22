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


// todo admin user commands: !add_admin, !add_moderator, !remove_moderator, !register_emoji
//  - register-emoji, send emoji and -10 for example
// todo implement per reaction social credit change
// todo session preservation and emoji verification
// todo limit unwrap usage
// todo event db table cleanup after a configurable amount of days
// todo initial social_credit score per env variable
// todo moderator needs to be room specific


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let conn = Connection::open("social_credit.db")?;

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
    let event_handler = Arc::new(EventHandler::new(shared_conn.clone(), client.clone()));

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
