//! Dates as pages show them, in the viewer's time zone.
//!
//! The zone is set for the whole request by the session middleware, so
//! code that formats a date doesn't need to know who is looking. Outside a
//! request (jobs, emails), dates are in UTC.

use std::future::Future;
use std::sync::LazyLock;

use jiff::tz::TimeZone;
use time::OffsetDateTime;

use crate::auth::CurrentUser;

tokio::task_local! {
    static ZONE: TimeZone;
}

/// Runs `future` with dates in `current`'s time zone.
pub async fn scope<F: Future>(current: &CurrentUser, future: F) -> F::Output {
    let zone = current
        .user
        .as_ref()
        .and_then(|u| moekura_core::user_settings::UserSettings::from_json(&u.settings).time_zone)
        .and_then(|name| zone(&name))
        .unwrap_or(TimeZone::UTC);
    ZONE.scope(zone, future).await
}

/// The time zone called `name`, if there is one.
pub fn zone(name: &str) -> Option<TimeZone> {
    TimeZone::get(name).ok()
}

/// The time zones a user can choose, alphabetically.
pub fn zone_names() -> &'static [String] {
    static NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
        let mut names: Vec<String> = jiff::tz::db()
            .available()
            .map(|name| name.as_str().to_owned())
            // Legacy aliases (`US/Pacific`, `EST5EDT`) only crowd the list.
            .filter(|name| name == "UTC" || (name.contains('/') && !name.starts_with("Etc/")))
            .filter(|name| name.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
            .filter(|name| !name.starts_with("US/") && !name.starts_with("SystemV/"))
            .collect();
        names.sort_unstable();
        names
    });
    &NAMES
}

/// `at`'s day, as `2026-09-29`.
pub fn day(at: OffsetDateTime) -> String {
    format(at, "%Y-%m-%d").unwrap_or_else(|| at.date().to_string())
}

/// `at`'s time of day, as `18:05`.
pub fn clock(at: OffsetDateTime) -> String {
    format(at, "%H:%M").unwrap_or_else(|| format!("{:02}:{:02}", at.hour(), at.minute()))
}

/// `at` in the zone in scope, if there is one.
fn format(at: OffsetDateTime, pattern: &str) -> Option<String> {
    ZONE.try_with(|zone| {
        jiff::Timestamp::from_second(at.unix_timestamp())
            .ok()
            .map(|t| t.to_zoned(zone.clone()).strftime(pattern).to_string())
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[tokio::test]
    async fn uses_the_zone_in_scope() {
        let late = datetime!(2026-09-29 23:30 UTC);
        assert_eq!(day(late), "2026-09-29");
        assert_eq!(clock(late), "23:30");
        let tokyo = zone("Asia/Tokyo").unwrap();
        ZONE.scope(tokyo, async {
            assert_eq!(day(late), "2026-09-30");
            assert_eq!(clock(late), "08:30");
        })
        .await;
    }

    #[test]
    fn lists_real_zones() {
        let names = zone_names();
        assert!(names.iter().any(|n| n == "Europe/Berlin"));
        assert!(names.iter().any(|n| n == "UTC"));
        assert!(!names.iter().any(|n| n == "US/Pacific"));
        assert!(names.iter().all(|n| zone(n).is_some()));
    }
}
