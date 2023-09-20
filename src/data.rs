use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error};

pub enum UserType {
    Default,
    Admin
}

pub struct User {
    pub id: i32,
    pub name: String,
    pub url: String,
    pub social_credit: i32,
    pub user_type: UserType,
}

pub struct Emoji {
    pub id: i32,
    pub emoji: String,
    pub social_credit: i32,
}

pub struct Sticker {
    pub id: i32,
    pub url: String,
    pub social_credit: i32,
}

pub fn create_db_tables(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user (
            id   INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            social_credit INTEGER NOT NULL,
            user_type INTEGER NOT NULL
    )", []);

    conn.execute("CREATE TABLE IF NOT EXISTS emoji (
            id   INTEGER PRIMARY KEY,
            emoji TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []);

    conn.execute("CREATE TABLE IF NOT EXISTS sticker (
            id   INTEGER PRIMARY KEY,
            url TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []);
}

pub fn insert_user(conn: &Arc<Mutex<Connection>>, user: &User) -> Result<(), Error> {
    let sql = "INSERT INTO user (name, url, social_credit, user_type) VALUES (?1, ?2, ?3, ?4)";

    // Assuming UserType can be converted to an integer for database storage.
    let user_type_as_int = match user.user_type {
        UserType::Default => 0,
        UserType::Admin => 1,
        // Add other variants as needed.
    };

    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        &[
            &user.name as &dyn rusqlite::ToSql,
            &user.url as &dyn rusqlite::ToSql,
            &user.social_credit as &dyn rusqlite::ToSql,
            &user_type_as_int as &dyn rusqlite::ToSql
        ]
    )?;

    Ok(())
}

pub fn get_user(conn: &Arc<Mutex<Connection>>, name: &String, url: &String) -> Option<User> {
    let sql = "SELECT * FROM user WHERE name=?1 AND url=?2";

    let connection = conn.lock().unwrap();
    let mut stmt = match connection.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            println!("Database error: {}", e);
            return None;
        }
    };

    let users: Result<Vec<User>, _> = stmt.query_map([name, url], |row| {
        Ok(User {
            id: row.get(0)?,
            name: row.get(1)?,
            url: row.get(2)?,
            social_credit: row.get(3)?,
            user_type: match row.get::<_, i32>(4)? {
                0 => UserType::Default,
                1 => UserType::Admin,
                _ => UserType::Default,
            },
        })
    }).and_then(|mapped_rows| mapped_rows.collect());

    match users {
        Ok(users) => {
            if users.len() > 1 {
                println!("Error: Multiple users found for name: {} and url: {}", name, url);
            }
            users.into_iter().next() // Returns the first user or None if the vector is empty.
        },
        Err(e) => {
            println!("Database error: {}", e);
            None
        },
    }
}
