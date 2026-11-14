//! Turning what people and agents type into exact moments, and moments back into local time.
//!
//! Item times are stored as Unix seconds. Anything without an explicit offset is read in a
//! time zone: the computer's own unless a caller names another one.

use anyhow::{Result, anyhow, bail};
use jiff::{Span, Timestamp, Zoned, civil, tz::TimeZone};

/// Which end of a range a time is for. It only matters for whole days: `--until 2026-11-11`
/// means through the end of that day, `--since 2026-11-11` means from its first moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    Start,
    End,
}

/// The time zone to read and show times in, and the current moment.
#[derive(Clone, Debug)]
pub struct Clock {
    pub tz: TimeZone,
    pub now: Timestamp,
}

impl Clock {
    /// The computer's time zone and the current time.
    pub fn system() -> Self {
        Self { tz: TimeZone::system(), now: Timestamp::now() }
    }

    /// A named IANA time zone (`America/Los_Angeles`), or the computer's when `name` is none.
    pub fn in_zone(name: Option<&str>) -> Result<Self> {
        let tz = match name.map(str::trim).filter(|n| !n.is_empty()) {
            None => TimeZone::system(),
            Some(n) if n.eq_ignore_ascii_case("utc") || n == "Z" => TimeZone::UTC,
            Some(n) => TimeZone::get(n).map_err(|_| {
                anyhow!("unknown time zone \"{n}\". Use an IANA name such as America/Los_Angeles or Europe/London.")
            })?,
        };
        Ok(Self { tz, now: Timestamp::now() })
    }

    pub fn zone_name(&self) -> String {
        match self.tz.iana_name() {
            Some(name) => name.to_string(),
            None => format_offset(self.tz.to_offset(self.now).seconds()),
        }
    }

    pub fn now_zoned(&self) -> Zoned {
        self.now.to_zoned(self.tz.clone())
    }

    pub fn zoned(&self, unix: f64) -> Zoned {
        timestamp(unix).to_zoned(self.tz.clone())
    }

    /// `Wed 2026-11-11 12:34`
    pub fn short(&self, unix: f64) -> String {
        self.zoned(unix).strftime("%a %Y-%m-%d %H:%M").to_string()
    }

    /// `Wed 2026-11-11 12:34:05 -08:00`
    pub fn full(&self, unix: f64) -> String {
        let z = self.zoned(unix);
        format!("{} {}", z.strftime("%a %Y-%m-%d %H:%M:%S"), format_offset(z.offset().seconds()))
    }

    /// `2026-11-11T12:34:05-08:00`
    pub fn rfc3339(&self, unix: f64) -> String {
        let z = self.zoned(unix);
        format!("{}{}", z.strftime("%Y-%m-%dT%H:%M:%S"), format_offset(z.offset().seconds()))
    }

    /// `2026-11-11`, the local calendar day an item falls on.
    pub fn day(&self, unix: f64) -> String {
        self.zoned(unix).strftime("%Y-%m-%d").to_string()
    }

    /// `Wednesday, November 11, 2026`
    pub fn day_label(&self, unix: f64) -> String {
        let z = self.zoned(unix);
        format!("{}, {} {}, {}", z.strftime("%A"), z.strftime("%B"), z.day(), z.year())
    }

    /// `12:34`
    pub fn time_of_day(&self, unix: f64) -> String {
        self.zoned(unix).strftime("%H:%M").to_string()
    }

    /// Reads a time. See the module documentation for the accepted forms.
    pub fn parse(&self, input: &str, bound: Bound) -> Result<f64> {
        parse_time(input, self, bound)
    }
}

/// `-08:00`, `+05:30`, `+00:00`
pub fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

pub fn timestamp(unix: f64) -> Timestamp {
    let unix = if unix.is_finite() { unix } else { 0.0 };
    let whole = unix.floor();
    let nanos = (((unix - whole) * 1e9).round() as i32).clamp(0, 999_999_999);
    Timestamp::new(whole as i64, nanos).unwrap_or(Timestamp::UNIX_EPOCH)
}

pub fn unix(ts: Timestamp) -> f64 {
    ts.as_nanosecond() as f64 / 1e9
}

