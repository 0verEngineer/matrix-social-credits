use std::sync::{Arc, Mutex};
use rusqlite::{Connection, Error, params, Params, ToSql};

#[derive(Clone)]
pub struct Event {
    pub id: String,
    pub event_type: String,
    pub handled: bool,
}

pub fn create_table_event(conn: &Connection) {
    conn.execute("CREATE TABLE IF NOT EXISTS event (
            id TEXT PRIMARY KEY,
            event_type TEXT NOT NULL,
            handled INTEGER NOT NULL
    )", []).expect("Failed to create event table");
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

