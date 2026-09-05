//! The weekly activity payout.
//!
//! Messages and images are counted as they arrive (see [`crate::data::activity`]); this is
//! the other half, which turns those counters into social credit once a week and announces
//! the result in the room.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use matrix_sdk::{Client, Room, RoomMemberships};
use rusqlite::Connection;
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::data::activity::{Activity, clear_room_activity, pending_activity};
use crate::data::bot_state::{LAST_PAYOUT_AT, get_timestamp, set_timestamp};
use crate::data::user::HtmlAndTextAnswer;
use crate::data::user_room_data::{RoomUser, add_social_credit_on, users_with_room_data};
use crate::utils::matrix_util::send_message;
use crate::utils::message::{escape_html, notice_html};
use crate::utils::schedule::PayoutSchedule;

/// How often the task looks at the clock.
///
/// Sleeping until the payout instead would mean recomputing the sleep on every clock change
/// and losing the schedule whenever the host suspends. A check a minute costs nothing.
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// A period shorter than this does not charge the inactivity penalty.
///
/// The schedule is a weekday, so a period is normally seven days -- except the first one after
/// the feature is switched on, which is however much is left until the next payout. That can
/// be a few hours, and docking everybody who did not happen to write during those hours is not
/// what the penalty is for. Half a week is the cutoff.
const MIN_PENALTY_PERIOD_HOURS: i64 = 84;

/// How the counters turn into points.
#[derive(Debug, Clone, Copy)]
pub struct PayoutConfig {
    pub points_per_message: i32,
    pub points_per_image: i32,
    /// Deducted from everybody who was in the room the whole period without sending
    /// anything. A positive number; `0` switches the penalty off.
    pub inactivity_penalty: i32,
    /// How many people the announcement names before it summarises the rest.
    pub max_entries: usize,
}

impl PayoutConfig {
    /// All three at zero means the whole feature is off, counting included.
    ///
    /// The penalty counts here too: deciding who was idle needs the same counters as awarding
    /// points does.
    pub fn is_enabled(&self) -> bool {
        self.points_per_message != 0 || self.points_per_image != 0 || self.inactivity_penalty != 0
    }
}

/// One line of the announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayoutEntry {
    pub name: String,
    pub points: i32,
    pub messages: i32,
    pub images: i32,
}

/// What one room's period came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomPayout {
    /// Everybody who sent something, highest first.
    pub earners: Vec<PayoutEntry>,
    /// Names of everybody who has a score here and sent nothing.
    pub idle: Vec<String>,
    /// What each of them lost, as a positive number.
    pub penalty: i32,
}

impl RoomPayout {
    fn is_empty(&self) -> bool {
        self.earners.is_empty() && self.idle.is_empty()
    }
}

/// Run the payout whenever the schedule says so.
pub fn spawn_payout_task(
    conn: Arc<Mutex<Connection>>,
    client: Client,
    schedule: PayoutSchedule,
    config: PayoutConfig,
) {
    info!(schedule = %schedule, "Activity payout scheduled");

    tokio::spawn(async move {
        // The first check is a whole interval away on purpose: at startup the client has not
        // synced yet, and a payout that is already due would award points into rooms it
        // cannot announce in.
        let mut interval =
            tokio::time::interval_at(Instant::now() + CHECK_INTERVAL, CHECK_INTERVAL);

        loop {
            interval.tick().await;

            let now = Utc::now();

            let last = {
                let Some(connection) = lock_or_stop(&conn) else {
                    return;
                };
                last_payout(&connection, now)
            };

            let Some(last) = last else {
                continue;
            };
            if !schedule.is_due(last, now) {
                continue;
            }

            {
                // Recorded before the payout runs. If anything below fails, the points are
                // still awarded only once; the other order could award them twice.
                let Some(connection) = lock_or_stop(&conn) else {
                    return;
                };
                if let Err(error) = set_timestamp(&connection, LAST_PAYOUT_AT, now.timestamp()) {
                    warn!(%error, "Unable to record the activity payout, skipping it");
                    continue;
                }
            }

            let run_config = penalty_for_period(config, now - last);
            if run_config.inactivity_penalty != config.inactivity_penalty {
                info!(
                    hours = (now - last).num_hours(),
                    "Short period, not charging the inactivity penalty this time"
                );
            }

            run_payout(&conn, &client, run_config).await;
        }
    });
}

