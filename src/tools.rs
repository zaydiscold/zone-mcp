//! The tools agents get, as plain functions from JSON arguments to a JSON answer.
//!
//! `mcp.rs` puts these on the wire. Keeping them free of protocol types means the same code
//! is tested directly and could back another surface later.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::daemon_info::DaemonInfo;
use crate::db::{Cursor, Db, Filter, Order, Row};
use crate::item::preview;
use crate::paths;
use crate::timeparse::{Bound, Clock, format_offset, unix};

pub const DEFAULT_LIST_LIMIT: usize = 50;
pub const MAX_LIST_LIMIT: usize = 200;
pub const DEFAULT_SEARCH_LIMIT: usize = 20;
pub const MAX_SEARCH_LIMIT: usize = 100;
pub const DEFAULT_MAX_CHARS: usize = 20_000;
pub const MAX_MAX_CHARS: usize = 200_000;
pub const MAX_GET_IDS: usize = 50;
pub const MAX_MARK_IDS: usize = 500;
const PREVIEW_CHARS: usize = 200;

/// What every agent is told once, when it connects.
pub const INSTRUCTIONS: &str = "\
Zone is this person's dictation library: everything they spoke into the Zone app on their iPhone, \
copied to this computer. Every item is their own spoken words turned into text, so expect speech \
recognition mistakes and informal phrasing.

There are three kinds of item. Filter on `kinds` to pick one.
- `dictation`: something they dictated. Most dictations have source `keyboard`: those are messages \
and text they typed by voice into other apps (chats, emails, search boxes, prompts), so they are \
usually short and addressed to someone else. Source `app` was dictated inside the Zone app, and \
source `note` is the raw dictation behind a voice note.
- `note`: a voice note they chose to save in Zone. These are notes to self.
- `meeting`: a recorded meeting with a transcript, one line per speaker turn, with a title.

How to work with it:
- Call `sync_status` first when a request mentions a day or time (\"Wednesday afternoon\", \
\"yesterday\"). It returns the current local date, weekday and time zone, and says how fresh the \
library is.
- `list_items` returns short cards for a time range, newest or oldest first, with a total count and \
a cursor for the next page. Times without an offset are read in the computer's time zone unless \
you pass `timezone`.
- `search_items` finds words. `get_items` returns full text for ids you picked from cards.
- `mark_processed` only sets a flag so you can work through items once (`unprocessed_only` skips \
flagged items). Nothing here can edit or delete the person's words.

This is private speech. Quote it only as far as the task needs.";

/// One tool, described for agents.
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only: bool,
}

const TIME_HELP: &str = "Accepts RFC 3339 with an offset (2026-11-11T12:00:00-08:00), a local date (2026-11-11), \
a local date and time (\"2026-11-11 12:00\"), a relative age (2h, 3d, 1w), or today / yesterday.";

fn time_property(which: &str) -> Value {
    let meaning = match which {
        "start" => "Only items created at or after this time. A bare date means the start of that day.",
        _ => "Only items created before this time. A bare date means through the end of that day.",
    };
    json!({ "type": "string", "description": format!("{meaning} {TIME_HELP}") })
}

fn common_filters() -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert("start".into(), time_property("start"));
    properties.insert("end".into(), time_property("end"));
    properties.insert(
        "kinds".into(),
        json!({
            "type": "array",
            "items": { "type": "string", "enum": ["dictation", "note", "meeting"] },
            "description": "Which kinds to include. Leave out for all three. dictation = text typed by voice (mostly messages in other apps), note = saved voice notes, meeting = meeting transcripts."
        }),
    );
    properties.insert(
        "sources".into(),
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Only dictations spoken in these places: keyboard (typed by voice in another app), app (inside Zone), note (the dictation behind a voice note). Notes and meetings have no source, so this filter leaves them out."
        }),
    );
    properties.insert(
        "timezone".into(),
        json!({
            "type": "string",
            "description": "IANA time zone for reading times without an offset and for showing times, for example America/Los_Angeles. Defaults to the computer's time zone."
        }),
    );
    properties
}

