use crate::data::user::{
    HtmlAndTextAnswer, User, UserType, find_all_users_with_room_data_in_db, find_user_in_db,
    insert_user, update_user,
};
use crate::data::user_room_data::{
    UserRoomData, find_user_room_data_by_user_id_and_room_id, insert_user_room_data,
};
use crate::utils::message::{escape_html, heading};
use matrix_sdk::ruma::{OwnedUserId, ServerName, UserId};
use matrix_sdk::{Room, RoomMemberships};
use rusqlite::Connection;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};

/// Upper bound on how many users a single `!list` answer shows.
///
/// A Matrix event may not exceed 64 KiB, and there was no limit at all before.
const MAX_LISTED_USERS: usize = 100;

pub fn compare_user(user1: &User, user2: &User) -> bool {
    user1.name == user2.name && user1.url == user2.url
}

/// Split a Matrix user id into the two columns the `user` table stores.
///
/// The database predates this and keeps localpart and server name in separate columns, so
/// this is the single place that maps between the two representations.
pub fn split_user_id(user_id: &UserId) -> (String, String) {
    (
        user_id.localpart().to_owned(),
        user_id.server_name().to_string(),
    )
}

/// Build a user id from an `ADMIN_USERNAME` value.
///
/// Accepts both a bare localpart (`alice`) and a full user id (`@alice:example.org`). A bare
/// localpart is resolved against `server_name`, which must be the server name from the bot's
/// own user id -- *not* the host of `MATRIX_HOMESERVER_URL`. With `.well-known` delegation
/// those two differ (`matrix.example.org` vs `example.org`), and deriving the domain from the
/// URL used to hand out an admin id that never matched a real user.
pub fn resolve_configured_user_id(
    configured: &str,
    server_name: &ServerName,
) -> Option<OwnedUserId> {
    let configured = configured.trim();

    if configured.starts_with('@') {
        return UserId::parse(configured).ok();
    }

    UserId::parse(format!("@{configured}:{server_name}")).ok()
}

/// Look up a user, creating the row and the room data if they are not there yet.
///
/// `room_id` is `None` when there is no room context, which is the case for the admin setup
/// at startup.
pub fn setup_user(
    conn: &Arc<Mutex<Connection>>,
    room_id: Option<&str>,
    user_id: &UserId,
    user_type: UserType,
    initial_social_credit: i32,
) -> Option<User> {
    let (username, domain) = split_user_id(user_id);

    let user_opt = find_user_in_db(conn, &username, &domain);
    if let Some(mut actual_user) = user_opt {
        setup_user_room_data_for_room(conn, room_id, &mut actual_user, initial_social_credit);
        return Some(actual_user);
    }

    debug!(user = %user_id, "User not found in db, creating new one");

    let user = User {
        id: -1,
        name: username.clone(),
        url: domain.clone(),
        user_type,
        room_data: None,
    };

    if insert_user(conn, &user).is_ok() {
        let Some(mut inserted_user) = find_user_in_db(conn, &username, &domain) else {
            error!(user = %user_id, "Failed to find user in db after inserting");
            return None;
        };
        setup_user_room_data_for_room(conn, room_id, &mut inserted_user, initial_social_credit);
        return Some(inserted_user);
    }

    None
}

fn setup_user_room_data_for_room(
    conn: &Arc<Mutex<Connection>>,
    room_id: Option<&str>,
    user: &mut User,
    initial_social_credit: i32,
) {
    let Some(room_id) = room_id else {
        return;
    };

    if let Ok(room_data) = find_user_room_data_by_user_id_and_room_id(conn, user.id, room_id) {
        user.room_data = Some(room_data);
        return;
    }

    debug!(user = %user.name, room_id, "Room data for user not found in db, creating");

    let room_data = UserRoomData {
        id: -1,
        user_id: user.id,
        room_id: room_id.to_owned(),
        social_credit: initial_social_credit,
    };

    if let Err(error) = insert_user_room_data(conn, &room_data) {
        // A uniqueness violation here means a concurrently handled event created the row
        // first, in which case the lookup below finds theirs.
        debug!(user = %user.name, %error, "Unable to insert room data for user");
    }

    // Read the row back instead of keeping the struct built above: that one still carries the
    // placeholder id -1, and the row id is what the user_reaction rows reference. Storing it
    // meant every reaction of a user new to the room was rejected by the foreign key.
    match find_user_room_data_by_user_id_and_room_id(conn, user.id, room_id) {
        Ok(stored) => user.room_data = Some(stored),
        Err(error) => {
            error!(user = %user.name, room_id, %error, "Room data for user is missing after insert");
        }
    }
}

