//! Shared `std::time` glue (BRS-35, spec: 05 — Stdlib de scripting): both
//! backends call these, so clock reads, sleeping, and ISO-8601
//! formatting and parsing behave identically.
//!
//! Decisions recorded here:
//!
//! - `time.now()` is Unix epoch seconds as a float (sub-millisecond
//!   precision); `time.nowMillis()` is epoch milliseconds as an int —
//!   the integer timestamp form that composes with `time.iso`.
//! - A clock before the epoch yields negative values instead of
//!   failing; reading the clock is the part of `std::time` that cannot
//!   fail. Reading a string can, so `time.parseIso` is the one member
//!   with an error namespace (`time.ParseError`).
//! - `time.iso(epochMillis)` formats in UTC as
//!   `YYYY-MM-DDTHH:MM:SS.mmmZ` using the proleptic Gregorian
//!   calendar (no locale, no timezone database).
//! - `time.parseIso(text)` is that renderer read backwards, and the
//!   contract between them is a round trip: `parseIso(iso(n)) == n`
//!   for every `n`. Anything the renderer can emit the scanner must
//!   accept, which is why the year is not fixed at four digits.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use brasa_resolver::TIME_PARSE_ERROR;

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

/// The local UTC offset in milliseconds **at `epoch_millis`**
/// (spec: 05 — Stdlib de scripting, BRS-147).
///
/// `std` cannot answer this: it has no notion of a zone, only of
/// instants. `localtime_r` is the C library call that resolves one
/// against the system zone, and taking the instant rather than
/// answering "now" is what makes a historical timestamp convert with
/// the offset that was in force THEN — a report that crosses a
/// daylight-saving change lands on the right days.
///
/// Reading the zone means reading the process environment, which is
/// only safe while nothing writes it. Nothing in this workspace does:
/// `env.set` records an overlay and leaves the host process's own
/// environment block untouched (`brasa_vm::Vm::env_overlay`), and no
/// `set_var` exists anywhere in the tree. That is what allows a plain
/// call here instead of a value cached before the VM's threads start.
///
/// Answers zero — UTC — where the platform has no `localtime_r`, and
/// where the call fails.
pub fn local_offset_millis(epoch_millis: i64) -> i64 {
    #[cfg(unix)]
    {
        let seconds = epoch_millis.div_euclid(1000);

        // SAFETY: `localtime_r` writes through the out pointer and
        // reads the environment, which nothing in this process
        // mutates (see above). The `tm` is fully owned here and
        // nothing borrowed from it escapes: only the integer offset
        // field is copied out.
        unsafe {
            let mut parts: libc::tm = std::mem::zeroed();

            if libc::localtime_r(&seconds, &mut parts).is_null() {
                return 0;
            }

            parts.tm_gmtoff as i64 * 1000
        }
    }

    #[cfg(not(unix))]
    {
        let _ = epoch_millis;
        0
    }
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

/// The largest year magnitude [`parse_iso`] will carry into the
/// calendar arithmetic. Roughly three times the widest year an epoch
/// millisecond can name, so it rejects nothing representable — it is
/// there to keep the era multiplication in [`civil_days`] inside `i64`
/// on the way to the range check that does the real work.
const MAX_YEAR: i64 = 999_999_999;

/// One failed timestamp parse: the qualified native-error name and the
/// message explaining which part of the input the scanner refused.
#[derive(Debug)]
pub struct TimeError {
    pub name: &'static str,
    pub message: String,
}

/// Reads an RFC 3339 / ISO-8601 timestamp into epoch milliseconds — the
/// inverse of [`iso_utc`], and the only fallible member of `std::time`.
///
/// The accepted shape is `<date>T<time><offset>`:
///
/// - The date is `YYYY-MM-DD`. Four digits is the RFC's year, but the
///   renderer is not bound by it, so the scanner takes four OR MORE
///   digits, plus the leading `-` form Rust's sign-aware padding emits
///   for a year before 1 (`-001-…`). Without that the round trip would
///   stop being total.
/// - The date and the time are joined by `T` or `t`: RFC 3339 §5.6
///   permits either case, and rejecting the lowercase one would refuse
///   a timestamp the RFC calls valid. A space is NOT a separator here —
///   it is the RFC's "by mutual agreement" extension, and there is no
///   agreement to read.
/// - The time is `HH:MM:SS`, optionally followed by `.` and ANY number
///   of fractional digits, because APIs emit three, six, or nine of
///   them interchangeably. Extra digits are TRUNCATED toward the
///   millisecond, never rounded: rounding `…59.9996Z` up would answer
///   a millisecond whose own rendering is a different string, so the
///   round trip would stop being a fixed point.
/// - The offset is `Z`/`z`, or `+HH:MM`/`-HH:MM`, and it is REQUIRED.
///   A timestamp with no offset does not denote an instant, and
///   assuming UTC would turn a caller's mistake into a silently wrong
///   answer hours away from the truth; the error says the offset is
///   what is missing.
///
/// Every field is range-checked against the calendar, so `02-30` and
/// `02-29` in a common year are refused rather than folded into March.
/// A leap second (`:60`) is refused too: epoch milliseconds do not
/// number leap seconds, so there is no value to answer with — every
/// candidate would collide with a second that has its own timestamp.
pub fn parse_iso(text: &str) -> Result<i64, TimeError> {
    let mut scan = Scan::new(text);

    let year = scan.year()?;
    scan.literal(b'-', "a `-` after the year")?;
    let month = scan.fixed(2, "month")?;
    scan.literal(b'-', "a `-` after the month")?;
    let day = scan.fixed(2, "day")?;

    scan.date_time_separator()?;

    let hour = scan.fixed(2, "hour")?;
    scan.literal(b':', "a `:` after the hour")?;
    let minute = scan.fixed(2, "minute")?;
    scan.literal(b':', "a `:` after the minute")?;
    let second = scan.fixed(2, "second")?;

    let fraction = scan.fraction_millis()?;
    let offset_minutes = scan.offset_minutes()?;

    scan.end()?;

    let days = civil_days(text, year, month, day)?;
    let time_of_day = time_of_day_millis(text, hour, minute, second)?;

    let utc_millis = days
        .checked_mul(86_400_000)
        .and_then(|day_millis| day_millis.checked_add(time_of_day))
        .and_then(|millis| millis.checked_add(fraction))
        .and_then(|millis| millis.checked_sub(offset_minutes * 60_000))
        .ok_or_else(|| reject(text, "the instant is too far from the epoch to represent"))?;

    Ok(utc_millis)
}

/// Validates a civil date and converts it to days since the epoch —
/// Howard Hinnant's `days_from_civil`, the exact inverse of
/// [`civil_from_days`]. The month length comes out of the same
/// leap-year rule the conversion uses rather than a parallel table,
/// which is what keeps the two halves from ever disagreeing.
fn civil_days(text: &str, year: i64, month: i64, day: i64) -> Result<i64, TimeError> {
    if !(1..=12).contains(&month) {
        return Err(reject(text, format_args!("month {month} is out of range")));
    }

    let last = days_in_month(year, month);
    if day < 1 || day > last {
        return Err(reject(
            text,
            format_args!("day {day} does not exist in month {month} of year {year}"),
        ));
    }

    let shifted = year - i64::from(month <= 2);
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let yoe = shifted - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;

    Ok(era * 146_097 + doe - 719_468)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            if leap { 29 } else { 28 }
        }
    }
}