/// Accepted forms:
///
/// - RFC 3339 with an offset: `2026-11-11T12:00:00-08:00`, `2026-11-11T20:00:00Z`
/// - a local date: `2026-11-11` (its first moment as a start, the end of that day as an end)
/// - a local date and time: `2026-11-11 12:00`, `2026-11-11T12:00:30`
/// - a time today: `14:30`
/// - how long ago: `90s`, `45m`, `2h`, `3d`, `1w` (with or without ` ago`)
/// - `now`, `today`, `yesterday`, `tomorrow`
/// - Unix seconds: `1794445200`
fn parse_time(input: &str, clock: &Clock, bound: Bound) -> Result<f64> {
    let text = input.trim();
    if text.is_empty() {
        bail!("empty time");
    }
    let lower = text.to_ascii_lowercase();
    let today = clock.now_zoned().date();

    match lower.as_str() {
        "now" => return Ok(unix(clock.now)),
        "today" => return day_bound(today, clock, bound),
        "yesterday" => return day_bound(today.yesterday()?, clock, bound),
        "tomorrow" => return day_bound(today.tomorrow()?, clock, bound),
        _ => {}
    }

    if let Some(ago) = parse_relative(&lower) {
        let now = clock.now_zoned();
        let then = match ago {
            Relative::Seconds(s) => {
                return Ok(unix(clock.now) - s);
            }
            Relative::Days(d) => now.checked_sub(Span::new().try_days(d)?)?,
        };
        return Ok(unix(then.timestamp()));
    }

    // Unix seconds. Nine digits or more, so a year alone is never mistaken for one.
    if text.len() >= 9 && text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && let Ok(value) = text.parse::<f64>() {
            return Ok(value);
        }

    // A whole local day.
    if text.len() == 10
        && let Ok(date) = text.parse::<civil::Date>() {
            return day_bound(date, clock, bound);
        }

    // A time of day, today.
    if text.len() <= 8 && text.contains(':')
        && let Ok(time) = text.parse::<civil::Time>() {
            return civil_to_unix(today.to_datetime(time), clock);
        }

    // An exact instant: an offset or Z is present.
    if let Ok(ts) = text.parse::<Timestamp>() {
        return Ok(unix(ts));
    }
    // `2026-11-11T12:00[America/Los_Angeles]`
    if text.contains('[')
        && let Ok(zoned) = text.parse::<Zoned>() {
            return Ok(unix(zoned.timestamp()));
        }

    // A local date and time, read in the clock's zone.
    if let Ok(datetime) = text.parse::<civil::DateTime>() {
        return civil_to_unix(datetime, clock);
    }

    bail!(
        "cannot read the time \"{text}\". Try 2026-11-11, \"2026-11-11 14:30\", \
         2026-11-11T14:30:00-08:00, 2h, 3d, today or yesterday."
    )
}

fn day_bound(date: civil::Date, clock: &Clock, bound: Bound) -> Result<f64> {
    let date = match bound {
        Bound::Start => date,
        Bound::End => date.tomorrow()?,
    };
    // The first moment of the day. Midnight can be missing on a clock change day in some
    // zones, in which case this is the first time that exists.
    let zoned = date.to_zoned(clock.tz.clone())?;
    Ok(unix(zoned.timestamp()))
}

/// A wall-clock time that does not exist (spring forward) becomes the time just after the
/// gap; one that happens twice (fall back) is the first of the two.
fn civil_to_unix(datetime: civil::DateTime, clock: &Clock) -> Result<f64> {
    let zoned = datetime.to_zoned(clock.tz.clone())?;
    Ok(unix(zoned.timestamp()))
}

enum Relative {
    Seconds(f64),
    Days(i64),
}

fn parse_relative(lower: &str) -> Option<Relative> {
    let text = lower.strip_suffix("ago").map(str::trim_end).unwrap_or(lower);
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = text.split_at(split);
    if number.is_empty() {
        return None;
    }
    let unit = unit.trim();
    let value: f64 = number.parse().ok()?;
    let seconds = |per: f64| Some(Relative::Seconds(value * per));
    match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => seconds(1.0),
        "m" | "min" | "mins" | "minute" | "minutes" => seconds(60.0),
        "h" | "hr" | "hrs" | "hour" | "hours" => seconds(3600.0),
        "d" | "day" | "days" | "w" | "wk" | "week" | "weeks" => {
            let per = if unit.starts_with('w') { 7.0 } else { 1.0 };
            if value.fract() == 0.0 {
                // Whole days are calendar days, so "3d" lands on the same wall-clock time
                // even across a clock change.
                Some(Relative::Days((value * per) as i64))
            } else {
                seconds(86400.0 * per)
            }
        }
        _ => None,
    }
}

