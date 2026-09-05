use crate::data::user_reaction::{
    has_reacted_to_message, insert_user_reaction, recent_reaction_window,
};
use rusqlite::{Connection, Error, Result, params};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tracing::{error, warn};

#[derive(Clone)]
pub struct UserRoomData {
    pub id: i32,
    pub user_id: i32,
    pub room_id: String,
    pub social_credit: i32,
}

impl UserRoomData {
    /// Seconds until this user may change somebody's score again, `0` if they may right now.
    ///
    /// The rule is "at most `reaction_limit` reactions within `reaction_period_minutes".
    /// Once the limit is reached, the wait is until the *oldest* reaction in the window drops
    /// out of it -- that is the first moment a slot frees up. The previous implementation
    /// measured from the *newest* one instead, so with a limit of 2 over 20 minutes and
    /// reactions at t=0 and t=19, it reported a wait until t=39 although t=20 was correct.
    ///
    /// It also called `duration_since(..).unwrap()` on every stored reaction, which panics as
    /// soon as one of them lies in the future -- possible after an NTP step or a container
    /// moving between hosts.
    pub fn get_time_till_user_can_react(
        &self,
        conn: &Arc<Mutex<Connection>>,
        reaction_period_minutes: i32,
        reaction_limit: i32,
    ) -> i64 {
        if reaction_limit <= 0 {
            return 0;
        }

        let period = Duration::from_secs(reaction_period_minutes.max(0) as u64 * 60);
        let now = SystemTime::now();
        let window_start = now.checked_sub(period).unwrap_or(SystemTime::UNIX_EPOCH);

        let (count, oldest) = {
            let connection = match conn.lock() {
                Ok(connection) => connection,
                Err(_) => {
                    error!("Database mutex is poisoned");
                    return 0;
                }
            };

            match recent_reaction_window(&connection, self.id, window_start) {
                Ok(window) => window,
                Err(error) => {
                    warn!(%error, "Unable to read the reaction window, allowing the reaction");
                    return 0;
                }
            }
        };

        if count < reaction_limit as u32 {
            return 0;
        }

        let Some(oldest) = oldest else {
            return 0;
        };

        // A reaction timestamped in the future would make duration_since fail; treat it as
        // "just happened" so the user waits the full period instead of the code panicking.
        let elapsed = now.duration_since(oldest).unwrap_or(Duration::ZERO);
        period.saturating_sub(elapsed).as_secs() as i64
    }

    pub fn has_user_already_reacted_to_message_event_id(
        &self,
        conn: &Arc<Mutex<Connection>>,
        message_event_id: &str,
    ) -> bool {
        let connection = match conn.lock() {
            Ok(connection) => connection,
            Err(_) => {
                error!("Database mutex is poisoned");
                return true;
            }
        };

        match has_reacted_to_message(&connection, self.id, message_event_id) {
            Ok(reacted) => reacted,
            Err(error) => {
                // Be conservative: on a read error, do not score the reaction twice.
                warn!(%error, "Unable to check for an earlier reaction, skipping this one");
                true
            }
        }
    }

    pub fn add_reaction(&mut self, conn: &Arc<Mutex<Connection>>, message_event_id: &str) {
        let connection = match conn.lock() {
            Ok(connection) => connection,
            Err(_) => {
                error!("Database mutex is poisoned");
                return;
            }
        };

        if let Err(error) =
            insert_user_reaction(&connection, self.id, SystemTime::now(), message_event_id)
        {
            error!(%error, "Failed to insert user reaction");
        }

        // user_reaction rows are kept indefinitely on purpose. The table serves the cooldown
        // window, which only looks at recent rows, and the "has this user already reacted to
        // this message" check, which has to remember every reaction. A time based cleanup
        // would quietly break the second one and let old messages be scored again. The rows
        // are tiny and indexed.
    }
}

