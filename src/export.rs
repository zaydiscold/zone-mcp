//! Writing items out for people: Markdown, plain text, JSON, JSON Lines and CSV.
//!
//! `zone export` and the page's Export share this. The layout follows what is being exported:
//! notes alone become a checklist (open and done), one meeting becomes a transcript under a
//! header, and anything else a dated document, each day's items grouped by kind.

use anyhow::{Result, bail};
use serde_json::Value;

use crate::cli::item_json;
use crate::db::{Order, Row};
use crate::timeparse::Clock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Text,
    Json,
    Jsonl,
    Csv,
}

impl Format {
    pub fn parse(name: &str) -> Result<Format> {
        match name.trim().to_ascii_lowercase().as_str() {
            "md" | "markdown" => Ok(Format::Markdown),
            "txt" | "text" => Ok(Format::Text),
            "json" => Ok(Format::Json),
            "jsonl" | "ndjson" => Ok(Format::Jsonl),
            "csv" => Ok(Format::Csv),
            other => bail!("the format must be md, txt, json, jsonl or csv, not {other}"),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Text => "txt",
            Format::Json => "json",
            Format::Jsonl => "jsonl",
            Format::Csv => "csv",
        }
    }
}

/// Which layout a set of items gets in Markdown and plain text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Only notes: a checklist.
    Checklist,
    /// One meeting: its transcript.
    Meeting,
    /// Anything else: a document by day.
    Dated,
}

impl Shape {
    pub fn of(rows: &[Row]) -> Shape {
        if !rows.is_empty() && rows.iter().all(|row| row.kind == "note") {
            Shape::Checklist
        } else if rows.len() == 1 && rows[0].kind == "meeting" {
            Shape::Meeting
        } else {
            Shape::Dated
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Checklist => "checklist",
            Shape::Meeting => "meeting",
            Shape::Dated => "dated",
        }
    }
}

/// Where a dictation was spoken or how a clip was sent, in plain words.
pub fn source_label(source: &str) -> &str {
    match source {
        "keyboard" => "Zone keyboard",
        "app" => "In the Zone app",
        "note" => "Voice note recording",
        "remote" => "Remote",
        "clipboard" => "From the clipboard",
        "share" => "Shared",
        "shortcut" => "Shortcut",
        "typed" => "Typed",
        other => other,
    }
}

fn kind_heading(kind: &str) -> String {
    match kind {
        "note" => "Notes".to_string(),
        "meeting" => "Meetings".to_string(),
        "dictation" => "Dictations".to_string(),
        "clip" => "Clips".to_string(),
        other => other.to_string(),
    }
}

/// One turn of a meeting: who spoke, when it began (seconds into the recording), the words.
pub struct Turn {
    pub speaker: Option<String>,
    pub start: Option<f64>,
    pub text: String,
}

/// A meeting's turns, from `extra.segments` when it has them and from its text lines when
/// not. Segments in a row by the same speaker are one turn.
pub fn turns(row: &Row) -> Vec<Turn> {
    let extra: Value = serde_json::from_str(&row.extra).unwrap_or(Value::Null);
    let mut turns: Vec<Turn> = Vec::new();
    let mut push = |speaker: Option<String>, start: Option<f64>, text: &str| {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match turns.last_mut() {
            Some(last) if last.speaker == speaker && speaker.is_some() => {
                last.text.push(' ');
                last.text.push_str(text);
            }
            _ => turns.push(Turn { speaker, start, text: text.to_string() }),
        }
    };
    match extra.get("segments").and_then(Value::as_array).filter(|segments| !segments.is_empty()) {
        Some(segments) => {
            for segment in segments {
                let speaker = segment.get("speaker").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
                push(speaker, segment.get("start").and_then(Value::as_f64), segment.get("text").and_then(Value::as_str).unwrap_or(""));
            }
        }
        None => {
            for line in row.text.lines() {
                match line.split_once(": ").filter(|(name, _)| !name.is_empty() && name.chars().count() <= 40 && !name.contains('.')) {
                    Some((name, words)) => push(Some(name.to_string()), None, words),
                    None => push(None, None, line),
                }
            }
        }
    }
    turns
}

