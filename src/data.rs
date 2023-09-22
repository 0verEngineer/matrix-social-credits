use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, Params, params, ToSql};

#[derive(Clone)]
pub enum UserType {
    Default,
    Moderator,
    Admin
}

#[derive(Clone)]
pub struct User {
    pub id: i32,
    pub name: String,
    pub url: String,
    pub social_credit: i32,
    pub user_type: UserType,
}

#[derive(Clone)]
pub struct Emoji {
    pub id: i32,
    pub emoji: String,
    pub social_credit: i32,
}

#[derive(Clone)]
pub struct Event {
    pub id: String,
    pub event_type: String,
    pub handled: bool,
}

pub fn create_db_tables(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            social_credit INTEGER NOT NULL,
            user_type INTEGER NOT NULL
    )", []).expect("Failed to create user table");

    conn.execute("CREATE TABLE IF NOT EXISTS emoji (
            id INTEGER PRIMARY KEY,
            emoji TEXT NOT NULL,
            social_credit INTEGER NOT NULL
    )", []).expect("Failed to create emoji table");

    conn.execute("CREATE TABLE IF NOT EXISTS event (
            id TEXT PRIMARY KEY,
            event_type TEXT NOT NULL,
            handled INTEGER NOT NULL
    )", []).expect("Failed to create event table");
}

pub fn insert_user(conn: &Arc<Mutex<Connection>>, user: &User) -> Result<(), Error> {
    let sql = "INSERT INTO user (name, url, social_credit, user_type) VALUES (?1, ?2, ?3, ?4)";
    let user_type_as_int = get_user_type_as_int(user);
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        &[
            &user.name as &dyn ToSql,
            &user.url as &dyn ToSql,
            &user.social_credit as &dyn ToSql,
            &user_type_as_int as &dyn ToSql
        ]
    )?;

    Ok(())
}

pub fn update_user(conn: &Arc<Mutex<Connection>>, user: &User) -> Result<(), Error> {
    let sql = "UPDATE user SET social_credit=?1, user_type=?2 WHERE id=?3";
    let connection = conn.lock().unwrap();
    let user_type_as_int = get_user_type_as_int(user);

    connection.execute(
        sql,
        &[
            &user.social_credit as &dyn ToSql,
            &user_type_as_int as &dyn ToSql,
            &user.id as &dyn ToSql,
        ]
    )?;

    Ok(())
}

fn get_user_type_as_int(user: &User) -> i32 {
    let user_type_as_int = match user.user_type {
        UserType::Default => 0,
        UserType::Moderator => 1,
        UserType::Admin => 2,
    };
    user_type_as_int
}

pub fn find_user_in_db(
    conn: &Arc<Mutex<Connection>>,
    name: &String, url: &String
) -> Option<User> {
    let sql = "SELECT * FROM user WHERE name=?1 AND url=?2";
    let params = params![name, url];
    match do_get_user_sql(conn, sql, params) {
        Ok(mut users) => {
            if users.len() > 1 {
                println!("Error: Multiple users found for name: {} and url: {}", name, url);
            }
            users.pop()
        },
        Err(e) => {
            println!("Database error: {}", e);
            None
        },
    }
}

pub fn find_all_users_in_db(conn: &Arc<Mutex<Connection>>) -> Option<Vec<User>> {
    let sql = "SELECT * FROM user";
    let params = params![];
    match do_get_user_sql(conn, sql, params) {
        Ok(users) => Some(users),
        Err(e) => {
            println!("Database error: {}", e);
            None
        },
    }
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


pub fn insert_event(conn: &Arc<Mutex<Connection>>, event: &Event) -> Result<(), Error> {
    let sql = "INSERT INTO event (id, event_type, handled) VALUES (?1, ?2, ?3)";

    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        &[
            &event.id as &dyn ToSql,
            &event.event_type as &dyn ToSql,
            &event.handled as &dyn ToSql
        ]
    )?;

    Ok(())
}

pub fn find_event_in_db(
    conn: &Arc<Mutex<Connection>>,
    id: &String
) -> Option<Event> {
    let sql = "SELECT * FROM event WHERE id=?1";
    let params = params![id];
    match do_get_event_sql(conn, sql, params) {
        Ok(mut users) => {
            if users.len() > 1 {
                println!("Error: Multiple events found for id: {}", id);
            }
            users.pop()
        },
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

fn do_get_user_sql<P: Params>(
    conn: &Arc<Mutex<Connection>>,
    sql: &str,
    params: P,
) -> Result<Vec<User>, Error> {
    let connection = conn.lock().unwrap();
    let mut stmt = match connection.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            println!("Database error: {}", e);
            return Err(e);
        }
    };

    let users: Result<Vec<User>, _> = stmt.query_map(params, |row| {
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

    return users;
}

fn do_get_event_sql<P: Params>(
    conn: &Arc<Mutex<Connection>>,
    sql: &str,
    params: P,
) -> Result<Vec<Event>, Error> {
    let connection = conn.lock().unwrap();
    let mut stmt = match connection.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            println!("Database error: {}", e);
            return Err(e);
        }
    };

    let events: Result<Vec<Event>, _> = stmt.query_map(params, |row| {
        Ok(Event {
            id: row.get(0)?,
            event_type: row.get(1)?,
            handled: row.get(2)?,
        })
    }).and_then(|mapped_rows| mapped_rows.collect());

    return events;
}