pub fn initial_admin_user_setup(conn: &Arc<Mutex<Connection>>, admin_user_id: &UserId) {
    let (username, domain) = split_user_id(admin_user_id);

    match find_user_in_db(conn, &username, &domain) {
        Some(mut admin_user) => {
            if !matches!(admin_user.user_type, UserType::Admin) {
                admin_user.user_type = UserType::Admin;
                update_user(conn, &admin_user).expect("Failed to update admin user");
            }
        }
        None => {
            // No room, so no room data is created here and the initial score is irrelevant.
            setup_user(conn, None, admin_user_id, UserType::Admin, 0)
                .expect("Failed to construct or register admin user");
        }
    }

    info!(admin = %admin_user_id, "Admin user configured");
}

/// Build the `!list` answer for `room`.
///
/// The scores come from the database, but who is shown comes from the room's current member
/// list. Previously this was a pure database query, and nothing ever removed a
/// `user_room_data` row: once somebody had written a single message in a room they stayed in
/// the list forever, including after leaving, being kicked or being banned.
///
/// Scores of users who left are kept in the database on purpose, so they are still there if
/// somebody rejoins -- they are only hidden from the listing.
pub async fn get_user_list_answer(
    conn: &Arc<Mutex<Connection>>,
    room: &Room,
    own_user_id: &UserId,
) -> HtmlAndTextAnswer {
    let (own_localpart, own_server) = split_user_id(own_user_id);
    let users_opt = find_all_users_with_room_data_in_db(
        conn,
        &room.room_id().to_string(),
        &own_localpart,
        &own_server,
    );
    let empty_answer = HtmlAndTextAnswer {
        html: String::from("No scores"),
        text: String::from("No Scores"),
    };

    let Some(mut users) = users_opt else {
        return empty_answer;
    };

    if users.is_empty() {
        return empty_answer;
    }

    let mut note = "";
    match current_room_members(room).await {
        Some(members) => {
            users.retain(|user| members.contains(&(user.name.clone(), user.url.clone())));
        }
        None => {
            // The homeserver did not hand out the member list (rate limited, or down). Show
            // the stored scores rather than nothing, but say that the list may be stale.
            note = " (member list unavailable, this may include users who left)";
        }
    }

    if users.is_empty() {
        return empty_answer;
    }

    // Sort users by social credit
    users.sort_by(|a, b| {
        let a_credit = a.room_data.as_ref().map_or(0, |sc| sc.social_credit);
        let b_credit = b.room_data.as_ref().map_or(0, |sc| sc.social_credit);
        b_credit.cmp(&a_credit).then_with(|| a.name.cmp(&b.name))
    });

    let total = users.len();
    let truncated = total > MAX_LISTED_USERS;
    users.truncate(MAX_LISTED_USERS);

    let mut text_entries: Vec<String> = Vec::with_capacity(users.len());
    let mut html_entries: Vec<String> = Vec::with_capacity(users.len());

    for (index, user) in users.into_iter().enumerate() {
        let Some(room_data) = user.room_data else {
            continue;
        };
        let rank = index + 1;
        text_entries.push(format!(
            "{}. {}: {}",
            rank, user.name, room_data.social_credit
        ));
        html_entries.push(format!(
            "{}. {}: <b>{}</b>",
            rank,
            escape_html(&user.name),
            room_data.social_credit
        ));
    }

    let cut_note = if truncated {
        format!(" (showing the top {MAX_LISTED_USERS} of {total})")
    } else {
        String::new()
    };

    HtmlAndTextAnswer {
        text: format!(
            "Social Credit Scores{note}{cut_note}:\n{}",
            text_entries.join("\n")
        ),
        html: format!(
            "{}{}",
            heading(&format!("Social Credit Scores{note}{cut_note}:")),
            html_entries.join("<br>")
        ),
    }
}

