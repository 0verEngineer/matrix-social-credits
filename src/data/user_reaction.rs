//! Recorded reactions: who reacted, when, and to which message.
//!
//! The table serves two independent purposes -- the cooldown window (recent rows only) and
//! the "already reacted to this message" check (all rows) -- which is why the queries below
//! are narrow instead of loading a user's whole history.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Error, params};


pub fn create_table_user_reaction(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user_reaction (
                id INTEGER PRIMARY KEY,
                user_room_data_id INTEGER NOT NULL REFERENCES user_room_data(id),
                time INTEGER NOT NULL,
                message_event_id TEXT NOT NULL
        )", []).expect("Failed to create user_reaction table");
}

/// Record a reaction at `time`.
pub fn insert_user_reaction(
    conn: &Connection,
    user_room_data_id: i32,
    time: SystemTime,
    message_event_id: &str,
) -> Result<(), Error> {
    // rusqlite 0.40 no longer implements ToSql for u64, and SQLite integers are i64 anyway.
    let epoch_secs = to_epoch_secs(time);

    conn.execute(
        "INSERT INTO user_reaction (user_room_data_id, time, message_event_id) VALUES (?1, ?2, ?3)",
        params![user_room_data_id, epoch_secs, message_event_id],
    )?;

    Ok(())
}

/// Whether this user already reacted to `message_event_id` at some point.
pub fn has_reacted_to_message(
    conn: &Connection,
    user_room_data_id: i32,
    message_event_id: &str,
) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM user_reaction \
         WHERE user_room_data_id = ?1 AND message_event_id = ?2)",
        params![user_room_data_id, message_event_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
}

/// How many reactions this user made since `since`, and when the oldest of them was.
///
/// Both numbers come from one query, and the whole history is never loaded into memory --
/// which it previously was, on every single incoming event.
pub fn recent_reaction_window(
    conn: &Connection,
    user_room_data_id: i32,
    since: SystemTime,
) -> Result<(u32, Option<SystemTime>), Error> {
    let since_secs = to_epoch_secs(since);

    conn.query_row(
        "SELECT COUNT(*), MIN(time) FROM user_reaction \
         WHERE user_room_data_id = ?1 AND time >= ?2",
        params![user_room_data_id, since_secs],
        |row| {
            let count: i64 = row.get(0)?;
            let oldest: Option<i64> = row.get(1)?;
            Ok((count.max(0) as u32, oldest.map(from_epoch_secs)))
        },
    )
}

fn to_epoch_secs(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn from_epoch_secs(secs: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64)
}