/// The tools on offer. `read_only` leaves out the one tool that writes.
pub fn definitions(read_only: bool) -> Vec<ToolDef> {
    let mut list_properties = common_filters();
    list_properties.insert(
        "order".into(),
        json!({ "type": "string", "enum": ["newest", "oldest"], "default": "newest", "description": "newest first (default) or oldest first." }),
    );
    list_properties.insert(
        "limit".into(),
        json!({ "type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT, "default": DEFAULT_LIST_LIMIT, "description": "How many cards to return." }),
    );
    list_properties.insert(
        "cursor".into(),
        json!({ "type": "string", "description": "next_cursor from the previous call, to continue where it stopped. Keep the other arguments the same." }),
    );
    list_properties.insert(
        "unprocessed_only".into(),
        json!({ "type": "boolean", "default": false, "description": "Skip items already flagged with mark_processed." }),
    );

    let mut search_properties = Map::new();
    search_properties.insert(
        "query".into(),
        json!({
            "type": "string",
            "description": "Words to find. Every word must appear. Use \"double quotes\" for an exact phrase, OR between alternatives, and a trailing * for the beginning of a word (transcri*). Case and accents do not matter."
        }),
    );
    search_properties.extend(common_filters());
    search_properties.insert(
        "limit".into(),
        json!({ "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "default": DEFAULT_SEARCH_LIMIT, "description": "How many matches to return, best match first." }),
    );

    let mut stats_properties = common_filters();
    stats_properties.insert(
        "group_by".into(),
        json!({ "type": "string", "enum": ["day", "kind", "source"], "default": "day", "description": "day (local calendar days), kind, or source." }),
    );

    let mut tools = vec![
        ToolDef {
            name: "list_items",
            title: "List dictations, notes and meetings",
            description: "List the person's spoken items in a time range as short cards (id, kind, source, local time with weekday and UTC offset, duration, word count, first 200 characters, processed flag), with the total number that match and a cursor for the next page. Use it for requests like \"everything from Wednesday 12pm to 5pm\" (start \"2026-11-11 12:00\", end \"2026-11-11 17:00\") or to work through the library newest to oldest. Kinds: dictation items with source keyboard are messages and text the person typed by voice in other apps; note items are voice notes they saved; meeting items are meeting transcripts. Cards are previews: call get_items for full text.",
            input_schema: json!({ "type": "object", "properties": list_properties, "additionalProperties": false }),
            read_only: true,
        },
        ToolDef {
            name: "search_items",
            title: "Search the spoken library",
            description: "Full-text search over everything the person dictated: dictations (mostly messages typed by voice in other apps), voice notes, and meeting transcripts and titles. Returns the best matches with a snippet around the matching words (matches are wrapped in [brackets]), plus the total number of matches and a truncated flag when there are more than were returned. Narrow with start, end, kinds or sources.",
            input_schema: json!({ "type": "object", "properties": search_properties, "required": ["query"], "additionalProperties": false }),
            read_only: true,
        },
        ToolDef {
            name: "get_items",
            title: "Read full items",
            description: "Return the full text of items by id (ids come from list_items or search_items). Long texts such as meeting transcripts are cut at max_chars and marked truncated, with next_offset to continue reading. For a meeting, text is the transcript with one \"Speaker: words\" line per turn.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_GET_IDS, "description": "Item ids." },
                    "max_chars": { "type": "integer", "minimum": 1, "maximum": MAX_MAX_CHARS, "default": DEFAULT_MAX_CHARS, "description": "Most characters of text to return per item." },
                    "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Character position to start from, for continuing a truncated item." },
                    "timezone": { "type": "string", "description": "IANA time zone for showing times. Defaults to the computer's time zone." }
                },
                "required": ["ids"],
                "additionalProperties": false
            }),
            read_only: true,
        },
        ToolDef {
            name: "library_stats",
            title: "Count items by day, kind or source",
            description: "Counts, word totals and recorded seconds for the library or a time range, grouped by local calendar day, by kind (dictation, note, meeting) or by source (keyboard, app, note). Use it to see how much there is before listing, or to find the busy days.",
            input_schema: json!({ "type": "object", "properties": stats_properties, "additionalProperties": false }),
            read_only: true,
        },
        ToolDef {
            name: "sync_status",
            title: "Current time and sync state",
            description: "The computer's current local date, weekday, time and time zone (use it to resolve words like \"Wednesday\" or \"yesterday\" into dates), which phones are paired, when each carrier last delivered items, and how many items of each kind the library holds. Call it first when a request depends on what day it is or on how fresh the library is.",
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            read_only: true,
        },
    ];
    if !read_only {
        tools.push(ToolDef {
            name: "mark_processed",
            title: "Flag items as processed",
            description: "Set or clear the processed flag on items, so a later list_items call with unprocessed_only skips them. This is the only tool that writes, and it writes only this flag: it never changes or deletes an item's text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_MARK_IDS, "description": "Item ids." },
                    "processed": { "type": "boolean", "default": true, "description": "true to flag, false to clear the flag." }
                },
                "required": ["ids"],
                "additionalProperties": false
            }),
            read_only: false,
        });
    }
    tools
}

