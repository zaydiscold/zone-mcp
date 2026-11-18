//! The numbers the page shows: how much was said, when, and of which kind.
//!
//! SQLite does the counting, grouped by quarter hour and kind (see [`Db::stat_buckets`]). The
//! quarter hours are then sorted into the local days and hours they fall in, which is exact in
//! every time zone and across a change of clocks, and takes a moment even for a large library.

use std::collections::BTreeMap;

use anyhow::Result;
use jiff::{Span, civil::Date};
use serde_json::{Map, Value, json};

use crate::db::{Db, Filter};
use crate::timeparse::Clock;

/// How many days the per-day chart covers, today included.
pub const CHART_DAYS: i64 = 30;

#[derive(Clone, Copy, Default)]
struct Sum {
    items: i64,
    words: i64,
    seconds: f64,
}

impl Sum {
    fn add(&mut self, items: i64, words: i64, seconds: f64) {
        self.items += items;
        self.words += words;
        self.seconds += seconds;
    }

    fn json(&self) -> Value {
        json!({ "items": self.items, "words": self.words, "seconds": self.seconds.round() })
    }
}

#[derive(Default)]
struct Day {
    total: Sum,
    kinds: BTreeMap<String, Sum>,
}

/// Everything the page's statistics show for the items a filter selects.
pub fn summary(db: &Db, clock: &Clock, filter: &Filter) -> Result<Value> {
    let mut days: BTreeMap<String, Day> = BTreeMap::new();
    let mut kinds: BTreeMap<String, Sum> = BTreeMap::new();
    let mut hours = [0i64; 24];
    // Monday first.
    let mut weekdays = [0i64; 7];
    let mut all = Sum::default();
    for (quarter, kind, items, words, seconds) in db.stat_buckets(filter)? {
        let at = clock.zoned(quarter as f64 * 900.0);
        let day = days.entry(at.strftime("%Y-%m-%d").to_string()).or_default();
        day.total.add(items, words, seconds);
        day.kinds.entry(kind.clone()).or_default().add(items, words, seconds);
        kinds.entry(kind).or_default().add(items, words, seconds);
        hours[at.hour() as usize % 24] += items;
        weekdays[at.weekday().to_monday_zero_offset() as usize % 7] += items;
        all.add(items, words, seconds);
    }

    let today: Date = clock.now_zoned().date();
    let back = |n: i64| today.checked_sub(Span::new().days(n)).map(|d| d.to_string()).unwrap_or_default();
    let mut chart = Vec::new();
    let (mut week, mut month) = (Sum::default(), Sum::default());
    let mut month_kinds: BTreeMap<String, Sum> = BTreeMap::new();
    for n in (0..CHART_DAYS).rev() {
        let name = back(n);
        let day = days.get(&name);
        let total = day.map(|d| d.total).unwrap_or_default();
        month.add(total.items, total.words, total.seconds);
        if n < 7 {
            week.add(total.items, total.words, total.seconds);
        }
        let mut by_kind = Map::new();
        for (kind, sum) in day.map(|d| &d.kinds).into_iter().flatten() {
            by_kind.insert(kind.clone(), json!(sum.items));
            month_kinds.entry(kind.clone()).or_default().add(sum.items, sum.words, sum.seconds);
        }
        chart.push(json!({ "day": name, "items": total.items, "words": total.words, "seconds": total.seconds.round(), "kinds": by_kind }));
    }

    // Days in a row with something said, ending today (or yesterday, when today is still empty).
    let mut streak = 0;
    let mut n = if days.contains_key(&back(0)) { 0 } else { 1 };
    while days.contains_key(&back(n)) {
        streak += 1;
        n += 1;
    }

    let busiest = |counts: &[i64]| counts.iter().enumerate().filter(|(_, n)| **n > 0).max_by_key(|(i, n)| (**n, std::cmp::Reverse(*i))).map(|(i, _)| i);
    let best_day = days.iter().max_by_key(|(name, day)| (day.total.items, std::cmp::Reverse((*name).clone()))).map(|(name, day)| json!({ "day": name, "items": day.total.items, "words": day.total.words }));
    let kind_map = |sums: &BTreeMap<String, Sum>| Value::Object(sums.iter().map(|(kind, sum)| (kind.clone(), sum.json())).collect());
    let day_counts: Map<String, Value> = days.iter().map(|(name, day)| (name.clone(), json!(day.total.items))).collect();

    Ok(json!({
        "today": days.get(&back(0)).map(|d| d.total).unwrap_or_default().json(),
        "week": week.json(),
        "month": month.json(),
        "all": all.json(),
        "by_kind": kind_map(&kinds),
        "by_kind_month": kind_map(&month_kinds),
        "days": chart,
        "hours": hours,
        "busiest_hour": busiest(&hours),
        "weekdays": weekdays,
        "busiest_weekday": busiest(&weekdays),
        "best_day": best_day,
        "streak_days": streak,
        "active_days": days.len(),
        "first_day": days.keys().next(),
        "day_counts": day_counts,
        "timezone": clock.zone_name(),
    }))
}

