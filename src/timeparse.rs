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

    // Words: Wednesday, last Friday 3pm, yesterday afternoon, noon. A window's first moment
    // as a start, its end as an end.
    if let Ok(window) = parse_spoken(&lower, clock) {
        return Ok(match bound {
            Bound::Start => window.0,
            Bound::End => window.1,
        });
    }

    bail!(
        "cannot read the time \"{text}\". Try 2026-11-11, \"2026-11-11 14:30\", \
         2026-11-11T14:30:00-08:00, 2h, 3d, today, yesterday, Wednesday, \"last Friday 3pm\" or \"yesterday afternoon\"."
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

// -------------------------------------------------------------------- windows in words

/// A stretch of time, as agents and people say it: `Wednesday 12pm to 5pm`,
/// `yesterday afternoon`, `last week`. `start` is in it, `end` is not.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
}

/// The parts of a day, in minutes after midnight. A part that runs past midnight ends after
/// 24:00 (on the next day).
pub const PARTS_OF_DAY: &[(&str, i64, i64)] = &[
    ("morning", 5 * 60, 12 * 60),
    ("afternoon", 12 * 60, 17 * 60),
    ("evening", 17 * 60, 21 * 60),
    ("night", 21 * 60, 29 * 60),
    // "tonight" and "last night" are evening and night together.
    ("tonight", 17 * 60, 29 * 60),
];

const WORDS_HELP: &str = "Say a day (today, yesterday, Wednesday, last Wednesday, this Monday, 2026-11-11, Nov 11, 3 days ago), \
optionally a part of it (morning, afternoon, evening, night, tonight, last night) or times (12pm, 5:30 am, noon, midnight, 17:00), \
or a span (this week, last week, this month, last 3 days, 2h), and join two with \"to\": \"Wednesday 12pm to 5pm\", \"Monday to Wednesday\".";

impl Clock {
    /// Reads a window: see [`WORDS_HELP`] for what it understands. Anything `parse` takes works
    /// too: a date is that whole day, an exact time is that moment.
    pub fn window(&self, input: &str) -> Result<Window> {
        let text = input.trim();
        if text.is_empty() {
            bail!("the window is empty. {WORDS_HELP}");
        }
        let lower = normalize(text);
        let (start, end) = match split_range(&lower) {
            Some((left, right)) => {
                let (start, _, left_spec) = side(&left, self, None)?;
                let (_, end, _) = side(&right, self, Some(&left_spec))?;
                (start, end)
            }
            None => {
                let (start, end, _) = side(&lower, self, None)?;
                (start, end)
            }
        };
        if end < start {
            bail!("\"{text}\" ends ({}) before it starts ({})", self.full(end), self.full(start));
        }
        Ok(Window { start, end })
    }

    /// A window in words: `Wednesday, November 11, 2026, 12:00 to 17:00` or
    /// `Monday, November 9, 2026 to Wednesday, November 11, 2026 (whole days)`.
    pub fn window_words(&self, start: Option<f64>, end: Option<f64>) -> String {
        let midnight = |t: f64| self.time_of_day(t) == "00:00";
        match (start, end) {
            (None, None) => "the whole library".to_string(),
            (Some(s), None) => format!("from {}, {} on", self.day_label(s), self.time_of_day(s)),
            (None, Some(e)) => format!("everything before {}, {}", self.day_label(e), self.time_of_day(e)),
            (Some(s), Some(e)) if midnight(s) && midnight(e) && e > s => {
                let last = e - 1.0;
                if self.day(s) == self.day(last) {
                    format!("{} (the whole day)", self.day_label(s))
                } else {
                    format!("{} to {} (whole days)", self.day_label(s), self.day_label(last))
                }
            }
            (Some(s), Some(e)) if self.day(s) == self.day(e) || (e > s && self.day(s) == self.day(e - 1.0) && midnight(e)) => {
                let until = if midnight(e) && e > s { "24:00".to_string() } else { self.time_of_day(e) };
                format!("{}, {} to {}", self.day_label(s), self.time_of_day(s), until)
            }
            (Some(s), Some(e)) => format!("{}, {} to {}, {}", self.day_label(s), self.time_of_day(s), self.day_label(e), self.time_of_day(e)),
        }
    }
}

