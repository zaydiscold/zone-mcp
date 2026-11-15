//! Editing JSON config files as text, so that nothing but the one entry changes.
//!
//! Several clients keep their settings in JSON with comments and trailing commas (Zed and
//! VS Code call it JSONC). Reading such a file into a value and writing it back would throw
//! the comments away and reformat everything, so instead the file is parsed only to find
//! where things are, and the edit is made to the text: one member is inserted, replaced or
//! taken out, indented like its neighbours. Every other byte stays as it was.

use serde_json::{Map, Value};

/// Deeper nesting than this is refused rather than risk the stack.
const MAX_DEPTH: usize = 256;

#[derive(Debug)]
enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    /// A string, number, `true`, `false` or `null`: its text is valid JSON on its own.
    Scalar,
}

#[derive(Debug)]
struct Node {
    /// Byte range of the value in the text.
    start: usize,
    end: usize,
    kind: Kind,
}

#[derive(Debug)]
struct Member {
    key: String,
    key_start: usize,
    value: Node,
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, what: &str) -> String {
        let line = self.text[..self.at.min(self.text.len())].matches('\n').count() + 1;
        format!("{what} on line {line}")
    }

    /// Skips whitespace and comments.
    fn trivia(&mut self) -> Result<(), String> {
        self.at = skip_trivia(self.text, self.at).ok_or_else(|| self.error("a comment that never ends"))?;
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        self.trivia()?;
        let start = self.at;
        match self.peek() {
            Some(b'{') => {
                self.at += 1;
                let mut members: Vec<Member> = Vec::new();
                loop {
                    self.trivia()?;
                    match self.peek() {
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        Some(b'"') => {}
                        _ => return Err(self.error("expected a quoted name or }")),
                    }
                    let key_start = self.at;
                    let key = self.string()?;
                    self.trivia()?;
                    if self.peek() != Some(b':') {
                        return Err(self.error("expected :"));
                    }
                    self.at += 1;
                    let value = self.value(depth + 1)?;
                    members.push(Member { key, key_start, value });
                    self.trivia()?;
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {}
                        _ => return Err(self.error("expected , or }")),
                    }
                }
                Ok(Node { start, end: self.at, kind: Kind::Object(members) })
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.trivia()?;
                    if self.peek() == Some(b']') {
                        self.at += 1;
                        break;
                    }
                    items.push(self.value(depth + 1)?);
                    self.trivia()?;
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {}
                        _ => return Err(self.error("expected , or ]")),
                    }
                }
                Ok(Node { start, end: self.at, kind: Kind::Array(items) })
            }
            Some(b'"') => {
                self.string()?;
                Ok(Node { start, end: self.at, kind: Kind::Scalar })
            }
            Some(_) => {
                while let Some(b) = self.peek() {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.') {
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                if self.at == start || serde_json::from_str::<Value>(&self.text[start..self.at]).is_err() {
                    return Err(self.error("expected a value"));
                }
                Ok(Node { start, end: self.at, kind: Kind::Scalar })
            }
            None => Err(self.error("the file ends where a value should be")),
        }
    }

    /// A quoted string, unescaped.
    fn string(&mut self) -> Result<String, String> {
        let start = self.at;
        self.at += 1;
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    break;
                }
                Some(b'\\') => self.at += 2,
                Some(b'\n') | None => return Err(self.error("a string that never ends")),
                Some(_) => self.at += 1,
            }
        }
        serde_json::from_str::<String>(&self.text[start..self.at]).map_err(|_| self.error("a string with a bad escape"))
    }
}

/// The position after any whitespace and comments from `at`. None for an unclosed `/*`.
fn skip_trivia(text: &str, mut at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    loop {
        match bytes.get(at) {
            Some(b' ' | b'\t' | b'\r' | b'\n') => at += 1,
            Some(b'/') if bytes.get(at + 1) == Some(&b'/') => {
                at = text[at..].find('\n').map(|n| at + n).unwrap_or(text.len());
            }
            Some(b'/') if bytes.get(at + 1) == Some(&b'*') => {
                at = at + 2 + text[at + 2..].find("*/")? + 2;
            }
            // A byte order mark at the very start.
            _ if at == 0 && text.starts_with('\u{feff}') => at += 3,
            _ => return Some(at),
        }
    }
}

