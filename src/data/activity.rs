//! Counters for the weekly activity payout.
//!
//! One row per user and room, counting up until the payout empties the table again. The
//! alternative would be to keep one row per message; the counters are enough because nothing
//! ever asks about a single message, only about the totals since the last payout.
use rusqlite::{Connection, Error, params};
use std::sync::{Arc, Mutex};
use tracing::error;

/// What a counted event was worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Message,
    Image,
}

/// One user's activity in one room since the last payout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub room_id: String,
    /// Row id in `user`, needed to change the score.
    pub user_id: i32,
    /// Localpart, for the payout message.
    pub name: String,
    /// Server name, needed to match the row against the room's member list.
    pub url: String,
    pub messages: i32,
    pub images: i32,
}

/// Count one message or image for this user in this room.
///
/// A single statement, so two events arriving at the same time cannot lose a count the way a
/// read-modify-write would.
pub fn record_activity(conn: &Arc<Mutex<Connection>>, user_room_data_id: i32, kind: ActivityKind) {
    let sql = match kind {
        ActivityKind::Message => {
            "INSERT INTO activity (user_room_data_id, messages, images) VALUES (?1, 1, 0) \
             ON CONFLICT(user_room_data_id) DO UPDATE SET messages = messages + 1"
        }
        ActivityKind::Image => {
            "INSERT INTO activity (user_room_data_id, messages, images) VALUES (?1, 0, 1) \
             ON CONFLICT(user_room_data_id) DO UPDATE SET images = images + 1"
        }
    };

    let connection = match conn.lock() {
        Ok(connection) => connection,
        Err(_) => {
            error!("Database mutex is poisoned");
            return;
        }
    };

    if let Err(error) = connection.execute(sql, params![user_room_data_id]) {
        error!(%error, ?kind, "Failed to record activity");
    }
}

/// Everything counted since the last payout, ordered by room.
pub fn pending_activity(conn: &Connection) -> Result<Vec<Activity>, Error> {
    let mut stmt = conn.prepare(
        "SELECT d.room_id, u.id, u.name, u.url, a.messages, a.images \
           FROM activity a \
           JOIN user_room_data d ON d.id = a.user_room_data_id \
           JOIN user u ON u.id = d.user_id \
          WHERE a.messages > 0 OR a.images > 0 \
          ORDER BY d.room_id",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(Activity {
            room_id: row.get(0)?,
            user_id: row.get(1)?,
            name: row.get(2)?,
            url: row.get(3)?,
            messages: row.get(4)?,
            images: row.get(5)?,
        })
    })?;

    rows.collect()
}

/// Start the next period for one room.
///
/// Per room rather than wholesale: a room whose payout had to be skipped -- the bot is no
/// longer in it, or its member list could not be read this time -- keeps its counters instead
/// of losing a period's worth of activity to a passing error.
pub fn clear_room_activity(conn: &Connection, room_id: &str) -> Result<usize, Error> {
    conn.execute(
        "DELETE FROM activity WHERE user_room_data_id IN \
         (SELECT id FROM user_room_data WHERE room_id = ?1)",
        params![room_id],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_db;

    fn seed(conn: &Connection, name: &str, room: &str) -> i32 {
        conn.execute(
            "INSERT OR IGNORE INTO user (name, url, user_type) VALUES (?1, 'example.org', 0)",
            params![name],
        )
        .unwrap();
        let user_id: i32 = conn
            .query_row(
                "SELECT id FROM user WHERE name = ?1 AND url = 'example.org'",
                params![name],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO user_room_data (user_id, room_id, social_credit) VALUES (?1, ?2, 250)",
            params![user_id, room],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM user_room_data WHERE user_id = ?1 AND room_id = ?2",
            params![user_id, room],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn counts_messages_and_images_separately() {
        let db = test_db();
        let room_data_id = {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", "!r")
        };

        record_activity(&db, room_data_id, ActivityKind::Message);
        record_activity(&db, room_data_id, ActivityKind::Message);
        record_activity(&db, room_data_id, ActivityKind::Image);

        let conn = db.lock().unwrap();
        let pending = pending_activity(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].messages, 2);
        assert_eq!(pending[0].images, 1);
        assert_eq!(pending[0].name, "alice");
        assert_eq!(pending[0].room_id, "!r");
    }

    #[test]
    fn counts_per_user_and_room() {
        let db = test_db();
        let (alice_r1, alice_r2, bob_r1) = {
            let conn = db.lock().unwrap();
            (
                seed(&conn, "alice", "!r1"),
                seed(&conn, "alice", "!r2"),
                seed(&conn, "bob", "!r1"),
            )
        };

        record_activity(&db, alice_r1, ActivityKind::Message);
        record_activity(&db, alice_r2, ActivityKind::Image);
        record_activity(&db, bob_r1, ActivityKind::Message);

        let conn = db.lock().unwrap();
        let pending = pending_activity(&conn).unwrap();
        assert_eq!(pending.len(), 3);
        assert!(
            pending
                .iter()
                .any(|a| a.name == "alice" && a.room_id == "!r2" && a.images == 1)
        );
    }

    #[test]
    fn clearing_starts_a_new_period_for_that_room_only() {
        let db = test_db();
        let (in_a, in_b) = {
            let conn = db.lock().unwrap();
            (seed(&conn, "alice", "!a"), seed(&conn, "alice", "!b"))
        };
        record_activity(&db, in_a, ActivityKind::Message);
        record_activity(&db, in_b, ActivityKind::Message);

        let conn = db.lock().unwrap();
        clear_room_activity(&conn, "!a").unwrap();

        let left = pending_activity(&conn).unwrap();
        assert_eq!(left.len(), 1, "the other room keeps its counters");
        assert_eq!(left[0].room_id, "!b");
    }
}