/// Runs a tool. Errors are sentences an agent can act on.
pub fn call(db: &mut Db, name: &str, args: &Map<String, Value>, read_only: bool) -> Result<Value> {
    match name {
        "list_items" => list_items(db, args),
        "search_items" => search_items(db, args),
        "get_items" => get_items(db, args),
        "library_stats" => library_stats(db, args),
        "sync_status" => sync_status(db),
        "mark_processed" if !read_only => mark_processed(db, args),
        "mark_processed" => bail!("this server was started with --read-only, so mark_processed is not available"),
        other => bail!("unknown tool \"{other}\""),
    }
}

// -------------------------------------------------------------------- arguments

fn opt_string(args: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => bail!("{key} must be a string"),
    }
}

/// A list of strings. A single string, or one with commas, is accepted too.
fn string_list(args: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let split = |s: &str| -> Vec<String> { s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect() };
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(split(s)),
        Some(Value::Array(values)) => {
            let mut out = Vec::new();
            for value in values {
                match value {
                    Value::String(s) => out.extend(split(s)),
                    _ => bail!("{key} must be a list of strings"),
                }
            }
            Ok(out)
        }
        Some(_) => bail!("{key} must be a list of strings"),
    }
}

fn opt_usize(args: &Map<String, Value>, key: &str) -> Result<Option<usize>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0).map(|f| f as u64)) {
            Some(value) => Ok(Some(value as usize)),
            None => bail!("{key} must be a whole number, zero or more"),
        },
        Some(Value::String(s)) => s.trim().parse::<usize>().map(Some).map_err(|_| anyhow!("{key} must be a whole number")),
        Some(_) => bail!("{key} must be a whole number"),
    }
}

fn opt_bool(args: &Map<String, Value>, key: &str) -> Result<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Ok(Some(true)),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Ok(Some(false)),
        Some(_) => bail!("{key} must be true or false"),
    }
}

fn clock(args: &Map<String, Value>) -> Result<Clock> {
    Clock::in_zone(opt_string(args, "timezone")?.as_deref())
}

/// The filter shared by list, search and stats, plus a description of the range it covers.
fn filter(args: &Map<String, Value>, clock: &Clock) -> Result<(Filter, Value)> {
    let start = match opt_string(args, "start")? {
        Some(text) => Some(clock.parse(&text, Bound::Start).map_err(|e| anyhow!("start: {e}"))?),
        None => None,
    };
    let end = match opt_string(args, "end")? {
        Some(text) => Some(clock.parse(&text, Bound::End).map_err(|e| anyhow!("end: {e}"))?),
        None => None,
    };
    if let (Some(s), Some(e)) = (start, end)
        && s >= e {
            bail!("start ({}) is not before end ({})", clock.full(s), clock.full(e));
        }
    let kinds = string_list(args, "kinds")?.into_iter().map(|k| normalize_kind(&k)).collect();
    let sources = string_list(args, "sources")?;
    let range = json!({
        "start": start.map(|t| clock.full(t)),
        "end": end.map(|t| clock.full(t)),
        "timezone": clock.zone_name(),
    });
    Ok((Filter { start, end, kinds, sources, unprocessed_only: false }, range))
}

/// Agents say "dictations", "notes", "meetings". The stored kinds are singular.
pub fn normalize_kind(kind: &str) -> String {
    let lower = kind.trim().to_ascii_lowercase();
    match lower.as_str() {
        "dictations" => "dictation".to_string(),
        "notes" => "note".to_string(),
        "meetings" => "meeting".to_string(),
        _ => lower,
    }
}

// -------------------------------------------------------------------- views

