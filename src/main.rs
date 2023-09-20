mod event_handlers;
mod data;
mod utils;

use matrix_sdk::{
    Client, config::SyncSettings,
    ruma::{user_id},
};
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use std::sync::{Arc, Mutex};
use rusqlite::{Connection};
use crate::data::create_db_tables;
use crate::event_handlers::{on_message_like_event, on_stripped_state_member};


// todo implement per command (e.g. !social_credit @user +1)
// todo implement per message answer/reaction
// todo implement register-emoji and timeout in 5 mins
// todo implement config file where the initial admin user can be set, check the db on every start and set this user to admin

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let conn = Connection::open("social_credit.db")?;

    create_db_tables(&conn);

    let shared_conn = Arc::new(Mutex::new(conn));
    let user = user_id!("@social-credit-system:matrix.hackinger.io");
    let client = Client::builder().user_id(user).build().await?;

    client.login_username(user, "Bxxsf2CbkfZH6Gasdf1").send().await?;

    client.add_event_handler(on_stripped_state_member);
    client.add_event_handler({
        let conn = shared_conn.clone();
        move |event: AnySyncMessageLikeEvent, room: Room| {
            let conn = conn.clone();
            async move {
                on_message_like_event(conn, event, room).await;
            }
        }
    });

    client.sync(SyncSettings::default()).await;
    Ok(())
}