//! Editing one entry of a top-level mapping in a YAML config, as text.
//!
//! Hermes keeps its MCP servers under `mcp_servers:` in a long, commented `config.yaml`, and
//! Goose keeps its extensions under `extensions:`. Rewriting such a file through a YAML
//! library would drop every comment and reflow the rest, so this works on lines instead: it
//! finds the block under the top-level key, finds the one entry by name, and replaces,
//! inserts or removes only that entry's lines. Everything else stays byte for byte.
//!
//! Only block style is handled for the mapping that holds the entries (which is what these
//! clients write). A mapping written inline, like `mcp_servers: {a: 1}`, is refused rather
//! than guessed at.

/// A value zone writes, or reads back from its own entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Yaml {
    Str(String),
    Bool(bool),
    Int(i64),
    List(Vec<String>),
    Map(Vec<(String, String)>),
}

/// The line break the file uses.
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// A double-quoted YAML string. JSON's escapes are a subset of YAML's.
pub fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("a string always serializes")
}

fn render_field(out: &mut String, indent: &str, unit: &str, key: &str, value: &Yaml, nl: &str) {
    match value {
        Yaml::Str(s) => out.push_str(&format!("{indent}{key}: {}{nl}", quote(s))),
        Yaml::Bool(b) => out.push_str(&format!("{indent}{key}: {b}{nl}")),
        Yaml::Int(n) => out.push_str(&format!("{indent}{key}: {n}{nl}")),
        Yaml::List(items) => {
            let items: Vec<String> = items.iter().map(|i| quote(i)).collect();
            out.push_str(&format!("{indent}{key}: [{}]{nl}", items.join(", ")));
        }
        Yaml::Map(pairs) if pairs.is_empty() => out.push_str(&format!("{indent}{key}: {{}}{nl}")),
        Yaml::Map(pairs) => {
            out.push_str(&format!("{indent}{key}:{nl}"));
            for (k, v) in pairs {
                out.push_str(&format!("{indent}{unit}{k}: {}{nl}", quote(v)));
            }
        }
    }
}

/// The lines of one entry: `name:` at `indent`, then its fields one step further in.
pub fn render_entry(indent: &str, unit: &str, name: &str, fields: &[(&str, Yaml)], nl: &str) -> String {
    let mut out = format!("{indent}{name}:{nl}");
    let inner = format!("{indent}{unit}");
    for (key, value) in fields {
        render_field(&mut out, &inner, unit, key, value, nl);
    }
    out
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank_or_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// The value after `key:` on a line, without a trailing comment, if the line is that key.
fn value_after_key<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    let rest = [key.to_string(), format!("\"{key}\""), format!("'{key}'")].iter().find_map(|k| content.strip_prefix(k.as_str()))?;
    let rest = rest.trim_start_matches(' ').strip_prefix(':')?;
    if !(rest.is_empty() || rest.starts_with([' ', '\t', '\r', '\n'])) {
        return None;
    }
    Some(strip_comment(rest).trim())
}

/// Takes off a ` # comment`, leaving quoted strings alone.
fn strip_comment(text: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut previous = ' ';
    for (at, ch) in text.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' && (previous == ' ' || previous == '\t') => return &text[..at],
            None => {}
        }
        previous = ch;
    }
    text
}

/// Where things are: the top-level key's line, its block, and the entry in it.
struct Layout {
    /// Index of the `key:` line, if the key is there.
    key_line: Option<usize>,
    /// What follows `key:` on its line (empty for a block).
    inline: String,
    /// One past the block's last line that is not blank or a comment.
    content_end: usize,
    /// Indent of the entries in the block, if it has any.
    child_indent: Option<usize>,
    /// The entry's lines, if it is there.
    entry: Option<(usize, usize)>,
}

