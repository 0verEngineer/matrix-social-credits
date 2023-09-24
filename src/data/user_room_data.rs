use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use rusqlite::{Connection, Error, params, Result};
use crate::data::user_reaction::{cleanup_table_user_reaction, get_user_reactions, UserReaction};

#[derive(Clone)]
pub struct UserRoomData {
    pub id: i32,
    pub user_id: i32,
    pub room_id: String,
    pub social_credit: i32,
    pub last_reactions: Vec<UserReaction>,
}

impl UserRoomData {
    /// Checks if a user is able to react and change the social credit of another user,
    /// returns 0 if the user can react,
    /// returns the time in seconds until the user can react again otherwise
    pub fn get_time_till_user_can_react(&self, reaction_period_minutes: i32, reaction_limit: i32) -> i64 {
        let now = SystemTime::now();

        // Filter out the reactions that are outside the reaction_period_minutes
        let recent_reactions: Vec<_> = self.last_reactions.iter()
            .filter(|&reaction| now.duration_since(reaction.time).unwrap() <= Duration::from_secs((reaction_period_minutes * 60) as u64))
            .collect();

        // If there are less than reaction_limit within the reaction_period_minutes, the user can react
        if recent_reactions.len() < reaction_limit as usize {
            return 0;
        }

        // Calculate the time until the most recent of these can expire
        if let Some(latest_reaction) = recent_reactions.iter().max() {
            if let Ok(time_since_latest) = now.duration_since(latest_reaction.time) {
                let time_left = Duration::from_secs((reaction_period_minutes * 60) as u64) - time_since_latest;
                return time_left.as_secs() as i64;
            }
        }

        // In case of any unexpected issue, assume the user can react
        0
    }

    pub fn add_reaction(&mut self, conn: &Arc<Mutex<Connection>>, reaction_period_minutes: i32) {
        let now = SystemTime::now();
        let reaction_period_duration = Duration::from_secs((reaction_period_minutes * 60) as u64);
        let prev = now - reaction_period_duration;
        let reaction = UserReaction::new(self.id, now);
        self.last_reactions.push(reaction.clone());

        if reaction.insert(&conn.lock().unwrap()).is_err() {
            println!("Failed to insert user reaction");
        }

        self.last_reactions.retain(|reaction| now.duration_since(reaction.time).unwrap_or(Duration::from_secs(0)) <= reaction_period_duration);
        if cleanup_table_user_reaction(
            &conn.lock().unwrap(),
            prev.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::from_secs(0)).as_secs() as i32).is_err()
        {
            println!("Failed to cleanup user reactions");
        }
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

pub fn update_user_room_data(conn: &Arc<Mutex<Connection>>, user_room_data: &UserRoomData) -> Result<(), Error> {
    let sql = "UPDATE user_room_data SET social_credit=?1 WHERE user_id=?2 AND room_id=?3";
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        params![
            &user_room_data.social_credit,
            &user_room_data.user_id,
            &user_room_data.room_id,
        ]
    )?;

    Ok(())
}

pub fn find_user_room_data_by_user_id_and_room_id(conn: &Arc<Mutex<Connection>>, user_id: i32, room_id: &String) -> Result<UserRoomData, Error> {
    let sql = "SELECT id, user_id, room_id, social_credit FROM user_room_data WHERE user_id=?1 AND room_id=?2";
    let connection = conn.lock().unwrap();

    let mut stmt = connection.prepare(sql)?;
    let mut rows = stmt.query(params![&user_id, room_id])?;

    if let Some(row) = rows.next()? {
        let reaction_result = get_user_reactions(&connection, row.get(0)?);
        let reactions = if let Ok(r) = &reaction_result {
            if r.is_empty() {
                Vec::<UserReaction>::new()
            } else {
                r.clone() // Clone is required since `r` is a reference and we want to own the data
            }
        } else {
            Vec::<UserReaction>::new()
        };

        let user_room_data = UserRoomData {
            id: row.get(0)?,
            user_id: row.get(1)?,
            room_id: row.get(2)?,
            social_credit: row.get(3)?,
            last_reactions: reactions
        };

        Ok(user_room_data)
    }
    else {
        Err(Error::QueryReturnedNoRows)
    }
}