/// Items per local day for the items a filter selects, for the calendar's dots.
pub fn day_counts(db: &Db, clock: &Clock, filter: &Filter) -> Result<Map<String, Value>> {
    let mut days: BTreeMap<String, i64> = BTreeMap::new();
    for (quarter, _, items, _, _) in db.stat_buckets(filter)? {
        *days.entry(clock.day(quarter as f64 * 900.0)).or_default() += items;
    }
    Ok(days.into_iter().map(|(day, count)| (day, json!(count))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Delivery;
    use crate::item::{Carrier, Item};

    fn item(id: usize, kind: &str, created_at: f64, text: &str, seconds: Option<f64>) -> Item {
        Item::from_json(&json!({
            "id": format!("57A70000-0000-4000-8000-{id:012X}"), "kind": kind, "source": if kind == "dictation" { json!("keyboard") } else { Value::Null },
            "created_at": created_at, "updated_at": created_at, "text": text, "duration_s": seconds,
        }))
        .unwrap()
    }

    #[test]
    fn days_hours_and_kinds_are_local() {
        let mut db = Db::open_in_memory().unwrap();
        let mut clock = Clock::in_zone(Some("America/Los_Angeles")).unwrap();
        // Tuesday 2026-11-17 18:40 in Los Angeles.
        let now = 1_794_969_600.0;
        clock.now = crate::timeparse::timestamp(now);
        assert_eq!(clock.day(now), "2026-11-17");
        let items = vec![
            item(1, "dictation", now - 600.0, "one two three", Some(4.0)),
            item(2, "note", now - 3600.0, "four five", None),
            // 23:50 the evening before: yesterday here, though already today in UTC.
            item(3, "dictation", now - 18.0 * 3600.0 - 50.0 * 60.0, "six", Some(2.0)),
            item(4, "meeting", now - 3.0 * 86400.0, "Ada: seven eight", Some(600.0)),
            // Before the clocks went back on November 1: still its own local day.
            item(5, "dictation", now - 20.0 * 86400.0, "nine", Some(1.0)),
            item(6, "dictation", now - 40.0 * 86400.0, "ten eleven", Some(1.0)),
        ];
        db.apply_items(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: now }).unwrap();
        let stats = summary(&db, &clock, &Filter::default()).unwrap();
        assert_eq!(stats["today"], json!({ "items": 2, "words": 5, "seconds": 4.0 }));
        assert_eq!(stats["week"]["items"], 4);
        assert_eq!(stats["month"]["items"], 5);
        assert_eq!(stats["all"], json!({ "items": 6, "words": 12, "seconds": 608.0 }));
        assert_eq!(stats["by_kind"]["dictation"]["items"], 4);
        assert_eq!(stats["by_kind_month"]["dictation"]["items"], 3);
        let days = stats["days"].as_array().unwrap();
        assert_eq!(days.len(), CHART_DAYS as usize);
        assert_eq!((days[29]["day"].as_str(), days[29]["items"].as_i64()), (Some("2026-11-17"), Some(2)));
        assert_eq!((days[28]["day"].as_str(), days[28]["kinds"]["dictation"].as_i64()), (Some("2026-11-16"), Some(1)));
        assert_eq!(days[9]["day"], "2026-10-28");
        assert_eq!(days[9]["items"], 1);
        assert_eq!(stats["day_counts"]["2026-10-08"], 1);
        // 18:30 (the dictation), 17:40 (the note), 23:50, and so on: the busiest hour is one
        // with the most items, the earliest when several tie.
        assert_eq!(stats["hours"][23], 1);
        assert_eq!(stats["hours"][17], 1);
        assert_eq!(stats["streak_days"], 2);
        assert_eq!(stats["active_days"], 5);
        assert_eq!(stats["first_day"], "2026-10-08");
        // A filter narrows every number.
        let notes = summary(&db, &clock, &Filter { kinds: vec!["note".into()], ..Default::default() }).unwrap();
        assert_eq!((notes["all"]["items"].as_i64(), notes["today"]["items"].as_i64()), (Some(1), Some(1)));
        assert_eq!(day_counts(&db, &clock, &Filter::default()).unwrap().len(), 5);
    }

    #[test]
    fn a_large_library_is_counted_quickly() {
        let mut db = Db::open_in_memory().unwrap();
        let now = 1_794_969_600.0;
        // 100,000 items over about fourteen months.
        for chunk in 0..200 {
            let items: Vec<Item> = (0..500).map(|i| item(chunk * 500 + i, if i % 9 == 0 { "note" } else { "dictation" }, now - (chunk * 500 + i) as f64 * 371.0, "amber basalt cedar delta", Some(3.0))).collect();
            db.apply_items(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: now }).unwrap();
        }
        let mut clock = Clock::in_zone(Some("Europe/London")).unwrap();
        clock.now = crate::timeparse::timestamp(now);
        let started = std::time::Instant::now();
        let stats = summary(&db, &clock, &Filter { fold_note_recordings: true, ..Default::default() }).unwrap();
        let took = started.elapsed();
        assert_eq!(stats["all"]["items"], 100_000);
        assert_eq!(stats["all"]["words"], 400_000);
        // Generous: an unoptimized test build on a slow shared machine.
        assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    }
}