fn layout(lines: &[&str], key: &str, name: &str) -> Layout {
    let Some(key_line) = lines.iter().position(|line| !line.starts_with([' ', '\t']) && value_after_key(line, key).is_some()) else {
        return Layout { key_line: None, inline: String::new(), content_end: lines.len(), child_indent: None, entry: None };
    };
    let inline = value_after_key(lines[key_line], key).unwrap_or_default().to_string();
    // The block runs until the next line at the left margin that is not a comment.
    let mut block_end = key_line + 1;
    while block_end < lines.len() {
        let line = lines[block_end];
        if !line.trim().is_empty() && !line.starts_with([' ', '\t']) && !line.starts_with('#') {
            break;
        }
        block_end += 1;
    }
    // Blank lines and comments at the end belong to what comes next.
    let mut content_end = block_end;
    while content_end > key_line + 1 && is_blank_or_comment(lines[content_end - 1]) {
        content_end -= 1;
    }
    let child_indent = lines[key_line + 1..content_end].iter().find(|l| !is_blank_or_comment(l)).map(|l| indent_of(l));
    let entry = child_indent.and_then(|indent| {
        let start = (key_line + 1..content_end).find(|&i| {
            let line = lines[i];
            indent_of(line) == indent && value_after_key(&line[indent..], name).is_some()
        })?;
        let mut end = start + 1;
        while end < content_end && (is_blank_or_comment(lines[end]) || indent_of(lines[end]) > indent) {
            end += 1;
        }
        while end > start + 1 && is_blank_or_comment(lines[end - 1]) {
            end -= 1;
        }
        Some((start, end))
    });
    Layout { key_line: Some(key_line), inline, content_end, child_indent, entry }
}

fn is_empty_inline(value: &str) -> bool {
    matches!(value, "" | "{}" | "null" | "~")
}

/// Adds or replaces the entry `name` under the top-level `key`. Returns the new text, or
/// None when the entry is already exactly this.
pub fn set(text: &str, key: &str, name: &str, fields: &[(&str, Yaml)]) -> Result<Option<String>, String> {
    let nl = newline(text);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let found = layout(&lines, key, name);
    let Some(key_line) = found.key_line else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&format!("{key}:{nl}"));
        out.push_str(&render_entry("  ", "  ", name, fields, nl));
        return Ok(Some(out));
    };
    if !is_empty_inline(&found.inline) {
        return Err(format!("\"{key}\" is written on one line ({key}: {{...}}), so zone will not change it"));
    }
    let indent = found.child_indent.unwrap_or(2).max(1);
    let entry = render_entry(&" ".repeat(indent), &" ".repeat(indent.min(4)), name, fields, nl);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match found.entry {
        Some((start, end)) => {
            if lines[start..end].concat().replace("\r\n", "\n") == entry.replace("\r\n", "\n") {
                return Ok(None);
            }
            out.splice(start..end, [entry]);
        }
        None => {
            if !found.inline.is_empty() {
                out[key_line] = format!("{key}:{nl}");
            }
            let at = found.content_end;
            if at > 0 && !out[at - 1].ends_with('\n') {
                out[at - 1].push_str(nl);
            }
            out.insert(at, entry);
        }
    }
    Ok(Some(out.concat()))
}

/// Takes out the entry `name` under `key`. Returns the new text, or None when it is not there.
/// When it was the only entry, the key is left as `key: {}`, which every reader takes as an
/// empty mapping (a bare `key:` would read as null).
pub fn remove(text: &str, key: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let found = layout(&lines, key, name);
    let (start, end) = found.entry?;
    let key_line = found.key_line?;
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.drain(start..end);
    let others = out[key_line + 1..found.content_end - (end - start)].iter().any(|l| !is_blank_or_comment(l));
    if !others {
        out[key_line] = format!("{key}: {{}}{}", newline(text));
    }
    Some(out.concat())
}

/// Reads a scalar: quoted or plain.
fn scalar(text: &str) -> String {
    let text = text.trim();
    if text.starts_with('"')
        && let Ok(s) = serde_json::from_str::<String>(text)
    {
        return s;
    }
    if let Some(inner) = text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    text.to_string()
}

/// Splits `a, "b, c", 'd'` at the commas outside quotes.
fn split_flow(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in text.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if ch == '\\' => escaped = true,
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ',' => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            None => {}
        }
        current.push(ch);
    }
    parts.push(current);
    parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// The fields of the entry `name` under `key`, read back. Lists and maps may be written in
