use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Params};

#[derive(Clone)]
pub struct Emoji {
    pub id: i32,
    pub room_id: String,
    pub emoji: String,
    pub social_credit: i32,
}

pub fn create_table_emoji(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS emoji (
            id INTEGER PRIMARY KEY,
            room_id TEXT NOT NULL,
            emoji TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []).expect("Failed to create emoji table");
}

pub fn insert_emoji(conn: &Arc<Mutex<Connection>>, emoji: &Emoji) -> Result<(), Error> {
    let sql = "INSERT INTO emoji (room_id, emoji, social_credit) VALUES (?1, ?2, ?3)";
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        &[
            &emoji.room_id as &dyn rusqlite::ToSql,
            &emoji.emoji as &dyn rusqlite::ToSql,
            &emoji.social_credit as &dyn rusqlite::ToSql,
        ]
    )?;

    Ok(())
}

pub fn find_emoji_in_db(conn: &Arc<Mutex<Connection>>, emoji: &String, room_id: &String) -> Option<Emoji> {
    let sql = "SELECT * FROM emoji WHERE emoji = :emoji AND room_id = :room_id";
    let params = params![emoji, room_id];
    match do_get_emoji_sql(conn, sql, params) {
        Ok(mut emoji) => {
            if emoji.len() == 1 {
                return Some(emoji.remove(0));
            }
            None
        },
        Err(e) => {
            println!("Database error: {}", e);
            None
        },
    }
}

pub fn find_all_emoji_for_room_in_db(conn: &Arc<Mutex<Connection>>, room_id: &String) -> Option<Vec<Emoji>> {
    let sql = "SELECT * FROM emoji WHERE room_id = :room_id";
    let params = params![room_id];
    match do_get_emoji_sql(conn, sql, params) {
        Ok(emoji) => Some(emoji),
        Err(e) => {
            println!("Database error: {}", e);
            None
        },
    }
}

fn do_get_emoji_sql<P:Params>(
    conn: &Arc<Mutex<Connection>>,
    sql: &str,
    params: P,
) -> Result<Vec<Emoji>, Error> {
    let connection = conn.lock().unwrap();
    let mut stmt = match connection.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            println!("Database error: {}", e);
            return Err(e);
        }
    };

    let emoji: Result<Vec<Emoji>, _> = stmt.query_map(params, |row| {
        Ok(Emoji {
            id: row.get(0)?,
            room_id: row.get(1)?,
            emoji: row.get(2)?,
            social_credit: row.get(3)?,
        })
    }).and_then(|mapped_rows| mapped_rows.collect());

    return emoji;
}
