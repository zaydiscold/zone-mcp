//! The tools agents get, as plain functions from JSON arguments to a JSON answer.
//!
//! `mcp.rs` puts these on the wire. Keeping them free of protocol types means the same code
//! is tested directly and could back another surface later.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::daemon_info::DaemonInfo;
use crate::db::{Cursor, Db, Filter, Order, Row, SearchCursor};
use crate::item::preview;
use crate::paths;
use crate::scope::Scope;
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

/// What a server lets its agent do: whether it may flag items, and which items it sees.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    /// Leaves out `mark_processed`, the one tool that writes.
    pub read_only: bool,
    pub scope: Scope,
}

/// What an agent is told once, when it connects: [`INSTRUCTIONS`], and for a server with a
/// limited view, what that view leaves out.
pub fn instructions(scope: &Scope) -> String {
    match scope.for_agents() {
        Some(limit) => format!("{INSTRUCTIONS}\n\n{limit}"),
        None => INSTRUCTIONS.to_string(),
    }
}

/// What every agent is told once, when it connects.
pub const INSTRUCTIONS: &str = "\
Zone is this person's spoken library from the Zone app on their iPhone, copied to this computer. \
Dictations, notes and meetings are their own speech recognized on the phone, so expect misheard \
words and informal phrasing. Clips are exact text.

Kinds (filter with `kinds`):
- `dictation`: something they dictated. Source `keyboard` is text typed by voice into other apps \
(messages, emails, prompts), usually addressed to someone else. Source `app` was dictated inside \
Zone, source `note` is the raw dictation behind a voice note.
- `note`: a voice note they saved, a note to self.
- `meeting`: a meeting transcript with a title, one \"Speaker: words\" line per turn.
- `clip`: text they sent to this computer on purpose (copied, shared, sent by a shortcut or typed). \
The newest clip is \"the thing I just sent you\": list_items with kinds [\"clip\"] and limit 1.

How to work with it:
- For a time, pass the person's words as `when` to list_items: \"Wednesday 12pm to 5pm\", \
\"yesterday afternoon\", \"last week\". One call returns the cards, with full text when the page is \
small. `range` says the exact window it read. sync_status is only for the current date, the time \
zone or how fresh the library is.
- search_items finds words and copes with plurals, other word forms and misheard or misspelled \
words. Each hit says how it matched.
- get_items reads full text by id; long items continue from next_offset.
- Answers say when there is more (pass next_cursor as cursor) and why a list came back empty.
- mark_processed only sets a flag so you can work through items once (unprocessed_only skips \
flagged items). Nothing here can edit or delete the person's words.

This is private speech. Quote it only as far as the task needs.";

/// One tool, described for agents.
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: String,
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

/// How `when` is described to agents.
const WHEN_HELP: &str = "The time window in words, read in the computer's time zone (or `timezone`): \"Wednesday 12pm to 5pm\", \"yesterday afternoon\", \"this morning\", \"last week\", \"3 days ago\", \"Monday to Wednesday\", \"2026-11-11\". A weekday alone is the most recent one, today included; \"last Wednesday\" is the one before today. Parts of the day: morning 5:00 to 12:00, afternoon 12:00 to 17:00, evening 17:00 to 21:00, night 21:00 to 5:00, tonight and last night 17:00 to 5:00. Use it instead of start and end. The answer's `range` says the exact window it read, so check it.";

