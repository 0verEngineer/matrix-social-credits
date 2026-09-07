//! When the weekly activity payout is due.
//!
//! The schedule is a weekday plus a time of day in a configured time zone, not a fixed
//! interval: "every seven days" would drift away from Sunday evening as soon as the bot is
//! restarted at an awkward moment, and would ignore daylight saving entirely.

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

/// A recurring point in time, once per week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayoutSchedule {
    weekday: Weekday,
    time: NaiveTime,
    timezone: Tz,
}

impl PayoutSchedule {
    /// Build a schedule from the configured strings.
    ///
    /// `day` is an English weekday name (`sunday`, `sun`), `time` is `HH:MM`, `timezone` an
    /// IANA name such as `Europe/Vienna`.
    pub fn new(day: &str, time: &str, timezone: &str) -> Result<Self, String> {
        let weekday = parse_weekday(day)?;

        let time = NaiveTime::parse_from_str(time.trim(), "%H:%M")
            .map_err(|_| format!("'{time}' is not a time of day in HH:MM form"))?;

        let timezone: Tz = timezone
            .trim()
            .parse()
            .map_err(|_| format!("'{timezone}' is not an IANA time zone name"))?;

        Ok(Self {
            weekday,
            time,
            timezone,
        })
    }

    /// The first occurrence of the schedule strictly after `after`.
    pub fn next_after(&self, after: DateTime<Utc>) -> DateTime<Utc> {
        let local = after.with_timezone(&self.timezone);
        let mut date = local.date_naive();

        // Eight days rather than seven: today may already be past the time of day, in which
        // case the answer is the same weekday next week.
        for _ in 0..8 {
            if date.weekday() == self.weekday
                && let Some(candidate) = self.resolve(date)
                && candidate > local
            {
                return candidate.with_timezone(&Utc);
            }
            date = date.succ_opt().unwrap_or(date);
        }

        // Unreachable in practice; a week always contains the weekday. Rather than panic in a
        // background task, fall back to a week from now.
        after + Duration::days(7)
    }

    /// Whether a payout is due, given when the last one happened.
    pub fn is_due(&self, last_payout: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        self.next_after(last_payout) <= now
    }

    /// Turn a local date into the instant the payout happens on that date.
    ///
    /// Returns `None` only if the configured time of day does not exist on that date, which
    /// happens in the hour a daylight saving change skips. The payout then moves to the next
    /// hour that does exist rather than being lost.
    fn resolve(&self, date: chrono::NaiveDate) -> Option<DateTime<Tz>> {
        for extra_hours in 0..4 {
            let naive = date.and_time(self.time) + Duration::hours(extra_hours);
            if let Some(resolved) = self.timezone.from_local_datetime(&naive).earliest() {
                return Some(resolved);
            }
        }
        None
    }
}

impl std::fmt::Display for PayoutSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {} ({})",
            self.weekday,
            self.time.format("%H:%M"),
            self.timezone.name()
        )
    }
}