fn time_of_day_millis(text: &str, hour: i64, minute: i64, second: i64) -> Result<i64, TimeError> {
    if hour > 23 {
        return Err(reject(text, format_args!("hour {hour} is out of range")));
    }
    if minute > 59 {
        return Err(reject(
            text,
            format_args!("minute {minute} is out of range"),
        ));
    }
    if second > 59 {
        return Err(reject(
            text,
            format_args!("second {second} is out of range (leap seconds have no epoch timestamp)"),
        ));
    }

    Ok(hour * 3_600_000 + minute * 60_000 + second * 1_000)
}

fn reject(text: &str, detail: impl fmt::Display) -> TimeError {
    TimeError {
        name: TIME_PARSE_ERROR,
        message: format!("cannot parse timestamp {text:?}: {detail}"),
    }
}

/// A cursor over the input's bytes. Every field of a timestamp is
/// ASCII, so byte positions are character positions and a non-ASCII
/// byte simply fails the digit test it is offered to.
struct Scan<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Scan<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            at: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn reject(&self, detail: impl fmt::Display) -> TimeError {
        reject(self.text, detail)
    }

    fn literal(&mut self, byte: u8, expected: &str) -> Result<(), TimeError> {
        if self.peek() != Some(byte) {
            return Err(self.reject(format_args!("expected {expected}")));
        }
        self.at += 1;

        Ok(())
    }

    /// Reads exactly `count` digits, so `2024-1-01` is a malformed date
    /// rather than a January the padding was forgotten on.
    fn fixed(&mut self, count: usize, field: &str) -> Result<i64, TimeError> {
        let end = self.at + count;
        let Some(digits) = self.bytes.get(self.at..end) else {
            return Err(self.reject(format_args!("the {field} is cut short")));
        };

        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(self.reject(format_args!("the {field} is not {count} digits")));
        }
        self.at = end;

        Ok(digits
            .iter()
            .fold(0, |acc, d| acc * 10 + i64::from(d - b'0')))
    }

    /// The year: an optional `-`, then digits. Four is the minimum for
    /// a positive year because that is the RFC's shape and what the
    /// renderer's padding guarantees; a negative year is exempt, since
    /// `{:04}` spends one of those four columns on the sign.
    fn year(&mut self) -> Result<i64, TimeError> {
        let negative = self.peek() == Some(b'-');
        if negative {
            self.at += 1;
        }

        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }

        let digits = &self.bytes[start..self.at];
        if digits.is_empty() || (!negative && digits.len() < 4) {
            return Err(self.reject("expected a four-digit year"));
        }

        let mut year: i64 = 0;
        for digit in digits {
            year = year
                .checked_mul(10)
                .and_then(|shifted| shifted.checked_add(i64::from(digit - b'0')))
                .ok_or_else(|| self.reject("the year is too large to represent"))?;
        }

        // Bounded here rather than at the multiplication to
        // milliseconds, because the era arithmetic in `civil_days` runs
        // first and would overflow on the way there. The limit is far
        // outside what an epoch millisecond can represent anyway, so
        // nothing reachable is lost.
        if year > MAX_YEAR {
            return Err(self.reject("the year is too far from the epoch to represent"));
        }

        Ok(if negative { -year } else { year })
    }

    fn date_time_separator(&mut self) -> Result<(), TimeError> {
        match self.peek() {
            Some(b'T' | b't') => {
                self.at += 1;
                Ok(())
            }
            _ => Err(self.reject("expected `T` between the date and the time")),
        }
    }

    /// The fractional second, truncated to whole milliseconds. Digits
    /// past the third are consumed and dropped; they still have to BE
    /// digits, so a trailing `.5x` is malformed rather than half a
    /// second.
    fn fraction_millis(&mut self) -> Result<i64, TimeError> {
        if self.peek() != Some(b'.') {
            return Ok(0);
        }
        self.at += 1;

        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }

        let digits = &self.bytes[start..self.at];
        if digits.is_empty() {
            return Err(self.reject("expected at least one fractional-second digit"));
        }

        let millis = digits
            .iter()
            .chain(b"000")
            .take(3)
            .fold(0, |acc, d| acc * 10 + i64::from(d - b'0'));

        Ok(millis)
    }

    /// The offset, in minutes east of UTC. Absent is an error rather
    /// than an assumed UTC — see [`parse_iso`].
    fn offset_minutes(&mut self) -> Result<i64, TimeError> {
        let sign = match self.peek() {
            Some(b'Z' | b'z') => {
                self.at += 1;
                return Ok(0);
            }
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => {
                return Err(self.reject("expected a `Z` or a `+HH:MM` offset (a timestamp without an offset does not name an instant)"));
            }
        };
        self.at += 1;

        let hours = self.fixed(2, "offset hour")?;
        self.literal(b':', "a `:` in the offset")?;
        let minutes = self.fixed(2, "offset minute")?;

        if hours > 23 {
            return Err(self.reject(format_args!("offset hour {hours} is out of range")));
        }
        if minutes > 59 {
            return Err(self.reject(format_args!("offset minute {minutes} is out of range")));
        }

        Ok(sign * (hours * 60 + minutes))
    }

    fn end(&self) -> Result<(), TimeError> {
        if self.at != self.bytes.len() {
            return Err(self.reject("unexpected text after the offset"));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{iso_utc, parse_iso};

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

    /// The seven timestamps the renderer is pinned on, read back. This
    /// is the contract between the two members: whatever `iso` writes,
    /// `parseIso` must return unchanged.
    #[test]
    fn parsing_the_rendered_form_recovers_the_timestamp() {
        const PINNED: &[i64] = &[
            0,
            1_000_000_000_000,
            1_700_000_000_123,
            951_782_400_000,
            946_684_799_999,
            -1,
            -86_400_000,
        ];

        for millis in PINNED {
            let rendered = iso_utc(*millis);
            assert_eq!(
                parse_iso(&rendered).map_err(|error| error.message),
                Ok(*millis)
            );
        }
    }

    /// The round trip has to survive the sign, too: before year 1 the
    /// renderer's padding spends a column on the `-`, so the year is
    /// three digits and the scanner's four-digit minimum must not
    /// apply.
    #[test]
    fn the_round_trip_holds_before_year_one() {
        for millis in [
            -62_167_219_200_000_i64,
            -62_167_219_200_001,
            -62_198_755_200_000,
        ] {
            let rendered = iso_utc(millis);
            assert_eq!(parse_iso(&rendered).map_err(|e| e.message), Ok(millis));
        }
    }

    #[test]
    fn utc_timestamps_parse_to_epoch_millis() {
        assert_eq!(ok("1970-01-01T00:00:00Z"), 0);
        assert_eq!(ok("2023-11-14T22:13:20.123Z"), 1_700_000_000_123);
        assert_eq!(ok("1999-12-31T23:59:59.999Z"), 946_684_799_999);
    }

    /// RFC 3339 §5.6 permits either case for both markers.
    #[test]
    fn the_lowercase_separator_and_zone_are_accepted() {
        assert_eq!(ok("2023-11-14t22:13:20.123z"), 1_700_000_000_123);
    }

    /// Two offsets that are neither symmetric nor whole hours, so a
    /// dropped minute or a flipped sign cannot pass unnoticed.
    #[test]
    fn offsets_move_the_instant_in_both_directions() {
        let utc = ok("2023-11-14T22:13:20.000Z");

        assert_eq!(ok("2023-11-15T03:43:20.000+05:30"), utc);
        assert_eq!(ok("2023-11-14T14:13:20.000-08:00"), utc);
    }

    #[test]
    fn fractional_digits_truncate_toward_the_millisecond() {
        assert_eq!(ok("1970-01-01T00:00:00.9Z"), 900);
        assert_eq!(ok("1970-01-01T00:00:00.99Z"), 990);
        assert_eq!(ok("1970-01-01T00:00:00.999999Z"), 999);
        // Truncation, not rounding: rounding up would answer a
        // millisecond whose own rendering is a different string.
        assert_eq!(ok("1970-01-01T00:00:00.9996Z"), 999);
        assert_eq!(ok("1970-01-01T00:00:00.000000001Z"), 0);
    }

    #[test]
    fn the_leap_year_rule_decides_february() {
        assert_eq!(ok("2000-02-29T00:00:00Z"), 951_782_400_000);
        assert_eq!(ok("2024-02-29T00:00:00Z"), 1_709_164_800_000);
        assert!(err("1900-02-29T00:00:00Z").contains("day 29 does not exist"));
        assert!(err("2023-02-29T00:00:00Z").contains("day 29 does not exist"));
        assert!(err("2024-02-30T00:00:00Z").contains("day 30 does not exist"));
    }

    #[test]
    fn out_of_range_fields_are_refused_by_name() {
        assert!(err("2024-13-01T00:00:00Z").contains("month 13 is out of range"));
        assert!(err("2024-00-01T00:00:00Z").contains("month 0 is out of range"));
        assert!(err("2024-01-32T00:00:00Z").contains("day 32 does not exist"));
        assert!(err("2024-01-01T25:00:00Z").contains("hour 25 is out of range"));
        assert!(err("2024-01-01T00:60:00Z").contains("minute 60 is out of range"));
    }

    /// No epoch millisecond numbers a leap second, so `:60` has no
    /// answer that is not some other second's.
    #[test]
    fn a_leap_second_has_no_timestamp() {
        assert!(err("2016-12-31T23:59:60Z").contains("second 60 is out of range"));
    }

    /// Guessing UTC here is the failure mode the whole member exists to
    /// avoid, so the message has to point at the offset.
    #[test]
    fn a_naive_timestamp_is_refused_for_its_missing_offset() {
        let message = err("2024-01-01T00:00:00");

        assert!(message.contains("offset"), "{message}");
        assert!(message.contains("does not name an instant"), "{message}");
    }

    #[test]
    fn malformed_shapes_are_refused() {
        assert!(err("").contains("four-digit year"));
        assert!(err("not a timestamp").contains("four-digit year"));
        assert!(err("24-01-01T00:00:00Z").contains("four-digit year"));
        assert!(err("2024-1-01T00:00:00Z").contains("the month is not 2 digits"));
        assert!(err("2024-01-01 00:00:00Z").contains("expected `T`"));
        assert!(err("2024-01-01T00:00:00.Z").contains("fractional-second digit"));
        assert!(err("2024-01-01T00:00:00Z ").contains("unexpected text"));
        assert!(err("2024-01-01T00:00:00+0530").contains("`:` in the offset"));
        assert!(err("2024-01-01T00").contains("expected a `:` after the hour"));
        assert!(err("2024-01-01T0").contains("the hour is cut short"));
    }

    #[test]
    fn an_unrepresentable_offset_or_year_is_refused() {
        assert!(err("2024-01-01T00:00:00+99:00").contains("offset hour 99 is out of range"));
        assert!(err("2024-01-01T00:00:00+00:99").contains("offset minute 99 is out of range"));
        assert!(err("99999999999999999999-01-01T00:00:00Z").contains("year is too large"));

        // A year that fits an `i64` but not the calendar arithmetic
        // that reads it, in both signs.
        assert!(err("9223372036854775807-01-01T00:00:00Z").contains("too far from the epoch"));
        assert!(err("-9223372036854775807-01-01T00:00:00Z").contains("too far from the epoch"));

        // Inside the year bound, out of range once it becomes millis.
        assert!(err("999999999-01-01T00:00:00Z").contains("too far from the epoch"));
    }

    #[test]
    fn the_error_carries_the_time_namespace() {
        let error = parse_iso("nope").expect_err("`nope` is not a timestamp");

        assert_eq!(error.name, "time.ParseError");
        assert!(error.message.starts_with("cannot parse timestamp \"nope\""));
    }

    fn ok(text: &str) -> i64 {
        parse_iso(text).unwrap_or_else(|error| panic!("{}", error.message))
    }

    fn err(text: &str) -> String {
        match parse_iso(text) {
            Ok(millis) => panic!("`{text}` parsed to {millis}"),
            Err(error) => error.message,
        }
    }
}
