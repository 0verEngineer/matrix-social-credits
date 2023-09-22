use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Result};

#[derive(Clone)]
pub struct UserSocialCredit {
    pub id: i32,
    pub user_id: i32,
    pub room_id: String,
    pub social_credit: i32,
}

pub fn create_table_user_social_credit(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user_social_credit (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            room_id TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []).expect("Failed to create user_social_credit table");
}

pub fn insert_user_social_credit(conn: &Arc<Mutex<Connection>>, user_social_credit: &UserSocialCredit) -> Result<(), Error> {
    let sql = "INSERT INTO user_social_credit (user_id, room_id, social_credit) VALUES (?1, ?2, ?3)";
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        params![
            &user_social_credit.user_id,
            &user_social_credit.room_id,
            &user_social_credit.social_credit
        ]
    )?;

    Ok(())
}

pub fn update_user_social_credit(conn: &Arc<Mutex<Connection>>, user_social_credit: &UserSocialCredit) -> Result<(), Error> {
    let sql = "UPDATE user_social_credit SET social_credit=?1 WHERE user_id=?2 AND room_id=?3";
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        params![
            &user_social_credit.social_credit,
            &user_social_credit.user_id,
            &user_social_credit.room_id,
        ]
    )?;

    Ok(())
}

pub fn find_user_social_credit_by_user_id_and_room_id(conn: &Arc<Mutex<Connection>>, user_id: i32, room_id: &String) -> Result<Option<UserSocialCredit>, Error> {
    let sql = "SELECT id, user_id, room_id, social_credit FROM user_social_credit WHERE user_id=?1 AND room_id=?2";
    let connection = conn.lock().unwrap();

    let mut stmt = connection.prepare(sql)?;
    let mut rows = stmt.query(params![&user_id, room_id])?;

    if let Some(row) = rows.next()? {
        let user_social_credit = UserSocialCredit {
            id: row.get(0)?,
            user_id: row.get(1)?,
            room_id: row.get(2)?,
            social_credit: row.get(3)?,
        };

        Ok(Some(user_social_credit))
    }
    else {
        Ok(None)
    }
}