/// either flow or block style.
pub fn get(text: &str, key: &str, name: &str) -> Option<Vec<(String, Yaml)>> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (start, end) = layout(&lines, key, name).entry?;
    let body: Vec<&str> = lines[start + 1..end].iter().copied().filter(|l| !is_blank_or_comment(l)).collect();
    let field_indent = body.first().map(|l| indent_of(l))?;
    let mut fields = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let line = body[i].trim_end();
        i += 1;
        if indent_of(line) != field_indent {
            continue;
        }
        let content = &line[field_indent..];
        let Some((field, value)) = content.split_once(':') else { continue };
        let field = scalar(field);
        let value = strip_comment(value).trim();
        let parsed = if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            Yaml::List(split_flow(inner).iter().map(|s| scalar(s)).collect())
        } else if let Some(inner) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
            Yaml::Map(split_flow(inner).iter().filter_map(|p| p.split_once(':').map(|(k, v)| (scalar(k), scalar(v)))).collect())
        } else if value.is_empty() {
            // A block list or mapping on the lines below.
            let mut items = Vec::new();
            let mut pairs = Vec::new();
            while i < body.len() && indent_of(body[i]) > field_indent {
                let inner = body[i].trim();
                if let Some(item) = inner.strip_prefix("- ") {
                    items.push(scalar(strip_comment(item)));
                } else if let Some((k, v)) = inner.split_once(':') {
                    pairs.push((scalar(k), scalar(strip_comment(v))));
                }
                i += 1;
            }
            if !items.is_empty() { Yaml::List(items) } else { Yaml::Map(pairs) }
        } else if value == "true" || value == "false" {
            Yaml::Bool(value == "true")
        } else if let Ok(n) = value.parse::<i64>() {
            Yaml::Int(n)
        } else {
            Yaml::Str(scalar(value))
        };
        fields.push((field, parsed));
    }
    Some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<(&'static str, Yaml)> {
        vec![
            ("command", Yaml::Str("/opt/zone".into())),
            ("args", Yaml::List(vec!["mcp".into()])),
            ("env", Yaml::Map(vec![("ZONE_HOME".into(), "/tmp/z one".into())])),
        ]
    }

    const HERMES: &str = "\
model:
  default: some-model # the usual one
# Servers the agent can use.
mcp_servers:
  files:
    command: npx
    args:
      - -y
      - files
    env:
      TOKEN: secret-value
    enabled: true
  web:
    url: http://127.0.0.1:9000/mcp
    timeout: 30

# Voice settings.
voice:
  enabled: false