pub fn insert_user_room_data(
    conn: &Arc<Mutex<Connection>>,
    user_room_data: &UserRoomData,
) -> Result<(), Error> {
    let sql = "INSERT INTO user_room_data (user_id, room_id, social_credit) VALUES (?1, ?2, ?3)";
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        params![
            &user_room_data.user_id,
            &user_room_data.room_id,
            &user_room_data.social_credit
        ],
    )?;

    Ok(())
}

/// Add `delta` to a user's score in one statement and return the new value.
///
/// The previous flow read the score, added the emoji value in Rust and wrote the result back
/// with `SET social_credit = ?`. Event handlers run concurrently, so two reactions landing at
/// the same time read the same starting value and one of the two changes was lost. Letting
/// SQLite do the arithmetic removes the read-modify-write window.
pub fn add_social_credit(
    conn: &Arc<Mutex<Connection>>,
    user_id: i32,
    room_id: &str,
    delta: i32,
) -> Result<i32, Error> {
    let connection = conn.lock().unwrap();
    add_social_credit_on(&connection, user_id, room_id, delta)
}

/// Same as [`add_social_credit`], for callers that already hold the connection -- the payout
/// applies every change of a period inside one transaction.
pub fn add_social_credit_on(
    conn: &Connection,
    user_id: i32,
    room_id: &str,
    delta: i32,
) -> Result<i32, Error> {
    let sql = "UPDATE user_room_data SET social_credit = social_credit + ?1 \
               WHERE user_id = ?2 AND room_id = ?3 \
               RETURNING social_credit";

    conn.query_row(sql, params![delta, user_id, room_id], |row| row.get(0))
}

/// Everybody who has a score in this room.
///
/// The activity payout needs the full list, not just the people who did something: whoever is
/// on it and has no activity is the one who gets docked.
pub fn users_with_room_data(conn: &Connection, room_id: &str) -> Result<Vec<RoomUser>, Error> {
    let mut stmt = conn.prepare(
        "SELECT u.id, u.name, u.url \
           FROM user_room_data d \
           JOIN user u ON u.id = d.user_id \
          WHERE d.room_id = ?1",
    )?;

    let rows = stmt.query_map(params![room_id], |row| {
        Ok(RoomUser {
            user_id: row.get(0)?,
            name: row.get(1)?,
            url: row.get(2)?,
        })
    })?;

    rows.collect()
}

/// A user that has a score in some room, in the shape the `user` table stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomUser {
    pub user_id: i32,
    pub name: String,
    pub url: String,
}