/// A short card for one item.
pub fn card(row: &Row, clock: &Clock) -> Value {
    let mut card = Map::new();
    card.insert("id".into(), json!(row.id));
    card.insert("kind".into(), json!(row.kind));
    card.insert("source".into(), json!(row.source));
    card.insert("time".into(), json!(clock.full(row.created_at)));
    if let Some(title) = &row.title {
        card.insert("title".into(), json!(title));
    }
    card.insert("duration_s".into(), json!(row.duration_s.map(round1)));
    card.insert("words".into(), json!(row.words));
    card.insert("preview".into(), json!(preview(&row.text, PREVIEW_CHARS)));
    card.insert("processed".into(), json!(row.processed_at.is_some()));
    Value::Object(card)
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn kind_counts(rows: &[(String, i64)]) -> Value {
    let mut map = Map::new();
    for (kind, count) in rows {
        map.insert(kind.clone(), json!(count));
    }
    Value::Object(map)
}

// -------------------------------------------------------------------- tools

fn list_items(db: &mut Db, args: &Map<String, Value>) -> Result<Value> {
    let clock = clock(args)?;
    let (mut filter, range) = filter(args, &clock)?;
    filter.unprocessed_only = opt_bool(args, "unprocessed_only")?.unwrap_or(false);
    let order = match opt_string(args, "order")? {
        Some(text) => Order::parse(&text)?,
        None => Order::Newest,
    };
    let limit = opt_usize(args, "limit")?.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);
    let cursor = match opt_string(args, "cursor")? {
        Some(text) => Some(Cursor::decode(&text)?),
        None => None,
    };

    let total = db.count(&filter)?;
    let by_kind = db.counts_by_kind_filtered(&filter)?;
    // One extra row tells whether there is a next page.
    let mut rows = db.list(&filter, order, limit + 1, cursor)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more { rows.last().map(|row| Cursor::after(row).encode()) } else { None };
    let items: Vec<Value> = rows.iter().map(|row| card(row, &clock)).collect();

    Ok(json!({
        "range": range,
        "order": if order == Order::Newest { "newest" } else { "oldest" },
        "total": total,
        "total_by_kind": kind_counts(&by_kind),
        "returned": items.len(),
        "next_cursor": next_cursor,
        "items": items,
    }))
}

fn search_items(db: &mut Db, args: &Map<String, Value>) -> Result<Value> {
    let Some(query) = opt_string(args, "query")? else {
        bail!("query is required: the words to look for");
    };
    let clock = clock(args)?;
    let (filter, range) = filter(args, &clock)?;
    let limit = opt_usize(args, "limit")?.unwrap_or(DEFAULT_SEARCH_LIMIT).clamp(1, MAX_SEARCH_LIMIT);
    let result = db.search(&query, &filter, None, limit)?;
    let items: Vec<Value> = result
        .hits
        .iter()
        .map(|hit| {
            let mut entry = Map::new();
            entry.insert("id".into(), json!(hit.row.id));
            entry.insert("kind".into(), json!(hit.row.kind));
            entry.insert("source".into(), json!(hit.row.source));
            entry.insert("time".into(), json!(clock.full(hit.row.created_at)));
            if let Some(title) = &hit.row.title {
                entry.insert("title".into(), json!(title));
            }
            entry.insert("words".into(), json!(hit.row.words));
            entry.insert("snippet".into(), json!(hit.snippet));
            entry.insert("processed".into(), json!(hit.row.processed_at.is_some()));
            Value::Object(entry)
        })
        .collect();
    let truncated = result.total > items.len() as i64;
    let mut out = Map::new();
    out.insert("query".into(), json!(query));
    out.insert("range".into(), range);
    out.insert("total".into(), json!(result.total));
    out.insert("returned".into(), json!(items.len()));
    out.insert("truncated".into(), json!(truncated));
    if truncated {
        out.insert(
            "note".into(),
            json!(format!(
                "Showing the {} best of {} matches. Raise limit (up to {MAX_SEARCH_LIMIT}) or narrow with start, end, kinds or sources.",
                items.len(),
                result.total
            )),
        );
    }
    match result.matched {
        "prefix" => {
            out.insert("matched".into(), json!("No whole-word match, so words were matched by their beginnings."));
        }
        "substring" => {
            out.insert("matched".into(), json!("The query has no searchable words, so it was matched as literal text."));
        }
        _ => {}
    }
    out.insert("items".into(), json!(items));
    Ok(Value::Object(out))
}