fn parse(text: &str) -> Result<Node, String> {
    let mut parser = Parser { text, bytes: text.as_bytes(), at: 0 };
    let root = parser.value(0)?;
    parser.trivia()?;
    if parser.at != text.len() {
        return Err(parser.error("more after the end of the JSON"));
    }
    Ok(root)
}

fn to_value(text: &str, node: &Node) -> Value {
    match &node.kind {
        Kind::Object(members) => Value::Object(members.iter().map(|m| (m.key.clone(), to_value(text, &m.value))).collect::<Map<_, _>>()),
        Kind::Array(items) => Value::Array(items.iter().map(|n| to_value(text, n)).collect()),
        Kind::Scalar => serde_json::from_str(&text[node.start..node.end]).unwrap_or(Value::Null),
    }
}

/// Why a file cannot be edited.
#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// Not JSON, even allowing comments and trailing commas.
    Invalid(String),
    /// The whole file, or something on the way to the entry, is not an object. The string
    /// names what, like `"mcpServers"` or `the file`.
    NotAnObject(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Invalid(why) => write!(f, "is not valid JSON ({why})"),
            Problem::NotAnObject(what) => write!(f, "has {what} that is not an object"),
        }
    }
}

fn parse_root(text: &str) -> Result<Option<Node>, Problem> {
    if text.trim_start_matches('\u{feff}').trim().is_empty() {
        return Ok(None);
    }
    let root = parse(text).map_err(Problem::Invalid)?;
    if !matches!(root.kind, Kind::Object(_)) {
        return Err(Problem::NotAnObject("the file".to_string()));
    }
    Ok(Some(root))
}

fn describe(path: &[&str]) -> String {
    path.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(" > ")
}

fn members(node: &Node) -> Option<&Vec<Member>> {
    match &node.kind {
        Kind::Object(members) => Some(members),
        _ => None,
    }
}

/// The value at `path`, if there is one.
pub fn get(text: &str, path: &[&str]) -> Result<Option<Value>, Problem> {
    let Some(root) = parse_root(text)? else {
        return Ok(None);
    };
    let mut node = &root;
    for key in path {
        let Some(found) = members(node).and_then(|m| m.iter().rev().find(|m| m.key == *key)) else {
            return Ok(None);
        };
        node = &found.value;
    }
    Ok(Some(to_value(text, node)))
}

// -------------------------------------------------------------------- layout

/// The whitespace at the start of the line that holds `at`.
fn line_indent(text: &str, at: usize) -> &str {
    let line_start = text[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let rest = &text[line_start..];
    let width = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    &rest[..width]
}

/// One step of indentation as the file uses it: a tab, or the smallest number of spaces
/// a line is indented by. Two spaces when the file does not say.
fn indent_unit(text: &str) -> String {
    let mut smallest: Option<usize> = None;
    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.len() == line.len() {
            continue;
        }
        if line.starts_with('\t') {
            return "\t".to_string();
        }
        let width = line.len() - trimmed.len();
        smallest = Some(smallest.map_or(width, |s: usize| s.min(width)));
    }
    " ".repeat(smallest.unwrap_or(2).clamp(1, 8))
}

/// The line break the file uses.
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// A value as indented JSON, every line after the first starting with `base`.
fn render(value: &Value, base: &str, unit: &str, nl: &str) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(unit.as_bytes());
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(value, &mut serializer).expect("a JSON value always serializes");
    String::from_utf8(out).expect("JSON is UTF-8").replace('\n', &format!("{nl}{base}"))
}

fn quoted(key: &str) -> String {
    serde_json::to_string(key).expect("a string always serializes")
}