fn parse_weekday(day: &str) -> Result<Weekday, String> {
    // chrono's FromStr for Weekday accepts both the full name and the three letter form,
    // case insensitively.
    day.trim()
        .parse::<Weekday>()
        .map_err(|_| format!("'{day}' is not a weekday name such as 'sunday' or 'sun'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn schedule() -> PayoutSchedule {
        PayoutSchedule::new("sunday", "20:00", "Europe/Vienna").unwrap()
    }

    fn vienna(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn accepts_the_configured_forms() {
        assert!(PayoutSchedule::new("sunday", "20:00", "Europe/Vienna").is_ok());
        assert!(PayoutSchedule::new("SUN", "07:30", "UTC").is_ok());
        assert!(PayoutSchedule::new(" monday ", " 00:00 ", " America/New_York ").is_ok());
    }

    #[test]
    fn rejects_nonsense_with_a_usable_message() {
        assert!(PayoutSchedule::new("someday", "20:00", "UTC").is_err());
        assert!(PayoutSchedule::new("sunday", "8pm", "UTC").is_err());
        assert!(PayoutSchedule::new("sunday", "25:00", "UTC").is_err());
        assert!(PayoutSchedule::new("sunday", "20:00", "Middle/Earth").is_err());
    }

    /// 2026-09-06 is a Sunday.
    #[test]
    fn finds_the_next_sunday_evening() {
        // Wednesday -> the coming Sunday.
        let next = schedule().next_after(vienna("2026-09-02T12:00:00+02:00"));
        assert_eq!(next, vienna("2026-09-06T20:00:00+02:00"));
    }

    #[test]
    fn earlier_the_same_sunday_still_means_today() {
        let next = schedule().next_after(vienna("2026-09-06T19:59:00+02:00"));
        assert_eq!(next, vienna("2026-09-06T20:00:00+02:00"));
    }

    /// The moment after a payout, the next one is a full week away -- not a second later.
    #[test]
    fn just_after_the_payout_the_next_one_is_a_week_out() {
        let next = schedule().next_after(vienna("2026-09-06T20:00:01+02:00"));
        assert_eq!(next, vienna("2026-09-13T20:00:00+02:00"));
    }

    #[test]
    fn exactly_on_the_payout_the_next_one_is_a_week_out() {
        let next = schedule().next_after(vienna("2026-09-06T20:00:00+02:00"));
        assert_eq!(next, vienna("2026-09-13T20:00:00+02:00"));
    }

    /// The whole point of a time zone: 20:00 local is a different UTC instant in summer and
    /// in winter.
    #[test]
    fn the_utc_instant_follows_daylight_saving() {
        let summer = schedule().next_after(vienna("2026-07-01T00:00:00+02:00"));
        let winter = schedule().next_after(vienna("2026-12-01T00:00:00+01:00"));

        assert_eq!(summer.format("%H:%M").to_string(), "18:00");
        assert_eq!(winter.format("%H:%M").to_string(), "19:00");
    }

    /// A time of day that does not exist because the clocks jumped forward moves to the next
    /// hour instead of being skipped for a week.
    #[test]
    fn a_time_lost_to_a_clock_change_still_happens() {
        // In Vienna the clocks go forward on 2026-03-29, so 02:30 does not exist that day.
        let schedule = PayoutSchedule::new("sunday", "02:30", "Europe/Vienna").unwrap();

        let next = schedule.next_after(vienna("2026-03-28T00:00:00+01:00"));

        assert_eq!(
            next.with_timezone(&chrono_tz::Europe::Vienna)
                .date_naive()
                .to_string(),
            "2026-03-29"
        );
        assert_eq!(next, vienna("2026-03-29T03:30:00+02:00"));
    }

    #[test]
    fn is_due_only_once_the_moment_has_passed() {
        let schedule = schedule();
        let last = vienna("2026-08-30T20:00:00+02:00");

        assert!(!schedule.is_due(last, vienna("2026-09-06T19:59:00+02:00")));
        assert!(schedule.is_due(last, vienna("2026-09-06T20:00:00+02:00")));
        assert!(schedule.is_due(last, vienna("2026-09-06T20:01:00+02:00")));
    }

    /// A bot that was down for three weeks pays out once when it comes back, not three times.
    #[test]
    fn a_long_outage_is_still_a_single_payout() {
        let schedule = schedule();
        let last = vienna("2026-08-16T20:00:00+02:00");
        let now = vienna("2026-09-07T09:00:00+02:00");

        assert!(schedule.is_due(last, now));

        // After paying out, "now" becomes the new last payout and nothing is due again.
        assert!(!schedule.is_due(now, now));
    }

    #[test]
    fn utc_works_without_a_named_zone() {
        let schedule = PayoutSchedule::new("friday", "23:00", "UTC").unwrap();
        let next = schedule.next_after(Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).unwrap());
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 9, 4, 23, 0, 0).unwrap());
    }

    #[test]
    fn displays_itself_for_the_log() {
        assert_eq!(schedule().to_string(), "Sun at 20:00 (Europe/Vienna)");
    }
}