fn common_filters(scope: &Scope) -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert("when".into(), json!({ "type": "string", "description": WHEN_HELP }));
    properties.insert("start".into(), time_property("start"));
    properties.insert("end".into(), time_property("end"));
    let all = if scope.kinds.is_empty() { "Leave out for all of them." } else { "Leave out for every kind this server shows." };
    properties.insert(
        "kinds".into(),
        json!({
            "type": "array",
            "items": { "type": "string", "enum": scope.visible_kinds() },
            "description": format!("Which kinds to include. {all} dictation = text typed by voice (mostly messages in other apps), note = saved voice notes, meeting = meeting transcripts, clip = text the person sent to this computer on purpose (the newest clip is \"the thing I just sent you\").")
        }),
    );
    properties.insert(
        "sources".into(),
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Only items from these sources. For dictations, where they were spoken: keyboard (typed by voice in another app), app (inside Zone), note (the dictation behind a voice note). For clips, how they were sent: clipboard, share, shortcut, typed. Notes and meetings have no source, so this filter leaves them out."
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

/// The tools on offer. A read-only server leaves out the one tool that writes, and a server
/// with a limited view says so in the tools that list and search.
pub fn definitions(access: &Access) -> Vec<ToolDef> {
    let read_only = access.read_only;
    let scope = &access.scope;
    // Said again where the agent reads it right before use.
    let limit = match scope.short() {
        Some(short) => format!(" This server has a limited view ({short}): nothing else in the library can be reached from here."),
        None => String::new(),
    };
    let mut list_properties = common_filters(scope);
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
        "include_text".into(),
        json!({ "type": "string", "enum": ["auto", "full", "preview"], "default": "auto", "description": format!("auto (default): each card carries its full text when this page's text, meetings aside, is at most {AUTO_TEXT_BUDGET} characters, else a 200-character preview; meetings are previews. full: full text (at most {FULL_TEXT_BUDGET} characters a page, the rest as previews). preview: previews only. A card whose preview is not the whole text says \"cut\": true; get_items reads it.") }),
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
    search_properties.extend(common_filters(scope));
    search_properties.insert(
        "limit".into(),
        json!({ "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "default": DEFAULT_SEARCH_LIMIT, "description": "How many matches to return, best match first." }),
    );
    search_properties.insert(
        "cursor".into(),
        json!({ "type": "string", "description": "next_cursor from the previous call, for the matches after it. Keep the other arguments the same." }),
    );

    let mut stats_properties = common_filters(scope);
    stats_properties.insert(
        "group_by".into(),
        json!({ "type": "string", "enum": ["day", "kind", "source"], "default": "day", "description": "day (local calendar days), kind, or source." }),
    );

    let mut tools = vec![
        ToolDef {
            name: "list_items",
            title: "List dictations, notes, meetings and clips",
            description: format!("{}{limit}", "List the person's items in a time window, newest or oldest first, as cards (id, kind, source, local time with weekday, duration, words, processed flag) with their full text when the page is small, else 200-character previews. For \"what did I say Wednesday 12pm to 5pm\" pass when \"Wednesday 12pm to 5pm\": one call, no other tool needed first. Returns the total that match, counts by kind, `range` (the exact window read, in ISO and in words), `more` and next_cursor when there is another page, and a note saying why when nothing matches. kinds [\"clip\"] with limit 1 is \"the thing I just sent you\"."),
            input_schema: json!({ "type": "object", "properties": list_properties, "additionalProperties": false }),
            read_only: true,
        },
        ToolDef {
            name: "search_items",
            title: "Search the spoken library",
            description: format!("{}{limit}", "Full-text search over everything the person dictated: dictations (mostly messages typed by voice in other apps), voice notes, meeting transcripts and titles, and clips (text they sent to this computer on purpose). Returns the best matches with a snippet around the matching words (matches are wrapped in [brackets]), plus the total number of matches and a truncated flag when there are more than were returned. Narrow with start, end, kinds or sources."),
            input_schema: json!({ "type": "object", "properties": search_properties, "required": ["query"], "additionalProperties": false }),
            read_only: true,
        },
        ToolDef {
            name: "get_items",
            title: "Read full items",
            description: "Return the full text of items by id (ids come from list_items or search_items). Long texts such as meeting transcripts are cut at max_chars and marked truncated, with next_offset to continue reading. For a meeting, text is the transcript with one \"Speaker: words\" line per turn.".to_string(),
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
            description: format!("{}{limit}", "Counts, word totals and recorded seconds for the library or a time range, grouped by local calendar day, by kind (dictation, note, meeting, clip) or by source (keyboard, app, note for dictations; clipboard, share, shortcut, typed for clips). Use it to see how much there is before listing, or to find the busy days."),
            input_schema: json!({ "type": "object", "properties": stats_properties, "additionalProperties": false }),
            read_only: true,
        },
        ToolDef {
            name: "sync_status",
            title: "Current time and sync state",
            description: "The computer's current local date, weekday, time and time zone, which phones are paired, when each way in last delivered items, and how many items of each kind the library holds. Not needed before list_items or search_items (they read \"Wednesday\" or \"yesterday afternoon\" themselves); use it when the person asks how fresh the library is or what day it is.".to_string(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            read_only: true,
        },
    ];
    if !read_only {
        tools.push(ToolDef {
            name: "mark_processed",
            title: "Flag items as processed",
            description: "Set or clear the processed flag on items, so a later list_items call with unprocessed_only skips them. This is the only tool that writes, and it writes only this flag: it never changes or deletes an item's text.".to_string(),
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
///
/// Every tool works inside `access.scope`: an item outside it is not listed, found, read,
/// counted or flagged, and asking for it by id answers as if it did not exist.
pub fn call(db: &mut Db, name: &str, args: &Map<String, Value>, access: &Access) -> Result<Value> {
    let scope = &access.scope;
    match name {
        "list_items" => list_items(db, args, scope),
        "search_items" => search_items(db, args, scope),
        "get_items" => get_items(db, args, scope),
        "library_stats" => library_stats(db, args, scope),
        "sync_status" => sync_status(db, scope),
        "mark_processed" if !access.read_only => mark_processed(db, args, scope),
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
/// Asking by name for a kind or a source the server does not show is an error, so the agent
/// learns why instead of concluding there is nothing.
fn filter(args: &Map<String, Value>, clock: &Clock, scope: &Scope) -> Result<(Filter, Value)> {
    let when = opt_string(args, "when")?;
    let mut start = match opt_string(args, "start")? {
        Some(text) => Some(clock.parse(&text, Bound::Start).map_err(|e| anyhow!("start: {e}"))?),
        None => None,
    };
    let mut end = match opt_string(args, "end")? {
        Some(text) => Some(clock.parse(&text, Bound::End).map_err(|e| anyhow!("end: {e}"))?),
        None => None,
    };
    if let Some(text) = &when {
        if start.is_some() || end.is_some() {
            bail!("give when, or start and end, not both. when \"{text}\" already sets the window.");
        }
        let window = clock.window(text).map_err(|e| anyhow!("when: {e}"))?;
        if window.start == window.end {
            bail!("when \"{text}\" is a single moment ({}), not a window. Add a second time: \"{text} to 5pm\", or a part of the day.", clock.full(window.start));
        }
        start = Some(window.start);
        end = Some(window.end);
    }
    if let (Some(s), Some(e)) = (start, end)
        && s >= e {
            bail!("start ({}) is not before end ({})", clock.full(s), clock.full(e));
        }
    let kinds: Vec<String> = string_list(args, "kinds")?.into_iter().map(|k| normalize_kind(&k)).collect();
    let sources = string_list(args, "sources")?;
    if let Some(hidden) = kinds.iter().find(|k| crate::scope::KINDS.contains(&k.as_str()) && !scope.allows_kind(k)) {
        bail!(
            "this server does not show {hidden} items: it has a limited view ({}). Leave kinds out, or tell the person this connection does not include them.",
            scope.short().unwrap_or_default()
        );
    }
    if let Some(hidden) = sources.iter().find(|s| scope.hides_source(s)) {
        bail!(
            "this server does not show items with source {hidden}: it has a limited view ({}). Tell the person this connection does not include them.",
            scope.short().unwrap_or_default()
        );
    }
    let range = json!({
        "when": when,
        "words": clock.window_words(start, end),
        "start": start.map(|t| clock.full(t)),
        "end": end.map(|t| clock.full(t)),
        "start_iso": start.map(|t| clock.rfc3339(t)),
        "end_iso": end.map(|t| clock.rfc3339(t)),
        "timezone": clock.zone_name(),
    });
    Ok((Filter { start, end, kinds, sources, scope: scope.clone(), ..Filter::default() }, range))
}

/// Agents say "dictations", "notes", "meetings". The stored kinds are singular.
pub fn normalize_kind(kind: &str) -> String {
    let lower = kind.trim().to_ascii_lowercase();
    match lower.as_str() {
        "dictations" => "dictation".to_string(),
        "notes" => "note".to_string(),
        "meetings" => "meeting".to_string(),
        "clips" => "clip".to_string(),
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

fn list_items(db: &mut Db, args: &Map<String, Value>, scope: &Scope) -> Result<Value> {
    let clock = clock(args)?;
    let (mut filter, range) = filter(args, &clock, scope)?;
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
    let mode = TextMode::parse(opt_string(args, "include_text")?.as_deref())?;

    let total = db.count(&filter)?;
    let by_kind = db.counts_by_kind_filtered(&filter)?;
    // One extra row tells whether there is a next page.
    let mut rows = db.list(&filter, order, limit + 1, cursor)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more { rows.last().map(|row| Cursor::after(row).encode()) } else { None };
    let (items, text, text_note) = cards_with_text(&rows, &clock, mode);

    let mut notes = Vec::new();
    if let Some(note) = text_note {
        notes.push(note);
    }
    if more {
        notes.push(format!(
            "There are more: this page has {} of {total}. Call list_items again with cursor set to next_cursor (keep the other arguments) for the next ones.",
            items.len()
        ));
    }
    if total == 0 {
        notes.push(empty_note(db, &filter, &clock, None)?);
    }
    let mut out = Map::new();
    out.insert("range".into(), range);
    out.insert("order".into(), json!(if order == Order::Newest { "newest" } else { "oldest" }));
    out.insert("total".into(), json!(total));
    out.insert("total_by_kind".into(), kind_counts(&by_kind));
    out.insert("returned".into(), json!(items.len()));
    out.insert("more".into(), json!(more));
    out.insert("next_cursor".into(), json!(next_cursor));
    out.insert("text".into(), json!(text));
    if !notes.is_empty() {
        out.insert("note".into(), json!(notes.join(" ")));
    }
    out.insert("items".into(), json!(items));
    Ok(Value::Object(out))
}

/// How much of each item's text `list_items` returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextMode {
    /// Full text when the page's text is small, else previews. Meetings stay previews.
    Auto,
    Full,
    Preview,
}

impl TextMode {
    fn parse(text: Option<&str>) -> Result<TextMode> {
        match text.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
            None | Some("auto") => Ok(TextMode::Auto),
            Some("full") | Some("true") => Ok(TextMode::Full),
            Some("preview") | Some("previews") | Some("false") => Ok(TextMode::Preview),
            Some(other) => bail!("include_text must be auto, full or preview, not \"{other}\""),
        }
    }
}

/// `auto` returns full text when the page's text (meetings aside) is at most this long.
pub const AUTO_TEXT_BUDGET: usize = 12_000;
/// `full` returns at most this much text in one page; items past it come as previews.
pub const FULL_TEXT_BUDGET: usize = 60_000;

/// Cards for a page, with full text or previews as `mode` says. Returns the cards, `full`,
/// `preview` or `mixed`, and a sentence when some text was left out.
fn cards_with_text(rows: &[Row], clock: &Clock, mode: TextMode) -> (Vec<Value>, &'static str, Option<String>) {
    let length = |row: &Row| row.text.chars().count();
    let page_chars: usize = rows.iter().filter(|r| r.kind != "meeting").map(length).sum();
    let mut budget = match mode {
        TextMode::Preview => 0,
        TextMode::Auto if page_chars > AUTO_TEXT_BUDGET => 0,
        TextMode::Auto => AUTO_TEXT_BUDGET,
        TextMode::Full => FULL_TEXT_BUDGET,
    };
    let (mut full, mut previews, mut meetings_held) = (0usize, 0usize, 0usize);
    let mut cards = Vec::with_capacity(rows.len());
    for row in rows {
        let mut card = card(row, clock);
        let chars = length(row);
        let meeting_held = mode == TextMode::Auto && row.kind == "meeting";
        // In full mode a long item takes what is left of the budget, and says where to go on.
        if !meeting_held && budget > 0 && (chars <= budget || mode == TextMode::Full) {
            let shown: String = row.text.chars().take(DEFAULT_MAX_CHARS.min(budget)).collect();
            let returned = shown.chars().count();
            budget -= returned;
            if let Some(map) = card.as_object_mut() {
                map.remove("preview");
                map.insert("text".into(), json!(shown));
                if returned < chars {
                    map.insert("total_chars".into(), json!(chars));
                    map.insert("next_offset".into(), json!(returned));
                }
            }
            full += 1;
        } else {
            if meeting_held && chars > PREVIEW_CHARS {
                meetings_held += 1;
            }
            if chars > PREVIEW_CHARS
                && let Some(map) = card.as_object_mut()
            {
                map.insert("cut".into(), json!(true));
                map.insert("total_chars".into(), json!(chars));
            }
            previews += 1;
        }
        cards.push(card);
    }
    let cut = cards.iter().filter(|c| c["cut"] == true).count();
    let shape = match (full, previews) {
        (_, 0) => "full",
        (0, _) => "preview",
        _ => "mixed",
    };
    let note = if cut == 0 {
        None
    } else if mode == TextMode::Auto && page_chars > AUTO_TEXT_BUDGET {
        Some(format!(
            "The text of this page is {page_chars} characters, more than fits, so items are previews (\"cut\": true where the preview is not the whole text). Call get_items with the ids you need, or list again with include_text \"full\" and a smaller limit."
        ))
    } else if meetings_held == cut {
        Some("Meeting transcripts are previews unless you pass include_text \"full\". get_items reads one in full.".to_string())
    } else {
        Some("Items marked \"cut\": true are previews: get_items returns their full text.".to_string())
    };
    (cards, shape, note)
}

/// Why a list or search came back empty, in a sentence an agent can act on: a filter it set
/// leaves out what is there, or nothing was recorded in the window at all.
fn empty_note(db: &Db, filter: &Filter, clock: &Clock, query: Option<&str>) -> Result<String> {
    let window_only = Filter { start: filter.start, end: filter.end, scope: filter.scope.clone(), ..Filter::default() };
    let in_window = db.count(&window_only)?;
    let mut set = Vec::new();
    if let Some(query) = query {
        set.push(format!("the words \"{query}\""));
    }
    if !filter.kinds.is_empty() {
        set.push(format!("kinds {}", json!(filter.kinds)));
    }
    if !filter.sources.is_empty() {
        set.push(format!("sources {}", json!(filter.sources)));
    }
    if filter.unprocessed_only {
        set.push("unprocessed_only".to_string());
    }
    let window = clock.window_words(filter.start, filter.end);
    if in_window > 0 && !set.is_empty() {
        let by_kind = db.counts_by_kind_filtered(&window_only)?;
        let mut sentence = format!(
            "Nothing matches, but that comes from what you set: {} ({in_window} items are in {window}: {}).",
            set.join(", "),
            kind_counts(&by_kind)
        );
        if filter.unprocessed_only && query.is_none() {
            sentence.push_str(" Without unprocessed_only, items already marked processed are listed too.");
        }
        return Ok(sentence);
    }
    let counts = db.counts_in(&filter.scope)?;
    Ok(match counts.span {
        None => "The library is empty: nothing has arrived from the phone yet (sync_status says how it is connected).".to_string(),
        Some((oldest, newest)) if filter.start.is_some() || filter.end.is_some() => format!(
            "Nothing was recorded in {window}. The library runs from {} to {}, so check the window in range, or widen it.",
            clock.full(oldest),
            clock.full(newest)
        ),
        Some(_) => "Nothing matches.".to_string(),
    })
}

fn search_items(db: &mut Db, args: &Map<String, Value>, scope: &Scope) -> Result<Value> {
    let Some(query) = opt_string(args, "query")? else {
        bail!("query is required: the words to look for");
    };
    let clock = clock(args)?;
    let (filter, range) = filter(args, &clock, scope)?;
    let limit = opt_usize(args, "limit")?.unwrap_or(DEFAULT_SEARCH_LIMIT).clamp(1, MAX_SEARCH_LIMIT);
    let offset = match opt_string(args, "cursor")? {
        Some(text) => SearchCursor::decode(&text)?.0,
        None => 0,
    };
    let result = db.search_page(&query, &filter, None, limit, offset)?;
    let next_cursor = SearchCursor::next(offset, result.hits.len(), result.total).map(|c| c.encode());
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
    let truncated = next_cursor.is_some();
    let mut out = Map::new();
    out.insert("query".into(), json!(query));
    out.insert("range".into(), range);
    out.insert("total".into(), json!(result.total));
    out.insert("returned".into(), json!(items.len()));
    out.insert("truncated".into(), json!(truncated));
    out.insert("next_cursor".into(), json!(next_cursor));
    if truncated {
        let shown = if offset == 0 {
            format!("Showing the {} best of {} matches.", items.len(), result.total)
        } else {
            format!("Showing matches {} to {} of {}.", offset + 1, offset + items.len(), result.total)
        };
        out.insert(
            "note".into(),
            json!(format!("{shown} Pass next_cursor as cursor for the ones after, or narrow with start, end, kinds or sources.")),
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

fn get_items(db: &mut Db, args: &Map<String, Value>, scope: &Scope) -> Result<Value> {
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
        // An item outside this server's view answers exactly like one that does not exist.
        if row.deleted || !scope.allows(&row.kind, row.source.as_deref()) {
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

fn library_stats(db: &mut Db, args: &Map<String, Value>, scope: &Scope) -> Result<Value> {
    let clock = clock(args)?;
    let (filter, range) = filter(args, &clock, scope)?;
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
/// How the daemon's watch on the backup folder is going, in words a person can act on.
/// Null when no folder is set.
pub fn backup_folder_state(daemon: Option<&DaemonInfo>, folder: Option<&str>, now: f64) -> Value {
    if folder.is_none() {
        return Value::Null;
    }
    let Some(daemon) = daemon else {
        return json!({ "state": "not_watching", "message": "zone is not running, so the folder is not being watched" });
    };
    let watch = &daemon.backup;
    if let Some(waited) = watch.waiting_for(now) {
        return json!({
            "state": "waiting",
            "message": crate::backup::waiting_reason(),
            "waiting_s": waited.round(),
            "fix": crate::backup::waiting_fix(),
        });
    }
    if let Some(error) = &watch.error {
        return json!({ "state": "error", "message": error });
    }
    match watch.read_finished_at {
        Some(t) => json!({ "state": "watching", "last_read_age_s": (now - t).max(0.0).round() }),
        None => json!({ "state": "starting", "message": "the first read has not finished yet" }),
    }
}

/// `scope` is what the asker may see: the item counts cover that and nothing else, and a
/// limited view is named so the numbers are not mistaken for the whole library.
pub fn sync_status(db: &mut Db, scope: &Scope) -> Result<Value> {
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
                // What the person calls it on this computer, else what it calls itself.
                "name": d.label(),
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
    let counts = db.counts_in(scope)?;
    let mut by_source = Map::new();
    for (kind, source, count) in &counts.by_kind_and_source {
        if kind == "dictation" {
            by_source.insert(source.clone().unwrap_or_else(|| "(none)".to_string()), json!(count));
        }
    }
    let span = counts.span;
    let backup_folder = db.meta_get("backup_folder")?;
    let backup_folder_state = backup_folder_state(daemon.as_ref(), backup_folder.as_deref(), now_unix);

    let mut status = json!({
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
            "flash": last("last_sync_flash")?,
            "backup_folder": last("last_sync_backup")?,
        },
        "backup_folder": backup_folder,
        "backup_folder_state": backup_folder_state,
        "items": {
            "total": counts.total,
            "by_kind": kind_counts(&counts.by_kind),
            "dictations_by_source": by_source,
            // Dictations that are the recording behind a voice note which is itself in the
            // library: the same words twice. The page counts and lists a voice note once
            // (`total` less this), and so does Zone for Mac.
            "recordings_behind_notes": db.count_folded(&crate::db::Filter { scope: scope.clone(), ..Default::default() })?,
            "unprocessed": counts.unprocessed,
            "deleted_on_phone": counts.deleted,
            "oldest": span.map(|(oldest, _)| clock.full(oldest)),
            "newest": span.map(|(_, newest)| clock.full(newest)),
        },
    });
    if let Some(note) = scope.for_agents() {
        status["scope"] = json!({ "kinds": scope.visible_kinds(), "excluded_sources": scope.exclude_sources, "note": note });
    }
    Ok(status)
}

fn mark_processed(db: &mut Db, args: &Map<String, Value>, scope: &Scope) -> Result<Value> {
    let ids = string_list(args, "ids")?;
    if ids.is_empty() {
        bail!("ids is required: the items to flag");
    }
    if ids.len() > MAX_MARK_IDS {
        bail!("at most {MAX_MARK_IDS} ids per call (got {})", ids.len());
    }
    let processed = opt_bool(args, "processed")?.unwrap_or(true);
    // Only what this server can see can be flagged through it.
    let (mut visible, mut hidden) = (Vec::new(), Vec::new());
    for id in ids {
        match db.get(&id)? {
            Some(row) if !scope.allows(&row.kind, row.source.as_deref()) => hidden.push(id.trim().to_ascii_uppercase()),
            _ => visible.push(id),
        }
    }
    let (found, mut missing) = db.mark_processed(&visible, processed, paths::now_unix())?;
    missing.extend(hidden);
    Ok(json!({
        "processed": processed,
        "updated": found.len(),
        "not_found": missing,
        "unprocessed_remaining": db.counts_in(scope)?.unprocessed,
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

    /// The tools of a server that shows everything, as most of these tests want them.
    fn call(db: &mut Db, name: &str, args: &Map<String, Value>, read_only: bool) -> Result<Value> {
        super::call(db, name, args, &Access { read_only, scope: Scope::default() })
    }

    fn definitions(read_only: bool) -> Vec<ToolDef> {
        super::definitions(&Access { read_only, scope: Scope::default() })
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
    fn one_call_reads_a_window_in_words_with_its_text() {
        let mut db = library();
        let out = call(
            &mut db,
            "list_items",
            &args(json!({ "when": "Nov 11 2026 12pm to 5pm", "timezone": "America/Los_Angeles", "order": "oldest" })),
            false,
        )
        .unwrap();
        assert_eq!(ids(&out), ["B-NOON", "C-NOTE", "D-APP"]);
        assert_eq!(out["range"]["start_iso"], "2026-11-11T12:00:00-08:00");
        assert_eq!(out["range"]["end_iso"], "2026-11-11T17:00:00-08:00");
        assert_eq!(out["range"]["words"], "Wednesday, November 11, 2026, 12:00 to 17:00");
        assert_eq!(out["range"]["when"], "Nov 11 2026 12pm to 5pm");
        // Small page: the whole text is there, no second call needed.
        assert_eq!((out["text"].as_str(), out["items"][1]["text"].as_str()), (Some("full"), Some("a note to self about the harbor trip")));
        assert!(out["items"][1].get("preview").is_none() && out.get("note").is_none(), "{out}");
        assert_eq!((out["more"].as_bool(), &out["next_cursor"]), (Some(false), &Value::Null));
        // The same window as start and end, and both at once is refused.
        let same = call(&mut db, "list_items", &args(json!({ "start": "2026-11-11 12pm", "end": "2026-11-11 5pm", "timezone": "America/Los_Angeles" })), false).unwrap();
        assert_eq!(same["total"], 3);
        let both = call(&mut db, "list_items", &args(json!({ "when": "2026-11-11", "start": "2026-11-11" })), false).unwrap_err().to_string();
        assert!(both.contains("not both"), "{both}");
        let point = call(&mut db, "list_items", &args(json!({ "when": "2026-11-11 3pm" })), false).unwrap_err().to_string();
        assert!(point.contains("single moment"), "{point}");
        let bad = call(&mut db, "list_items", &args(json!({ "when": "whenever" })), false).unwrap_err().to_string();
        assert!(bad.starts_with("when: cannot read") && bad.contains("Wednesday 12pm to 5pm"), "{bad}");
    }

    #[test]
    fn text_modes_and_meetings() {
        let mut db = library();
        // auto: the meeting stays a preview, marked cut, with a note on how to read it.
        let out = call(&mut db, "list_items", &args(json!({})), false).unwrap();
        assert_eq!(out["text"], "mixed");
        let meeting = &out["items"][0];
        assert_eq!((meeting["kind"].as_str(), meeting["cut"].as_bool()), (Some("meeting"), Some(true)));
        assert!(meeting["preview"].is_string() && meeting.get("text").is_none());
        assert!(out["note"].as_str().unwrap().contains("include_text \"full\""), "{out}");
        // full: the meeting too.
        let out = call(&mut db, "list_items", &args(json!({ "include_text": "full" })), false).unwrap();
        assert_eq!(out["text"], "full");
        assert_eq!(out["items"][0]["text"].as_str().unwrap().chars().count(), 1450);
        // preview: none.
        let out = call(&mut db, "list_items", &args(json!({ "include_text": "preview" })), false).unwrap();
        assert_eq!(out["text"], "preview");
        assert!(out["items"].as_array().unwrap().iter().all(|i| i.get("text").is_none()));
        assert!(call(&mut db, "list_items", &args(json!({ "include_text": "all" })), false).unwrap_err().to_string().contains("auto, full or preview"));
        // A big page in auto mode is previews, with a note saying how to get the rest.
        let long: Vec<Item> = (0..20).map(|n| make(&format!("L{n:02}"), "note", None, "2026-11-13T18:00:00Z", &"word ".repeat(200))).collect();
        db.apply_items(&long, &Delivery { carrier: Carrier::Link, device_id: None, received_at: 1.0 }).unwrap();
        let out = call(&mut db, "list_items", &args(json!({ "kinds": ["note"] })), false).unwrap();
        assert_eq!(out["text"], "preview");
        assert!(out["note"].as_str().unwrap().contains("more than fits") && out["note"].as_str().unwrap().contains("get_items"), "{}", out["note"]);
        // full mode stops at its budget and says where to go on.
        let out = call(&mut db, "list_items", &args(json!({ "kinds": ["note"], "include_text": "full" })), false).unwrap();
        let total: usize = out["items"].as_array().unwrap().iter().filter_map(|i| i["text"].as_str()).map(|t| t.chars().count()).sum();
        assert!(total <= FULL_TEXT_BUDGET);
    }

    #[test]
    fn more_pages_and_empty_answers_are_explained() {
        let mut db = library();
        let out = call(&mut db, "list_items", &args(json!({ "limit": 2 })), false).unwrap();
        assert_eq!(out["more"], true);
        assert!(out["note"].as_str().unwrap().contains("this page has 2 of 6") && out["note"].as_str().unwrap().contains("next_cursor"), "{out}");
        // Empty because of a filter the agent set: said so, with what is there.
        let out = call(&mut db, "list_items", &args(json!({ "when": "2026-11-11", "timezone": "America/Los_Angeles", "kinds": ["meeting"] })), false).unwrap();
        let note = out["note"].as_str().unwrap();
        assert!(note.contains("comes from what you set: kinds [\"meeting\"]") && note.contains("5 items are in"), "{note}");
        // Empty because nothing was said then: said so, with where the library starts and ends.
        let out = call(&mut db, "list_items", &args(json!({ "when": "2026-10-01", "timezone": "America/Los_Angeles" })), false).unwrap();
        assert!(out["note"].as_str().unwrap().starts_with("Nothing was recorded in Thursday, October 1, 2026"), "{}", out["note"]);
        let out = call(&mut db, "list_items", &args(json!({ "unprocessed_only": true, "kinds": ["note"] })), false).unwrap();
        assert_eq!(out["total"], 1);
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
        // The cursor leads to the other match, and then there is no further page.
        let cursor = out["next_cursor"].as_str().unwrap().to_string();
        let rest = call(&mut db, "search_items", &args(json!({ "query": "harbor", "limit": 1, "cursor": cursor })), false).unwrap();
        assert_eq!((rest["returned"].as_i64(), rest["truncated"].as_bool(), &rest["next_cursor"]), (Some(1), Some(false), &Value::Null));
        assert_ne!(ids(&rest), ids(&out));
        assert!(call(&mut db, "search_items", &args(json!({ "query": "harbor", "cursor": "nope" })), false).unwrap_err().to_string().contains("cursor"));
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

    fn limited(kinds: &[&str], hidden: &[&str]) -> Access {
        let list = |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };
        Access { read_only: false, scope: Scope::new(&list(kinds), &list(hidden)).unwrap() }
    }

    /// The library has 3 keyboard dictations, 1 app dictation, 1 note and 1 meeting.
    #[test]
    fn a_limited_view_holds_in_every_tool() {
        let mut db = library();
        let no_keyboard = limited(&[], &["keyboard"]);
        let notes_and_meetings = limited(&["note", "meeting"], &[]);
        let ask = |db: &mut Db, access: &Access, name: &str, a: Value| super::call(db, name, &args(a), access);

        // list_items: the hidden items are not listed and not counted.
        let out = ask(&mut db, &no_keyboard, "list_items", json!({})).unwrap();
        assert_eq!(out["total"], 3);
        assert_eq!(ids(&out), ["F-MEET", "D-APP", "C-NOTE"]);
        assert_eq!(out["total_by_kind"], json!({ "dictation": 1, "meeting": 1, "note": 1 }));
        let out = ask(&mut db, &notes_and_meetings, "list_items", json!({ "order": "oldest" })).unwrap();
        assert_eq!((ids(&out), out["total"].as_i64()), (vec!["C-NOTE".to_string(), "F-MEET".to_string()], Some(2)));
        // Asking for a visible kind works. Asking by name for a hidden one says why.
        assert_eq!(ask(&mut db, &notes_and_meetings, "list_items", json!({ "kinds": ["notes"] })).unwrap()["total"], 1);
        let refused = ask(&mut db, &notes_and_meetings, "list_items", json!({ "kinds": ["dictation"] })).unwrap_err().to_string();
        assert!(refused.contains("does not show dictation items") && refused.contains("limited view"), "{refused}");
        let refused = ask(&mut db, &no_keyboard, "list_items", json!({ "sources": ["Keyboard"] })).unwrap_err().to_string();
        assert!(refused.contains("source Keyboard"), "{refused}");
        assert_eq!(ask(&mut db, &no_keyboard, "list_items", json!({ "sources": ["app"] })).unwrap()["total"], 1);

        // search_items: "harbor" is in a keyboard dictation and in a note.
        let out = ask(&mut db, &no_keyboard, "search_items", json!({ "query": "harbor" })).unwrap();
        assert_eq!((ids(&out), out["total"].as_i64()), (vec!["C-NOTE".to_string()], Some(1)));
        assert_eq!(ask(&mut db, &no_keyboard, "search_items", json!({ "query": "exactly" })).unwrap()["total"], 0);
        // A query with no searchable words takes another path through the library. Same view.
        assert_eq!(ask(&mut db, &no_keyboard, "search_items", json!({ "query": "..." })).unwrap()["total"], 0);
        assert_eq!(ask(&mut db, &notes_and_meetings, "search_items", json!({ "query": "noon" })).unwrap()["total"], 0);

        // get_items: a hidden id answers like one that does not exist.
        let out = ask(&mut db, &no_keyboard, "get_items", json!({ "ids": ["B-NOON", "C-NOTE", "D-APP", "NOPE"] })).unwrap();
        assert_eq!((ids(&out), out["not_found"].clone()), (vec!["C-NOTE".to_string(), "D-APP".to_string()], json!(["B-NOON", "NOPE"])));
        assert!(!out.to_string().contains("exactly at noon"));
        let out = ask(&mut db, &notes_and_meetings, "get_items", json!({ "ids": ["D-APP", "F-MEET"] })).unwrap();
        assert_eq!((ids(&out), out["not_found"].clone()), (vec!["F-MEET".to_string()], json!(["D-APP"])));

        // library_stats: counts and words of the visible items only.
        let out = ask(&mut db, &no_keyboard, "library_stats", json!({ "group_by": "source" })).unwrap();
        assert_eq!(out["total"]["items"], 3);
        assert!(out["groups"].as_array().unwrap().iter().all(|g| g["source"] != "keyboard"), "{out}");
        let out = ask(&mut db, &notes_and_meetings, "library_stats", json!({ "group_by": "kind" })).unwrap();
        assert_eq!((out["total"]["items"].as_i64(), out["groups"].as_array().unwrap().len()), (Some(2), 2));

        // sync_status: the totals are the view's, and it says that it is one.
        let out = ask(&mut db, &no_keyboard, "sync_status", json!({})).unwrap();
        assert_eq!((out["items"]["total"].as_i64(), out["items"]["unprocessed"].as_i64()), (Some(3), Some(3)));
        assert_eq!(out["items"]["by_kind"], json!({ "dictation": 1, "meeting": 1, "note": 1 }));
        assert_eq!(out["items"]["dictations_by_source"], json!({ "app": 1 }));
        assert_eq!(out["items"]["recordings_behind_notes"], 0);
        assert_eq!(out["scope"]["excluded_sources"], json!(["keyboard"]));
        assert!(out["scope"]["note"].as_str().unwrap().contains("limited view"));
        // The oldest visible item is the note, not the keyboard dictation before it.
        assert!(out["items"]["oldest"].as_str().unwrap().contains("2026-11-11"));
        let out = ask(&mut db, &notes_and_meetings, "sync_status", json!({})).unwrap();
        assert_eq!((out["items"]["total"].as_i64(), &out["items"]["dictations_by_source"]), (Some(2), &json!({})));
        assert_eq!(out["scope"]["kinds"], json!(["note", "meeting"]));
        // Without a limit nothing is said about one.
        let out = super::sync_status(&mut db, &Scope::default()).unwrap();
        assert_eq!((out["items"]["total"].as_i64(), out.get("scope")), (Some(6), None));

        // mark_processed: a hidden item cannot be flagged, and is not counted as remaining.
        let out = ask(&mut db, &no_keyboard, "mark_processed", json!({ "ids": ["b-noon", "C-NOTE"] })).unwrap();
        assert_eq!((out["updated"].as_i64(), out["not_found"].clone(), out["unprocessed_remaining"].as_i64()), (Some(1), json!(["B-NOON"]), Some(2)));
        assert_eq!(db.unprocessed_items().unwrap(), 5, "only the note was flagged");
        let out = ask(&mut db, &no_keyboard, "list_items", json!({ "unprocessed_only": true })).unwrap();
        assert_eq!(out["total"], 2);
    }

    #[test]
    fn clips_are_a_kind_agents_can_ask_for_and_a_view_can_hide() {
        let mut db = library();
        let clip = |id: &str, source: &str, time: &str, text: &str| Item {
            id: id.to_string(),
            kind: "clip".to_string(),
            source: Some(source.to_string()),
            created_at: at(time),
            updated_at: at(time),
            text: text.to_string(),
            title: None,
            duration_s: None,
            engine: None,
            status: None,
            extra: "{}".to_string(),
            rev: None,
            deleted: false,
        };
        let clips = [
            clip("G-CLIP", "clipboard", "2026-11-12T19:00:00Z", "https://example.com/harbor-map"),
            clip("H-CLIP", "typed", "2026-11-12T19:05:00Z", "the gate code is four two"),
        ];
        db.apply_items(&clips, &Delivery { carrier: Carrier::Link, device_id: None, received_at: 0.0 }).unwrap();
        let everything = Access::default();
        let ask = |db: &mut Db, access: &Access, name: &str, a: Value| super::call(db, name, &args(a), access);

        // "The thing I just sent you": the newest clip.
        let out = ask(&mut db, &everything, "list_items", json!({ "kinds": ["clip"], "limit": 1 })).unwrap();
        assert_eq!((out["total"].as_i64(), out["items"][0]["id"].as_str(), out["items"][0]["source"].as_str()), (Some(2), Some("H-CLIP"), Some("typed")));
        assert_eq!(out["items"][0]["text"], "the gate code is four two");
        let out = ask(&mut db, &everything, "list_items", json!({ "kinds": "clips", "sources": ["clipboard"] })).unwrap();
        assert_eq!((out["total"].as_i64(), out["items"][0]["id"].as_str()), (Some(1), Some("G-CLIP")));
        let out = ask(&mut db, &everything, "search_items", json!({ "query": "harbor", "kinds": ["clip"] })).unwrap();
        assert_eq!((out["total"].as_i64(), out["items"][0]["id"].as_str()), (Some(1), Some("G-CLIP")));
        let out = ask(&mut db, &everything, "library_stats", json!({ "kinds": ["clip"], "group_by": "source" })).unwrap();
        assert_eq!((out["total"]["items"].as_i64(), out["groups"].as_array().unwrap().len()), (Some(2), 2));
        let out = ask(&mut db, &everything, "library_stats", json!({ "group_by": "kind" })).unwrap();
        assert!(out["groups"].as_array().unwrap().iter().any(|g| g["kind"] == "clip" && g["items"] == 2), "{out}");
        let out = ask(&mut db, &everything, "get_items", json!({ "ids": ["G-CLIP"] })).unwrap();
        assert_eq!(out["items"][0]["text"], "https://example.com/harbor-map");
        assert_eq!(super::sync_status(&mut db, &Scope::default()).unwrap()["items"]["by_kind"]["clip"], 2);

        // Notes only, and any list of kinds without clip, leaves clips out of every tool.
        for view in [limited(&["note"], &[]), limited(&["dictation", "note", "meeting"], &[])] {
            let refused = ask(&mut db, &view, "list_items", json!({ "kinds": ["clip"] })).unwrap_err().to_string();
            assert!(refused.contains("does not show clip items"), "{refused}");
            let out = ask(&mut db, &view, "list_items", json!({})).unwrap();
            assert!(out["items"].as_array().unwrap().iter().all(|i| i["kind"] != "clip"), "{out}");
            assert_eq!(ask(&mut db, &view, "search_items", json!({ "query": "gate" })).unwrap()["total"], 0);
            assert_eq!(ask(&mut db, &view, "get_items", json!({ "ids": ["H-CLIP"] })).unwrap()["not_found"], json!(["H-CLIP"]));
            assert_eq!(ask(&mut db, &view, "mark_processed", json!({ "ids": ["H-CLIP"] })).unwrap()["updated"], 0);
            assert!(ask(&mut db, &view, "sync_status", json!({})).unwrap()["items"]["by_kind"].get("clip").is_none());
            let told = instructions(&view.scope);
            assert!(told.contains("clips") && told.contains("hidden"), "{told}");
        }
        // Only clips: an agent that gets what the person sends and nothing they said.
        let only_clips = limited(&["clip"], &[]);
        let out = ask(&mut db, &only_clips, "list_items", json!({})).unwrap();
        assert_eq!(out["total_by_kind"], json!({ "clip": 2 }));
        // A clip's source can be hidden like a dictation's.
        let no_typed = limited(&[], &["typed"]);
        assert_eq!(ask(&mut db, &no_typed, "list_items", json!({ "kinds": ["clip"] })).unwrap()["total"], 1);
    }

    #[test]
    fn a_limited_view_is_told_to_the_agent() {
        let access = limited(&["note", "meeting"], &["keyboard"]);
        let told = instructions(&access.scope);
        assert!(told.starts_with(INSTRUCTIONS) && told.contains("limited view") && told.contains("only voice notes and meeting transcripts"), "{told}");
        assert_eq!(instructions(&Scope::default()), INSTRUCTIONS);
        let tools = super::definitions(&access);
        let list = tools.iter().find(|t| t.name == "list_items").unwrap();
        assert_eq!(list.input_schema["properties"]["kinds"]["items"]["enum"], json!(["note", "meeting"]));
        assert!(list.description.contains("limited view (only voice notes and meeting transcripts, hides keyboard)"), "{}", list.description);
        for name in ["search_items", "library_stats"] {
            assert!(tools.iter().find(|t| t.name == name).unwrap().description.contains("limited view"), "{name}");
        }
        // The default says nothing of the kind and offers every kind.
        let plain = definitions(false);
        assert!(plain.iter().all(|t| !t.description.contains("limited view")));
        assert_eq!(plain[0].input_schema["properties"]["kinds"]["items"]["enum"], json!(["dictation", "note", "meeting", "clip"]));
        assert!(!told.contains('\u{2014}'));
    }

    #[test]
    fn instructions_explain_the_kinds_plainly() {
        for needle in ["dictation", "note", "meeting", "keyboard", "typed by voice", "voice note", "transcript", "`clip`", "sent to this computer on purpose", "the thing I just sent you"] {
            assert!(INSTRUCTIONS.contains(needle), "instructions should mention {needle}");
        }
        for tool in definitions(false) {
            assert!(!tool.description.contains('\u{2014}'), "{} has an em dash", tool.name);
            assert_eq!(tool.input_schema["type"], "object");
        }
        assert!(!INSTRUCTIONS.contains('\u{2014}'));
    }
}