/// The users currently joined to `room`, in the `(localpart, server name)` shape the `user`
/// table stores.
///
/// Returns `None` when the member list could not be obtained, so callers can tell "nobody is
/// in the room" apart from "we do not know who is in the room".
async fn current_room_members(room: &Room) -> Option<HashSet<(String, String)>> {
    match room.members(RoomMemberships::JOIN).await {
        Ok(members) => Some(
            members
                .iter()
                .map(|member| split_user_id(member.user_id()))
                .collect(),
        ),
        Err(error) => {
            warn!(room_id = %room.room_id(), %error, "Unable to load the room member list");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{ServerName, user_id};

    use super::{resolve_configured_user_id, split_user_id};

    #[test]
    fn splits_a_user_id_into_the_stored_columns() {
        assert_eq!(
            split_user_id(user_id!("@alice:example.org")),
            ("alice".to_owned(), "example.org".to_owned())
        );
    }

    #[test]
    fn resolves_a_bare_localpart_against_the_server_name() {
        let server = ServerName::parse("example.org").unwrap();
        assert_eq!(
            resolve_configured_user_id("alice", &server).unwrap(),
            user_id!("@alice:example.org")
        );
    }

    /// The server name in a user id is not the host of MATRIX_HOMESERVER_URL. With
    /// .well-known delegation the URL is https://matrix.example.org while ids read
    /// @alice:example.org -- deriving the domain from the URL produced an admin id that never
    /// matched anybody.
    #[test]
    fn uses_the_server_name_not_the_homeserver_host() {
        let server = ServerName::parse("example.org").unwrap();
        let resolved = resolve_configured_user_id("alice", &server).unwrap();
        assert_eq!(resolved.server_name().as_str(), "example.org");
    }

    #[test]
    fn accepts_a_full_user_id_on_another_server() {
        let server = ServerName::parse("example.org").unwrap();
        assert_eq!(
            resolve_configured_user_id("@bob:other.example", &server).unwrap(),
            user_id!("@bob:other.example")
        );
    }

    #[test]
    fn ignores_surrounding_whitespace() {
        let server = ServerName::parse("example.org").unwrap();
        assert_eq!(
            resolve_configured_user_id("  alice  ", &server).unwrap(),
            user_id!("@alice:example.org")
        );
    }

    #[test]
    fn rejects_something_that_is_not_a_user_id() {
        let server = ServerName::parse("example.org").unwrap();
        assert!(resolve_configured_user_id("@not a user:", &server).is_none());
    }
}

#[cfg(test)]
mod db_tests {
    use matrix_sdk::ruma::user_id;

    use super::setup_user;
    use crate::data::user::UserType;
    use crate::test_support::test_db;

    const ROOM: &str = "!room:example.org";

    /// setup_user does a non-atomic find, insert, find. Calling it twice must not end up with
    /// two rows for the same person -- which is what the "Multiple users found" log line in
    /// the old code was about.
    #[test]
    fn setting_up_the_same_user_twice_keeps_one_row() {
        let db = test_db();

        let first = setup_user(
            &db,
            None,
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();
        let second = setup_user(
            &db,
            None,
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();

        assert_eq!(first.id, second.id);

        let conn = db.lock().unwrap();
        let users: i64 = conn
            .query_row("SELECT COUNT(*) FROM user", [], |r| r.get(0))
            .unwrap();
        assert_eq!(users, 1);
    }

    #[test]
    fn stores_the_localpart_and_the_server_name_separately() {
        let db = test_db();

        let created = setup_user(
            &db,
            None,
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();

        assert_eq!(created.name, "alice");
        assert_eq!(created.url, "example.org");
    }

    /// The room data used to be handed back with the placeholder id -1 it was built with,
    /// because the insert never read the row back. user_reaction rows reference that id, so
    /// the first reaction of a user new to a room was rejected by the foreign key -- meaning
    /// it was never recorded, the cooldown did not count it, and the same message could be
    /// scored a second time.
    #[test]
    fn room_data_of_a_new_user_carries_the_real_row_id() {
        let db = test_db();

        let user = setup_user(
            &db,
            Some(ROOM),
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();

        let mut room_data = user.room_data.expect("room data must be created");
        assert!(
            room_data.id > 0,
            "expected a real row id, got {}",
            room_data.id
        );

        // The foreign key only accepts an existing row, so this is what actually proves it.
        room_data.add_reaction(&db, "$m1");
        assert!(room_data.has_user_already_reacted_to_message_event_id(&db, "$m1"));
    }

    #[test]
    fn room_data_of_a_known_user_is_read_from_the_database() {
        let db = test_db();

        let first = setup_user(
            &db,
            Some(ROOM),
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();
        let second = setup_user(
            &db,
            Some(ROOM),
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();

        assert_eq!(
            first.room_data.unwrap().id,
            second.room_data.unwrap().id,
            "the second call must find the existing row, not create another one"
        );
    }

    #[test]
    fn without_a_room_no_room_data_is_created() {
        let db = test_db();

        let user = setup_user(
            &db,
            None,
            user_id!("@alice:example.org"),
            UserType::Default,
            250,
        )
        .unwrap();

        assert!(user.room_data.is_none());
    }
}
