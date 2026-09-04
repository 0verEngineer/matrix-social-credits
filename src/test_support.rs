//! Helpers shared by the unit tests.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::data::migrations::migrate;

/// A migrated in-memory database, in the shape the production code expects.
pub fn test_db() -> Arc<Mutex<Connection>> {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.pragma_update(None, "foreign_keys", true).expect("foreign keys");
    migrate(&conn).expect("migrations");
    Arc::new(Mutex::new(conn))
}

/// A legacy database: the pre-migration schema without any of the constraints, so the
/// migration path can be exercised against realistic bad data.
pub fn legacy_db() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.pragma_update(None, "foreign_keys", false).expect("foreign keys");
    conn.execute_batch(
        "
        CREATE TABLE user (id INTEGER PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, user_type INTEGER NOT NULL);
        CREATE TABLE user_room_data (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES user(id), room_id TEXT NOT NULL, social_credit INTEGER NOT NULL);
        CREATE TABLE user_reaction (id INTEGER PRIMARY KEY, user_room_data_id INTEGER NOT NULL REFERENCES user_room_data(id), time INTEGER NOT NULL, message_event_id TEXT NOT NULL);
        CREATE TABLE emoji (id INTEGER PRIMARY KEY, room_id TEXT NOT NULL, emoji TEXT NOT NULL, social_credit INTEGER NOT NULL);
        CREATE TABLE event (id TEXT PRIMARY KEY, event_type TEXT NOT NULL, handled INTEGER NOT NULL);
        ",
    )
    .expect("legacy schema");
    conn
}