/// The people who speak in a meeting, in the order they first do.
pub fn speakers(row: &Row) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for turn in turns(row) {
        if let Some(name) = turn.speaker
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// `1:05` or `1:02:05` into a recording.
pub fn stamp(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as i64;
    if total >= 3600 { format!("{}:{:02}:{:02}", total / 3600, total % 3600 / 60, total % 60) } else { format!("{}:{:02}", total / 60, total % 60) }
}

/// How long a recording is, in words: `40 s`, `14 min`, `1 h 5 min`.
pub fn length(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as i64;
    if total < 60 {
        format!("{} s", total.max(1))
    } else if total < 3600 {
        format!("{} min", (total as f64 / 60.0).round() as i64)
    } else {
        format!("{} h {} min", total / 3600, total % 3600 / 60)
    }
}

fn and_list(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A file name for an export: `zone-notes-2026-11-17.md`.
pub fn file_name(rows: &[Row], clock: &Clock, format: Format) -> String {
    let today = clock.day(crate::timeparse::unix(clock.now));
    let what = match Shape::of(rows) {
        Shape::Checklist => "notes".to_string(),
        Shape::Meeting => {
            let title: String = rows[0].title.as_deref().unwrap_or("").chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
            let slug = title.split('-').filter(|part| !part.is_empty()).take(6).collect::<Vec<_>>().join("-");
            if slug.is_empty() { "meeting".to_string() } else { format!("meeting-{slug}") }
        }
        Shape::Dated => "library".to_string(),
    };
    format!("zone-{what}-{today}.{}", format.extension())
}

/// Writes `rows` out in `format`, in `order`. `heading` names a dated document.
pub fn render(rows: &[Row], clock: &Clock, format: Format, order: Order, heading: Option<&str>) -> String {
    let mut rows: Vec<&Row> = rows.iter().collect();
    rows.sort_by(|a, b| a.created_at.total_cmp(&b.created_at).then(a.n.cmp(&b.n)));
    if order == Order::Newest {
        rows.reverse();
    }
    match format {
        Format::Jsonl => rows.iter().map(|row| format!("{}\n", item_json(row, clock))).collect(),
        Format::Json => {
            let list: Vec<Value> = rows.iter().map(|row| item_json(row, clock)).collect();
            format!("{}\n", serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".to_string()))
        }
        Format::Csv => csv(&rows, clock),
        Format::Markdown | Format::Text => {
            let owned: Vec<Row> = rows.iter().map(|row| (*row).clone()).collect();
            let markdown = format == Format::Markdown;
            match Shape::of(&owned) {
                Shape::Checklist => checklist(&rows, clock, order, markdown),
                Shape::Meeting => meeting(rows[0], clock, markdown, 1),
                Shape::Dated => dated(&rows, clock, order, heading, markdown),
            }
        }
    }
}

fn exported_line(clock: &Clock, order: Order) -> String {
    let now = crate::timeparse::unix(clock.now);
    format!("Exported {} ({}), {} first.", clock.short(now), clock.zone_name(), if order == Order::Newest { "newest" } else { "oldest" })
}

fn heading(level: usize, text: &str, markdown: bool) -> String {
    if markdown { format!("{} {text}\n\n", "#".repeat(level)) } else if level == 1 { format!("{}\n{}\n\n", text.to_uppercase(), "=".repeat(text.chars().count())) } else { format!("{text}\n{}\n\n", "-".repeat(text.chars().count())) }
}

/// One checklist line: the box, the text with later lines indented under it, and when.
fn check_line(row: &Row, clock: &Clock, markdown: bool) -> String {
    let done = row.processed_at.is_some();
    let lead = match (markdown, done) {
        (true, true) => "- [x] ",
        (true, false) => "- [ ] ",
        (false, true) => "[x] ",
        (false, false) => "[ ] ",
    };
    let indent = " ".repeat(if markdown { 6 } else { 4 });
    let mut out = String::from(lead);
    let mut lines = row.text.trim().lines().map(str::trim_end).filter(|line| !line.trim().is_empty());
    out.push_str(lines.next().unwrap_or("(empty)"));
    out.push('\n');
    for line in lines {
        out.push_str(&format!("{indent}{line}\n"));
    }
    let when = clock.short(row.created_at);
    out.push_str(&if markdown { format!("{indent}*{when}*\n") } else { format!("{indent}{when}\n") });
    out
}

fn checklist(rows: &[&Row], clock: &Clock, order: Order, markdown: bool) -> String {
    let open = rows.iter().filter(|row| row.processed_at.is_none()).count();
    let mut out = heading(1, "Notes", markdown);
    out.push_str(&format!("{}, {open} open. {}\n\n", count(rows.len(), "note", "notes"), exported_line(clock, order)));
    for row in rows {
        out.push_str(&check_line(row, clock, markdown));
    }
    out
}

fn meeting(row: &Row, clock: &Clock, markdown: bool, level: usize) -> String {
    let title = row.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or("Meeting");
    let mut out = heading(level, title, markdown);
    let names = speakers(row);
    let mut facts = vec![format!("{} at {}", clock.day_label(row.created_at), clock.time_of_day(row.created_at))];
    if let Some(seconds) = row.duration_s {
        facts.push(length(seconds));
    }
    if !names.is_empty() {
        facts.push(and_list(&names));
    }
    facts.push(count(row.words as usize, "word", "words"));
    out.push_str(&format!("{}\n\n", facts.join(" \u{b7} ")));
    for turn in turns(row) {
        let at = turn.start.map(|s| format!(" ({})", stamp(s))).unwrap_or_default();
        match (&turn.speaker, markdown) {
            (Some(name), true) => out.push_str(&format!("**{name}**{at}\n{}\n\n", turn.text)),
            (Some(name), false) => out.push_str(&format!("{name}{at}:\n{}\n\n", turn.text)),
            (None, _) => out.push_str(&format!("{}\n\n", turn.text)),
        }
    }
    out
}

fn dated(rows: &[&Row], clock: &Clock, order: Order, title: Option<&str>, markdown: bool) -> String {
    let mut out = heading(1, title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or("Zone library"), markdown);
    out.push_str(&format!("{}. {}\n\n", count(rows.len(), "item", "items"), exported_line(clock, order)));
    let mut at = 0;
    while at < rows.len() {
        let day = clock.day(rows[at].created_at);
        let end = rows[at..].iter().position(|row| clock.day(row.created_at) != day).map(|n| at + n).unwrap_or(rows.len());
        out.push_str(&heading(2, &clock.day_label(rows[at].created_at), markdown));
        let today = &rows[at..end];
        let mut kinds: Vec<&str> = vec!["note", "meeting", "dictation", "clip"];
        for row in today {
            if !kinds.contains(&row.kind.as_str()) {
                kinds.push(&row.kind);
            }
        }
        for kind in kinds {
            let of_kind: Vec<&&Row> = today.iter().filter(|row| row.kind == kind).collect();
            if of_kind.is_empty() {
                continue;
            }
            out.push_str(&heading(3, &kind_heading(kind), markdown));
            for row in of_kind {
                match kind {
                    "note" => out.push_str(&check_line(row, clock, markdown)),
                    "meeting" => out.push_str(&meeting(row, clock, markdown, 4)),
                    _ => {
                        let time = clock.time_of_day(row.created_at);
                        let from = row.source.as_deref().map(|s| format!(" ({})", source_label(s))).unwrap_or_default();
                        let text = row.text.trim().replace('\n', if markdown { "\n  " } else { "\n    " });
                        out.push_str(&if markdown { format!("- **{time}**{from} {text}\n") } else { format!("{time}{from}\n    {text}\n") });
                    }
                }
            }
            out.push('\n');
        }
        at = end;
    }
    out
}

fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text.to_string() }
}

/// A field whose words came from a phone. A spreadsheet takes a cell that begins with `=`,
/// `+`, `-` or `@` (or a tab or a return before one) as a formula, and a formula can fetch an
/// address or start a program. Such a field gets an apostrophe in front, which a spreadsheet
/// shows as text and does not run. It is the one place an export changes what was said.
fn csv_words(text: &str) -> String {
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) { csv_field(&format!("'{text}")) } else { csv_field(text) }
}

