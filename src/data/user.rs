use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Params, Statement, ToSql};
use crate::data::user_room_data::UserRoomData;
use tracing::{error, warn};

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
    pub user_type: UserType,
    pub room_data: Option<UserRoomData>
}

pub struct HtmlAndTextAnswer {
    pub text: String,
    pub html: String,
}

pub fn create_table_user(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS user (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            user_type INTEGER NOT NULL
    )", []).expect("Failed to create user table");
}

pub fn insert_user(conn: &Arc<Mutex<Connection>>, user: &User) -> Result<(), Error> {
    let sql = "INSERT INTO user (name, url, user_type) VALUES (?1, ?2, ?3)";
    let user_type_as_int = get_user_type_as_int(user);
    let connection = conn.lock().unwrap();

    connection.execute(
        sql,
        &[
            &user.name as &dyn ToSql,
            &user.url as &dyn ToSql,
            &user_type_as_int as &dyn ToSql
        ]
    )?;

    Ok(())
}

pub fn update_user(conn: &Arc<Mutex<Connection>>, user: &User) -> Result<(), Error> {
    let sql = "UPDATE user SET user_type=?1 WHERE id=?2";
    let connection = conn.lock().unwrap();
    let user_type_as_int = get_user_type_as_int(user);

    connection.execute(
        sql,
        &[
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
    let sql = "SELECT id, name, url, user_type FROM user WHERE name=?1 AND url=?2";
    let params = params![name, url];
    match do_get_user_sql(conn, sql, params) {
        Ok(mut users) => {
            if users.len() > 1 {
                warn!(%name, %url, "Multiple users found for the same name and url");
            }
            users.pop()
        },
        Err(e) => {
            error!(error = %e, "Database error");
            None
        },
    }
}

/// All users that have room data for `room_id`, except the bot itself.
///
/// The bot used to be filtered by the hardcoded name `social-credit-system`, which silently
/// stopped working as soon as the bot account was called anything else. It is now excluded by
/// the localpart and server name of its actual user id.
pub fn find_all_users_with_room_data_in_db(
    conn: &Arc<Mutex<Connection>>,
    room_id: &String,
    own_localpart: &str,
    own_server_name: &str,
) -> Option<Vec<User>> {
    let sql = "SELECT user.id, user.name, user.url, user.user_type, user_room_data.id, user_room_data.user_id, user_room_data.room_id, user_room_data.social_credit \
                        FROM user INNER JOIN user_room_data ON user.id=user_room_data.user_id \
                        WHERE user_room_data.room_id=?1 AND NOT (user.name=?2 AND user.url=?3)";
    let params = params![room_id, own_localpart, own_server_name];
    let connection = conn.lock().unwrap();

    let mut stmt = match connection.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            error!(error = %e, "Database error");
            return None;
        }
    };

    let users = do_get_user_sql_inner(params, &mut stmt, true);

    match users {
        Ok(users) => Some(users),
        Err(e) => {
            error!(error = %e, "Database error");
            None
        }
    }
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
            error!(error = %e, "Database error");
            return Err(e);
        }
    };

    let users = do_get_user_sql_inner(params, &mut stmt, false);

    return users;
}

fn do_get_user_sql_inner<P: Params>(params: P, stmt: &mut Statement, with_room_data: bool) -> Result<Vec<User>, Error> {
    let users: Result<Vec<User>, _> = stmt.query_map(params, |row| {
        Ok(User {
            id: row.get(0)?,
            name: row.get(1)?,
            url: row.get(2)?,
            user_type: match row.get::<_, i32>(3)? {
                0 => UserType::Default,
                1 => UserType::Moderator,
                2 => UserType::Admin,
                _ => UserType::Default,
            },
            room_data: match with_room_data {
                true => Some(UserRoomData {
                    id: row.get(4)?,
                    user_id: row.get(5)?,
                    room_id: row.get(6)?,
                    social_credit: row.get(7)?,
                }),
                false => None,
            },
        })
    }).and_then(|mapped_rows| mapped_rows.collect());
    users
}