";

    #[test]
    fn an_entry_is_added_and_removed_leaving_every_other_byte() {
        let added = set(HERMES, "mcp_servers", "zone", &fields()).unwrap().unwrap();
        assert_eq!(
            added,
            HERMES.replace(
                "    timeout: 30\n",
                "    timeout: 30\n  zone:\n    command: \"/opt/zone\"\n    args: [\"mcp\"]\n    env:\n      ZONE_HOME: \"/tmp/z one\"\n"
            )
        );
        // Again: nothing to do.
        assert_eq!(set(&added, "mcp_servers", "zone", &fields()).unwrap(), None);
        let read = get(&added, "mcp_servers", "zone").unwrap();
        assert_eq!(read[0], ("command".to_string(), Yaml::Str("/opt/zone".into())));
        assert_eq!(read[1], ("args".to_string(), Yaml::List(vec!["mcp".into()])));
        assert_eq!(read[2], ("env".to_string(), Yaml::Map(vec![("ZONE_HOME".into(), "/tmp/z one".into())])));
        // Other entries read back too, in block style.
        let files = get(HERMES, "mcp_servers", "files").unwrap();
        assert_eq!(files[1], ("args".to_string(), Yaml::List(vec!["-y".into(), "files".into()])));
        assert_eq!(files[3], ("enabled".to_string(), Yaml::Bool(true)));

        assert_eq!(remove(&added, "mcp_servers", "zone").unwrap(), HERMES);
        assert_eq!(remove(HERMES, "mcp_servers", "zone"), None);
    }

    #[test]
    fn a_changed_entry_is_replaced_in_place() {
        let first = set(HERMES, "mcp_servers", "zone", &fields()).unwrap().unwrap();
        let moved = first.replace("mcp_servers:\n  files:", "mcp_servers:\n  zone:\n    command: /old/zone # an old copy\n    args: [mcp]\n\n  files:");
        let moved = moved.replacen("  zone:\n    command: \"/opt/zone\"\n    args: [\"mcp\"]\n    env:\n      ZONE_HOME: \"/tmp/z one\"\n", "", 1);
        assert_eq!(get(&moved, "mcp_servers", "zone").unwrap()[0].1, Yaml::Str("/old/zone".into()));
        let updated = set(&moved, "mcp_servers", "zone", &fields()).unwrap().unwrap();
        assert!(updated.starts_with("model:\n  default: some-model # the usual one\n# Servers the agent can use.\nmcp_servers:\n  zone:\n    command: \"/opt/zone\""), "{updated}");
        // The blank line after the old entry stays where it was.
        assert!(updated.contains("ZONE_HOME: \"/tmp/z one\"\n\n  files:"), "{updated}");
        assert_eq!(updated.matches("zone:").count(), 1);
    }

    #[test]
    fn missing_empty_and_inline_keys() {
        // No key at all: it is added at the end.
        let text = set("model: x", "mcp_servers", "zone", &fields()).unwrap().unwrap();
        assert!(text.starts_with("model: x\nmcp_servers:\n  zone:\n    command: \"/opt/zone\"\n"), "{text}");
        assert_eq!(remove(&text, "mcp_servers", "zone").unwrap(), "model: x\nmcp_servers: {}\n");
        assert!(set("", "extensions", "zone", &fields()).unwrap().unwrap().starts_with("extensions:\n  zone:\n"));
        // An empty mapping written inline becomes a block.
        for empty in ["mcp_servers: {}\nnext: 1\n", "mcp_servers:\nnext: 1\n", "mcp_servers: ~ # none\nnext: 1\n"] {
            let text = set(empty, "mcp_servers", "zone", &fields()).unwrap().unwrap();
            assert!(text.starts_with("mcp_servers:\n  zone:\n    command:"), "{text}");
            assert!(text.ends_with("next: 1\n"), "{text}");
        }
        // A mapping written inline with things in it is left alone.
        assert!(set("mcp_servers: {a: 1}\n", "mcp_servers", "zone", &fields()).is_err());
        // A key that only starts the same is another key.
        let text = set("mcp_servers_old:\n  zone: 1\n", "mcp_servers", "zone", &fields()).unwrap().unwrap();
        assert!(text.starts_with("mcp_servers_old:\n  zone: 1\nmcp_servers:\n  zone:\n"), "{text}");
    }

    #[test]
    fn four_space_files_and_windows_line_ends() {
        let text = "extensions:\r\n    developer:\r\n        enabled: true\r\n";
        let added = set(text, "extensions", "zone", &[("enabled", Yaml::Bool(true)), ("timeout", Yaml::Int(300)), ("envs", Yaml::Map(vec![]))]).unwrap().unwrap();
        assert_eq!(added, "extensions:\r\n    developer:\r\n        enabled: true\r\n    zone:\r\n        enabled: true\r\n        timeout: 300\r\n        envs: {}\r\n");
        let read = get(&added, "extensions", "zone").unwrap();
        assert_eq!(read, vec![("enabled".into(), Yaml::Bool(true)), ("timeout".into(), Yaml::Int(300)), ("envs".into(), Yaml::Map(vec![]))]);
        assert_eq!(remove(&added, "extensions", "zone").unwrap(), text);
    }

    #[test]
    fn quoting_survives_odd_paths() {
        let odd = vec![("command", Yaml::Str("C:\\Users\\Zed \"Z\"\\zone.exe".into())), ("args", Yaml::List(vec!["mcp".into(), "a, b".into()]))];
        let text = set("", "mcp_servers", "zone", &odd).unwrap().unwrap();
        let read = get(&text, "mcp_servers", "zone").unwrap();
        assert_eq!(read[0].1, Yaml::Str("C:\\Users\\Zed \"Z\"\\zone.exe".into()));
        assert_eq!(read[1].1, Yaml::List(vec!["mcp".into(), "a, b".into()]));
        assert_eq!(scalar("'it''s'"), "it's");
        assert_eq!(strip_comment("\"a # b\" # c"), "\"a # b\" ");
    }
}