fn csv(rows: &[&Row], clock: &Clock) -> String {
    let mut out = String::from("id,kind,source,created,done,duration_s,words,title,text\r\n");
    for row in rows {
        let fields = [
            row.id.clone(),
            row.kind.clone(),
            row.source.clone().unwrap_or_default(),
            clock.rfc3339(row.created_at),
            if row.processed_at.is_some() { "yes" } else { "no" }.to_string(),
            row.duration_s.map(|s| format!("{s}")).unwrap_or_default(),
            row.words.to_string(),
            row.title.clone().unwrap_or_default(),
            row.text.clone(),
        ];
        // The created time, done, the duration and the word count are zone's own.
        let field = |(index, field): (usize, &String)| if (3..=6).contains(&index) { csv_field(field) } else { csv_words(field) };
        out.push_str(&fields.iter().enumerate().map(field).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(n: i64, kind: &str, created_at: f64, text: &str) -> Row {
        Row {
            n,
            id: format!("E0000000-0000-4000-8000-{n:012X}"),
            kind: kind.to_string(),
            source: (kind == "dictation").then(|| "keyboard".to_string()),
            created_at,
            updated_at: created_at,
            text: text.to_string(),
            title: None,
            duration_s: None,
            engine: None,
            status: None,
            extra: "{}".to_string(),
            rev: None,
            deleted: false,
            processed_at: None,
            device_id: None,
            carrier: "link".to_string(),
            received_at: created_at,
            words: crate::item::word_count(text),
        }
    }

    fn clock() -> Clock {
        let mut clock = Clock::in_zone(Some("America/Los_Angeles")).unwrap();
        clock.now = crate::timeparse::timestamp(1_794_969_600.0);
        clock
    }

    #[test]
    fn notes_are_a_checklist() {
        let mut done = row(1, "note", 1_794_960_000.0, "Call the plumber");
        done.processed_at = Some(1.0);
        let rows = vec![done, row(2, "note", 1_794_963_600.0, "Buy cedar boards\nand the long screws")];
        assert_eq!(Shape::of(&rows), Shape::Checklist);
        let md = render(&rows, &clock(), Format::Markdown, Order::Newest, None);
        assert!(md.starts_with("# Notes\n\n2 notes, 1 open. Exported Tue 2026-11-17 18:40 (America/Los_Angeles), newest first.\n\n"), "{md}");
        let first = md.find("- [ ] Buy cedar boards\n      and the long screws\n      *Tue 2026-11-17 17:00*\n").expect(&md);
        let second = md.find("- [x] Call the plumber\n").expect(&md);
        assert!(first < second);
        let oldest = render(&rows, &clock(), Format::Markdown, Order::Oldest, None);
        assert!(oldest.find("[x] Call").unwrap() < oldest.find("[ ] Buy").unwrap());
        let text = render(&rows, &clock(), Format::Text, Order::Oldest, None);
        assert!(text.starts_with("NOTES\n=====\n") && text.contains("\n[x] Call the plumber\n") && !text.contains('*') && !text.contains('#'), "{text}");
        assert_eq!(file_name(&rows, &clock(), Format::Markdown), "zone-notes-2026-11-17.md");
    }

    #[test]
    fn one_meeting_is_a_transcript() {
        let mut meeting = row(3, "meeting", 1_794_960_000.0, "Ada: hello there\nAda: and welcome\nGrace: thank you");
        meeting.title = Some("Roof: plan & budget".to_string());
        meeting.duration_s = Some(840.0);
        meeting.extra = json!({ "segments": [
            { "speaker": "Ada", "start": 0.0, "end": 2.0, "text": "hello there" },
            { "speaker": "Ada", "start": 2.0, "end": 4.0, "text": "and welcome" },
            { "speaker": "Grace", "start": 65.5, "end": 70.0, "text": "thank you" },
            { "speaker": null, "start": 3700.0, "end": 3701.0, "text": "(laughter)" },
        ] })
        .to_string();
        let rows = vec![meeting.clone()];
        assert_eq!(Shape::of(&rows), Shape::Meeting);
        assert_eq!(speakers(&meeting), ["Ada", "Grace"]);
        let md = render(&rows, &clock(), Format::Markdown, Order::Oldest, None);
        assert_eq!(
            md,
            "# Roof: plan & budget\n\nTuesday, November 17, 2026 at 16:00 \u{b7} 14 min \u{b7} Ada and Grace \u{b7} 9 words\n\n\
             **Ada** (0:00)\nhello there and welcome\n\n**Grace** (1:05)\nthank you\n\n(laughter)\n\n"
        );
        assert_eq!(file_name(&rows, &clock(), Format::Text), "zone-meeting-roof-plan-budget-2026-11-17.txt");
        // Without segments the lines of the text are the turns.
        meeting.extra = "{}".to_string();
        let plain = render(&[meeting], &clock(), Format::Text, Order::Oldest, None);
        assert!(plain.contains("Ada:\nhello there and welcome\n\nGrace:\nthank you\n"), "{plain}");
    }

    #[test]
    fn a_mix_is_a_document_by_day_and_kind() {
        let mut meeting = row(4, "meeting", 1_794_873_600.0, "Ada: one");
        meeting.title = Some("Standup".to_string());
        let rows = vec![
            row(1, "dictation", 1_794_960_000.0, "typed by voice"),
            row(2, "note", 1_794_963_600.0, "a note"),
            row(3, "clip", 1_794_963_700.0, "https://example.invalid/a,b"),
            meeting,
        ];
        assert_eq!(Shape::of(&rows), Shape::Dated);
        let md = render(&rows, &clock(), Format::Markdown, Order::Oldest, Some("Last two days"));
        let order: Vec<usize> = ["# Last two days", "## Monday, November 16, 2026", "### Meetings", "#### Standup", "## Tuesday, November 17, 2026", "### Notes", "- [ ] a note", "### Dictations", "- **16:00** (Zone keyboard) typed by voice", "### Clips"]
            .iter()
            .map(|needle| md.find(needle).unwrap_or_else(|| panic!("{needle} missing in\n{md}")))
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{md}");
        assert!(md.contains("4 items. Exported"));

        let csv = render(&rows, &clock(), Format::Csv, Order::Oldest, None);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "id,kind,source,created,done,duration_s,words,title,text");
        assert_eq!(lines[1], "E0000000-0000-4000-8000-000000000004,meeting,,2026-11-16T16:00:00-08:00,no,,2,Standup,Ada: one");
        assert!(lines[4].ends_with(",clip,,2026-11-17T17:01:40-08:00,no,,1,,\"https://example.invalid/a,b\""), "{}", lines[4]);
        assert_eq!(csv_field("say \"hi\"\nnow"), "\"say \"\"hi\"\"\nnow\"");
        // Nothing a phone sent is a formula to a spreadsheet.
        for start in ["=HYPERLINK(\"http://example.invalid\",\"x\")", "+1 555 0100", "-2+3", "@SUM(A1)", "\t=1+1"] {
            assert!(csv_words(start).trim_start_matches('"').starts_with('\''), "{start}");
        }
        assert_eq!(csv_words("=1+1,2"), "\"'=1+1,2\"");
        assert_eq!(csv_words("a = b - c"), "a = b - c");
        let hostile = Row { title: Some("=cmd|' /C calc'!A0".into()), text: "@SUM(1)".into(), source: Some("+x".into()), ..rows[0].clone() };
        let line = render(&[hostile], &clock(), Format::Csv, Order::Oldest, None);
        assert!(line.contains(",'+x,") && line.contains(",'=cmd|' /C calc'!A0,'@SUM(1)"), "{line}");

        let json: Value = serde_json::from_str(&render(&rows, &clock(), Format::Json, Order::Newest, None)).unwrap();
        assert_eq!((json.as_array().unwrap().len(), json[0]["kind"].as_str()), (4, Some("clip")));
        let jsonl = render(&rows, &clock(), Format::Jsonl, Order::Oldest, None);
        assert_eq!(jsonl.lines().count(), 4);
        assert!(Format::parse("pdf").is_err());
        assert_eq!(Format::parse("Markdown").unwrap(), Format::Markdown);
        assert_eq!(stamp(3725.9), "1:02:05");
    }
}
