use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Error, params};
use tracing::{info, warn};

use crate::utils::emoji_util::normalize_emoji;

// Every schema statement the bot has ever issued lives in this file, in the migration that
// introduced it. That is the whole point of the arrangement: a migration is a record of what
// happened, so none of the SQL below may be edited after it has shipped. `event` has no
// `seen_at` column in migration 1 because migration 2 adds it -- "tidying that up" would make
// migration 2 fail on a fresh database with a duplicate column, and only for new installs.

/// Schema version this build expects. Stored in SQLite's `user_version`.
const SCHEMA_VERSION: i32 = 3;

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
    let journal_mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        warn!(journal_mode, "Could not switch the database to WAL mode");
    }

    migrate(&conn)?;

    Ok(conn)
}

/// Bring the schema of an already opened connection up to date.
///
/// Split out from [`open_and_migrate`] so the tests can run it against an in-memory database.
pub fn migrate(conn: &Connection) -> Result<(), Error> {
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
        migrate_to_v1(conn)?;
        version = 1;
        conn.pragma_update(None, "user_version", version)?;
    }

    if version < 2 {
        info!("Applying schema migration 2: constraints, indexes, event retention");
        migrate_to_v2(conn)?;
        version = 2;
        conn.pragma_update(None, "user_version", version)?;
    }

    if version < 3 {
        info!("Applying schema migration 3: activity counters, bot state");
        migrate_to_v3(conn)?;
        version = 3;
        conn.pragma_update(None, "user_version", version)?;
    }

    let _ = version;
    Ok(())
}

/// The schema as it was before it was versioned.
///
/// `IF NOT EXISTS` throughout, because the databases this first meets are the ones that
/// already have these tables and a `user_version` of 0.
fn migrate_to_v1(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS user (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            user_type INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS user_room_data (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES user(id),
            room_id TEXT NOT NULL,
            social_credit INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS user_reaction (
            id INTEGER PRIMARY KEY,
            user_room_data_id INTEGER NOT NULL REFERENCES user_room_data(id),
            time INTEGER NOT NULL,
            message_event_id TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS emoji (
            id INTEGER PRIMARY KEY,
            room_id TEXT NOT NULL,
            emoji TEXT NOT NULL,
            social_credit INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS event (
            id TEXT PRIMARY KEY,
            event_type TEXT NOT NULL,
            handled INTEGER NOT NULL
        );
        ",
    )
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
    conn.execute(
        "UPDATE event SET seen_at = ?1 WHERE seen_at = 0",
        params![now_epoch_secs()],
    )?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_event_seen_at ON event (seen_at);")?;

    Ok(())
}

/// Counters for the weekly activity payout, and somewhere to remember when the last one was.
fn migrate_to_v3(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS activity (
            user_room_data_id INTEGER PRIMARY KEY REFERENCES user_room_data(id) ON DELETE CASCADE,
            messages INTEGER NOT NULL DEFAULT 0,
            images INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS bot_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )
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
            conn.execute(
                "UPDATE emoji SET emoji = ?1 WHERE id = ?2",
                params![normalized, id],
            )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{legacy_db, test_db};

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn a_fresh_database_ends_up_at_the_current_version() {
        let db = test_db();
        let conn = db.lock().unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn user_name_and_url_are_unique() {
        let db = test_db();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO user (name, url, user_type) VALUES ('a', 'b', 0)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO user (name, url, user_type) VALUES ('a', 'b', 0)",
                []
            )
            .is_err(),
            "a second user with the same name and url must be rejected"
        );
    }

    #[test]
    fn an_emoji_can_only_be_registered_once_per_room() {
        let db = test_db();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO emoji (room_id, emoji, social_credit) VALUES ('!r', '😑', -25)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO emoji (room_id, emoji, social_credit) VALUES ('!r', '😑', 5)",
                []
            )
            .is_err()
        );
        // ... but the same emoji in a different room is fine.
        conn.execute(
            "INSERT INTO emoji (room_id, emoji, social_credit) VALUES ('!other', '😑', 5)",
            [],
        )
        .unwrap();
    }

    /// The production database had no uniqueness at all, and setup_user's
    /// find-insert-find is not atomic -- the old code even logged "Multiple users found".
    #[test]
    fn migration_merges_duplicate_users_and_repoints_their_rows() {
        let conn = legacy_db();
        conn.execute_batch(
            "
            INSERT INTO user (id, name, url, user_type) VALUES (1,'alice','example.org',0), (2,'alice','example.org',0), (3,'bob','example.org',2);
            INSERT INTO user_room_data (id, user_id, room_id, social_credit) VALUES (10,1,'!r',250), (11,2,'!r',300), (12,3,'!r',400);
            INSERT INTO user_reaction (id, user_room_data_id, time, message_event_id) VALUES (100,10,1700000000,'$m1'), (101,11,1700000100,'$m1'), (102,11,1700000200,'$m2'), (103,12,1700000300,'$m3');
            ",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert_eq!(
            count(&conn, "user"),
            2,
            "the duplicated alice must be merged away"
        );
        assert_eq!(count(&conn, "user_room_data"), 2);
        // $m1 was recorded twice for what turned out to be the same user.
        assert_eq!(count(&conn, "user_reaction"), 3);

        let surviving_user: i32 = conn
            .query_row(
                "SELECT user_id FROM user_room_data WHERE room_id='!r' AND social_credit=250",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            surviving_user, 1,
            "room data must point at the surviving user"
        );

        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_reaction r LEFT JOIN user_room_data d ON r.user_room_data_id = d.id WHERE d.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            orphans, 0,
            "no reaction may be left pointing at a deleted row"
        );
    }

    /// Entries registered before normalization existed carry variation selectors or skin
    /// tones and would never match an incoming reaction again.
    #[test]
    fn migration_normalizes_and_deduplicates_stored_emojis() {
        let conn = legacy_db();
        conn.execute_batch(
            "
            INSERT INTO emoji (id, room_id, emoji, social_credit) VALUES (1,'!r','😑\u{fe0f}',-25), (2,'!r','😑',-25), (3,'!r','👍🏽',10);
            ",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let stored: Vec<String> = {
            let mut stmt = conn.prepare("SELECT emoji FROM emoji ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(stored, vec!["😑".to_owned(), "👍".to_owned()]);
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = legacy_db();
        conn.execute(
            "INSERT INTO user (id, name, url, user_type) VALUES (1,'a','b',0)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        assert_eq!(count(&conn, "user"), 1);
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn existing_event_rows_survive_the_first_retention_run() {
        let conn = legacy_db();
        conn.execute(
            "INSERT INTO event (id, event_type, handled) VALUES ('$e1','m.reaction',1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();
        cleanup_events(&conn, 30).unwrap();

        assert_eq!(
            count(&conn, "event"),
            1,
            "seen_at must be backfilled, not left at 0"
        );
    }

    #[test]
    fn retention_drops_old_markers_and_keeps_recent_ones() {
        let db = test_db();
        let conn = db.lock().unwrap();
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO event (id, event_type, handled, seen_at) VALUES ('$old','m.reaction',1,?1)",
            params![now - 60 * 24 * 60 * 60],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO event (id, event_type, handled, seen_at) VALUES ('$new','m.reaction',1,?1)",
            params![now],
        )
        .unwrap();

        let removed = cleanup_events(&conn, 30).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(count(&conn, "event"), 1);
    }
}
