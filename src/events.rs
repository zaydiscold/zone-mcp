//! The feed: one line for each thing that happened, for `zone events` and for apps that show
//! what zone is doing (a menu bar app, a status line).
//!
//! Events are rows of the `events` table in the library, so they survive a restart, and any
//! `zone` program can add one: the daemon (sessions, flashes, the relay, the backup folder)
//! and the commands a person runs (`zone pair`, `zone unpair`, `zone import`). The table keeps
//! the newest two thousand.
//!
//! An event holds names and counts only: which phone, how many items, which path, how long a
//! clip was. It never holds an item's text, a pairing secret or the web page's key.
//!
//! Every line has `seq` (a number that only goes up and is never used twice), `at` (Unix
//! seconds), `time` (the same moment as local RFC 3339) and `event` (the kind). The rest
//! depends on the kind; README.md lists them all.

use std::time::Duration;

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::db::{Db, EventRow};
use crate::paths;
use crate::timeparse::{Bound, Clock, human_duration};

/// How often `--follow` looks for new events.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// Adds an event to the feed. `fields` is a JSON object of names and counts. The feed is a
/// courtesy: when it cannot be written, the work it describes still stands, so a failure is
/// logged and nothing more.
pub fn record(db: &Db, event: &str, fields: Value) {
    let data = if fields.is_object() { fields.to_string() } else { "{}".to_string() };
    if let Err(error) = db.add_event(paths::now_unix(), event, &data) {
        crate::warn!("events: could not record {event}: {error:#}");
    }
}

/// One event as the JSON object `zone events --json` prints.
pub fn to_json(row: &EventRow, clock: &Clock) -> Value {
    let mut out = Map::new();
    out.insert("seq".into(), json!(row.seq));
    out.insert("at".into(), json!((row.at * 1000.0).round() / 1000.0));
    out.insert("time".into(), json!(clock.rfc3339(row.at)));
    out.insert("event".into(), json!(row.event));
    if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&row.data) {
        for (key, value) in fields {
            // The four fields above are the feed's own and cannot be overwritten.
            out.entry(key).or_insert(value);
        }
    }
    Value::Object(out)
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a batch of items did to the library, in words: `3 new, 1 changed, 2 deleted`.
fn counts(event: &Value) -> String {
    let n = |key: &str| event[key].as_u64().unwrap_or(0);
    let mut parts = Vec::new();
    if n("new") > 0 {
        parts.push(format!("{} new", n("new")));
    }
    if n("changed") > 0 {
        parts.push(format!("{} changed", n("changed")));
    }
    if n("deleted") > 0 {
        parts.push(format!("{} deleted", n("deleted")));
    }
    if parts.is_empty() {
        return match n("items") {
            0 => "nothing to send".to_string(),
            items => format!("{}, all already here", plural(items, "item", "items")),
        };
    }
    parts.join(", ")
}

/// One event as a sentence for a person.
pub fn describe(event: &Value) -> String {
    let text = |key: &str| event[key].as_str().unwrap_or("");
    let device = match event["device"].as_str() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "A phone".to_string(),
    };
    match text("event") {
        "daemon_started" => format!("Zone {} started", text("version")),
        "daemon_stopped" => format!("Zone stopped after {}", human_duration(event["uptime_s"].as_f64().unwrap_or(0.0))),
        "relay_connected" => format!("Reachable through the relay {}", text("relay_url")),
        "relay_lost" => "Lost the relay: phones on other networks cannot reach this computer".to_string(),
        "pair_offered" => "A pairing code was shown".to_string(),
        "paired" => format!("{device} paired by {}", if text("via") == "flash" { "Flash" } else { "Link" }),
        "unpaired" => format!("{device} was unpaired"),
        "link_session" => {
            let seconds = event["duration_s"].as_f64().unwrap_or(0.0);
            let mut line = format!("{device} synced over Link: {} ({}, {seconds:.1} s)", counts(event), text("path"));
            match text("end") {
                "idle" => line.push_str(", then went quiet"),
                "error" => line.push_str(", ended by an error"),
                _ => {}
            }
            line
        }
        "link_refused" => {
            let why = match text("reason") {
                "not_paired" => "it is not paired with this computer",
                "bad_secret" => "its pairing code is wrong, used or expired",
                "version" => "it speaks a newer version of the protocol",
                "busy" => "too many phones were syncing at once",
                other => other,
            };
            format!("{device} was refused: {why}")
        }
        "flash_received" => format!(
            "{device} sent a {}Flash: {}{}{}",
            if event["color"] == true { "Color " } else { "" },
            counts(event),
            if event["paired"] == true { ", and is now paired" } else { "" },
            // Flashes recorded before the check say nothing about it.
            match (event["checked"].as_bool(), event["check_problem"].as_str()) {
                (Some(true), _) => format!(". {} checked", crate::flash::check::thousands(event["items"].as_u64().unwrap_or(0) as usize)),
                (Some(false), Some(problem)) => format!(". The check found a problem: {problem}"),
                _ => String::new(),
            }
        ),
        "flash_failed" => format!("A Flash was not stored: {}", text("reason")),
        "backup_import" => format!("Backup folder read: {}", counts(event)),
        "clip" => {
            let length = event["length"].as_u64().unwrap_or(0);
            let from = match text("source") {
                "clipboard" => " from its clipboard",
                "share" => " shared from an app",
                "shortcut" => " from a shortcut",
                "typed" => " typed in Zone",
                _ => "",
            };
            let then = match (event["copied"] == true, text("not_copied")) {
                (true, _) => ", now on this computer's clipboard",
                (false, "failed") => ", kept (the clipboard could not be written)",
                (false, "old") => ", kept (sent a while ago, so the clipboard was left alone)",
                _ => ", kept",
            };
            format!("{device} sent a clip: {}{from}{then}", plural(length, "character", "characters"))
        }
        "agents_switched" => {
            let by = match text("by") {
                "page" => " on the page",
                "mac" => " in Zone for Mac",
                "cli" => " with zone agents",
                _ => "",
            };
            if event["on"] == true {
                format!("AI agents were let back in{by}")
            } else {
                format!("AI agents were turned off{by}: every zone server now says access is off")
            }
        }
        "resync_requested" => match event["device"].as_str() {
            Some(name) => format!("{name} was asked to send everything again"),
            None => "Every phone was asked to send everything again".to_string(),
        },
        other => format!("{other} {}", {
            let mut rest = event.clone();
            if let Some(map) = rest.as_object_mut() {
                for key in ["seq", "at", "time", "event"] {
                    map.remove(key);
                }
            }
            rest
        }),
    }
}

