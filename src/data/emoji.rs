use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Params};
use tracing::error;

#[derive(Clone)]
pub struct Emoji {
    /// Row id. Kept so the struct mirrors the table; not read by the bot itself.
    #[allow(dead_code)]
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
    let sql = "SELECT id, room_id, emoji, social_credit FROM emoji WHERE emoji = ?1 AND room_id = ?2";
    let params = params![emoji, room_id];
    match do_get_emoji_sql(conn, sql, params) {
        Ok(mut emoji) => {
            if emoji.len() == 1 {
                return Some(emoji.remove(0));
            }
            None
        },
        Err(e) => {
            error!(error = %e, "Database error");
            None
        },
    }
}

pub fn find_all_emoji_for_room_in_db(conn: &Arc<Mutex<Connection>>, room_id: &String) -> Option<Vec<Emoji>> {
    let sql = "SELECT id, room_id, emoji, social_credit FROM emoji WHERE room_id = ?1";
    let params = params![room_id];
    match do_get_emoji_sql(conn, sql, params) {
        Ok(emoji) => Some(emoji),
        Err(e) => {
            error!(error = %e, "Database error");
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
            error!(error = %e, "Database error");
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

pub fn delete_emoji(conn: &Arc<Mutex<Connection>>, emoji: &String, room_id: &String) -> Result<usize, Error> {
    let sql = "DELETE FROM emoji WHERE emoji = ?1 AND room_id = ?2";
    let connection = conn.lock().unwrap();

    connection.execute(sql, params![emoji, room_id])
}

#[cfg(test)]
mod tests {
    use super::{Emoji, delete_emoji, find_all_emoji_for_room_in_db, find_emoji_in_db, insert_emoji};
    use crate::test_support::test_db;

    fn emoji(room: &str, symbol: &str, credit: i32) -> Emoji {
        Emoji { id: -1, room_id: room.to_owned(), emoji: symbol.to_owned(), social_credit: credit }
    }

    #[test]
    fn registers_and_finds_an_emoji() {
        let db = test_db();
        insert_emoji(&db, &emoji("!r", "😑", -25)).unwrap();

        let found = find_emoji_in_db(&db, &"😑".to_owned(), &"!r".to_owned()).unwrap();

        assert_eq!(found.social_credit, -25);
    }

    #[test]
    fn emojis_are_scoped_to_a_room() {
        let db = test_db();
        insert_emoji(&db, &emoji("!r", "😑", -25)).unwrap();

        assert!(find_emoji_in_db(&db, &"😑".to_owned(), &"!other".to_owned()).is_none());
    }

    #[test]
    fn unregistering_removes_only_that_rooms_entry() {
        let db = test_db();
        insert_emoji(&db, &emoji("!r", "😑", -25)).unwrap();
        insert_emoji(&db, &emoji("!other", "😑", -25)).unwrap();

        assert_eq!(delete_emoji(&db, &"😑".to_owned(), &"!r".to_owned()).unwrap(), 1);

        assert!(find_emoji_in_db(&db, &"😑".to_owned(), &"!r".to_owned()).is_none());
        assert!(find_emoji_in_db(&db, &"😑".to_owned(), &"!other".to_owned()).is_some());
    }

    #[test]
    fn lists_every_emoji_of_a_room() {
        let db = test_db();
        insert_emoji(&db, &emoji("!r", "😑", -25)).unwrap();
        insert_emoji(&db, &emoji("!r", "👍", 10)).unwrap();
        insert_emoji(&db, &emoji("!other", "🎉", 1)).unwrap();

        let listed = find_all_emoji_for_room_in_db(&db, &"!r".to_owned()).unwrap();

        assert_eq!(listed.len(), 2);
    }
}
