//! A tiny key/value table for state the bot has to remember across restarts.
//!
//! Currently only the timestamp of the last activity payout. A table rather than a file so it
//! is covered by the same backup as everything else.
use rusqlite::{Connection, Error, OptionalExtension, params};

/// When the last activity payout happened, as a Unix timestamp in seconds.
pub const LAST_PAYOUT_AT: &str = "last_payout_at";

pub fn get_state(conn: &Connection, key: &str) -> Result<Option<String>, Error> {
    conn.query_row(
        "SELECT value FROM bot_state WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
}

pub fn set_state(conn: &Connection, key: &str, value: &str) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO bot_state (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Read a timestamp written by [`set_timestamp`].
pub fn get_timestamp(conn: &Connection, key: &str) -> Result<Option<i64>, Error> {
    Ok(get_state(conn, key)?.and_then(|value| value.parse::<i64>().ok()))
}

pub fn set_timestamp(conn: &Connection, key: &str, seconds: i64) -> Result<(), Error> {
    set_state(conn, key, &seconds.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_db;

    #[test]
    fn stores_and_reads_a_value_back() {
        let db = test_db();
        let conn = db.lock().unwrap();

        assert_eq!(get_state(&conn, "nothing").unwrap(), None);

        set_state(&conn, "key", "value").unwrap();
        assert_eq!(get_state(&conn, "key").unwrap().as_deref(), Some("value"));

        set_state(&conn, "key", "other").unwrap();
        assert_eq!(get_state(&conn, "key").unwrap().as_deref(), Some("other"));
    }

    #[test]
    fn timestamps_survive_the_round_trip() {
        let db = test_db();
        let conn = db.lock().unwrap();

        set_timestamp(&conn, LAST_PAYOUT_AT, 1_700_000_000).unwrap();

        assert_eq!(
            get_timestamp(&conn, LAST_PAYOUT_AT).unwrap(),
            Some(1_700_000_000)
        );
    }

    /// A value that is not a number must not take the payout task down; it is treated as
    /// "never paid out" and rewritten on the next run.
    #[test]
    fn a_broken_timestamp_reads_as_missing() {
        let db = test_db();
        let conn = db.lock().unwrap();
        set_state(&conn, LAST_PAYOUT_AT, "not a number").unwrap();

        assert_eq!(get_timestamp(&conn, LAST_PAYOUT_AT).unwrap(), None);
    }
}