pub fn find_user_room_data_by_user_id_and_room_id(
    conn: &Arc<Mutex<Connection>>,
    user_id: i32,
    room_id: &str,
) -> Result<UserRoomData, Error> {
    let sql = "SELECT id, user_id, room_id, social_credit FROM user_room_data WHERE user_id=?1 AND room_id=?2";
    let connection = conn.lock().unwrap();

    let mut stmt = connection.prepare(sql)?;
    let mut rows = stmt.query(params![user_id, room_id])?;

    if let Some(row) = rows.next()? {
        Ok(UserRoomData {
            id: row.get(0)?,
            user_id: row.get(1)?,
            room_id: row.get(2)?,
            social_credit: row.get(3)?,
        })
    } else {
        Err(Error::QueryReturnedNoRows)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{
        UserRoomData, add_social_credit, find_user_room_data_by_user_id_and_room_id,
        insert_user_room_data,
    };
    use crate::data::user_reaction::insert_user_reaction;
    use crate::test_support::test_db;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    const ROOM: &str = "!room:example.org";

    fn seed(db: &Arc<Mutex<Connection>>) -> UserRoomData {
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO user (id, name, url, user_type) VALUES (1,'alice','example.org',0)",
                [],
            )
            .unwrap();
        }
        insert_user_room_data(
            db,
            &UserRoomData {
                id: -1,
                user_id: 1,
                room_id: ROOM.to_owned(),
                social_credit: 250,
            },
        )
        .unwrap();
        find_user_room_data_by_user_id_and_room_id(db, 1, ROOM).unwrap()
    }

    fn record_reaction(
        db: &Arc<Mutex<Connection>>,
        room_data: &UserRoomData,
        minutes_ago: u64,
        message: &str,
    ) {
        let when = SystemTime::now() - Duration::from_secs(minutes_ago * 60);
        let conn = db.lock().unwrap();
        insert_user_reaction(&conn, room_data.id, when, message).unwrap();
    }

    #[test]
    fn no_cooldown_below_the_limit() {
        let db = test_db();
        let room_data = seed(&db);
        record_reaction(&db, &room_data, 1, "$m1");

        assert_eq!(room_data.get_time_till_user_can_react(&db, 20, 2), 0);
    }

    /// With a limit of 2 over 20 minutes and reactions at t=0 and t=19, the user may react
    /// again at t=20. The old code measured from the newest reaction and reported t=39.
    #[test]
    fn the_wait_is_measured_from_the_oldest_reaction_in_the_window() {
        let db = test_db();
        let room_data = seed(&db);
        record_reaction(&db, &room_data, 19, "$m1");
        record_reaction(&db, &room_data, 0, "$m2");

        let remaining = room_data.get_time_till_user_can_react(&db, 20, 2);

        assert!(
            remaining > 0,
            "the limit is reached, so there has to be a wait"
        );
        assert!(
            (30..=70).contains(&remaining),
            "expected roughly one minute left, got {remaining}s"
        );
    }

    #[test]
    fn reactions_outside_the_window_do_not_count() {
        let db = test_db();
        let room_data = seed(&db);
        record_reaction(&db, &room_data, 120, "$m1");
        record_reaction(&db, &room_data, 90, "$m2");

        assert_eq!(room_data.get_time_till_user_can_react(&db, 20, 2), 0);
    }

    /// A timestamp in the future is possible after an NTP step or a container moving hosts.
    /// The previous implementation called duration_since(..).unwrap() and panicked.
    #[test]
    fn a_reaction_in_the_future_does_not_panic() {
        let db = test_db();
        let room_data = seed(&db);
        {
            let conn = db.lock().unwrap();
            let future = SystemTime::now() + Duration::from_secs(3600);
            insert_user_reaction(&conn, room_data.id, future, "$m1").unwrap();
            insert_user_reaction(&conn, room_data.id, future, "$m2").unwrap();
        }

        let remaining = room_data.get_time_till_user_can_react(&db, 20, 2);

        assert!(
            (0..=20 * 60).contains(&remaining),
            "unexpected wait {remaining}s"
        );
    }

    #[test]
    fn a_limit_of_zero_disables_the_cooldown() {
        let db = test_db();
        let room_data = seed(&db);
        record_reaction(&db, &room_data, 0, "$m1");

        assert_eq!(room_data.get_time_till_user_can_react(&db, 20, 0), 0);
    }

    #[test]
    fn already_reacted_only_matches_the_same_message() {
        let db = test_db();
        let mut room_data = seed(&db);
        room_data.add_reaction(&db, "$m1");

        assert!(room_data.has_user_already_reacted_to_message_event_id(&db, "$m1"));
        assert!(!room_data.has_user_already_reacted_to_message_event_id(&db, "$m2"));
    }

    /// Event handlers run concurrently. Reading the score, adding in Rust and writing the
    /// result back lost one of two simultaneous changes.
    #[test]
    fn the_score_update_is_additive() {
        let db = test_db();
        let room_data = seed(&db);

        assert_eq!(
            add_social_credit(&db, room_data.user_id, ROOM, -25).unwrap(),
            225
        );
        assert_eq!(
            add_social_credit(&db, room_data.user_id, ROOM, 10).unwrap(),
            235
        );

        let reloaded = find_user_room_data_by_user_id_and_room_id(&db, 1, ROOM).unwrap();
        assert_eq!(reloaded.social_credit, 235);
    }

    #[test]
    fn updating_a_missing_row_is_an_error_rather_than_a_silent_no_op() {
        let db = test_db();
        seed(&db);

        assert!(add_social_credit(&db, 1, "!other:example.org", 10).is_err());
    }
}
