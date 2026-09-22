//! Which rooms the bot is switched on in.
//!
//! The bot joins every room it is invited into, and until this table existed it started
//! counting, scoring and answering there straight away. That put the weekly payout into every
//! old test room and into rooms the bot cannot be removed from. A room now has to be
//! activated by the admin first; everything else is left alone.
//!
//! A room without a row is inactive. Activating and deactivating only flip the flag -- scores,
//! emojis and activity counters are untouched either way.
use std::collections::HashSet;

use rusqlite::{Connection, Error, OptionalExtension, params};

pub fn is_room_active(conn: &Connection, room_id: &str) -> Result<bool, Error> {
    let active: Option<bool> = conn
        .query_row(
            "SELECT active FROM room WHERE room_id = ?1",
            params![room_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(active.unwrap_or(false))
}

pub fn set_room_active(conn: &Connection, room_id: &str, active: bool) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO room (room_id, active) VALUES (?1, ?2) \
         ON CONFLICT(room_id) DO UPDATE SET active = excluded.active",
        params![room_id, active],
    )?;
    Ok(())
}

/// The ids of every active room, for the payout to check the joined rooms against.
pub fn active_rooms(conn: &Connection) -> Result<HashSet<String>, Error> {
    let mut stmt = conn.prepare("SELECT room_id FROM room WHERE active = 1")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_db;

    const ROOM: &str = "!room:example.org";

    #[test]
    fn a_room_nobody_has_touched_is_inactive() {
        let db = test_db();
        let conn = db.lock().unwrap();

        assert!(!is_room_active(&conn, ROOM).unwrap());
        assert!(active_rooms(&conn).unwrap().is_empty());
    }

    #[test]
    fn activating_and_deactivating_flip_the_flag() {
        let db = test_db();
        let conn = db.lock().unwrap();

        set_room_active(&conn, ROOM, true).unwrap();
        assert!(is_room_active(&conn, ROOM).unwrap());
        assert_eq!(
            active_rooms(&conn).unwrap(),
            HashSet::from([ROOM.to_owned()])
        );

        set_room_active(&conn, ROOM, false).unwrap();
        assert!(!is_room_active(&conn, ROOM).unwrap());
        assert!(active_rooms(&conn).unwrap().is_empty());
    }

    #[test]
    fn activating_twice_is_harmless() {
        let db = test_db();
        let conn = db.lock().unwrap();

        set_room_active(&conn, ROOM, true).unwrap();
        set_room_active(&conn, ROOM, true).unwrap();

        assert_eq!(active_rooms(&conn).unwrap().len(), 1);
    }
}
