use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use rusqlite::{Connection, Error, params, Params};
use tracing::{error, warn};

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
    // seen_at is what the retention job in data::migrations keys off.
    let sql = "INSERT INTO event (id, event_type, handled, seen_at) VALUES (?1, ?2, ?3, ?4)";
    let seen_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let connection = conn.lock().unwrap();

    connection.execute(sql, params![&event.id, &event.event_type, &event.handled, seen_at])?;

    Ok(())
}

pub fn find_event_in_db(
    conn: &Arc<Mutex<Connection>>,
    id: &String
) -> Option<Event> {
    let sql = "SELECT id, event_type, handled FROM event WHERE id=?1";
    let params = params![id];
    match do_get_event_sql(conn, sql, params) {
        Ok(mut users) => {
            if users.len() > 1 {
                warn!(event_id = %id, "Multiple events found for the same id");
            }
            users.pop()
        },
        Err(e) => {
            error!(error = %e, "Database error");
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
            error!(error = %e, "Database error");
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


#[cfg(test)]
mod tests {
    use super::{Event, find_event_in_db, insert_event};
    use crate::test_support::test_db;

    fn event(id: &str) -> Event {
        Event { id: id.to_owned(), event_type: "m.reaction".to_owned(), handled: true }
    }

    #[test]
    fn records_and_finds_a_handled_event() {
        let db = test_db();
        insert_event(&db, &event("$e1")).unwrap();

        assert!(find_event_in_db(&db, &"$e1".to_owned()).is_some());
        assert!(find_event_in_db(&db, &"$e2".to_owned()).is_none());
    }

    /// The deduplication depends on the second insert failing.
    #[test]
    fn the_same_event_cannot_be_recorded_twice() {
        let db = test_db();
        insert_event(&db, &event("$e1")).unwrap();

        assert!(insert_event(&db, &event("$e1")).is_err());
    }

    #[test]
    fn a_recorded_event_carries_a_timestamp_for_the_retention_job() {
        let db = test_db();
        insert_event(&db, &event("$e1")).unwrap();

        let conn = db.lock().unwrap();
        let seen_at: i64 =
            conn.query_row("SELECT seen_at FROM event WHERE id = '$e1'", [], |r| r.get(0)).unwrap();
        assert!(seen_at > 0);
    }
}
