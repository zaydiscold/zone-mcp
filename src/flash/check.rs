//! The flash's own account of what it carries, and the computer's check of it.
//!
//! A phone may put a summary beside the items (PROTOCOL.md, "Carrier 2: Flash"):
//!
//! ```json
//! "check": {"items": 1764, "oldest": 1741000000.25, "newest": 1794800000.5, "digest": "<64 hex>"}
//! ```
//!
//! `items` is how many entries the item list has. `oldest` and `newest` are the smallest and
//! largest `created_at` of the entries that are not deletions. `digest` is SHA-256, in lowercase
//! hex, over these lines for each entry in order, each line ended by a line feed:
//!
//! - `id`, then `kind`, as sent (`-` when absent or not a string);
//! - `1` when `deleted` is true, else `0`;
//! - `created_at`, then `updated_at`: the 16 lowercase hex digits of the number's IEEE 754
//!   binary64 bits, minus zero read as zero (`-` when absent or null);
//! - the lowercase hex SHA-256 of `text` as UTF-8 (`-` when absent or null).
//!
//! The times go in as bits, so a time that changed in its last binary digit on the way (in
//! printing or in parsing) does not match. Older phones send no summary; the computer then
//! checks what it can on its own. Either way, after storing, every item is read back from the
//! library and compared with what arrived.

use std::collections::HashMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::db::Row;
use crate::item::Item;
use crate::paths::hex;

/// What the phone said it sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Declared {
    pub items: Option<u64>,
    pub oldest: Option<f64>,
    pub newest: Option<f64>,
    pub digest: Option<String>,
}

/// What the computer found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    /// Entries in the flash's item list.
    pub items: usize,
    /// The phone's summary, when it sent one.
    pub declared: Option<Declared>,
    /// Different ids among the entries.
    pub distinct: usize,
    /// Entries whose id came earlier in the same flash. The later one is what is kept.
    pub duplicates: usize,
    /// The span of `created_at` over the entries that are not deletions.
    pub oldest: Option<f64>,
    pub newest: Option<f64>,
    /// Items read back from the library after storing, and how many matched what arrived.
    pub read_back: usize,
    pub read_back_matching: usize,
    /// What did not check out, in words for the person. Empty when everything did.
    pub problems: Vec<String>,
}

/// 1764 as "1,764".
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn time_bits(value: Option<&Value>) -> String {
    match value.and_then(Value::as_f64) {
        // Adding zero turns minus zero into zero and leaves everything else as it is.
        Some(seconds) => format!("{:016x}", (seconds + 0.0).to_bits()),
        None => "-".to_string(),
    }
}

fn label(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("-")
}

/// The digest of an item list, as described at the top of this file.
pub fn digest(items: &[Value]) -> String {
    let mut all = Sha256::new();
    for entry in items {
        let field = |key: &str| entry.get(key);
        let text = match field("text").and_then(Value::as_str) {
            Some(text) => hex(Sha256::digest(text.as_bytes()).as_slice()),
            None => "-".to_string(),
        };
        let deleted = if field("deleted").and_then(Value::as_bool) == Some(true) { "1" } else { "0" };
        let lines = [label(field("id")), label(field("kind")), deleted, &time_bits(field("created_at")), &time_bits(field("updated_at")), &text];
        for line in lines {
            all.update(line.as_bytes());
            all.update(b"\n");
        }
    }
    hex(all.finalize().as_slice())
}

fn same_time(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a + 0.0).to_bits() == (b + 0.0).to_bits(),
        (None, None) => true,
        _ => false,
    }
}