/// Lowercase, `a.m.` as `am`, commas as spaces, one space between words.
fn normalize(text: &str) -> String {
    let lower = text.to_lowercase().replace("a.m.", "am").replace("p.m.", "pm").replace([',', '\u{2013}'], " ");
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `A to B`, `from A to B`, `between A and B`, `A until B`, `A - B`.
fn split_range(text: &str) -> Option<(String, String)> {
    let padded = format!(" {text} ");
    // "Wednesday between 12 and 5", "Wednesday from noon to 5pm": the day goes with the first side.
    let padded = padded.replacen(" from ", " ", 1);
    let text = match padded.split_once(" between ") {
        Some((before, rest)) => format!("{before} {}", rest.replacen(" and ", " to ", 1)),
        None => padded,
    };
    let text = text.trim();
    for separator in [" to ", " until ", " till ", " through ", " thru ", " - "] {
        if let Some((a, b)) = text.split_once(separator) {
            return Some((a.trim().to_string(), b.trim().to_string()));
        }
    }
    None
}

/// What one side of a window says.
#[derive(Clone, Debug, Default)]
struct Spec {
    /// The days it covers, first and last (inclusive).
    days: Option<(civil::Date, civil::Date)>,
    /// A time of day, minutes after midnight.
    minutes: Option<i64>,
    /// Whether the time was said with am or pm.
    meridiem: bool,
    /// A part of the day: minutes after midnight, start and end.
    part: Option<(i64, i64)>,
    /// An exact span (`last 3 days`, `2h`): Unix seconds.
    span: Option<(f64, f64)>,
}

/// One side: its window, and what it said (so the other side can borrow its day).
fn side(text: &str, clock: &Clock, left: Option<&Spec>) -> Result<(f64, f64, Spec)> {
    let spec = match spec(text, clock) {
        Ok(spec) => spec,
        Err(error) => {
            // The forms `parse` takes: an ISO date, a date and time, an offset.
            let start = parse_exact(text, clock, Bound::Start);
            let end = parse_exact(text, clock, Bound::End);
            return match (start, end) {
                (Some(start), Some(end)) => Ok((start, end, Spec { span: Some((start, end)), ..Spec::default() })),
                _ => Err(error),
            };
        }
    };
    let mut spec = spec;
    if let (Some(left), None) = (left, spec.days) {
        // "Wednesday 12pm to 5pm": the second side is on the first side's day.
        spec.days = left.days.map(|(_, last)| (last, last));
        if let (Some(m), false, Some(left_minutes)) = (spec.minutes, spec.meridiem, left.minutes) {
            // "12pm to 5": a bare hour after an afternoon start is in the afternoon too.
            if m < 12 * 60 && left_minutes >= 12 * 60 {
                spec.minutes = Some(m + 12 * 60);
            }
        }
    }
    let (start, end) = window_of(&spec, clock)?;
    // "10pm to 2am": the end is on the next day.
    if let Some(left) = left
        && spec.minutes.is_some()
        && left.minutes.is_some()
        && spec.days == left.days.map(|(_, last)| (last, last))
    {
        let (left_start, _) = window_of(left, clock)?;
        if start <= left_start {
            let next = Spec { days: spec.days.map(|(d, _)| (d.tomorrow().unwrap_or(d), d.tomorrow().unwrap_or(d))), ..spec.clone() };
            let (s, e) = window_of(&next, clock)?;
            return Ok((s, e, next));
        }
    }
    Ok((start, end, spec))
}

/// The forms of [`parse_time`] that are not words, without its word fallback.
fn parse_exact(text: &str, clock: &Clock, bound: Bound) -> Option<f64> {
    let text = text.to_uppercase();
    if text.len() == 10
        && let Ok(date) = text.parse::<civil::Date>()
    {
        return day_bound(date, clock, bound).ok();
    }
    if let Ok(ts) = text.parse::<Timestamp>() {
        return Some(unix(ts));
    }
    if let Ok(datetime) = text.replace(' ', "T").parse::<civil::DateTime>() {
        return civil_to_unix(datetime, clock).ok();
    }
    None
}

/// The window one side covers.
fn window_of(spec: &Spec, clock: &Clock) -> Result<(f64, f64)> {
    if let Some(span) = spec.span {
        return Ok(span);
    }
    let today = clock.now_zoned().date();
    let (first, last) = spec.days.unwrap_or((today, today));
    let at = |date: civil::Date, minutes: i64| -> Result<f64> {
        let datetime = date.to_datetime(civil::Time::midnight()).checked_add(Span::new().try_minutes(minutes)?)?;
        civil_to_unix(datetime, clock)
    };
    match (spec.minutes, spec.part) {
        (Some(minutes), _) => {
            let t = at(last, minutes)?;
            Ok((t, t))
        }
        (None, Some((from, to))) => Ok((at(first, from)?, at(last, to)?)),
        (None, None) => Ok((at(first, 0)?, at(last.tomorrow()?, 0)?)),
    }
}

fn weekday_of(word: &str) -> Option<i8> {
    let names = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    let short = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let extra = [("tues", 1), ("weds", 2), ("thur", 3), ("thurs", 3)];
    names
        .iter()
        .position(|n| *n == word)
        .or_else(|| short.iter().position(|n| *n == word))
        .map(|i| i as i8)
        .or_else(|| extra.iter().find(|(n, _)| *n == word).map(|(_, i)| *i))
}

fn month_of(word: &str) -> Option<i8> {
    let names = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    names.iter().position(|n| word.len() >= 3 && n.starts_with(word)).map(|i| i as i8 + 1)
}

/// A time of day said as `12pm`, `5:30`, `5:30 pm`, `17:00`, `noon`, `midnight`. Returns the
/// minutes after midnight, whether am or pm was said, and how many words it took.
fn time_of(words: &[&str]) -> Option<(i64, bool, usize)> {
    let word = *words.first()?;
    match word {
        "noon" | "midday" => return Some((12 * 60, true, 1)),
        "midnight" => return Some((0, true, 1)),
        _ => {}
    }
    let (body, mut meridiem) = if let Some(b) = word.strip_suffix("am") {
        (b, Some(false))
    } else if let Some(b) = word.strip_suffix("pm") {
        (b, Some(true))
    } else {
        (word, None)
    };
    let mut used = 1;
    if meridiem.is_none() {
        match words.get(1) {
            Some(&"am") => {
                meridiem = Some(false);
                used = 2;
            }
            Some(&"pm") => {
                meridiem = Some(true);
                used = 2;
            }
            _ => {}
        }
    }
    let (hour, minute) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok().filter(|m| (0..60).contains(m) && body.len() - h.len() == 3)?),
        None => (body.parse::<i64>().ok()?, 0),
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == ':') {
        return None;
    }
    let hour = match meridiem {
        Some(pm) if (1..=12).contains(&hour) => (hour % 12) + if pm { 12 } else { 0 },
        Some(_) => return None,
        // A bare number is a time only with a colon, or as an hour on the clock (`to 5`).
        None if body.contains(':') && (0..24).contains(&hour) => hour,
        None if (0..24).contains(&hour) && words.len() == 1 => hour,
        None => return None,
    };
    Some((hour * 60 + minute, meridiem.is_some() || body.contains(':') && hour >= 13, used))
}