fn get_items(db: &mut Db, args: &Map<String, Value>) -> Result<Value> {
    let ids = string_list(args, "ids")?;
    if ids.is_empty() {
        bail!("ids is required: one or more item ids from list_items or search_items");
    }
    if ids.len() > MAX_GET_IDS {
        bail!("at most {MAX_GET_IDS} ids per call (got {})", ids.len());
    }
    let clock = clock(args)?;
    let max_chars = opt_usize(args, "max_chars")?.unwrap_or(DEFAULT_MAX_CHARS).clamp(1, MAX_MAX_CHARS);
    let offset = opt_usize(args, "offset")?.unwrap_or(0);

    let mut items = Vec::new();
    let mut not_found = Vec::new();
    for id in &ids {
        let Some(row) = db.get(id)? else {
            not_found.push(id.clone());
            continue;
        };
        if row.deleted {
            not_found.push(id.clone());
            continue;
        }
        let total_chars = row.text.chars().count();
        let start = offset.min(total_chars);
        let text: String = row.text.chars().skip(start).take(max_chars).collect();
        let returned = text.chars().count();
        let truncated = start + returned < total_chars;
        let mut entry = Map::new();
        entry.insert("id".into(), json!(row.id));
        entry.insert("kind".into(), json!(row.kind));
        entry.insert("source".into(), json!(row.source));
        entry.insert("time".into(), json!(clock.full(row.created_at)));
        if row.updated_at > row.created_at + 1.0 {
            entry.insert("updated".into(), json!(clock.full(row.updated_at)));
        }
        if let Some(title) = &row.title {
            entry.insert("title".into(), json!(title));
        }
        entry.insert("duration_s".into(), json!(row.duration_s.map(round1)));
        entry.insert("words".into(), json!(row.words));
        entry.insert("engine".into(), json!(row.engine));
        entry.insert("status".into(), json!(row.status));
        entry.insert("processed".into(), json!(row.processed_at.is_some()));
        entry.insert("total_chars".into(), json!(total_chars));
        if start > 0 {
            entry.insert("offset".into(), json!(start));
        }
        entry.insert("truncated".into(), json!(truncated));
        if truncated {
            entry.insert("next_offset".into(), json!(start + returned));
        }
        entry.insert("text".into(), json!(text));
        items.push(Value::Object(entry));
    }
    Ok(json!({ "items": items, "not_found": not_found }))
}

fn library_stats(db: &mut Db, args: &Map<String, Value>) -> Result<Value> {
    let clock = clock(args)?;
    let (filter, range) = filter(args, &clock)?;
    let group_by = opt_string(args, "group_by")?.unwrap_or_else(|| "day".to_string()).to_ascii_lowercase();
    if !matches!(group_by.as_str(), "day" | "kind" | "source") {
        bail!("group_by must be day, kind or source, not \"{group_by}\"");
    }
    let rows = db.stat_rows(&filter)?;

    #[derive(Default)]
    struct Group {
        items: i64,
        words: i64,
        seconds: f64,
        kinds: Vec<(String, i64)>,
        weekday: Option<String>,
    }
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Group> = std::collections::HashMap::new();
    let (mut total_items, mut total_words, mut total_seconds) = (0i64, 0i64, 0.0f64);
    for row in &rows {
        let key = match group_by.as_str() {
            "day" => clock.day(row.created_at),
            "kind" => row.kind.clone(),
            _ => row.source.clone().unwrap_or_else(|| "(none)".to_string()),
        };
        let group = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Group::default()
        });
        group.items += 1;
        group.words += row.words;
        group.seconds += row.duration_s.unwrap_or(0.0);
        if group_by == "day" {
            if group.weekday.is_none() {
                group.weekday = Some(clock.zoned(row.created_at).strftime("%a").to_string());
            }
            match group.kinds.iter_mut().find(|(k, _)| *k == row.kind) {
                Some(entry) => entry.1 += 1,
                None => group.kinds.push((row.kind.clone(), 1)),
            }
        }
        total_items += 1;
        total_words += row.words;
        total_seconds += row.duration_s.unwrap_or(0.0);
    }
    if group_by != "day" {
        order.sort_by(|a, b| groups[b].items.cmp(&groups[a].items).then(a.cmp(b)));
    }
    let list: Vec<Value> = order
        .iter()
        .map(|key| {
            let group = &groups[key];
            let mut entry = Map::new();
            entry.insert(group_by.clone(), json!(key));
            if let Some(weekday) = &group.weekday {
                entry.insert("weekday".into(), json!(weekday));
            }
            entry.insert("items".into(), json!(group.items));
            entry.insert("words".into(), json!(group.words));
            entry.insert("recorded_s".into(), json!(group.seconds.round()));
            if group_by == "day" {
                entry.insert("by_kind".into(), kind_counts(&group.kinds));
            }
            Value::Object(entry)
        })
        .collect();
    Ok(json!({
        "range": range,
        "group_by": group_by,
        "total": { "items": total_items, "words": total_words, "recorded_s": total_seconds.round() },
        "groups": list,
    }))
}