/// Checks the item list of a flash against the phone's summary, when there is one. `flash` is
/// the whole flash object; `items` its item list.
pub fn examine(flash: &Value, items: &[Value]) -> Check {
    let mut check = Check { items: items.len(), ..Check::default() };
    let mut seen: HashMap<String, usize> = HashMap::new();
    for entry in items {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            let count = seen.entry(id.trim().to_ascii_uppercase()).or_default();
            if *count > 0 {
                check.duplicates += 1;
            }
            *count += 1;
        }
        if entry.get("deleted").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(created) = entry.get("created_at").and_then(Value::as_f64).filter(|t| t.is_finite()) {
            check.oldest = Some(check.oldest.map_or(created, |t| t.min(created)));
            check.newest = Some(check.newest.map_or(created, |t| t.max(created)));
        }
    }
    check.distinct = seen.len();

    let Some(summary) = flash.get("check").filter(|v| v.is_object()) else {
        return check;
    };
    let declared = Declared {
        items: summary.get("items").and_then(Value::as_u64),
        oldest: summary.get("oldest").and_then(Value::as_f64),
        newest: summary.get("newest").and_then(Value::as_f64),
        digest: summary.get("digest").and_then(Value::as_str).map(str::to_ascii_lowercase),
    };
    if let Some(said) = declared.items
        && said != items.len() as u64
    {
        check.problems.push(format!(
            "the phone said it sent {} {} and {} arrived.",
            thousands(said as usize),
            if said == 1 { "item" } else { "items" },
            thousands(items.len())
        ));
    }
    if summary.get("oldest").is_some() && !same_time(declared.oldest, check.oldest)
        || summary.get("newest").is_some() && !same_time(declared.newest, check.newest)
    {
        check.problems.push("the times of the oldest and newest items are not the ones the phone sent.".to_string());
    }
    if let Some(said) = &declared.digest
        && *said != digest(items)
    {
        check.problems.push("the items are not exactly what the phone sealed: an id, a time or a text differs.".to_string());
    }
    check.declared = Some(declared);
    check
}

/// True when a row read back from the library holds what `item` carried.
pub fn stored_as_sent(row: &Row, item: &Item) -> bool {
    let bits = |t: f64| (t + 0.0).to_bits();
    if row.id != item.id || row.kind != item.kind || row.deleted != item.deleted || bits(row.updated_at) != bits(item.updated_at) {
        return false;
    }
    if item.deleted {
        return row.text.is_empty();
    }
    bits(row.created_at) == bits(item.created_at)
        && row.source == item.source
        && row.text == item.text
        && row.title == item.title
        && row.duration_s.map(bits) == item.duration_s.map(bits)
        && row.engine == item.engine
        && row.status == item.status
        && row.extra == item.extra
        && (item.rev.is_none() || row.rev == item.rev)
}

impl Check {
    /// After storing: reads every item back (the last of each id, which is the one kept) and
    /// notes how many hold exactly what arrived.
    pub fn read_back(&mut self, items: &[Item], get: impl Fn(&str) -> Option<Row>) {
        let mut last: HashMap<&str, &Item> = HashMap::new();
        for item in items {
            last.insert(item.id.as_str(), item);
        }
        self.read_back = last.len();
        self.read_back_matching = last.values().filter(|item| get(&item.id).is_some_and(|row| stored_as_sent(&row, item))).count();
        let differing = self.read_back - self.read_back_matching;
        if differing > 0 {
            self.problems.push(format!(
                "{} {} not read back from the library as {} arrived.",
                thousands(differing),
                if differing == 1 { "item was" } else { "items were" },
                if differing == 1 { "it" } else { "they" }
            ));
        }
    }

    pub fn all_checked(&self) -> bool {
        self.problems.is_empty()
    }