pub struct Options {
    pub json: bool,
    pub follow: bool,
    /// At most this many of the newest matching events.
    pub limit: usize,
    /// Only events at or after this time.
    pub since: Option<String>,
    /// Only events numbered above this one.
    pub after: Option<i64>,
}

fn print(row: &EventRow, clock: &Clock, as_json: bool) {
    let value = to_json(row, clock);
    if as_json {
        println!("{value}");
    } else {
        println!("{}  {}", clock.zoned(row.at).strftime("%a %Y-%m-%d %H:%M:%S"), describe(&value));
    }
}

/// True when whoever was reading this program's output is gone. Only pipes and sockets can
/// say so: a terminal or a file never does.
#[cfg(unix)]
fn reader_gone() -> bool {
    // Asking whether writing is possible is what makes macOS say that the reader hung up
    // (Linux says so whatever is asked).
    let mut fd = libc::pollfd { fd: 1, events: libc::POLLOUT, revents: 0 };
    // SAFETY: one valid pollfd, a count of one, and no waiting.
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    ready > 0 && fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
}

#[cfg(not(unix))]
fn reader_gone() -> bool {
    // Found out at the next line written instead.
    false
}

/// `zone events`
pub fn run(options: &Options) -> Result<()> {
    let db = Db::open_default()?;
    let clock = Clock::system();
    let since = options.since.as_deref().map(|text| clock.parse(text, Bound::Start)).transpose()?;
    // Read before the backlog, so nothing that arrives in between is printed twice or missed.
    let top = db.last_event_seq()?;
    let backlog = if options.limit == 0 { Vec::new() } else { db.events(options.after, since, options.limit)? };
    for row in &backlog {
        print(row, &clock, options.json);
    }
    if !options.follow {
        if backlog.is_empty() && !options.json {
            println!("{}", if top == 0 { "Nothing has happened yet." } else { "No events match." });
        }
        return Ok(());
    }
    let mut last = top.max(backlog.last().map(|row| row.seq).unwrap_or(0)).max(options.after.unwrap_or(0));
    loop {
        std::thread::sleep(FOLLOW_POLL);
        if crate::out::stdout_closed() || reader_gone() {
            return Ok(());
        }
        for row in db.events(Some(last), None, usize::MAX)? {
            last = row.seq;
            print(&row, &clock, options.json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::EVENTS_KEPT;

    #[test]
    fn events_are_numbered_kept_and_capped() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.last_event_seq().unwrap(), 0);
        assert!(db.events(None, None, 10).unwrap().is_empty());
        for n in 0..5 {
            db.add_event(1000.0 + n as f64, "link_session", &json!({ "device": "Test iPhone", "new": n }).to_string()).unwrap();
        }
        let all = db.events(None, None, 100).unwrap();
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        // The newest two, oldest first.
        assert_eq!(db.events(None, None, 2).unwrap().iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
        // After a number, and from a time.
        assert_eq!(db.events(Some(3), None, 100).unwrap().iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(db.events(None, Some(1003.0), 100).unwrap().iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(db.events(Some(4), Some(1001.0), 100).unwrap().len(), 1);

        // The oldest are dropped past the cap, and numbers are never used twice.
        for _ in 0..EVENTS_KEPT + 20 {
            db.add_event(2000.0, "relay_lost", "{}").unwrap();
        }
        let kept = db.events(None, None, usize::MAX).unwrap();
        assert_eq!(kept.len() as i64, EVENTS_KEPT);
        assert_eq!(kept.last().unwrap().seq, 5 + EVENTS_KEPT + 20);
        assert_eq!(kept[0].seq, 5 + 20 + 1);
        assert_eq!(db.last_event_seq().unwrap(), 5 + EVENTS_KEPT + 20);
    }

    #[test]
    fn lines_have_the_four_fields_first_and_read_well() {
        let clock = Clock::in_zone(Some("America/Los_Angeles")).unwrap();
        let row = EventRow {
            seq: 7,
            at: 1794772842.9004,
            event: "link_session".to_string(),
            data: json!({ "device": "Test iPhone", "items": 5, "new": 3, "changed": 1, "deleted": 0, "unchanged": 1, "path": "direct", "duration_s": 0.4, "end": "bye", "seq": 99 })
                .to_string(),
        };
        let value = to_json(&row, &clock);
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(&keys[..5], ["seq", "at", "time", "event", "device"]);
        // A field cannot stand in for the feed's own.
        assert_eq!((value["seq"].as_i64(), value["at"].as_f64()), (Some(7), Some(1794772842.9)));
        assert_eq!(value["time"], clock.rfc3339(row.at));
        assert_eq!(value["time"].as_str().unwrap().len(), "2026-11-15T12:00:42-08:00".len());
        assert_eq!(describe(&value), "Test iPhone synced over Link: 3 new, 1 changed (direct, 0.4 s)");

        let say = |event: &str, fields: Value| describe(&to_json(&EventRow { seq: 1, at: 0.0, event: event.to_string(), data: fields.to_string() }, &clock));
        assert_eq!(say("link_session", json!({ "device": "P", "items": 0, "path": "relay", "duration_s": 1.0, "end": "bye" })), "P synced over Link: nothing to send (relay, 1.0 s)");
        assert_eq!(say("link_session", json!({ "items": 4, "path": "relay", "duration_s": 61.0, "end": "idle" })), "A phone synced over Link: 4 items, all already here (relay, 61.0 s), then went quiet");
        assert_eq!(say("flash_received", json!({ "device": "P", "items": 9, "new": 9, "color": true, "paired": true })), "P sent a Color Flash: 9 new, and is now paired");
        assert_eq!(
            say("flash_received", json!({ "device": "P", "items": 1764, "new": 3, "unchanged": 1761, "checked": true })),
            format!("P sent a Flash: {}. 1,764 checked", counts(&json!({ "new": 3, "unchanged": 1761 })))
        );
        assert!(
            say("flash_received", json!({ "device": "P", "items": 2, "new": 2, "checked": false, "check_problem": "1 item was not read back from the library as it arrived." }))
                .ends_with(". The check found a problem: 1 item was not read back from the library as it arrived.")
        );
        assert_eq!(say("paired", json!({ "device": "P", "via": "flash" })), "P paired by Flash");
        assert_eq!(say("clip", json!({ "device": "P", "source": "clipboard", "length": 42, "copied": true })), "P sent a clip: 42 characters from its clipboard, now on this computer's clipboard");
        assert_eq!(say("clip", json!({ "device": "P", "source": "typed", "length": 1, "copied": false, "not_copied": "off" })), "P sent a clip: 1 character typed in Zone, kept");
        assert_eq!(say("clip", json!({ "source": "share", "length": 9, "copied": false, "not_copied": "old" })), "A phone sent a clip: 9 characters shared from an app, kept (sent a while ago, so the clipboard was left alone)");
        assert_eq!(say("link_refused", json!({ "device": null, "reason": "not_paired" })), "A phone was refused: it is not paired with this computer");
        assert_eq!(say("daemon_stopped", json!({ "uptime_s": 125.0 })), "Zone stopped after 2m 05s");
        assert_eq!(say("resync_requested", json!({ "device": null, "phones": 2 })), "Every phone was asked to send everything again");
        // A kind this version does not know still prints.
        assert_eq!(say("something_new", json!({ "n": 1 })), "something_new {\"n\":1}");
        // Data that is not an object adds nothing.
        let odd = to_json(&EventRow { seq: 2, at: 0.0, event: "relay_lost".to_string(), data: "[1]".to_string() }, &clock);
        assert_eq!(odd.as_object().unwrap().len(), 4);
    }
}