/// Reads one side of a window in words.
fn spec(text: &str, clock: &Clock) -> Result<Spec> {
    let today = clock.now_zoned().date();
    let now = unix(clock.now);
    let words: Vec<&str> = text.split(' ').filter(|w| !w.is_empty()).collect();
    let mut spec = Spec::default();
    let mut i = 0;
    let set_days = |spec: &mut Spec, first: civil::Date, last: civil::Date| -> Result<()> {
        if spec.days.is_some() || spec.span.is_some() {
            bail!("\"{text}\" names two days. Join two with \"to\": \"Monday to Wednesday\".");
        }
        spec.days = Some((first, last));
        Ok(())
    };
    let monday = |date: civil::Date| -> Result<civil::Date> { Ok(date.checked_sub(Span::new().try_days(date.weekday().to_monday_zero_offset() as i64)?)?) };
    while i < words.len() {
        let word = words[i];
        let next = words.get(i + 1).copied();
        i += 1;
        if matches!(word, "on" | "at" | "in" | "the" | "of" | "during" | "around" | "about") {
            continue;
        }
        if let Some((from, to)) = PARTS_OF_DAY.iter().find(|(name, ..)| *name == word).map(|(_, a, b)| (*a, *b)) {
            if spec.part.is_some() || spec.minutes.is_some() {
                bail!("\"{text}\" names two times of day");
            }
            spec.part = Some((from, to));
            continue;
        }
        if let Some((minutes, said, used)) = time_of(&words[i - 1..]) {
            if spec.part.is_some() || spec.minutes.is_some() {
                bail!("\"{text}\" names two times of day. Join two with \"to\": \"12pm to 5pm\".");
            }
            spec.minutes = Some(minutes);
            spec.meridiem = said;
            i += used - 1;
            continue;
        }
        match word {
            "today" => set_days(&mut spec, today, today)?,
            "yesterday" => set_days(&mut spec, today.yesterday()?, today.yesterday()?)?,
            "tomorrow" => set_days(&mut spec, today.tomorrow()?, today.tomorrow()?)?,
            "this" | "last" | "next" | "past" => {
                let Some(what) = next else { bail!("\"{text}\" ends with \"{word}\". {WORDS_HELP}") };
                i += 1;
                if let Some(day) = weekday_of(what) {
                    let back = (today.weekday().to_monday_zero_offset() - day + 7) % 7;
                    let date = match word {
                        // The most recent such day before today.
                        "last" | "past" => today.checked_sub(Span::new().days(if back == 0 { 7 } else { back as i64 }))?,
                        // That day in this week (weeks start on Monday), even when it is ahead.
                        "this" => monday(today)?.checked_add(Span::new().days(day as i64))?,
                        // The next such day after today.
                        _ => today.checked_add(Span::new().days(if back == 0 { 7 } else { (7 - back) as i64 }))?,
                    };
                    set_days(&mut spec, date, date)?;
                    continue;
                }
                if let Some((_, from, to)) = PARTS_OF_DAY.iter().find(|(name, ..)| *name == what) {
                    // "this morning" is today's; "last night" is yesterday evening into today.
                    let date = if word == "last" { today.yesterday()? } else { today };
                    set_days(&mut spec, date, date)?;
                    spec.part = Some(if what == "night" && word == "last" { (17 * 60, 29 * 60) } else { (*from, *to) });
                    continue;
                }
                let this_monday = monday(today)?;
                match (word, what) {
                    ("this", "week") => set_days(&mut spec, this_monday, today)?,
                    ("last" | "past", "week") => set_days(&mut spec, this_monday.checked_sub(Span::new().days(7))?, this_monday.yesterday()?)?,
                    ("next", "week") => set_days(&mut spec, this_monday.checked_add(Span::new().days(7))?, this_monday.checked_add(Span::new().days(13))?)?,
                    ("this", "weekend") => set_days(&mut spec, this_monday.checked_add(Span::new().days(5))?, this_monday.checked_add(Span::new().days(6))?)?,
                    ("last" | "past", "weekend") => set_days(&mut spec, this_monday.checked_sub(Span::new().days(2))?, this_monday.yesterday()?)?,
                    ("this", "month") => set_days(&mut spec, today.first_of_month(), today)?,
                    ("last" | "past", "month") => {
                        let last = today.first_of_month().yesterday()?;
                        set_days(&mut spec, last.first_of_month(), last)?
                    }
                    ("this", "year") => set_days(&mut spec, civil::Date::new(today.year(), 1, 1)?, today)?,
                    ("last" | "past", _) if what.parse::<f64>().is_ok() || what == "a" || what == "an" => {
                        // "last 3 days", "past 2 hours": from that long ago until now.
                        let number = if what.parse::<f64>().is_ok() { what } else { "1" };
                        let Some(unit) = words.get(i) else { bail!("\"{text}\": {word} {what} what? Say days, hours or weeks.") };
                        i += 1;
                        let Some(ago) = parse_relative(&format!("{number}{unit}")) else { bail!("\"{text}\": cannot read \"{unit}\". Say minutes, hours, days or weeks.") };
                        let from = match ago {
                            Relative::Seconds(s) => now - s,
                            Relative::Days(d) => unix(clock.now_zoned().checked_sub(Span::new().try_days(d)?)?.timestamp()),
                        };
                        if spec.days.is_some() || spec.span.is_some() {
                            bail!("\"{text}\" names two spans");
                        }
                        spec.span = Some((from, now));
                    }
                    _ => bail!("cannot read \"{word} {what}\" in \"{text}\". {WORDS_HELP}"),
                }
            }
            _ if weekday_of(word).is_some() => {
                // The most recent such day, today included.
                let day = weekday_of(word).unwrap_or(0);
                let back = (today.weekday().to_monday_zero_offset() - day + 7) % 7;
                let date = today.checked_sub(Span::new().days(back as i64))?;
                set_days(&mut spec, date, date)?;
            }
            _ if word.len() == 10 && word.parse::<civil::Date>().is_ok() => {
                let date: civil::Date = word.parse()?;
                set_days(&mut spec, date, date)?;
            }
            _ if month_of(word).is_some() || (next.and_then(month_of).is_some() && word.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse::<i8>().is_ok()) => {
                // "Nov 11", "November 11 2026", "11 November".
                let (month, day_word) = match month_of(word) {
                    Some(month) => (month, next.unwrap_or("")),
                    None => (month_of(next.unwrap_or("")).unwrap_or(1), word),
                };
                let Ok(day) = day_word.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse::<i8>() else {
                    bail!("\"{text}\": which day of {word}? Say \"Nov 11\".")
                };
                i += 1;
                let year = match words.get(i).and_then(|w| w.parse::<i16>().ok()).filter(|y| *y > 1900) {
                    Some(year) => {
                        i += 1;
                        year
                    }
                    None => {
                        // Without a year: the most recent such day, today included.
                        let this_year = civil::Date::new(today.year(), month, day).map_err(|_| anyhow!("\"{text}\" is not a day of the calendar"))?;
                        if this_year > today { today.year() - 1 } else { today.year() }
                    }
                };
                let date = civil::Date::new(year, month, day).map_err(|_| anyhow!("\"{text}\" is not a day of the calendar"))?;
                set_days(&mut spec, date, date)?;
            }
            _ if word.parse::<i64>().is_ok() && next.is_some_and(|n| n.starts_with("day") || n.starts_with("week")) => {
                // "3 days ago": that whole day. "2 weeks ago": that week, Monday to Sunday.
                let count: i64 = word.parse()?;
                let unit = next.unwrap_or("days");
                i += 1;
                if words.get(i) == Some(&"ago") {
                    i += 1;
                }
                if unit.starts_with("week") {
                    let first = monday(today)?.checked_sub(Span::new().try_days(7 * count)?)?;
                    set_days(&mut spec, first, first.checked_add(Span::new().days(6))?)?;
                } else {
                    let date = today.checked_sub(Span::new().try_days(count)?)?;
                    set_days(&mut spec, date, date)?;
                }
            }
            _ => match parse_relative(&words[i - 1..].join(" ")) {
                // "2h", "90m", "3d", "1 week ago": from then until now.
                Some(ago) if spec.days.is_none() && spec.span.is_none() => {
                    let from = match ago {
                        Relative::Seconds(s) => now - s,
                        Relative::Days(d) => unix(clock.now_zoned().checked_sub(Span::new().try_days(d)?)?.timestamp()),
                    };
                    spec.span = Some((from, now));
                    i = words.len();
                }
                _ => bail!("cannot read \"{word}\" in \"{text}\". {WORDS_HELP}"),
            },
        }
    }
    if spec.days.is_none() && spec.span.is_none() && spec.minutes.is_none() && spec.part.is_none() {
        bail!("\"{text}\" says no time. {WORDS_HELP}");
    }
    if spec.span.is_some() && (spec.minutes.is_some() || spec.part.is_some()) {
        bail!("\"{text}\" mixes a span with a time of day");
    }
    if let (Some((first, last)), Some(_)) = (spec.days, spec.minutes.or(spec.part.map(|p| p.0)))
        && first != last
        && spec.minutes.is_some()
    {
        bail!("\"{text}\" puts one time on several days. Say a day: \"Wednesday 3pm\".");
    }
    Ok(spec)
}