/// Switch the penalty off for a period too short to have been able to take part in.
fn penalty_for_period(config: PayoutConfig, period: TimeDelta) -> PayoutConfig {
    if period.num_hours() >= MIN_PENALTY_PERIOD_HOURS {
        return config;
    }

    PayoutConfig {
        inactivity_penalty: 0,
        ..config
    }
}

/// Lock the database, or give up on the payout for good.
///
/// A poisoned mutex means some other task panicked while holding it. That does not heal, so
/// the caller ends the task instead of logging the same line every minute until the bot is
/// restarted.
fn lock_or_stop(conn: &Arc<Mutex<Connection>>) -> Option<MutexGuard<'_, Connection>> {
    match conn.lock() {
        Ok(connection) => Some(connection),
        Err(_) => {
            error!("Database mutex is poisoned, the activity payout is stopping");
            None
        }
    }
}

/// When the last payout happened, starting a period if there is none yet.
///
/// `None` means there is nothing to compare the schedule against this time round, never that
/// a payout is due.
fn last_payout(conn: &Connection, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let stored = match get_timestamp(conn, LAST_PAYOUT_AT) {
        Ok(stored) => stored,
        Err(error) => {
            warn!(%error, "Unable to read the last activity payout");
            return None;
        }
    };

    if let Some(seconds) = stored {
        match DateTime::from_timestamp(seconds, 0) {
            Some(last) => return Some(last),
            None => {
                // Only reachable if something wrote nonsense into the table. Left as it was,
                // this would stall the payout forever without saying a word.
                warn!(
                    seconds,
                    "Stored payout timestamp is out of range, starting a new period"
                );
            }
        }
    }

    // Either the first start with this feature or an unusable timestamp. Begin the period now
    // rather than paying out for one that never happened.
    if let Err(error) = set_timestamp(conn, LAST_PAYOUT_AT, now.timestamp()) {
        warn!(%error, "Unable to record the start of the activity period");
    }

    None
}

/// Award the points of one period, dock the idle, and announce both.
async fn run_payout(conn: &Arc<Mutex<Connection>>, client: &Client, config: PayoutConfig) {
    // Who is in which room has to come from the client, before the database is touched: the
    // penalty may only reach people who are actually still in the room, and the awards may
    // only reach rooms the bot is still in.
    let rooms = client.joined_rooms();
    let own_user_id = client.user_id().map(|id| id.to_owned());

    let mut by_id: HashMap<String, Room> = HashMap::with_capacity(rooms.len());
    let mut rosters: Vec<(String, HashSet<(String, String)>)> = Vec::with_capacity(rooms.len());
    for room in rooms {
        match room.members(RoomMemberships::JOIN).await {
            Ok(members) => {
                let present = members
                    .iter()
                    .map(|member| member.user_id())
                    .filter(|user_id| Some(*user_id) != own_user_id.as_deref())
                    .map(|user_id| {
                        (
                            user_id.localpart().to_owned(),
                            user_id.server_name().to_string(),
                        )
                    })
                    .collect();
                rosters.push((room.room_id().to_string(), present));
                by_id.insert(room.room_id().to_string(), room);
            }
            Err(error) => {
                // Without the member list the bot cannot tell who is still there, and docking
                // people who left would quietly eat the scores it deliberately keeps for them.
                warn!(
                    room_id = %room.room_id(),
                    %error,
                    "Skipping this room's payout, the member list is unavailable"
                );
            }
        }
    }

    let payouts = {
        let Some(mut connection) = lock_or_stop(conn) else {
            return;
        };
        match apply_payout(&mut connection, &rosters, config) {
            Ok(payouts) => payouts,
            Err(error) => {
                error!(%error, "Activity payout failed, the counters are kept for the next run");
                return;
            }
        }
    };

    if payouts.is_empty() {
        info!("Activity payout: nothing happened this period");
        return;
    }

    for (room_id, payout) in payouts {
        let Some(room) = by_id.get(&room_id) else {
            continue;
        };

        info!(
            room_id,
            earners = payout.earners.len(),
            idle = payout.idle.len(),
            "Announcing the activity payout"
        );

        let answer = format_payout(&payout, config.max_entries);
        send_message(room, notice_html(answer.text, answer.html)).await;
    }
}

