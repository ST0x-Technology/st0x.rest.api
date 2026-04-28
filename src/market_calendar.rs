use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use chrono_tz::America::New_York;

/// True when the NYSE regular trading session is currently open in
/// `America/New_York`: 09:30 (inclusive) through 16:00 (exclusive),
/// Monday through Friday.
///
/// Holidays (e.g. Independence Day, Thanksgiving) are NOT modeled. On a
/// holiday this returns true, which means stale-marked orders will be
/// re-quoted once and likely re-marked on the next cycle. That single
/// wasted quote per order per holiday is acceptable in exchange for not
/// needing to maintain a holiday calendar.
pub(crate) fn is_nyse_open(now_utc: DateTime<Utc>) -> bool {
    let now_et = now_utc.with_timezone(&New_York);

    if matches!(now_et.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }

    let minutes = now_et.hour() * 60 + now_et.minute();
    let market_open = 9 * 60 + 30;
    let market_close = 16 * 60;
    minutes >= market_open && minutes < market_close
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn et(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        // Build a wall-clock time in New_York and convert to UTC. This
        // automatically handles EDT vs EST.
        let local = New_York
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("unique New_York timestamp");
        local.with_timezone(&Utc)
    }

    #[test]
    fn test_open_at_market_open() {
        // Monday 2026-04-27 09:30 ET (DST in effect → 13:30 UTC).
        assert!(is_nyse_open(et(2026, 4, 27, 9, 30)));
    }

    #[test]
    fn test_open_one_minute_before_close() {
        assert!(is_nyse_open(et(2026, 4, 27, 15, 59)));
    }

    #[test]
    fn test_closed_at_market_close() {
        // 16:00 is the bell. Closed exactly at close (exclusive).
        assert!(!is_nyse_open(et(2026, 4, 27, 16, 0)));
    }

    #[test]
    fn test_closed_one_minute_before_open() {
        assert!(!is_nyse_open(et(2026, 4, 27, 9, 29)));
    }

    #[test]
    fn test_closed_overnight() {
        assert!(!is_nyse_open(et(2026, 4, 27, 3, 0)));
        assert!(!is_nyse_open(et(2026, 4, 27, 22, 0)));
    }

    #[test]
    fn test_closed_on_saturday() {
        // 2026-04-25 is a Saturday.
        assert!(!is_nyse_open(et(2026, 4, 25, 12, 0)));
    }

    #[test]
    fn test_closed_on_sunday() {
        // 2026-04-26 is a Sunday.
        assert!(!is_nyse_open(et(2026, 4, 26, 12, 0)));
    }

    #[test]
    fn test_open_during_winter_est() {
        // 2026-01-15 (Thursday) is well inside EST. 10:00 ET → 15:00 UTC.
        assert!(is_nyse_open(et(2026, 1, 15, 10, 0)));
        assert!(!is_nyse_open(et(2026, 1, 15, 8, 0)));
    }

    #[test]
    fn test_dst_transition_safe() {
        // 2026-03-09 (Monday after DST starts on Sunday 2026-03-08).
        assert!(is_nyse_open(et(2026, 3, 9, 10, 0)));
    }
}
