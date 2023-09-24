use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Params, Statement, ToSql};
use crate::data::user_reaction::{get_user_reactions, UserReaction};
use crate::data::user_room_data::UserRoomData;

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

pub fn find_all_users_with_room_data_in_db(conn: &Arc<Mutex<Connection>>, room_id: &String) -> Option<Vec<User>> {
    let sql = "SELECT user.id, user.name, user.url, user.user_type, user_room_data.id, user_room_data.user_id, user_room_data.room_id, user_room_data.social_credit \
                        FROM user INNER JOIN user_room_data ON user.id=user_room_data.user_id WHERE user_room_data.room_id=?1 AND user.name NOT LIKE 'social-credit-system'";
    let params = params![room_id];
    let connection = conn.lock().unwrap();

    let mut stmt = match connection.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            println!("Database error: {}", e);
            return None;
        }
    };

    let users = do_get_user_sql_inner(params, &mut stmt, &connection, true);

    if users.is_err() {
        println!("Database error: {}", users.err().unwrap());
        return None;
    }

    return Some(users.unwrap());
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

    let users = do_get_user_sql_inner(params, &mut stmt, &connection, false);

    return users;
}

fn do_get_user_sql_inner<P: Params>(params: P, stmt: &mut Statement, conn: &Connection, with_room_data: bool) -> Result<Vec<User>, Error> {
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
                    last_reactions: get_user_reactions(conn, row.get(4)?)
                        .or_else(|_| -> Result<Vec<UserReaction>, Error> {
                            Ok(Vec::<UserReaction>::new())
                        }).unwrap(),
                }),
                false => None,
            },
        })
    }).and_then(|mapped_rows| mapped_rows.collect());
    users
}