/// Turn the counters into scores and empty them, in one transaction.
///
/// All of it or none of it: a crash halfway through must not leave points awarded with the
/// counters still standing, which would award them again next week.
///
/// Only the rooms in `rosters` are settled and cleared. A room the bot has left, or one whose
/// member list could not be read this time, keeps its counters: a passing error must not cost
/// a room a period's worth of activity.
fn apply_payout(
    conn: &mut Connection,
    rosters: &[(String, HashSet<(String, String)>)],
    config: PayoutConfig,
) -> Result<Vec<(String, RoomPayout)>, rusqlite::Error> {
    let transaction = conn.transaction()?;

    let mut counted: BTreeMap<String, Vec<Activity>> = BTreeMap::new();
    for activity in pending_activity(&transaction)? {
        counted
            .entry(activity.room_id.clone())
            .or_default()
            .push(activity);
    }

    let mut payouts = Vec::new();

    for (room_id, present) in rosters {
        let room_id = room_id.as_str();
        let activity = counted.remove(room_id).unwrap_or_default();

        let mut payout = RoomPayout {
            penalty: config.inactivity_penalty,
            ..RoomPayout::default()
        };
        // Everybody who sent something, whether or not they are still here and whether or
        // not it was worth any points. They took part, so they are not idle.
        let mut active: HashSet<i32> = HashSet::with_capacity(activity.len());

        for entry in &activity {
            active.insert(entry.user_id);

            if !present.contains(&(entry.name.clone(), entry.url.clone())) {
                continue;
            }
            if let Some(awarded) = to_entry(entry, config) {
                add_social_credit_on(&transaction, entry.user_id, room_id, awarded.points)?;
                payout.earners.push(awarded);
            }
        }

        if config.inactivity_penalty != 0 {
            for user in idle_members(&transaction, room_id, present, &active)? {
                add_social_credit_on(
                    &transaction,
                    user.user_id,
                    room_id,
                    -config.inactivity_penalty,
                )?;
                payout.idle.push(user.name);
            }
        }

        clear_room_activity(&transaction, room_id)?;

        if payout.is_empty() {
            continue;
        }

        // Highest first, then alphabetically so the order is stable between equal scores.
        payout
            .earners
            .sort_by(|a, b| b.points.cmp(&a.points).then_with(|| a.name.cmp(&b.name)));
        payout.idle.sort();

        payouts.push((room_id.to_owned(), payout));
    }

    transaction.commit()?;

    Ok(payouts)
}

/// Everybody with a score in the room who is still a member and sent nothing.
fn idle_members(
    conn: &Connection,
    room_id: &str,
    present: &HashSet<(String, String)>,
    active: &HashSet<i32>,
) -> Result<Vec<RoomUser>, rusqlite::Error> {
    Ok(users_with_room_data(conn, room_id)?
        .into_iter()
        .filter(|user| !active.contains(&user.user_id))
        .filter(|user| present.contains(&(user.name.clone(), user.url.clone())))
        .collect())
}

fn to_entry(activity: &Activity, config: PayoutConfig) -> Option<PayoutEntry> {
    // Saturating throughout: a pathological configuration must not panic a background task.
    let points = activity
        .messages
        .saturating_mul(config.points_per_message)
        .saturating_add(activity.images.saturating_mul(config.points_per_image));

    if points == 0 {
        return None;
    }

    Some(PayoutEntry {
        name: activity.name.clone(),
        points,
        messages: activity.messages,
        images: activity.images,
    })
}

/// Build the announcement.
///
/// One message, and a bounded one: only the first `max_entries` people are named, the rest
/// are summarised in a single line. A room with fifty active people would otherwise produce
/// a wall of text every week. The idle are a single line whatever their number -- being named
/// is the point, taking up half the message is not.
pub fn format_payout(payout: &RoomPayout, max_entries: usize) -> HtmlAndTextAnswer {
    let max_entries = max_entries.max(1);

    let earned = payout
        .earners
        .iter()
        .fold(0i32, |sum, entry| sum.saturating_add(entry.points));
    let lost = (payout.idle.len() as i32).saturating_mul(payout.penalty);

    let (listed, rest) = payout
        .earners
        .split_at(payout.earners.len().min(max_entries));

    let mut text_lines = vec!["🧧 Weekly Social Credit".to_owned(), String::new()];
    let mut entry_lines = Vec::with_capacity(listed.len());

    for (index, entry) in listed.iter().enumerate() {
        let rank = index + 1;
        let detail = describe(entry);

        text_lines.push(format!(
            "{rank}. {}: {} ({detail})",
            entry.name,
            signed(entry.points)
        ));
        entry_lines.push(format!(
            "{rank}. <b>{}</b>: {} <i>({})</i>",
            escape_html(&entry.name),
            signed(entry.points),
            escape_html(&detail)
        ));
    }

    // `<h3>` is a block of its own, so it needs no break after it; the paragraph breaks are
    // doubled to match the blank lines of the plaintext body.
    let mut html = format!(
        "<h3>🧧 Weekly Social Credit</h3>{}",
        entry_lines.join("<br>")
    );

    if !rest.is_empty() {
        let rest_points = rest
            .iter()
            .fold(0i32, |sum, entry| sum.saturating_add(entry.points));
        let line = format!(
            "… and {}, {} together",
            plural(rest.len() as i32, "more comrade", "more comrades"),
            signed(rest_points)
        );
        push_paragraph(&mut text_lines, &mut html, &line, true);
    }

    if !payout.idle.is_empty() {
        let line = format!(
            "Idle: {} — {} each",
            name_list(&payout.idle, max_entries),
            signed(-payout.penalty)
        );
        push_paragraph(&mut text_lines, &mut html, &line, false);
    }

    let summary = summarise(payout.earners.len(), earned, payout.idle.len(), lost);
    push_paragraph(&mut text_lines, &mut html, &summary, true);

    HtmlAndTextAnswer {
        text: text_lines.join("\n"),
        html,
    }
}

