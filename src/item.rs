//! One library item as it travels between the phone and the computer (see PROTOCOL.md).

use anyhow::{Result, bail};
use serde_json::{Map, Value};

/// How an item reached this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// Sent by a paired phone over iroh.
    Link,
    /// Read from a Zone backup folder.
    Backup,
    /// Read from the phone's screen by the computer's camera.
    Flash,
}

impl Carrier {
    /// True when the items come from the phone itself, which is the source of truth. A
    /// backup folder is a copy that can be stale.
    pub fn is_the_phone(self) -> bool {
        matches!(self, Carrier::Link | Carrier::Flash)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Carrier::Link => "link",
            Carrier::Backup => "backup",
            Carrier::Flash => "flash",
        }
    }
}

/// A validated incoming item: either a full record or a deletion.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub source: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub text: String,
    pub title: Option<String>,
    pub duration_s: Option<f64>,
    pub engine: Option<String>,
    pub status: Option<String>,
    /// Kind-specific details as a JSON object, serialized.
    pub extra: String,
    pub rev: Option<String>,
    pub deleted: bool,
}

const MAX_ID_LEN: usize = 128;
const MAX_LABEL_LEN: usize = 64;

impl Item {
    /// Reads one item object. Unknown fields are ignored. Errors describe the field that is
    /// wrong and never include the item's text.
    pub fn from_json(value: &Value) -> Result<Item> {
        let Some(object) = value.as_object() else {
            bail!("item is not a JSON object");
        };
        let id = match object.get("id").and_then(Value::as_str) {
            Some(id) if !id.trim().is_empty() && id.len() <= MAX_ID_LEN => id.trim().to_ascii_uppercase(),
            Some(_) => bail!("item id is empty or too long"),
            None => bail!("item has no id"),
        };
        let kind = match object.get("kind").and_then(Value::as_str) {
            Some(kind) if !kind.is_empty() && kind.len() <= MAX_LABEL_LEN => kind.to_string(),
            Some(_) => bail!("item kind is empty or too long"),
            None => bail!("item has no kind"),
        };
        let deleted = object.get("deleted").and_then(Value::as_bool).unwrap_or(false);
        let created = number(object, "created_at");
        let updated = number(object, "updated_at");
        let (created_at, updated_at) = match (created, updated, deleted) {
            (Some(c), Some(u), _) => (c, u),
            (Some(c), None, _) => (c, c),
            // A deletion carries only the time it happened.
            (None, Some(u), true) => (u, u),
            (None, _, false) => bail!("item has no created_at"),
            (None, None, true) => bail!("deletion has no updated_at"),
        };
        let extra = match object.get("extra") {
            Some(Value::Object(map)) if !deleted => Value::Object(map.clone()).to_string(),
            _ => "{}".to_string(),
        };
        Ok(Item {
            id,
            kind,
            source: string(object, "source"),
            created_at,
            updated_at,
            text: if deleted { String::new() } else { string(object, "text").unwrap_or_default() },
            title: if deleted { None } else { string(object, "title") },
            duration_s: number(object, "duration_s"),
            engine: string(object, "engine"),
            status: string(object, "status"),
            extra,
            rev: string(object, "rev"),
            deleted,
        })
    }

    /// The wire form, as the phone would send it.
    #[cfg(test)]
    pub fn to_json(&self) -> Value {
        if self.deleted {
            return serde_json::json!({ "id": self.id, "kind": self.kind, "deleted": true, "updated_at": self.updated_at });
        }
        let extra: Value = serde_json::from_str(&self.extra).unwrap_or_else(|_| serde_json::json!({}));
        serde_json::json!({
            "id": self.id,
            "kind": self.kind,
            "source": self.source,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "text": self.text,
            "title": self.title,
            "duration_s": self.duration_s,
            "engine": self.engine,
            "status": self.status,
            "extra": extra,
            "rev": self.rev,
        })
    }
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// Words as the phone counts them: runs of non-whitespace.
pub fn word_count(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

/// The first `max_chars` characters on one line, with an ellipsis when cut.
pub fn preview(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut last_space = true;
    let mut cut = false;
    for ch in text.chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch == ' ' && last_space {
            continue;
        }
        if count == max_chars {
            cut = true;
            break;
        }
        last_space = ch == ' ';
        out.push(ch);
        count += 1;
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    if cut {
        out.push('\u{2026}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_protocol_example() {
        let value = json!({
            "id": "9f1c2e6a-0000-4000-8000-000000000001",
            "kind": "dictation",
            "source": "keyboard",
            "created_at": 1794445200.25,
            "updated_at": 1794445203.5,
            "text": "the words",
            "title": null,
            "duration_s": 4.2,
            "engine": "fluidMini",
            "status": "finished",
            "extra": {},
            "rev": "5b1f0c9a2d3e4f60",
            "something_new": [1, 2, 3]
        });
        let item = Item::from_json(&value).unwrap();
        assert_eq!(item.id, "9F1C2E6A-0000-4000-8000-000000000001");
        assert_eq!(item.kind, "dictation");
        assert_eq!(item.source.as_deref(), Some("keyboard"));
        assert_eq!(item.created_at, 1794445200.25);
        assert_eq!(item.updated_at, 1794445203.5);
        assert_eq!(item.text, "the words");
        assert_eq!(item.title, None);
        assert_eq!(item.duration_s, Some(4.2));
        assert_eq!(item.rev.as_deref(), Some("5b1f0c9a2d3e4f60"));
        assert!(!item.deleted);
        let again = Item::from_json(&item.to_json()).unwrap();
        assert_eq!(again, item);
    }

    #[test]
    fn reads_a_deletion() {
        let value = json!({ "id": "A", "kind": "dictation", "deleted": true, "updated_at": 1794445300.0 });
        let item = Item::from_json(&value).unwrap();
        assert!(item.deleted);
        assert_eq!(item.updated_at, 1794445300.0);
        assert_eq!(item.created_at, 1794445300.0);
        assert_eq!(item.text, "");
    }

    #[test]
    fn missing_fields() {
        assert!(Item::from_json(&json!({ "kind": "note", "created_at": 1.0 })).is_err());
        assert!(Item::from_json(&json!({ "id": "A", "created_at": 1.0 })).is_err());
        assert!(Item::from_json(&json!({ "id": "A", "kind": "note" })).is_err());
        assert!(Item::from_json(&json!("text")).is_err());
        // updated_at may be left out: it then equals created_at.
        let item = Item::from_json(&json!({ "id": "A", "kind": "note", "created_at": 5.0 })).unwrap();
        assert_eq!(item.updated_at, 5.0);
        assert_eq!(item.extra, "{}");
    }

    #[test]
    fn previews_collapse_whitespace() {
        assert_eq!(preview("  hello\n\n  there   friend ", 100), "hello there friend");
        assert_eq!(preview("abcdef", 3), "abc\u{2026}");
        assert_eq!(word_count("  one two\nthree "), 3);
    }
}
