use std::time::SystemTime;

#[derive(Clone)]
pub struct UserReaction {
    pub id: i32,
    pub user_room_data_id: i32,
    pub time: SystemTime,
}

impl UserReaction {
    pub fn new(user_room_data_id: i32, reaction_time: SystemTime) -> Self {
        Self {
            id: -1,
            user_room_data_id,
            time: reaction_time,
        }
    }

    pub fn insert(&self, conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
        let sql = "INSERT INTO user_reaction (user_room_data_id, time) VALUES (?1, ?2)";
        let epoch_secs = self.time.duration_since(SystemTime::UNIX_EPOCH).expect("Time went backwards").as_secs();

        conn.execute(
            sql,
            &[
                &self.user_room_data_id as &dyn rusqlite::ToSql,
                &epoch_secs as &dyn rusqlite::ToSql,
            ]
        )?;
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
                time INTEGER NOT NULL
        )", []).expect("Failed to create user_reaction table");
}

pub fn get_user_reactions(conn: &rusqlite::Connection, user_room_data_id: i32) -> Result<Vec<UserReaction>, rusqlite::Error> {
    let sql = "SELECT id, user_room_data_id, time FROM user_reaction WHERE user_room_data_id = ?1";

    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([user_room_data_id])?;

    let mut reactions = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let user_room_data_id: i32 = row.get(1)?;
        let time: i64 = row.get(2)?;
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(time as u64);

        reactions.push(UserReaction {
            id,
            user_room_data_id,
            time,
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