/// Append the same line to both bodies, as its own paragraph.
fn push_paragraph(text_lines: &mut Vec<String>, html: &mut String, line: &str, italic: bool) {
    text_lines.push(String::new());
    text_lines.push(line.to_owned());

    let escaped = escape_html(line);
    if italic {
        html.push_str(&format!("<br><br><i>{escaped}</i>"));
    } else {
        html.push_str(&format!("<br><br>{escaped}"));
    }
}

/// "carol, dave, erin and 2 more", so one idle person and forty cost the same single line.
fn name_list(names: &[String], max_names: usize) -> String {
    if names.len() <= max_names {
        return names.join(", ");
    }

    let (listed, rest) = names.split_at(max_names);
    format!("{} and {} more", listed.join(", "), rest.len())
}

fn summarise(earners: usize, earned: i32, idle: usize, lost: i32) -> String {
    match (earners, idle) {
        (0, _) => format!(
            "{} lost {} this period",
            plural(idle as i32, "idle comrade", "idle comrades"),
            plural(lost, "point", "points")
        ),
        (_, 0) => format!(
            "{} earned {} this period",
            plural(earners as i32, "comrade", "comrades"),
            plural(earned, "point", "points")
        ),
        _ => format!(
            "{} earned {}, {} lost {} this period",
            plural(earners as i32, "comrade", "comrades"),
            plural(earned, "point", "points"),
            plural(idle as i32, "idle comrade", "idle comrades"),
            plural(lost, "point", "points")
        ),
    }
}

/// "23 messages, 3 images", leaving out whichever half is zero.
fn describe(entry: &PayoutEntry) -> String {
    let mut parts = Vec::with_capacity(2);
    if entry.messages > 0 {
        parts.push(plural(entry.messages, "message", "messages"));
    }
    if entry.images > 0 {
        parts.push(plural(entry.images, "image", "images"));
    }
    parts.join(", ")
}

