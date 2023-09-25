use std::time::SystemTime;
use rusqlite::Error;

#[derive(Clone)]
pub struct UserReaction {
    pub id: i32,
    pub user_room_data_id: i32,
    pub time: SystemTime,
    pub message_event_id: String, // The event id of the message that was reacted to
}

impl UserReaction {
    pub fn new(user_room_data_id: i32, reaction_time: SystemTime, message_event_id: String) -> Self {
        Self {
            id: -1,
            user_room_data_id,
            time: reaction_time,
            message_event_id,
        }
    }

    pub fn insert(&self, conn: &rusqlite::Connection) -> Result<(), String> {
        let sql = "INSERT INTO user_reaction (user_room_data_id, time, message_event_id) VALUES (?1, ?2, ?3)";
        let duration_since_epoch_opt = self.time.duration_since(SystemTime::UNIX_EPOCH);
        if duration_since_epoch_opt.is_err() {
            let msg = "Failed to convert reaction time to epoch seconds";
            println!("{}", msg);
            return Err(msg.to_string());
        }
        let epoch_secs = duration_since_epoch_opt.unwrap().as_secs();

        conn.execute(
            sql,
            &[
                &self.user_room_data_id as &dyn rusqlite::ToSql,
                &epoch_secs as &dyn rusqlite::ToSql,
                &self.message_event_id as &dyn rusqlite::ToSql,
            ]
        ).map_err(|err| err.to_string())?;
        Ok(())
    }
}

/// Deletes all reactions that are older than the given epoch time
pub fn cleanup_table_user_reaction(conn: &rusqlite::Connection, epoch_time: i32) -> Result<(), rusqlite::Error> {
    let sql = "DELETE FROM user_reaction WHERE time < ?1";

    conn.execute(
        sql,
        [epoch_time]
    )?;
    Ok(())
}

pub fn create_table_user_reaction(conn: &rusqlite::Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user_reaction (
                id INTEGER PRIMARY KEY,
                user_room_data_id INTEGER NOT NULL REFERENCES user_room_data(id),
                time INTEGER NOT NULL,
                message_event_id TEXT NOT NULL
        )", []).expect("Failed to create user_reaction table");
}

pub fn get_user_reactions(conn: &rusqlite::Connection, user_room_data_id: i32) -> Result<Vec<UserReaction>, rusqlite::Error> {
    let sql = "SELECT id, user_room_data_id, time, message_event_id FROM user_reaction WHERE user_room_data_id = ?1";

    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([user_room_data_id])?;

    let mut reactions = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let user_room_data_id: i32 = row.get(1)?;
        let time: i64 = row.get(2)?;
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(time as u64);
        let message_event_id: String = row.get(3)?;

        reactions.push(UserReaction {
            id,
            user_room_data_id,
            time,
            message_event_id
        });
    }

    Ok(reactions)
}

impl PartialOrd for UserReaction {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.time.partial_cmp(&other.time)
    }
}

impl Ord for UserReaction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time.cmp(&other.time)
    }
}

impl PartialEq for UserReaction {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.user_room_data_id == other.user_room_data_id
    }
}

impl Eq for UserReaction {}