/// Reads a whole window in words, for `parse_time`'s fallback: (start, end).
fn parse_spoken(lower: &str, clock: &Clock) -> Result<(f64, f64)> {
    let window = clock.window(lower)?;
    Ok((window.start, window.end))
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

    /// Thursday 2026-11-19, 10:00 in Los Angeles.
    fn thursday() -> Clock {
        la("2026-11-19T18:00:00Z")
    }

    fn window(clock: &Clock, text: &str) -> (String, String) {
        let w = clock.window(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        (clock.rfc3339(w.start), clock.rfc3339(w.end))
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn weekdays_are_the_most_recent_one_not_in_the_future() {
        let clock = thursday();
        assert_eq!(window(&clock, "Wednesday"), pair("2026-11-18T00:00:00-08:00", "2026-11-19T00:00:00-08:00"));
        assert_eq!(window(&clock, "wed"), window(&clock, "Wednesday"));
        assert_eq!(window(&clock, "on Monday"), pair("2026-11-16T00:00:00-08:00", "2026-11-17T00:00:00-08:00"));
        // Today's own weekday is today. "last" is always before today.
        assert_eq!(window(&clock, "Thursday").0, "2026-11-19T00:00:00-08:00");
        assert_eq!(window(&clock, "last Thursday").0, "2026-11-12T00:00:00-08:00");
        assert_eq!(window(&clock, "last Wednesday").0, "2026-11-18T00:00:00-08:00");
        // A weekday ahead in the week is last week's.
        assert_eq!(window(&clock, "Friday").0, "2026-11-13T00:00:00-08:00");
        // "this" is the day in this week, Monday to Sunday, even when it is ahead.
        assert_eq!(window(&clock, "this Monday").0, "2026-11-16T00:00:00-08:00");
        assert_eq!(window(&clock, "this Saturday").0, "2026-11-21T00:00:00-08:00");
        assert_eq!(window(&clock, "next Monday").0, "2026-11-23T00:00:00-08:00");
    }

    #[test]
    fn times_parts_and_ranges() {
        let clock = thursday();
        assert_eq!(window(&clock, "Wednesday 12pm to 5pm"), pair("2026-11-18T12:00:00-08:00", "2026-11-18T17:00:00-08:00"));
        assert_eq!(window(&clock, "Wednesday between 12 PM and 5 p.m."), window(&clock, "Wednesday 12pm to 5pm"));
        assert_eq!(window(&clock, "from Wednesday noon until 5:30 pm"), pair("2026-11-18T12:00:00-08:00", "2026-11-18T17:30:00-08:00"));
        assert_eq!(window(&clock, "wednesday 12pm to 5"), window(&clock, "Wednesday 12pm to 5pm"));
        assert_eq!(window(&clock, "yesterday afternoon"), pair("2026-11-18T12:00:00-08:00", "2026-11-18T17:00:00-08:00"));
        assert_eq!(window(&clock, "this morning"), pair("2026-11-19T05:00:00-08:00", "2026-11-19T12:00:00-08:00"));
        assert_eq!(window(&clock, "tonight"), pair("2026-11-19T17:00:00-08:00", "2026-11-20T05:00:00-08:00"));
        assert_eq!(window(&clock, "last night"), pair("2026-11-18T17:00:00-08:00", "2026-11-19T05:00:00-08:00"));
        assert_eq!(window(&clock, "Tuesday evening"), pair("2026-11-17T17:00:00-08:00", "2026-11-17T21:00:00-08:00"));
        assert_eq!(window(&clock, "Monday to Wednesday"), pair("2026-11-16T00:00:00-08:00", "2026-11-19T00:00:00-08:00"));
        assert_eq!(window(&clock, "Tuesday 10pm to 2am"), pair("2026-11-17T22:00:00-08:00", "2026-11-18T02:00:00-08:00"));
        assert_eq!(window(&clock, "Wednesday 9:15 to Thursday 8am"), pair("2026-11-18T09:15:00-08:00", "2026-11-19T08:00:00-08:00"));
        assert_eq!(window(&clock, "12am to noon"), pair("2026-11-19T00:00:00-08:00", "2026-11-19T12:00:00-08:00"));
        assert_eq!(window(&clock, "2026-11-11 12:00 to 2026-11-11 17:00"), pair("2026-11-11T12:00:00-08:00", "2026-11-11T17:00:00-08:00"));
        assert_eq!(window(&clock, "2026-11-11"), pair("2026-11-11T00:00:00-08:00", "2026-11-12T00:00:00-08:00"));
        assert_eq!(window(&clock, "Nov 11 3pm to 4pm"), pair("2026-11-11T15:00:00-08:00", "2026-11-11T16:00:00-08:00"));
        assert_eq!(window(&clock, "11 November 2025").0, "2025-11-11T00:00:00-08:00");
        // Without a year, a date ahead of today is last year's.
        assert_eq!(window(&clock, "December 2").0, "2025-12-02T00:00:00-08:00");
    }

    #[test]
    fn spans_weeks_and_days_ago() {
        let clock = thursday();
        assert_eq!(window(&clock, "this week"), pair("2026-11-16T00:00:00-08:00", "2026-11-20T00:00:00-08:00"));
        assert_eq!(window(&clock, "last week"), pair("2026-11-09T00:00:00-08:00", "2026-11-16T00:00:00-08:00"));
        assert_eq!(window(&clock, "3 days ago"), pair("2026-11-16T00:00:00-08:00", "2026-11-17T00:00:00-08:00"));
        assert_eq!(window(&clock, "2 weeks ago"), pair("2026-11-02T00:00:00-08:00", "2026-11-09T00:00:00-08:00"));
        assert_eq!(window(&clock, "last month"), pair("2026-10-01T00:00:00-07:00", "2026-11-01T00:00:00-07:00"));
        assert_eq!(window(&clock, "last 3 days"), pair("2026-11-16T10:00:00-08:00", "2026-11-19T10:00:00-08:00"));
        assert_eq!(window(&clock, "past 2 hours"), pair("2026-11-19T08:00:00-08:00", "2026-11-19T10:00:00-08:00"));
        assert_eq!(window(&clock, "2h"), pair("2026-11-19T08:00:00-08:00", "2026-11-19T10:00:00-08:00"));
        assert_eq!(window(&clock, "yesterday"), pair("2026-11-18T00:00:00-08:00", "2026-11-19T00:00:00-08:00"));
    }

    #[test]
    fn start_and_end_take_words_too() {
        let clock = thursday();
        assert_eq!(clock.rfc3339(clock.parse("Wednesday", Bound::Start).unwrap()), "2026-11-18T00:00:00-08:00");
        assert_eq!(clock.rfc3339(clock.parse("Wednesday", Bound::End).unwrap()), "2026-11-19T00:00:00-08:00");
        assert_eq!(clock.rfc3339(clock.parse("Wednesday 5pm", Bound::End).unwrap()), "2026-11-18T17:00:00-08:00");
        assert_eq!(clock.rfc3339(clock.parse("noon", Bound::Start).unwrap()), "2026-11-19T12:00:00-08:00");
        // What was read before reads the same.
        assert_eq!(clock.parse("3d", Bound::Start).unwrap(), utc("2026-11-16T18:00:00Z"));
    }

    #[test]
    fn windows_in_words_and_mistakes_explained() {
        let clock = thursday();
        let words = |text: &str| {
            let w = clock.window(text).unwrap();
            clock.window_words(Some(w.start), Some(w.end))
        };
        assert_eq!(words("Wednesday 12pm to 5pm"), "Wednesday, November 18, 2026, 12:00 to 17:00");
        assert_eq!(words("Wednesday"), "Wednesday, November 18, 2026 (the whole day)");
        assert_eq!(words("last week"), "Monday, November 9, 2026 to Sunday, November 15, 2026 (whole days)");
        assert_eq!(words("tonight"), "Thursday, November 19, 2026, 17:00 to Friday, November 20, 2026, 05:00");
        assert_eq!(clock.window_words(None, None), "the whole library");
        for bad in ["", "whenever", "Wednesday Thursday", "3pm 4pm", "Wednesday to Monday", "13pm", "last fortnight", "Feb 30"] {
            let error = clock.window(bad).unwrap_err().to_string();
            assert!(!error.is_empty() && !error.contains('\u{2014}'), "{bad}: {error}");
        }
        assert!(clock.window("whenever").unwrap_err().to_string().contains("Wednesday 12pm to 5pm"));
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
