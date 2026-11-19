//! MCP resources: parts of the library a person can attach to a conversation in clients that
//! offer resources (a paperclip, an @-mention), without the agent calling a tool.
//!
//! - `zone://clip/latest`: the newest clip, "the thing I just sent you".
//! - `zone://notes/open`: voice notes not marked done, as a checklist, with their ids.
//! - `zone://meeting/latest`: the newest meeting transcript.
//! - `zone://today`: everything from today.
//! - `zone://item/{id}` and `zone://day/{day}` (a date, or words such as `yesterday`).
//!
//! Each is Markdown, written by the same code as `zone export`, cut at
//! [`MAX_CHARS`] with a line saying how to read the rest. Each honours the server's view: a
//! resource for a kind the view hides is not listed, and reading it says so.

use anyhow::{Result, bail};

use crate::db::{Db, Filter, Order, Row};
use crate::export::{Format, render};
use crate::scope::Scope;
use crate::timeparse::Clock;

/// The most text one resource returns.
pub const MAX_CHARS: usize = 50_000;
/// The most items one list resource reads.
const MAX_ITEMS: usize = 500;

pub struct Fixed {
    pub uri: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The kinds it shows: listed when the view shows at least one.
    pub kinds: &'static [&'static str],
}

pub const FIXED: &[Fixed] = &[
    Fixed {
        uri: "zone://clip/latest",
        name: "clip-latest",
        title: "Newest clip from the phone",
        description: "The newest text the person sent to this computer on purpose: \"the thing I just sent you\". Exact text, not speech.",
        kinds: &["clip"],
    },
    Fixed {
        uri: "zone://notes/open",
        name: "notes-open",
        title: "Open voice notes",
        description: "Voice notes not yet marked done, oldest first, as a checklist with their ids (for mark_processed).",
        kinds: &["note"],
    },
    Fixed {
        uri: "zone://meeting/latest",
        name: "meeting-latest",
        title: "Latest meeting transcript",
        description: "The newest meeting: title, time and the transcript, one \"Speaker: words\" line per turn.",
        kinds: &["meeting"],
    },
    Fixed {
        uri: "zone://today",
        name: "today",
        title: "Today",
        description: "Everything from today on this computer's clock, grouped by kind. Speech, so expect recognition mistakes.",
        kinds: &["dictation", "note", "meeting", "clip"],
    },
];

pub struct Template {
    pub uri_template: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        uri_template: "zone://item/{id}",
        name: "item",
        title: "One item",
        description: "One dictation, note, meeting or clip in full, by its id from list_items or search_items.",
    },
    Template {
        uri_template: "zone://day/{day}",
        name: "day",
        title: "One day",
        description: "Everything from one day: a date (2026-11-11) or words (yesterday, Wednesday, last Friday).",
    },
];

/// The fixed resources a server with this view lists.
pub fn listed(scope: &Scope) -> Vec<&'static Fixed> {
    FIXED.iter().filter(|r| r.kinds.iter().any(|k| scope.allows_kind(k))).collect()
}

/// Reads a resource as Markdown.
pub fn read(db: &Db, uri: &str, scope: &Scope) -> Result<String> {
    let clock = Clock::system();
    let uri = uri.trim();
    let Some(path) = uri.strip_prefix("zone://") else {
        bail!("\"{uri}\" is not a zone resource. They start with zone://, for example zone://today or zone://item/<id>.");
    };
    let within = |kind: &str| -> Result<()> {
        if !scope.allows_kind(kind) {
            bail!("this server does not show {kind} items: it has a limited view ({}).", scope.short().unwrap_or_default());
        }
        Ok(())
    };
    let newest = |kind: &str, unprocessed: bool, limit: usize| -> Result<Vec<Row>> {
        let filter = Filter { kinds: vec![kind.to_string()], unprocessed_only: unprocessed, scope: scope.clone(), ..Filter::default() };
        db.list(&filter, Order::Newest, limit, None)
    };
    let text = match path.trim_end_matches('/') {
        "clip/latest" => {
            within("clip")?;
            match newest("clip", false, 1)?.pop() {
                Some(row) => format!("Sent {} ({}).\n\n{}\n", clock.full(row.created_at), row.source.as_deref().unwrap_or("clip"), row.text),
                None => "No clip yet. The person sends one from Zone on the iPhone (Send to computer).\n".to_string(),
            }
        }
        "notes/open" => {
            within("note")?;
            let rows = newest("note", true, MAX_ITEMS)?;
            if rows.is_empty() {
                "No open voice notes: every note is marked done, or there are none yet.\n".to_string()
            } else {
                let mut text = render(&rows, &clock, Format::Markdown, Order::Oldest, None);
                let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
                ids.reverse();
                text.push_str(&format!("\nIds, oldest first (mark_processed marks a note done): {}\n", ids.join(", ")));
                text
            }
        }
        "meeting/latest" => {
            within("meeting")?;
            match newest("meeting", false, 1)? {
                rows if rows.is_empty() => "No meeting transcript yet.\n".to_string(),
                rows => format!("{}\nId: {}\n", render(&rows, &clock, Format::Markdown, Order::Oldest, None), rows[0].id),
            }
        }
        "today" => day(db, &clock, "today", scope)?,
        other => match other.split_once('/') {
            Some(("item", id)) => {
                let id = percent_decode(id);
                match db.get(&id)? {
                    Some(row) if !row.deleted && scope.allows(&row.kind, row.source.as_deref()) => {
                        format!("{}\nId: {}\n", render(std::slice::from_ref(&row), &clock, Format::Markdown, Order::Oldest, None), row.id)
                    }
                    // Outside the view answers like an item that does not exist.
                    _ => bail!("no item has the id {id}. Ids come from list_items or search_items."),
                }
            }
            Some(("day", words)) => day(db, &clock, &percent_decode(words), scope)?,
            _ => bail!("there is no resource {uri}. There are zone://clip/latest, zone://notes/open, zone://meeting/latest, zone://today, zone://item/<id> and zone://day/<day>."),
        },
    };
    Ok(cap(text))
}