    /// For the page and the feed. No item's text.
    pub fn to_json(&self) -> Value {
        json!({
            "items": self.items,
            "declared": self.declared.as_ref().and_then(|d| d.items),
            "distinct": self.distinct,
            "duplicates": self.duplicates,
            "oldest": self.oldest,
            "newest": self.newest,
            "read_back": self.read_back,
            "all_checked": self.all_checked(),
            "problem": if self.problems.is_empty() { Value::Null } else { json!(self.problems.join(" ")) },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_of(items: &[Value]) -> Value {
        let live: Vec<f64> =
            items.iter().filter(|i| i["deleted"] != json!(true)).filter_map(|i| i["created_at"].as_f64()).collect();
        json!({
            "items": items.len(),
            "oldest": live.iter().copied().reduce(f64::min),
            "newest": live.iter().copied().reduce(f64::max),
            "digest": digest(items),
        })
    }

    #[test]
    fn thousands_reads_like_a_person_writes() {
        assert_eq!(
            [0, 7, 999, 1000, 1764, 65535, 1_000_000].map(thousands),
            ["0", "7", "999", "1,000", "1,764", "65,535", "1,000,000"]
        );
    }

    /// The JSON text of the check vector, as `scripts/flash_reference.py` has it. The phone's
    /// tests reproduce the same digest from items they encode (ZoneCore, LinkFlashCheckTests).
    pub const VECTOR: &str = concat!(
        r#"[{"created_at":1794445200.25,"duration_s":4.2,"engine":"fluidMini","extra":{},"#,
        r#""id":"9F1C2E6A-0000-4000-8000-000000000001","kind":"dictation","rev":"5b1f0c9a2d3e4f60","#,
        r#""source":"keyboard","status":"finished","text":"the words","title":null,"updated_at":1794445203.5},"#,
        r#"{"created_at":1804773829.0427866,"id":"9F1C2E6A-0000-4000-8000-000000000002","kind":"note","#,
        r#""text":"caf\u00e9 \ud83d\ude00 \u05e9\u05dc\u05d5\u05dd a\u0000b","updated_at":1804773829.0427866},"#,
        r#"{"deleted":true,"id":"9F1C2E6A-0000-4000-8000-000000000003","kind":"dictation","updated_at":1794445300}]"#
    );
    pub const VECTOR_DIGEST: &str = "9bbd47a61c92a30de9cbd968fa4de4bed8e6d230d17ed7dabbfa6a63f3d02a6f";

    #[test]
    fn the_digest_vector() {
        // Parsed from text, so the number parser is part of what is checked: 1804773829.0427866
        // is one of the times a parser that does not round correctly reads one digit off.
        let items: Value = serde_json::from_str(VECTOR).unwrap();
        let items = items.as_array().unwrap();
        assert_eq!(items[1]["created_at"].as_f64().unwrap().to_bits(), 1804773829.0427866f64.to_bits());
        assert_eq!(items[1]["text"], "caf\u{e9} \u{1f600} \u{5e9}\u{5dc}\u{5d5}\u{5dd} a\u{0}b");
        assert_eq!(digest(items), VECTOR_DIGEST);
    }

    #[test]
    fn a_flash_that_matches_its_summary_checks_out() {
        let items: Vec<Value> = crate::fakephone::synthetic_items(0, 40);
        let flash = json!({ "t": "flash", "v": 1, "items": items, "check": summary_of(&items) });
        let check = examine(&flash, &items);
        assert!(check.all_checked(), "{:?}", check.problems);
        assert_eq!((check.items, check.distinct, check.duplicates), (40, 40, 0));
        assert_eq!(check.declared.as_ref().and_then(|d| d.items), Some(40));
        // Without a summary there is nothing to compare, and nothing is wrong.
        let bare = examine(&json!({ "t": "flash", "v": 1, "items": items }), &items);
        assert!(bare.all_checked() && bare.declared.is_none());
        assert_eq!((bare.oldest, bare.newest), (check.oldest, check.newest));
    }

    #[test]
    fn a_count_a_time_or_a_text_that_differs_is_found() {
        let items: Vec<Value> = crate::fakephone::synthetic_items(0, 5);
        let summary = summary_of(&items);
        let with = |changed: Vec<Value>| examine(&json!({ "items": changed, "check": summary }), &changed);
        // One item fewer.
        assert!(with(items[..4].to_vec()).problems.iter().any(|p| p.contains("said it sent 5 items and 4 arrived")));
        // A time one binary digit off: the kind of change a lossy number parser makes.
        let mut nudged = items.clone();
        let t = nudged[2]["created_at"].as_f64().unwrap();
        nudged[2]["created_at"] = json!(f64::from_bits(t.to_bits() + 1));
        let found = with(nudged);
        assert!(found.problems.iter().any(|p| p.contains("not exactly what the phone sealed")), "{:?}", found.problems);
        // A text that differs.
        let mut edited = items.clone();
        edited[0]["text"] = json!("something else");
        assert!(!with(edited).all_checked());
        // Duplicate ids are counted, not a problem in themselves.
        let mut twice = items.clone();
        twice.push(items[1].clone());
        let flash = json!({ "items": twice, "check": summary_of(&twice) });
        let check = examine(&flash, twice.as_slice());
        assert!(check.all_checked());
        assert_eq!((check.items, check.distinct, check.duplicates), (6, 5, 1));
    }

    #[test]
    fn minus_zero_and_absent_times_are_written_one_way() {
        let a = json!([{ "id": "A", "kind": "note", "created_at": -0.0, "updated_at": 0 }]);
        let b = json!([{ "id": "A", "kind": "note", "created_at": 0.0, "updated_at": 0.0 }]);
        assert_eq!(digest(a.as_array().unwrap()), digest(b.as_array().unwrap()));
        let c = json!([{ "id": "A", "kind": "note", "created_at": null, "updated_at": 0.0, "text": null }]);
        let d = json!([{ "id": "A", "kind": "note", "updated_at": 0.0 }]);
        assert_eq!(digest(c.as_array().unwrap()), digest(d.as_array().unwrap()));
        assert_ne!(digest(b.as_array().unwrap()), digest(d.as_array().unwrap()));
    }
}