fn plural(count: i32, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Points are usually positive here, but a negative configuration is allowed, and "+-5" would
/// look broken.
fn signed(points: i32) -> String {
    if points >= 0 {
        format!("+{points}")
    } else {
        points.to_string()
    }
}

#[cfg(test)]
mod payout_tests {
    use super::*;
    use crate::data::activity::{ActivityKind, record_activity};
    use crate::test_support::test_db;
    use rusqlite::params;

    const ROOM: &str = "!room:example.org";

    /// The lock is taken and dropped inside, so a test can keep using the database after.
    fn payout(
        db: &Arc<Mutex<Connection>>,
        rosters: &[(String, HashSet<(String, String)>)],
        config: PayoutConfig,
    ) -> Vec<(String, RoomPayout)> {
        let mut conn = db.lock().unwrap();
        apply_payout(&mut conn, rosters, config).unwrap()
    }

    /// One room whose members are exactly `names`.
    fn roster(room: &str, names: &[&str]) -> Vec<(String, HashSet<(String, String)>)> {
        vec![(
            room.to_owned(),
            names
                .iter()
                .map(|name| ((*name).to_owned(), "example.org".to_owned()))
                .collect(),
        )]
    }

    fn config() -> PayoutConfig {
        PayoutConfig {
            points_per_message: 1,
            points_per_image: 5,
            inactivity_penalty: 0,
            max_entries: 10,
        }
    }

    fn config_with_penalty() -> PayoutConfig {
        PayoutConfig {
            inactivity_penalty: 50,
            ..config()
        }
    }

    fn seed(conn: &Connection, name: &str, room: &str) -> i32 {
        conn.execute(
            "INSERT OR IGNORE INTO user (name, url, user_type) VALUES (?1, 'example.org', 0)",
            params![name],
        )
        .unwrap();
        let user_id: i32 = conn
            .query_row(
                "SELECT id FROM user WHERE name = ?1 AND url = 'example.org'",
                params![name],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO user_room_data (user_id, room_id, social_credit) VALUES (?1, ?2, 250)",
            params![user_id, room],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM user_room_data WHERE user_id = ?1 AND room_id = ?2",
            params![user_id, room],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn score(conn: &Connection, name: &str, room: &str) -> i32 {
        conn.query_row(
            "SELECT d.social_credit FROM user_room_data d JOIN user u ON u.id = d.user_id \
             WHERE u.name = ?1 AND d.room_id = ?2",
            params![name, room],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn awards_the_points_and_empties_the_counters() {
        let db = test_db();
        let alice = {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM)
        };

        for _ in 0..3 {
            record_activity(&db, alice, ActivityKind::Message);
        }
        record_activity(&db, alice, ActivityKind::Image);

        let announcements = payout(&db, &roster(ROOM, &["alice"]), config());

        assert_eq!(announcements.len(), 1);
        let (room_id, room_payout) = &announcements[0];
        assert_eq!(room_id, ROOM);
        assert_eq!(room_payout.earners.len(), 1);
        assert_eq!(
            room_payout.earners[0].points, 8,
            "3 messages at 1 plus 1 image at 5"
        );

        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "alice", ROOM), 258);
        assert!(
            pending_activity(&conn).unwrap().is_empty(),
            "the period has to start over"
        );
    }

    /// Running twice in a row must not pay anybody a second time.
    #[test]
    fn a_second_payout_finds_nothing() {
        let db = test_db();
        let alice = {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM)
        };
        record_activity(&db, alice, ActivityKind::Message);

        payout(&db, &roster(ROOM, &["alice"]), config());
        let second = payout(&db, &roster(ROOM, &["alice"]), config());

        assert!(second.is_empty());
        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "alice", ROOM), 251);
    }

    #[test]
    fn each_room_is_announced_on_its_own_and_sorted_by_points() {
        let db = test_db();
        let (alice_a, bob_a, alice_b) = {
            let conn = db.lock().unwrap();
            (
                seed(&conn, "alice", "!a:example.org"),
                seed(&conn, "bob", "!a:example.org"),
                seed(&conn, "alice", "!b:example.org"),
            )
        };

        record_activity(&db, alice_a, ActivityKind::Message);
        record_activity(&db, bob_a, ActivityKind::Image);
        record_activity(&db, alice_b, ActivityKind::Message);

        let mut rosters = roster("!a:example.org", &["alice", "bob"]);
        rosters.extend(roster("!b:example.org", &["alice"]));

        let announcements = payout(&db, &rosters, config());

        assert_eq!(announcements.len(), 2);
        let room_a = &announcements[0].1.earners;
        assert_eq!(room_a[0].name, "bob", "5 points beat 1 point");
        assert_eq!(room_a[1].name, "alice");
        assert_eq!(announcements[1].1.earners.len(), 1);
    }

    /// Scores are per room, so activity in one room must not move the score in another.
    #[test]
    fn points_land_in_the_room_they_were_earned_in() {
        let db = test_db();
        let alice_in_a = {
            let conn = db.lock().unwrap();
            let id = seed(&conn, "alice", "!a:example.org");
            seed(&conn, "alice", "!b:example.org");
            id
        };

        record_activity(&db, alice_in_a, ActivityKind::Image);

        let mut rosters = roster("!a:example.org", &["alice"]);
        rosters.extend(roster("!b:example.org", &["alice"]));
        payout(&db, &rosters, config());

        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "alice", "!a:example.org"), 255);
        assert_eq!(score(&conn, "alice", "!b:example.org"), 250);
    }

    /// The first start with the feature begins a period rather than paying one out.
    #[test]
    fn the_first_run_only_starts_the_clock() {
        let db = test_db();
        let conn = db.lock().unwrap();
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        assert_eq!(last_payout(&conn, now), None);
        assert_eq!(
            get_timestamp(&conn, LAST_PAYOUT_AT).unwrap(),
            Some(1_700_000_000),
            "the period has to be recorded, or every restart would start over"
        );
    }

    #[test]
    fn a_recorded_payout_is_read_back() {
        let db = test_db();
        let conn = db.lock().unwrap();
        let then = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let now = DateTime::from_timestamp(1_700_600_000, 0).unwrap();

        last_payout(&conn, then);

        assert_eq!(last_payout(&conn, now), Some(then));
    }

    /// A timestamp that cannot be turned back into a date used to stall the payout forever
    /// without a word: it read as "nothing to compare against", and the row was there, so it
    /// was never rewritten either.
    #[test]
    fn an_impossible_timestamp_starts_a_new_period_instead_of_stalling() {
        let db = test_db();
        let conn = db.lock().unwrap();
        set_timestamp(&conn, LAST_PAYOUT_AT, i64::MAX).unwrap();
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        assert_eq!(last_payout(&conn, now), None);

        // ... and the next round works normally again.
        assert_eq!(
            last_payout(&conn, now),
            Some(now),
            "the broken value has to be replaced, not read again"
        );
    }

    /// A background task must not be brought down by a silly configuration.
    #[test]
    fn absurd_point_values_saturate_instead_of_overflowing() {
        let config = PayoutConfig {
            points_per_message: i32::MAX,
            points_per_image: i32::MAX,
            inactivity_penalty: 0,
            max_entries: 10,
        };
        let activity = Activity {
            room_id: "!r".to_owned(),
            user_id: 1,
            name: "alice".to_owned(),
            url: "example.org".to_owned(),
            messages: 1000,
            images: 1000,
        };

        assert_eq!(to_entry(&activity, config).unwrap().points, i32::MAX);
    }

    #[test]
    fn the_idle_lose_the_penalty() {
        let db = test_db();
        let alice = {
            let conn = db.lock().unwrap();
            let id = seed(&conn, "alice", ROOM);
            seed(&conn, "bob", ROOM);
            id
        };
        record_activity(&db, alice, ActivityKind::Message);

        let announcements = payout(&db, &roster(ROOM, &["alice", "bob"]), config_with_penalty());

        let room_payout = &announcements[0].1;
        assert_eq!(room_payout.earners.len(), 1);
        assert_eq!(room_payout.idle, vec!["bob".to_owned()]);

        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "alice", ROOM), 251);
        assert_eq!(score(&conn, "bob", ROOM), 200);
    }

    /// Scores of people who left are kept on purpose, so that somebody who rejoins finds them
    /// again. Docking them every week would quietly eat exactly those scores.
    #[test]
    fn somebody_who_left_the_room_is_not_docked() {
        let db = test_db();
        {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM);
            seed(&conn, "departed", ROOM);
        }

        payout(&db, &roster(ROOM, &["alice"]), config_with_penalty());

        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "departed", ROOM), 250, "no longer a member");
        assert_eq!(score(&conn, "alice", ROOM), 200, "still there, still idle");
    }

    /// A room the bot is not in, or one whose member list could not be read, is left alone
    /// entirely -- nobody is awarded and nobody is docked.
    #[test]
    fn a_room_that_is_not_in_the_roster_is_untouched() {
        let db = test_db();
        let alice = {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM)
        };
        record_activity(&db, alice, ActivityKind::Message);

        let announcements = payout(&db, &[], config_with_penalty());

        assert!(announcements.is_empty());
        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "alice", ROOM), 250);
        assert_eq!(
            pending_activity(&conn).unwrap().len(),
            1,
            "the counters have to survive, or a passing error costs a whole period"
        );
    }

    #[test]
    fn a_penalty_of_zero_docks_nobody() {
        let db = test_db();
        {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM);
        }

        let announcements = payout(&db, &roster(ROOM, &["alice"]), config());

        assert!(announcements.is_empty(), "nothing happened, nothing to say");
        let conn = db.lock().unwrap();
        assert_eq!(score(&conn, "alice", ROOM), 250);
    }

    /// Somebody in the room the bot has never seen an event from has no score to dock.
    #[test]
    fn a_member_without_a_score_is_not_invented() {
        let db = test_db();
        {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM);
        }

        let announcements = payout(
            &db,
            &roster(ROOM, &["alice", "stranger"]),
            config_with_penalty(),
        );

        assert_eq!(announcements[0].1.idle, vec!["alice".to_owned()]);
        let conn = db.lock().unwrap();
        let users: i64 = conn
            .query_row("SELECT COUNT(*) FROM user", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            users, 1,
            "no row may be created for somebody who never wrote"
        );
    }

    /// The very first period after switching the feature on runs from "now" to the next
    /// payout, which can be a few hours. Docking everybody who did not happen to write in
    /// those hours would be the feature's first impression, and a wrong one.
    #[test]
    fn a_short_period_does_not_charge_the_penalty() {
        let config = config_with_penalty();

        assert_eq!(
            penalty_for_period(config, TimeDelta::hours(29)).inactivity_penalty,
            0,
            "deployed on Saturday, payout on Sunday"
        );
        assert_eq!(
            penalty_for_period(config, TimeDelta::minutes(5)).inactivity_penalty,
            0
        );
    }

    #[test]
    fn a_full_period_charges_it() {
        let config = config_with_penalty();

        assert_eq!(
            penalty_for_period(config, TimeDelta::days(7)).inactivity_penalty,
            50
        );
        // Three and a half days is the cutoff, and it counts as long enough.
        assert_eq!(
            penalty_for_period(config, TimeDelta::hours(84)).inactivity_penalty,
            50
        );
        assert_eq!(
            penalty_for_period(config, TimeDelta::hours(83)).inactivity_penalty,
            0
        );
    }

    /// Three weeks of downtime is one long period, and the penalty is due for it.
    #[test]
    fn a_long_outage_still_charges_the_penalty() {
        assert_eq!(
            penalty_for_period(config_with_penalty(), TimeDelta::days(21)).inactivity_penalty,
            50
        );
    }

    #[test]
    fn nothing_counted_means_nothing_to_announce() {
        let db = test_db();
        {
            let conn = db.lock().unwrap();
            seed(&conn, "alice", ROOM);
        }

        assert!(payout(&db, &roster(ROOM, &["alice"]), config()).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, points: i32, messages: i32, images: i32) -> PayoutEntry {
        PayoutEntry {
            name: name.to_owned(),
            points,
            messages,
            images,
        }
    }

    /// A room where everybody was active.
    fn earned(earners: Vec<PayoutEntry>) -> RoomPayout {
        RoomPayout {
            earners,
            idle: Vec::new(),
            penalty: 0,
        }
    }

    /// A room where some were active and the rest were not.
    fn mixed(earners: Vec<PayoutEntry>, idle: &[&str], penalty: i32) -> RoomPayout {
        RoomPayout {
            earners,
            idle: idle.iter().map(|name| (*name).to_owned()).collect(),
            penalty,
        }
    }

    #[test]
    #[ignore = "prints the announcement for review, run with --ignored --nocapture"]
    fn preview() {
        let payout = mixed(
            vec![
                entry("julian", 47, 32, 3),
                entry("marie", 38, 23, 3),
                entry("tobias", 21, 16, 1),
                entry("lena", 12, 12, 0),
                entry("simon", 5, 0, 1),
                entry("nina", 4, 4, 0),
                entry("paul", 3, 3, 0),
                entry("hannah", 2, 2, 0),
                entry("felix", 1, 1, 0),
                entry("clara", 1, 1, 0),
                entry("jonas", 1, 1, 0),
                entry("emma", 1, 1, 0),
            ],
            &["stefan", "anna", "markus"],
            50,
        );
        let answer = format_payout(&payout, 10);
        println!("---------- plain ----------\n{}\n", answer.text);
        println!(
            "---------- html ----------\n{}\n",
            answer.html.replace("<br>", "<br>\n")
        );
    }

    #[test]
    fn a_single_person() {
        let answer = format_payout(&earned(vec![entry("alice", 38, 23, 3)]), 10);

        assert_eq!(
            answer.text,
            "🧧 Weekly Social Credit\n\
             \n\
             1. alice: +38 (23 messages, 3 images)\n\
             \n\
             1 comrade earned 38 points this period"
        );
    }

    #[test]
    fn leaves_out_the_half_that_is_zero() {
        let answer = format_payout(
            &earned(vec![entry("alice", 12, 12, 0), entry("bob", 5, 0, 1)]),
            10,
        );

        assert!(answer.text.contains("1. alice: +12 (12 messages)"));
        assert!(answer.text.contains("2. bob: +5 (1 image)"));
    }

    /// The point of max_entries: a busy room must not produce a wall of text.
    #[test]
    fn summarises_everybody_past_the_limit() {
        let entries: Vec<PayoutEntry> = (0..12)
            .map(|i| entry(&format!("user{i:02}"), 12 - i, 12 - i, 0))
            .collect();

        let answer = format_payout(&earned(entries), 3);

        let ranked = answer
            .text
            .lines()
            .filter(|line| {
                line.split_once(". ")
                    .is_some_and(|(rank, _)| rank.parse::<u32>().is_ok())
            })
            .count();
        assert_eq!(ranked, 3, "only max_entries people may be named");
        assert!(answer.text.contains("… and 9 more comrades, +45 together"));
        assert!(
            answer
                .text
                .contains("12 comrades earned 78 points this period")
        );
    }

    #[test]
    fn names_the_idle_and_says_what_it_cost_them() {
        let answer = format_payout(
            &mixed(vec![entry("alice", 12, 12, 0)], &["bob", "carol"], 50),
            10,
        );

        assert!(answer.text.contains("Idle: bob, carol — -50 each"));
        assert!(
            answer.text.contains(
                "1 comrade earned 12 points, 2 idle comrades lost 100 points this period"
            )
        );
    }

    /// However many are idle, it stays one line.
    #[test]
    fn a_crowd_of_idlers_is_still_one_line() {
        let idle: Vec<String> = (0..30).map(|i| format!("user{i:02}")).collect();
        let names: Vec<&str> = idle.iter().map(String::as_str).collect();

        let answer = format_payout(&mixed(Vec::new(), &names, 50), 3);

        let idle_lines: Vec<&str> = answer
            .text
            .lines()
            .filter(|line| line.starts_with("Idle: "))
            .collect();
        assert_eq!(idle_lines.len(), 1);
        assert_eq!(
            idle_lines[0],
            "Idle: user00, user01, user02 and 27 more — -50 each"
        );
        assert!(
            answer
                .text
                .contains("30 idle comrades lost 1500 points this period")
        );
    }

    #[test]
    fn a_room_where_nobody_was_idle_says_nothing_about_it() {
        let answer = format_payout(&earned(vec![entry("alice", 3, 3, 0)]), 10);

        assert!(!answer.text.contains("Idle"));
    }

    #[test]
    fn no_summary_line_when_everybody_fits() {
        let answer = format_payout(&earned(vec![entry("alice", 3, 3, 0)]), 10);

        assert!(!answer.text.contains("… and"));
    }

    #[test]
    fn singular_and_plural_are_both_readable() {
        let answer = format_payout(&earned(vec![entry("alice", 1, 1, 0)]), 10);

        assert!(answer.text.contains("1. alice: +1 (1 message)"));
        assert!(answer.text.contains("1 comrade earned 1 point this period"));
    }

    /// Names come from Matrix and are not trustworthy markup.
    #[test]
    fn escapes_names_in_the_html_body() {
        let answer = format_payout(&earned(vec![entry("<b>ovi</b>", 5, 5, 0)]), 10);

        assert!(answer.html.contains("&lt;b&gt;ovi&lt;/b&gt;"));
        assert!(!answer.html.contains("<b>ovi"));
    }

    #[test]
    fn a_negative_configuration_does_not_produce_plus_minus() {
        let answer = format_payout(&earned(vec![entry("alice", -6, 6, 0)]), 10);

        assert!(answer.text.contains("1. alice: -6 (6 messages)"));
    }

    #[test]
    fn zero_points_are_not_worth_a_line() {
        let config = PayoutConfig {
            points_per_message: 0,
            points_per_image: 5,
            inactivity_penalty: 0,
            max_entries: 10,
        };
        let activity = Activity {
            room_id: "!r".to_owned(),
            user_id: 1,
            name: "alice".to_owned(),
            url: "example.org".to_owned(),
            messages: 20,
            images: 0,
        };

        assert!(to_entry(&activity, config).is_none());
    }

    #[test]
    fn both_values_at_zero_switches_the_feature_off() {
        assert!(
            !PayoutConfig {
                points_per_message: 0,
                points_per_image: 0,
                inactivity_penalty: 0,
                max_entries: 10
            }
            .is_enabled()
        );
        assert!(
            PayoutConfig {
                points_per_message: 1,
                points_per_image: 0,
                inactivity_penalty: 0,
                max_entries: 10
            }
            .is_enabled()
        );
        assert!(
            PayoutConfig {
                points_per_message: 0,
                points_per_image: 0,
                inactivity_penalty: 50,
                max_entries: 10
            }
            .is_enabled(),
            "the penalty alone still needs the counters"
        );
    }
}