/// Everything in one day's window, as a dated document.
fn day(db: &Db, clock: &Clock, words: &str, scope: &Scope) -> Result<String> {
    let window = clock.window(words)?;
    let filter = Filter { start: Some(window.start), end: Some(window.end), scope: scope.clone(), fold_note_recordings: true, ..Filter::default() };
    let rows = db.list(&filter, Order::Oldest, MAX_ITEMS, None)?;
    let label = clock.window_words(Some(window.start), Some(window.end));
    if rows.is_empty() {
        return Ok(format!("Nothing was recorded in {label}.\n"));
    }
    let total = db.count(&filter)?;
    let mut text = render(&rows, clock, Format::Markdown, Order::Oldest, Some(&label));
    if total > rows.len() as i64 {
        text.push_str(&format!("\nThe first {} of {total} items. list_items with when \"{words}\" pages through all of them.\n", rows.len()));
    }
    Ok(text)
}

/// Cuts a resource at [`MAX_CHARS`], saying so.
fn cap(text: String) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_CHARS).collect();
    cut.push_str("\n\n(Cut here: this resource is longer than 50,000 characters. get_items reads an item from any offset, and list_items pages through a day.)\n");
    cut
}

/// `%20` and the like in a URI path.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Delivery;
    use crate::item::{Carrier, Item};

    fn item(id: &str, kind: &str, source: Option<&str>, ago: f64, text: &str) -> Item {
        let at = crate::paths::now_unix() - ago;
        Item {
            id: id.to_string(),
            kind: kind.to_string(),
            source: source.map(str::to_string),
            created_at: at,
            updated_at: at,
            text: text.to_string(),
            title: (kind == "meeting").then(|| "Roadmap".to_string()),
            duration_s: None,
            engine: None,
            status: None,
            extra: "{}".to_string(),
            rev: None,
            deleted: false,
        }
    }

    fn library() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        let items = [
            item("CLIP-OLD", "clip", Some("clipboard"), 600.0, "https://example.com/old"),
            item("CLIP-NEW", "clip", Some("share"), 60.0, "https://example.com/new"),
            item("NOTE-1", "note", None, 300.0, "buy cedar boards"),
            item("NOTE-2", "note", None, 200.0, "call the plumber"),
            item("MEET", "meeting", None, 100.0, "Ada: we ship friday.\nBo: agreed."),
            item("DICT", "dictation", Some("keyboard"), 50.0, "on my way"),
        ];
        db.apply_items(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: 1.0 }).unwrap();
        db.mark_processed(&["NOTE-1".to_string()], true, 1.0).unwrap();
        db
    }

    #[test]
    fn each_resource_reads_what_it_says() {
        let db = library();
        let all = Scope::default();
        let clip = read(&db, "zone://clip/latest", &all).unwrap();
        assert!(clip.contains("https://example.com/new") && !clip.contains("/old") && clip.contains("(share)"), "{clip}");
        let notes = read(&db, "zone://notes/open", &all).unwrap();
        assert!(notes.contains("call the plumber") && !notes.contains("cedar") && notes.contains("NOTE-2"), "{notes}");
        let meeting = read(&db, "zone://meeting/latest", &all).unwrap();
        assert!(meeting.contains("Roadmap") && meeting.contains("we ship friday") && meeting.contains("Id: MEET"), "{meeting}");
        let item = read(&db, "zone://item/dict", &all).unwrap();
        assert!(item.contains("on my way"), "{item}");
        assert!(read(&db, "zone://item/NOPE", &all).unwrap_err().to_string().contains("no item"));
        assert!(read(&db, "zone://nothing", &all).unwrap_err().to_string().contains("zone://today"));
        assert!(read(&db, "https://example.com", &all).is_err());
        let day = read(&db, "zone://day/2001-01-01", &all).unwrap();
        assert!(day.starts_with("Nothing was recorded in Monday, January 1, 2001"), "{day}");
        assert!(read(&db, "zone://day/whenever", &all).is_err());
        assert_eq!(listed(&all).len(), 4);
        assert_eq!(percent_decode("last%20Friday"), "last Friday");
    }

    #[test]
    fn a_limited_view_holds() {
        let db = library();
        let notes = Scope::new(&["note".to_string()], &[]).unwrap();
        assert_eq!(listed(&notes).iter().map(|r| r.uri).collect::<Vec<_>>(), ["zone://notes/open", "zone://today"]);
        assert!(read(&db, "zone://clip/latest", &notes).unwrap_err().to_string().contains("limited view"));
        // Hidden items are not read by id, and not in a day.
        assert!(read(&db, "zone://item/DICT", &notes).is_err());
        let no_keyboard = Scope::new(&[], &["keyboard".to_string()]).unwrap();
        assert!(read(&db, "zone://item/DICT", &no_keyboard).is_err());
        assert!(!cap("x".repeat(MAX_CHARS + 10)).starts_with(&"x".repeat(MAX_CHARS + 1)));
    }
}