/// Seconds as a short human duration: `8s`, `3m 05s`, `1h 12m`.
pub fn human_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    if total < 60 {
        format!("{total}s")
    } else if total < 3600 {
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{}h {:02}m", total / 3600, (total % 3600) / 60)
    }
}

/// How long ago, roughly: `just now`, `5 min ago`, `3 h ago`, `2 days ago`.
pub fn human_ago(seconds: f64) -> String {
    let s = seconds.max(0.0);
    if s < 45.0 {
        "just now".to_string()
    } else if s < 3600.0 {
        format!("{} min ago", (s / 60.0).round().max(1.0) as u64)
    } else if s < 86400.0 * 2.0 {
        format!("{} h ago", (s / 3600.0).round() as u64)
    } else {
        format!("{} days ago", (s / 86400.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn la(now: &str) -> Clock {
        Clock { tz: TimeZone::get("America/Los_Angeles").unwrap(), now: now.parse().unwrap() }
    }

    fn utc(text: &str) -> f64 {
        unix(text.parse::<Timestamp>().unwrap())
    }

    #[test]
    fn explicit_offset_ignores_the_local_zone() {
        let clock = la("2026-11-14T20:00:00Z");
        let expected = utc("2026-11-11T20:00:00Z");
        assert_eq!(clock.parse("2026-11-11T12:00:00-08:00", Bound::Start).unwrap(), expected);
        assert_eq!(clock.parse("2026-11-11T20:00:00Z", Bound::Start).unwrap(), expected);
        assert_eq!(clock.parse("2026-11-11T15:00:00-05:00", Bound::End).unwrap(), expected);
        assert_eq!(clock.parse("2026-11-12T01:30:00+05:30", Bound::End).unwrap(), expected);
    }

    #[test]
    fn local_date_time_uses_the_zone() {
        let clock = la("2026-11-14T20:00:00Z");
        // November is standard time, UTC-8.
        assert_eq!(clock.parse("2026-11-11 12:00", Bound::Start).unwrap(), utc("2026-11-11T20:00:00Z"));
        assert_eq!(clock.parse("2026-11-11T17:00", Bound::End).unwrap(), utc("2026-11-12T01:00:00Z"));
        assert_eq!(clock.parse("2026-11-11 12:00:30", Bound::Start).unwrap(), utc("2026-11-11T20:00:30Z"));
        // July is daylight time, UTC-7.
        assert_eq!(clock.parse("2026-07-04 12:00", Bound::Start).unwrap(), utc("2026-07-04T19:00:00Z"));
        // The same wall-clock text in another zone is a different instant.
        let london = Clock { tz: TimeZone::get("Europe/London").unwrap(), now: clock.now };
        assert_eq!(london.parse("2026-11-11 12:00", Bound::Start).unwrap(), utc("2026-11-11T12:00:00Z"));
    }

    #[test]
    fn whole_days_cover_the_day_as_a_range() {
        let clock = la("2026-11-14T20:00:00Z");
        let start = clock.parse("2026-11-11", Bound::Start).unwrap();
        let end = clock.parse("2026-11-11", Bound::End).unwrap();
        assert_eq!(start, utc("2026-11-11T08:00:00Z"));
        assert_eq!(end, utc("2026-11-12T08:00:00Z"));
        assert_eq!(end - start, 86400.0);
    }

    #[test]
    fn fall_back_day_is_twenty_five_hours() {
        // Clocks go back at 02:00 on 2026-11-01 in Los Angeles.
        let clock = la("2026-11-14T20:00:00Z");
        let start = clock.parse("2026-11-01", Bound::Start).unwrap();
        let end = clock.parse("2026-11-01", Bound::End).unwrap();
        assert_eq!(start, utc("2026-11-01T07:00:00Z"));
        assert_eq!(end, utc("2026-11-02T08:00:00Z"));
        assert_eq!(end - start, 25.0 * 3600.0);
        // 01:30 happens twice. The first one (daylight time) is used.
        assert_eq!(clock.parse("2026-11-01 01:30", Bound::Start).unwrap(), utc("2026-11-01T08:30:00Z"));
        // After the change the offset is -08:00.
        assert_eq!(clock.parse("2026-11-01 03:00", Bound::Start).unwrap(), utc("2026-11-01T11:00:00Z"));
        assert_eq!(clock.full(utc("2026-11-01T08:30:00Z")), "Sun 2026-11-01 01:30:00 -07:00");
        assert_eq!(clock.full(utc("2026-11-01T09:30:00Z")), "Sun 2026-11-01 01:30:00 -08:00");
    }

    #[test]
    fn spring_forward_day_is_twenty_three_hours() {
        // Clocks go forward at 02:00 on 2026-03-08 in Los Angeles.
        let clock = la("2026-11-14T20:00:00Z");
        let start = clock.parse("2026-03-08", Bound::Start).unwrap();
        let end = clock.parse("2026-03-08", Bound::End).unwrap();
        assert_eq!(end - start, 23.0 * 3600.0);
        // 02:30 does not exist. It becomes 03:30 daylight time.
        assert_eq!(clock.parse("2026-03-08 02:30", Bound::Start).unwrap(), utc("2026-03-08T10:30:00Z"));
    }

    #[test]
    fn relative_times() {
        let clock = la("2026-11-02T20:00:00Z"); // Mon 2026-11-02 12:00 PST
        let now = unix(clock.now);
        assert_eq!(clock.parse("now", Bound::Start).unwrap(), now);
        assert_eq!(clock.parse("2h", Bound::Start).unwrap(), now - 7200.0);
        assert_eq!(clock.parse("90s", Bound::Start).unwrap(), now - 90.0);
        assert_eq!(clock.parse("45m ago", Bound::Start).unwrap(), now - 2700.0);
        assert_eq!(clock.parse("1.5h", Bound::Start).unwrap(), now - 5400.0);
        // Three calendar days back crosses the clock change: same wall time, 73 hours ago.
        assert_eq!(clock.parse("3d", Bound::Start).unwrap(), utc("2026-10-30T19:00:00Z"));
        assert_eq!(clock.parse("1w", Bound::Start).unwrap(), utc("2026-10-26T19:00:00Z"));
        assert_eq!(clock.parse("today", Bound::Start).unwrap(), utc("2026-11-02T08:00:00Z"));
        assert_eq!(clock.parse("today", Bound::End).unwrap(), utc("2026-11-03T08:00:00Z"));
        assert_eq!(clock.parse("yesterday", Bound::Start).unwrap(), utc("2026-11-01T07:00:00Z"));
        assert_eq!(clock.parse("yesterday", Bound::End).unwrap(), utc("2026-11-02T08:00:00Z"));
        assert_eq!(clock.parse("14:30", Bound::Start).unwrap(), utc("2026-11-02T22:30:00Z"));
        assert_eq!(clock.parse("1794445200", Bound::Start).unwrap(), 1794445200.0);
    }

    #[test]
    fn rejects_nonsense() {
        let clock = la("2026-11-14T20:00:00Z");
        for bad in ["", "soon", "2026-13-40", "12 parsecs", "2026"] {
            assert!(clock.parse(bad, Bound::Start).is_err(), "{bad} should not parse");
        }
        assert!(Clock::in_zone(Some("Mars/Olympus")).is_err());
        assert!(Clock::in_zone(Some("America/New_York")).is_ok());
    }

    #[test]
    fn formatting() {
        let clock = la("2026-11-14T20:00:00Z");
        let t = utc("2026-11-11T20:34:05Z");
        assert_eq!(clock.short(t), "Wed 2026-11-11 12:34");
        assert_eq!(clock.full(t), "Wed 2026-11-11 12:34:05 -08:00");
        assert_eq!(clock.rfc3339(t), "2026-11-11T12:34:05-08:00");
        assert_eq!(clock.day(t), "2026-11-11");
        assert_eq!(clock.day_label(t), "Wednesday, November 11, 2026");
        assert_eq!(clock.zone_name(), "America/Los_Angeles");
        assert_eq!(format_offset(19800), "+05:30");
        assert_eq!(human_duration(4.2), "4s");
        assert_eq!(human_duration(185.0), "3m 05s");
        assert_eq!(human_duration(4350.0), "1h 12m");
    }
}
