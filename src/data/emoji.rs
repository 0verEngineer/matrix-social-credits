use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Params};

#[derive(Clone)]
pub struct Emoji {
    pub id: i32,
    pub emoji: String,
    pub social_credit: i32,
}

pub fn create_table_emoji(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS emoji (
            id INTEGER PRIMARY KEY,
            emoji TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []).expect("Failed to create emoji table");
}

pub fn find_all_emoji_in_db(conn: &Arc<Mutex<Connection>>) -> Option<Vec<Emoji>> {
    let sql = "SELECT * FROM emoji";
    let params = params![];
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
            emoji: row.get(1)?,
            social_credit: row.get(2)?,
        })
    }).and_then(|mapped_rows| mapped_rows.collect());

    return emoji;
}
