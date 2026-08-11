//! Shared `std::time` glue (BRS-35, `docs/spec/05-stdlib.md`): both
//! backends call these, so clock reads, sleeping, and ISO-8601
//! formatting behave identically.
//!
//! Decisions recorded here:
//!
//! - `time.now()` is Unix epoch seconds as a float (sub-millisecond
//!   precision); `time.nowMillis()` is epoch milliseconds as an int —
//!   the integer timestamp form that composes with `time.iso`.
//! - A clock before the epoch yields negative values instead of
//!   failing; `std::time` has no error namespace.
//! - `time.iso(epochMillis)` formats in UTC as
//!   `YYYY-MM-DDTHH:MM:SS.mmmZ` using the proleptic Gregorian
//!   calendar (no locale, no timezone database).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unix epoch seconds as a float.
pub fn now_seconds() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs_f64(),
        Err(before) => -before.duration().as_secs_f64(),
    }
}

/// Unix epoch milliseconds.
pub fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as i64,
        Err(before) => -(before.duration().as_millis() as i64),
    }
}

/// Sleeps for at least `ms` milliseconds; the caller rejects negative
/// durations before reaching here.
pub fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Formats an epoch-millisecond timestamp as basic UTC ISO-8601.
pub fn iso_utc(epoch_millis: i64) -> String {
    let days = epoch_millis.div_euclid(86_400_000);
    let ms_of_day = epoch_millis.rem_euclid(86_400_000);

    let (year, month, day) = civil_from_days(days);
    let hour = ms_of_day / 3_600_000;
    let minute = ms_of_day % 3_600_000 / 60_000;
    let second = ms_of_day % 60_000 / 1_000;
    let millis = ms_of_day % 1_000;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since the epoch to a proleptic Gregorian civil date — Howard
/// Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };

    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::iso_utc;

    #[test]
    fn iso_formats_known_timestamps() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_utc(1_000_000_000_000), "2001-09-09T01:46:40.000Z");
        assert_eq!(iso_utc(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        // Leap day and end-of-year boundaries.
        assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso_utc(946_684_799_999), "1999-12-31T23:59:59.999Z");
        // Pre-epoch timestamps floor toward earlier days.
        assert_eq!(iso_utc(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(iso_utc(-86_400_000), "1969-12-31T00:00:00.000Z");
    }
}