/// `value` under `path`, as nested objects: `["a", "b"]` and 1 give `{"a": {"b": 1}}`.
fn nest(path: &[&str], value: &Value) -> Value {
    path.iter().rev().fold(value.clone(), |inner, key| {
        let mut map = Map::new();
        map.insert(key.to_string(), inner);
        Value::Object(map)
    })
}

/// Where a line begins, if only spaces and tabs stand between it and `at`.
fn blank_line_start(text: &str, at: usize) -> Option<usize> {
    let before = &text[..at];
    let trimmed = before.trim_end_matches([' ', '\t']);
    (trimmed.is_empty() || trimmed.ends_with('\n')).then_some(trimmed.len())
}

/// The position after the line break that ends the line holding `at`, if only spaces and
/// tabs follow `at` on it.
fn blank_line_end(text: &str, at: usize) -> Option<usize> {
    let rest = &text[at..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let skipped = rest.len() - trimmed.len();
    if trimmed.starts_with("\r\n") {
        Some(at + skipped + 2)
    } else if trimmed.starts_with('\n') {
        Some(at + skipped + 1)
    } else if trimmed.is_empty() {
        Some(text.len())
    } else {
        None
    }
}

/// The comma that follows a value, if any (a trailing comma, or the separator).
fn comma_after(text: &str, at: usize) -> Option<usize> {
    let next = skip_trivia(text, at)?;
    (text.as_bytes().get(next) == Some(&b',')).then_some(next)
}

/// Applies edits (range, replacement) given in any order. Ranges must not overlap.
fn apply(text: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (start, end, with) in edits {
        out.replace_range(start..end, &with);
    }
    out
}

// -------------------------------------------------------------------- edits

/// Sets the value at `path`, creating objects on the way as needed. Returns the new text,
/// or None when the value there is already equal.
pub fn set(text: &str, path: &[&str], value: &Value) -> Result<Option<String>, Problem> {
    assert!(!path.is_empty(), "set needs a path");
    let Some(root) = parse_root(text)? else {
        return Ok(Some(format!("{}\n", render(&nest(path, value), "", "  ", "\n"))));
    };
    let unit = indent_unit(text);
    let mut node = &root;
    for (depth, key) in path.iter().enumerate() {
        let Some(list) = members(node) else {
            return Err(Problem::NotAnObject(describe(&path[..depth])));
        };
        let rest = &path[depth + 1..];
        match list.iter().rev().find(|m| m.key == *key) {
            Some(found) if rest.is_empty() => {
                if to_value(text, &found.value) == *value {
                    return Ok(None);
                }
                let base = line_indent(text, found.key_start);
                let inline = !text[node.start..node.end].contains('\n');
                let rendered = if inline { value.to_string() } else { render(value, base, &unit, newline(text)) };
                return Ok(Some(apply(text, vec![(found.value.start, found.value.end, rendered)])));
            }
            Some(found) => node = &found.value,
            None => return Ok(Some(insert(text, node, list, key, &nest(rest, value), &unit))),
        }
    }
    unreachable!("the loop returns at the last key")
}

/// Adds `"key": value` as the last member of an object.
fn insert(text: &str, object: &Node, list: &[Member], key: &str, value: &Value, unit: &str) -> String {
    let close = object.end - 1;
    let nl = newline(text);
    let base = line_indent(text, object.start).to_string();
    let Some(last) = list.last() else {
        // An empty object. Anything inside it (comments) is kept, before the new member.
        let inside = text[object.start + 1..close].trim_end();
        let indent = format!("{base}{unit}");
        let member = format!("{indent}{}: {}", quoted(key), render(value, &indent, unit, nl));
        let opened = if inside.trim().is_empty() { "" } else { inside };
        return apply(text, vec![(object.start + 1, object.end, format!("{opened}{nl}{member}{nl}{base}}}"))]);
    };
    if !text[object.start..object.end].contains('\n') {
        // All on one line: stay on one line.
        let member = format!(" {}: {}", quoted(key), value);
        return match comma_after(text, last.value.end) {
            Some(comma) => apply(text, vec![(comma + 1, comma + 1, member)]),
            None => apply(text, vec![(last.value.end, last.value.end, format!(",{member}"))]),
        };
    }
    let indent = line_indent(text, list[0].key_start).to_string();
    let indent = if blank_line_start(text, list[0].key_start).is_some() { indent } else { format!("{base}{unit}") };
    let trailing = comma_after(text, last.value.end);
    let member = format!("{indent}{}: {}{}", quoted(key), render(value, &indent, unit, nl), if trailing.is_some() { "," } else { "" });
    let mut edits = Vec::new();
    if trailing.is_none() {
        edits.push((last.value.end, last.value.end, ",".to_string()));
    }
    match blank_line_start(text, close) {
        Some(line) => edits.push((line, line, format!("{member}{nl}"))),
        None => edits.push((close, close, format!("{nl}{member}{nl}{base}"))),
    }
    apply(text, edits)
}

/// Takes out the member at `path`. Returns the new text, or None when it is not there.
pub fn remove(text: &str, path: &[&str]) -> Result<Option<String>, Problem> {
    assert!(!path.is_empty(), "remove needs a path");
    let Some(root) = parse_root(text)? else {
        return Ok(None);
    };
    let mut node = &root;
    for key in &path[..path.len() - 1] {
        let Some(found) = members(node).and_then(|m| m.iter().rev().find(|m| m.key == *key)) else {
            return Ok(None);
        };
        node = &found.value;
    }
    let Some(list) = members(node) else {
        return Ok(None);
    };
    let name = path[path.len() - 1];
    let Some(index) = list.iter().rposition(|m| m.key == name) else {
        return Ok(None);
    };
    let member = &list[index];
    let own_comma = comma_after(text, member.value.end);
    let after = own_comma.map(|c| c + 1).unwrap_or(member.value.end);
    // The member's own lines, when nothing else shares them.
    let line_start = blank_line_start(text, member.key_start);
    let line_end = blank_line_end(text, after);

    if list.len() == 1 {
        let close = node.end - 1;
        let start = line_start.unwrap_or(member.key_start);
        let end = line_end.unwrap_or(after).min(close);
        let rest = format!("{}{}", &text[node.start + 1..start.max(node.start + 1)], &text[end..close]);
        if rest.trim().is_empty() {
            return Ok(Some(apply(text, vec![(node.start, node.end, "{}".to_string())])));
        }
        return Ok(Some(apply(text, vec![(start, end, String::new())])));
    }
    let edits = if own_comma.is_none() {
        // The last member, with no comma of its own: the comma before it goes instead.
        let comma = comma_after(text, list[index - 1].value.end).expect("members are separated by commas");
        match (line_start, line_end) {
            (Some(start), Some(end)) => vec![(start, end, String::new()), (comma, comma + 1, String::new())],
            _ => vec![(comma, member.value.end, String::new())],
        }
    } else {
        match (line_start, line_end) {
            (Some(start), Some(end)) => vec![(start, end, String::new())],
            _ => {
                // Something shares the line: take the member and the spaces after its comma.
                let rest = &text[after..];
                let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
                vec![(member.key_start, after + spaces, String::new())]
            }
        }
    };
    Ok(Some(apply(text, edits)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn zone() -> Value {
        json!({ "command": "/opt/zone", "args": ["mcp"] })
    }

    fn strict(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("not strict JSON ({e}):\n{text}"))
    }

    #[test]
    fn a_new_file() {
        let text = set("", &["mcpServers", "zone"], &zone()).unwrap().unwrap();
        assert_eq!(text, "{\n  \"mcpServers\": {\n    \"zone\": {\n      \"command\": \"/opt/zone\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  }\n}\n");
        assert_eq!(get(&text, &["mcpServers", "zone", "command"]).unwrap(), Some(json!("/opt/zone")));
        assert_eq!(set(&text, &["mcpServers", "zone"], &zone()).unwrap(), None);
        let removed = remove(&text, &["mcpServers", "zone"]).unwrap().unwrap();
        assert_eq!(removed, "{\n  \"mcpServers\": {}\n}\n");
        assert_eq!(remove(&removed, &["mcpServers", "zone"]).unwrap(), None);
    }

    #[test]
    fn comments_and_trailing_commas_survive() {
        let original = "// Zed settings\n//\n{\n    \"theme\": \"One Dark\", // the theme\n    /* servers */\n    \"context_servers\": {\n        \"other\": {\n            \"command\": \"other\",\n        },\n    },\n    \"ui_font_size\": 16,\n}\n";
        let added = set(original, &["context_servers", "zone"], &zone()).unwrap().unwrap();
        assert_eq!(
            added,
            "// Zed settings\n//\n{\n    \"theme\": \"One Dark\", // the theme\n    /* servers */\n    \"context_servers\": {\n        \"other\": {\n            \"command\": \"other\",\n        },\n        \"zone\": {\n            \"command\": \"/opt/zone\",\n            \"args\": [\n                \"mcp\"\n            ]\n        },\n    },\n    \"ui_font_size\": 16,\n}\n"
        );
        assert_eq!(get(&added, &["context_servers", "zone", "args"]).unwrap(), Some(json!(["mcp"])));
        // Taking it out gives back the very same text.
        assert_eq!(remove(&added, &["context_servers", "zone"]).unwrap().unwrap(), original);
    }

    #[test]
    fn strict_json_stays_strict_and_round_trips() {
        let original = "{\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"files\": {\n      \"command\": \"npx\"\n    }\n  },\n  \"last\": true\n}\n";
        let added = set(original, &["mcpServers", "zone"], &zone()).unwrap().unwrap();
        let value = strict(&added);
        assert_eq!(value["mcpServers"]["zone"], zone());
        assert_eq!(value["mcpServers"]["files"]["command"], "npx");
        assert!(added.starts_with("{\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"files\": {\n      \"command\": \"npx\"\n    },\n    \"zone\": {\n"));
        assert_eq!(remove(&added, &["mcpServers", "zone"]).unwrap().unwrap(), original);

        // Replacing a different zone entry keeps its place.
        let changed = set(&added, &["mcpServers", "zone"], &json!({ "command": "/new/zone", "args": ["mcp", "--read-only"] })).unwrap().unwrap();
        let value = strict(&changed);
        assert_eq!(value["mcpServers"]["zone"]["command"], "/new/zone");
        assert!(changed.find("\"files\"").unwrap() < changed.find("\"zone\"").unwrap());
        assert!(changed.find("\"zone\"").unwrap() < changed.find("\"last\"").unwrap());
    }

    #[test]
    fn missing_parents_are_made_and_tabs_are_followed() {
        let original = "{\n\t\"a\": 1\n}";
        let added = set(original, &["mcp", "zone"], &json!({ "type": "local" })).unwrap().unwrap();
        assert_eq!(added, "{\n\t\"a\": 1,\n\t\"mcp\": {\n\t\t\"zone\": {\n\t\t\t\"type\": \"local\"\n\t\t}\n\t}\n}");
        let removed = remove(&added, &["mcp", "zone"]).unwrap().unwrap();
        assert_eq!(strict(&removed), json!({ "a": 1, "mcp": {} }));
    }

    #[test]
    fn empty_and_one_line_objects() {
        assert_eq!(strict(&set("{}", &["mcpServers", "zone"], &zone()).unwrap().unwrap())["mcpServers"]["zone"], zone());
        let text = set("{ \"mcpServers\": {} }", &["mcpServers", "zone"], &json!(1)).unwrap().unwrap();
        assert_eq!(strict(&text), json!({ "mcpServers": { "zone": 1 } }));
        // One line stays one line.
        let text = set("{ \"mcpServers\": { \"a\": 1 } }", &["mcpServers", "zone"], &json!(2)).unwrap().unwrap();
        assert_eq!(text, "{ \"mcpServers\": { \"a\": 1, \"zone\": 2 } }");
        assert_eq!(remove(&text, &["mcpServers", "zone"]).unwrap().unwrap(), "{ \"mcpServers\": { \"a\": 1 } }");
        let text = set("{\"a\":1,}", &["zone"], &json!(2)).unwrap().unwrap();
        assert_eq!(text, "{\"a\":1, \"zone\": 2}");
        // An empty object with a comment in it keeps the comment.
        let text = set("{\n  \"s\": {\n    // nothing yet\n  }\n}\n", &["s", "zone"], &json!(1)).unwrap().unwrap();
        assert!(text.contains("// nothing yet") && text.contains("\"zone\": 1"), "{text}");
        assert_eq!(get(&text, &["s", "zone"]).unwrap(), Some(json!(1)));
    }

    #[test]
    fn removing_from_the_middle_and_the_front() {
        let text = "{\n  \"s\": {\n    \"zone\": 1,\n    \"b\": 2\n  }\n}\n";
        assert_eq!(remove(text, &["s", "zone"]).unwrap().unwrap(), "{\n  \"s\": {\n    \"b\": 2\n  }\n}\n");
        let text = "{\n  \"s\": {\n    \"a\": 0,\n    \"zone\": 1,\n    \"b\": 2\n  }\n}\n";
        assert_eq!(remove(text, &["s", "zone"]).unwrap().unwrap(), "{\n  \"s\": {\n    \"a\": 0,\n    \"b\": 2\n  }\n}\n");
        let text = "{\"s\":{\"zone\":1,\"b\":2}}";
        assert_eq!(strict(&remove(text, &["s", "zone"]).unwrap().unwrap()), json!({ "s": { "b": 2 } }));
        let text = "{\"s\":{\"a\":0,\"zone\":1}}";
        assert_eq!(strict(&remove(text, &["s", "zone"]).unwrap().unwrap()), json!({ "s": { "a": 0 } }));
    }

    #[test]
    fn problems_are_reported_and_nothing_is_guessed() {
        assert!(matches!(set("{ not json", &["a"], &json!(1)), Err(Problem::Invalid(_))));
        assert!(matches!(set("[1, 2]", &["a"], &json!(1)), Err(Problem::NotAnObject(_))));
        assert_eq!(set("{ \"mcpServers\": 5 }", &["mcpServers", "zone"], &json!(1)), Err(Problem::NotAnObject("\"mcpServers\"".to_string())));
        assert!(matches!(set("{ \"a\": 1 } x", &["a"], &json!(1)), Err(Problem::Invalid(_))));
        assert!(matches!(set("{ /* open", &["a"], &json!(1)), Err(Problem::Invalid(_))));
        assert!(matches!(set("{ \"a\": tru }", &["a"], &json!(1)), Err(Problem::Invalid(_))));
        assert_eq!(get("", &["a"]).unwrap(), None);
        assert_eq!(remove("  \n", &["a"]).unwrap(), None);
        let deep = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
        assert!(matches!(get(&format!("{{\"a\": {deep}}}"), &["a"]), Err(Problem::Invalid(_))));
    }

    #[test]
    fn strings_with_escapes_and_odd_characters() {
        let text = "{\n  \"we\\\"ird\": \"a // not a comment /* nor this */\",\n  \"s\": {}\n}\n";
        let added = set(text, &["s", "zone"], &json!({ "command": "C:\\Users\\Zed\\zone.exe" })).unwrap().unwrap();
        let value = strict(&added);
        assert_eq!(value["we\"ird"], "a // not a comment /* nor this */");
        assert_eq!(value["s"]["zone"]["command"], "C:\\Users\\Zed\\zone.exe");
        // A byte order mark and Windows line ends.
        let text = "\u{feff}{\r\n  \"a\": 1\r\n}\r\n";
        let added = set(text, &["zone"], &json!(2)).unwrap().unwrap();
        assert_eq!(get(&added, &["zone"]).unwrap(), Some(json!(2)));
        assert_eq!(get(&added, &["a"]).unwrap(), Some(json!(1)));
        assert_eq!(remove(&added, &["zone"]).unwrap().unwrap(), text);
    }
}