/// Everything `sync_status` reports. Also what `zone status --json` prints.
pub fn sync_status(db: &mut Db) -> Result<Value> {
    let clock = Clock::system();
    let now = clock.now_zoned();
    let now_unix = unix(clock.now);
    let dir = paths::data_dir()?;
    let daemon = DaemonInfo::read_if_running(&dir);

    let phones: Vec<Value> = db
        .devices()?
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "app": d.app,
                "endpoint_id": d.endpoint_id,
                "paired": clock.full(d.paired_at),
                "last_sync": d.last_sync.map(|t| clock.full(t)),
                "last_sync_age_s": d.last_sync.map(|t| (now_unix - t).max(0.0).round()),
                "items_received": d.items_received,
                "last_path": d.last_path,
            })
        })
        .collect();

    let last = |key: &str| -> Result<Value> {
        Ok(match db.meta_f64(key)? {
            Some(t) => json!({ "time": clock.full(t), "age_s": (now_unix - t).max(0.0).round() }),
            None => Value::Null,
        })
    };
    let mut by_source = Map::new();
    for (kind, source, count) in db.counts_by_kind_and_source()? {
        if kind == "dictation" {
            by_source.insert(source.unwrap_or_else(|| "(none)".to_string()), json!(count));
        }
    }
    let span = db.time_span()?;

    Ok(json!({
        "now": {
            "local": clock.full(now_unix),
            "date": now.strftime("%Y-%m-%d").to_string(),
            "weekday": now.strftime("%A").to_string(),
            "timezone": clock.zone_name(),
            "utc_offset": format_offset(now.offset().seconds()),
            "rfc3339": clock.rfc3339(now_unix),
        },
        "computer": {
            "name": paths::computer_name(),
            "os": std::env::consts::OS,
            "zone_version": env!("CARGO_PKG_VERSION"),
        },
        "daemon_running": daemon.is_some(),
        "phones": phones,
        "last_sync": {
            "link": last("last_sync_link")?,
            "backup_folder": last("last_sync_backup")?,
        },
        "backup_folder": db.meta_get("backup_folder")?,
        "items": {
            "total": db.total_items()?,
            "by_kind": kind_counts(&db.counts_by_kind()?),
            "dictations_by_source": by_source,
            "unprocessed": db.unprocessed_items()?,
            "deleted_on_phone": db.deleted_items()?,
            "oldest": span.map(|(oldest, _)| clock.full(oldest)),
            "newest": span.map(|(_, newest)| clock.full(newest)),
        },
    }))
}

