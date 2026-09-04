use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use rusqlite::{Connection, Error, params, Result};
use crate::data::user_reaction::{has_reacted_to_message, insert_user_reaction, recent_reaction_window};
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

        if let Err(error) = insert_user_reaction(&connection, self.id, SystemTime::now(), message_event_id) {
            error!(%error, "Failed to insert user reaction");
        }

        // user_reaction rows are kept indefinitely on purpose. The table serves the cooldown
        // window, which only looks at recent rows, and the "has this user already reacted to
        // this message" check, which has to remember every reaction. A time based cleanup
        // would quietly break the second one and let old messages be scored again. The rows
        // are tiny and indexed.
    }
}

pub fn create_table_user_room_data(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user_room_data (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES user(id),
            room_id TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []).expect("Failed to create user_room_data table");
}

pub fn insert_user_room_data(conn: &Arc<Mutex<Connection>>, user_room_data: &UserRoomData) -> Result<(), Error> {
    let sql = "INSERT INTO user_room_data (user_id, room_id, social_credit) VALUES (?1, ?2, ?3)";
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        params![
            &user_room_data.user_id,
            &user_room_data.room_id,
            &user_room_data.social_credit
        ]
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
    let sql = "UPDATE user_room_data SET social_credit = social_credit + ?1 \
               WHERE user_id = ?2 AND room_id = ?3 \
               RETURNING social_credit";
    let connection = conn.lock().unwrap();

    connection.query_row(sql, params![delta, user_id, room_id], |row| row.get(0))
}

pub fn find_user_room_data_by_user_id_and_room_id(conn: &Arc<Mutex<Connection>>, user_id: i32, room_id: &String) -> Result<UserRoomData, Error> {
    let sql = "SELECT id, user_id, room_id, social_credit FROM user_room_data WHERE user_id=?1 AND room_id=?2";
    let connection = conn.lock().unwrap();

    let mut stmt = connection.prepare(sql)?;
    let mut rows = stmt.query(params![&user_id, room_id])?;

    if let Some(row) = rows.next()? {
        Ok(UserRoomData {
            id: row.get(0)?,
            user_id: row.get(1)?,
            room_id: row.get(2)?,
            social_credit: row.get(3)?,
        })
    }
    else {
        Err(Error::QueryReturnedNoRows)
    }
}
