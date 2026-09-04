use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Error, params};
use tracing::{info, warn};

use crate::data::emoji::create_table_emoji;
use crate::data::event::create_table_event;
use crate::data::user::create_table_user;
use crate::data::user_reaction::create_table_user_reaction;
use crate::data::user_room_data::create_table_user_room_data;
use crate::utils::emoji_util::normalize_emoji;

/// Schema version this build expects. Stored in SQLite's `user_version`.
const SCHEMA_VERSION: i32 = 2;

/// How long SQLite waits for a lock before returning SQLITE_BUSY.
const BUSY_TIMEOUT_MS: i32 = 5_000;

/// Open the database, apply the connection pragmas and bring the schema up to date.
///
/// Previously the tables were only ever created with `CREATE TABLE IF NOT EXISTS`, so there
/// was no way to change the schema of a database that already existed. Everything below is
/// keyed off `PRAGMA user_version` instead.
pub fn open_and_migrate(path: impl AsRef<Path>) -> Result<Connection, Error> {
    let conn = Connection::open(path)?;

    // Must be set outside a transaction, and separately per connection.
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;

    // journal_mode returns the resulting mode as a row, so it cannot go through
    // pragma_update. WAL lets a reader and a writer work at the same time, which the previous
    // rollback journal did not.
    let journal_mode: String =
        conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        warn!(journal_mode, "Could not switch the database to WAL mode");
    }

    migrate(&conn)?;

    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), Error> {
    let mut version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if version > SCHEMA_VERSION {
        warn!(
            version,
            expected = SCHEMA_VERSION,
            "Database was written by a newer version of the bot"
        );
        return Ok(());
    }

    if version < 1 {
        info!("Applying schema migration 1: base tables");
        create_table_user(conn);
        create_table_user_room_data(conn);
        create_table_user_reaction(conn);
        create_table_emoji(conn);
        create_table_event(conn);
        version = 1;
        conn.pragma_update(None, "user_version", version)?;
    }

    if version < 2 {
        info!("Applying schema migration 2: constraints, indexes, event retention");
        migrate_to_v2(conn)?;
        version = 2;
        conn.pragma_update(None, "user_version", version)?;
    }

    let _ = version;
    Ok(())
}

/// Deduplicate the existing rows, then add the uniqueness the code always assumed.
///
/// None of these tables had a UNIQUE constraint. `setup_user` does a non-atomic
/// find-insert-find, and the code even logged "Multiple users found for name ... and url ..."
/// when that had gone wrong. `find_emoji_in_db` returns `None` when it finds more than one
/// row, so a duplicated emoji registration silently stopped working.
fn migrate_to_v2(conn: &Connection) -> Result<(), Error> {
    // Duplicates are resolved by keeping the lowest id and repointing everything that
    // referenced the discarded rows at it.
    conn.execute_batch(
        "
        UPDATE user_room_data
           SET user_id = (SELECT MIN(u2.id) FROM user u2
                           WHERE u2.name = (SELECT u1.name FROM user u1 WHERE u1.id = user_room_data.user_id)
                             AND u2.url  = (SELECT u1.url  FROM user u1 WHERE u1.id = user_room_data.user_id));

        DELETE FROM user
              WHERE id NOT IN (SELECT MIN(id) FROM user GROUP BY name, url);

        UPDATE user_reaction
           SET user_room_data_id = (SELECT MIN(d2.id) FROM user_room_data d2
                                     WHERE d2.user_id = (SELECT d1.user_id FROM user_room_data d1 WHERE d1.id = user_reaction.user_room_data_id)
                                       AND d2.room_id = (SELECT d1.room_id FROM user_room_data d1 WHERE d1.id = user_reaction.user_room_data_id));

        DELETE FROM user_room_data
              WHERE id NOT IN (SELECT MIN(id) FROM user_room_data GROUP BY user_id, room_id);

        DELETE FROM user_reaction
              WHERE id NOT IN (SELECT MIN(id) FROM user_reaction GROUP BY user_room_data_id, message_event_id);
        ",
    )?;

    normalize_stored_emojis(conn)?;

    conn.execute_batch(
        "
        DELETE FROM emoji
              WHERE id NOT IN (SELECT MIN(id) FROM emoji GROUP BY room_id, emoji);

        CREATE UNIQUE INDEX IF NOT EXISTS idx_user_name_url
            ON user (name, url);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_user_room_data_user_room
            ON user_room_data (user_id, room_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_user_reaction_message
            ON user_reaction (user_room_data_id, message_event_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_emoji_room_emoji
            ON emoji (room_id, emoji);

        CREATE INDEX IF NOT EXISTS idx_user_room_data_room
            ON user_room_data (room_id);
        CREATE INDEX IF NOT EXISTS idx_user_reaction_room_data
            ON user_reaction (user_room_data_id);
        CREATE INDEX IF NOT EXISTS idx_user_reaction_time
            ON user_reaction (time);

        ALTER TABLE event ADD COLUMN seen_at INTEGER NOT NULL DEFAULT 0;
        ",
    )?;

    // Existing rows get the current time so the first retention run does not wipe the whole
    // deduplication history at once.
    conn.execute("UPDATE event SET seen_at = ?1 WHERE seen_at = 0", params![now_epoch_secs()])?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_event_seen_at ON event (seen_at);")?;

    Ok(())
}

/// Rewrite stored emojis through [`normalize_emoji`].
///
/// Entries registered before the normalization was applied on both sides can contain
/// variation selectors or skin tone modifiers, and would never match an incoming reaction.
fn normalize_stored_emojis(conn: &Connection) -> Result<(), Error> {
    let rows: Vec<(i32, String)> = {
        let mut stmt = conn.prepare("SELECT id, emoji FROM emoji")?;
        let mapped = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        mapped.collect::<Result<Vec<_>, Error>>()?
    };

    for (id, emoji) in rows {
        let normalized = normalize_emoji(&emoji);
        if normalized != emoji {
            info!(from = %emoji, to = %normalized, "Normalizing a registered emoji");
            conn.execute("UPDATE emoji SET emoji = ?1 WHERE id = ?2", params![normalized, id])?;
        }
    }

    Ok(())
}

/// Drop deduplication markers for events older than `retention_days`.
///
/// The `event` table exists purely so an event is not scored twice, and nothing ever removed
/// a row from it: every event the bot has ever seen stayed there, and it is looked up for
/// every incoming event.
pub fn cleanup_events(conn: &Connection, retention_days: u32) -> Result<usize, Error> {
    let cutoff = now_epoch_secs() - i64::from(retention_days) * 24 * 60 * 60;
    conn.execute("DELETE FROM event WHERE seen_at < ?1", params![cutoff])
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