fn mark_processed(db: &mut Db, args: &Map<String, Value>) -> Result<Value> {
    let ids = string_list(args, "ids")?;
    if ids.is_empty() {
        bail!("ids is required: the items to flag");
    }
    if ids.len() > MAX_MARK_IDS {
        bail!("at most {MAX_MARK_IDS} ids per call (got {})", ids.len());
    }
    let processed = opt_bool(args, "processed")?.unwrap_or(true);
    let (found, missing) = db.mark_processed(&ids, processed, paths::now_unix())?;
    Ok(json!({
        "processed": processed,
        "updated": found.len(),
        "not_found": missing,
        "unprocessed_remaining": db.unprocessed_items()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Delivery;
    use crate::item::{Carrier, Item};
    use jiff::Timestamp;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn at(text: &str) -> f64 {
        unix(text.parse::<Timestamp>().unwrap())
    }

    fn make(id: &str, kind: &str, source: Option<&str>, when: &str, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: kind.to_string(),
            source: source.map(str::to_string),
            created_at: at(when),
            updated_at: at(when),
            text: text.to_string(),
            title: if kind == "meeting" { Some("Planning".to_string()) } else { None },
            duration_s: Some(4.25),
            engine: Some("fluidMini".to_string()),
            status: Some("inserted".to_string()),
            extra: "{}".to_string(),
            rev: None,
            deleted: false,
        }
    }

    /// Wednesday 2026-11-11 in Los Angeles (UTC-8), with items just inside and just outside
    /// the window from 12:00 to 17:00.
    fn library() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        let items = vec![
            make("A-BEFORE", "dictation", Some("keyboard"), "2026-11-11T19:59:59Z", "just before noon"),
            make("B-NOON", "dictation", Some("keyboard"), "2026-11-11T20:00:00Z", "exactly at noon about the harbor"),
            make("C-NOTE", "note", None, "2026-11-11T22:30:00Z", "a note to self about the harbor trip"),
            make("D-APP", "dictation", Some("app"), "2026-11-12T00:59:59Z", "one second before five"),
            make("E-FIVE", "dictation", Some("keyboard"), "2026-11-12T01:00:00Z", "exactly at five"),
            make("F-MEET", "meeting", None, "2026-11-12T18:00:00Z", &"Ada: a long transcript line. ".repeat(50)),
        ];
        db.apply_items(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: 1.0 }).unwrap();
        db
    }

    fn ids(value: &Value) -> Vec<String> {
        value["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn list_window_in_a_named_zone() {
        let mut db = library();
        let out = call(
            &mut db,
            "list_items",
            &args(json!({ "start": "2026-11-11 12:00", "end": "2026-11-11 17:00", "timezone": "America/Los_Angeles", "order": "oldest" })),
            false,
        )
        .unwrap();
        assert_eq!(ids(&out), ["B-NOON", "C-NOTE", "D-APP"]);
        assert_eq!(out["total"], 3);
        assert_eq!(out["total_by_kind"], json!({ "dictation": 2, "note": 1 }));
        assert_eq!(out["range"]["start"], "Wed 2026-11-11 12:00:00 -08:00");
        assert_eq!(out["range"]["end"], "Wed 2026-11-11 17:00:00 -08:00");
        assert_eq!(out["items"][0]["time"], "Wed 2026-11-11 12:00:00 -08:00");
        assert_eq!(out["next_cursor"], Value::Null);

        // The same instants given with explicit offsets, from another zone, select the same items.
        let out = call(
            &mut db,
            "list_items",
            &args(json!({ "start": "2026-11-11T15:00:00-05:00", "end": "2026-11-11T20:00:00-05:00", "timezone": "America/New_York" })),
            false,
        )
        .unwrap();
        assert_eq!(out["total"], 3);
        assert_eq!(out["items"][0]["time"], "Wed 2026-11-11 19:59:59 -05:00");

        // The same wall-clock text read in UTC is a different window.
        let out = call(&mut db, "list_items", &args(json!({ "start": "2026-11-11 12:00", "end": "2026-11-11 17:00", "timezone": "UTC" })), false).unwrap();
        assert_eq!(out["total"], 0);
    }

    #[test]
    fn list_pages_filters_and_flags() {
        let mut db = library();
        let first = call(&mut db, "list_items", &args(json!({ "limit": 4 })), false).unwrap();
        assert_eq!((first["total"].as_i64(), first["returned"].as_i64()), (Some(6), Some(4)));
        assert_eq!(ids(&first)[0], "F-MEET");
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        let second = call(&mut db, "list_items", &args(json!({ "limit": 4, "cursor": cursor })), false).unwrap();
        assert_eq!(ids(&second), ["B-NOON", "A-BEFORE"]);
        assert_eq!(second["next_cursor"], Value::Null);

        // Plural kinds and comma lists are understood.
        let out = call(&mut db, "list_items", &args(json!({ "kinds": "notes,meetings" })), false).unwrap();
        assert_eq!(out["total"], 2);
        let out = call(&mut db, "list_items", &args(json!({ "sources": ["keyboard"] })), false).unwrap();
        assert_eq!(out["total"], 3);

        // The card of a long item is a preview, not the text.
        let meeting = &first["items"][0];
        assert_eq!(meeting["title"], "Planning");
        assert!(meeting["preview"].as_str().unwrap().chars().count() <= 201);
        assert_eq!(meeting["processed"], false);

        let marked = call(&mut db, "mark_processed", &args(json!({ "ids": ["F-MEET", "missing"] })), false).unwrap();
        assert_eq!((marked["updated"].as_i64(), marked["not_found"].clone()), (Some(1), json!(["MISSING"])));
        let out = call(&mut db, "list_items", &args(json!({ "unprocessed_only": true })), false).unwrap();
        assert_eq!(out["total"], 5);
        call(&mut db, "mark_processed", &args(json!({ "ids": ["F-MEET"], "processed": false })), false).unwrap();
        let out = call(&mut db, "list_items", &args(json!({ "unprocessed_only": true })), false).unwrap();
        assert_eq!(out["total"], 6);
    }

    #[test]
    fn bad_arguments_are_explained() {
        let mut db = library();
        let mut err = |name: &str, a: Value| call(&mut db, name, &args(a), false).unwrap_err().to_string();
        assert!(err("list_items", json!({ "start": "whenever" })).starts_with("start: cannot read the time"));
        assert!(err("list_items", json!({ "start": "2026-11-12", "end": "2026-11-11" })).contains("is not before end"));
        assert!(err("list_items", json!({ "timezone": "Mars/Olympus" })).contains("unknown time zone"));
        assert!(err("list_items", json!({ "order": "sideways" })).contains("order must be"));
        assert!(err("list_items", json!({ "cursor": "zzz" })).contains("cursor"));
        assert!(err("search_items", json!({})).contains("query is required"));
        assert!(err("get_items", json!({})).contains("ids is required"));
        assert!(err("library_stats", json!({ "group_by": "moon" })).contains("group_by must be"));
        assert!(err("nope", json!({})).contains("unknown tool"));
    }

    #[test]
    fn read_only_hides_and_refuses_the_write() {
        let mut db = library();
        assert!(definitions(false).iter().any(|t| t.name == "mark_processed"));
        assert!(!definitions(true).iter().any(|t| t.name == "mark_processed"));
        assert_eq!(definitions(true).len(), 5);
        assert!(definitions(false).iter().filter(|t| !t.read_only).map(|t| t.name).eq(["mark_processed"]));
        let refused = call(&mut db, "mark_processed", &args(json!({ "ids": ["F-MEET"] })), true).unwrap_err();
        assert!(refused.to_string().contains("--read-only"));
        assert_eq!(db.unprocessed_items().unwrap(), 6);
    }

    #[test]
    fn search_reports_cuts() {
        let mut db = library();
        let out = call(&mut db, "search_items", &args(json!({ "query": "harbor", "limit": 1 })), false).unwrap();
        assert_eq!((out["total"].as_i64(), out["returned"].as_i64(), out["truncated"].as_bool()), (Some(2), Some(1), Some(true)));
        assert!(out["note"].as_str().unwrap().contains("1 best of 2"));
        assert!(out["items"][0]["snippet"].as_str().unwrap().contains("[harbor]"));
        let out = call(&mut db, "search_items", &args(json!({ "query": "harbor", "kinds": ["note"] })), false).unwrap();
        assert_eq!((ids(&out), out["truncated"].as_bool()), (vec!["C-NOTE".to_string()], Some(false)));
        let out = call(&mut db, "search_items", &args(json!({ "query": "zebra" })), false).unwrap();
        assert_eq!(out["total"], 0);
    }

    #[test]
    fn get_items_bounds_text() {
        let mut db = library();
        let out = call(&mut db, "get_items", &args(json!({ "ids": ["f-meet", "nope"], "max_chars": 100 })), false).unwrap();
        let item = &out["items"][0];
        assert_eq!(item["text"].as_str().unwrap().chars().count(), 100);
        assert_eq!((item["truncated"].as_bool(), item["next_offset"].as_i64()), (Some(true), Some(100)));
        let total = item["total_chars"].as_u64().unwrap() as usize;
        assert_eq!(out["not_found"], json!(["nope"]));
        // Reading on from next_offset reaches the end.
        let rest = call(&mut db, "get_items", &args(json!({ "ids": ["F-MEET"], "max_chars": 200000, "offset": 100 })), false).unwrap();
        let item = &rest["items"][0];
        assert_eq!(item["text"].as_str().unwrap().chars().count(), total - 100);
        assert_eq!(item["truncated"], false);
        // Short items come back whole by default.
        let small = call(&mut db, "get_items", &args(json!({ "ids": "B-NOON" })), false).unwrap();
        assert_eq!(small["items"][0]["text"], "exactly at noon about the harbor");
        assert_eq!(small["items"][0]["truncated"], false);
    }

    #[test]
    fn stats_group_by_local_day() {
        let mut db = library();
        let out = call(&mut db, "library_stats", &args(json!({ "timezone": "America/Los_Angeles" })), false).unwrap();
        // E-FIVE is 17:00 on the 11th in Los Angeles. F-MEET is 10:00 on the 12th.
        assert_eq!(out["groups"][0], json!({ "day": "2026-11-11", "weekday": "Wed", "items": 5, "words": 24, "recorded_s": 21.0, "by_kind": { "dictation": 4, "note": 1 } }));
        assert_eq!(out["groups"][1]["day"], "2026-11-12");
        assert_eq!(out["total"]["items"], 6);
        // In UTC the day boundary moves: two of the five fall on the 12th.
        let out = call(&mut db, "library_stats", &args(json!({ "timezone": "UTC" })), false).unwrap();
        assert_eq!((out["groups"][0]["items"].as_i64(), out["groups"][1]["items"].as_i64()), (Some(3), Some(3)));
        let out = call(&mut db, "library_stats", &args(json!({ "group_by": "kind" })), false).unwrap();
        assert_eq!(out["groups"][0]["kind"], "dictation");
        assert_eq!(out["groups"][0]["items"], 4);
        let out = call(&mut db, "library_stats", &args(json!({ "group_by": "source" })), false).unwrap();
        assert_eq!(out["groups"][0], json!({ "source": "keyboard", "items": 3, "words": 12, "recorded_s": 13.0 }));
    }

    #[test]
    fn instructions_explain_the_kinds_plainly() {
        for needle in ["dictation", "note", "meeting", "keyboard", "typed by voice", "voice note", "transcript"] {
            assert!(INSTRUCTIONS.contains(needle), "instructions should mention {needle}");
        }
        for tool in definitions(false) {
            assert!(!tool.description.contains('\u{2014}'), "{} has an em dash", tool.name);
            assert_eq!(tool.input_schema["type"], "object");
        }
        assert!(!INSTRUCTIONS.contains('\u{2014}'));
    }
}
