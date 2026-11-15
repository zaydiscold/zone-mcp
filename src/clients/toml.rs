//! Codex's `config.toml`: zone's server is the table `[mcp_servers.zone]`.
//!
//! The file is edited as lines. Zone's table (and any `[mcp_servers.zone.*]` sub-table) is
//! taken out and written again at the end; every other line stays as it was.

use super::{Launch, SERVER_NAME};

/// A TOML basic string.
pub fn string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Zone's table.
pub fn block(launch: &Launch) -> String {
    let args: Vec<String> = launch.args.iter().map(|a| string(a)).collect();
    let mut block = format!("[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [{}]\n", string(&launch.command), args.join(", "));
    if !launch.env.is_empty() {
        let pairs: Vec<String> = launch.env.iter().map(|(k, v)| format!("{k} = {}", string(v))).collect();
        block.push_str(&format!("env = {{ {} }}\n", pairs.join(", ")));
    }
    block
}

fn header_name(line: &str) -> Option<String> {
    let line = line.trim();
    let inner = line.strip_prefix('[')?.trim_start_matches('[');
    let end = inner.find(']')?;
    Some(inner[..end].split('.').map(|part| part.trim().trim_matches('"').trim_matches('\'')).collect::<Vec<_>>().join("."))
}

fn is_zone_header(line: &str) -> bool {
    header_name(line).is_some_and(|name| name == format!("mcp_servers.{SERVER_NAME}") || name.starts_with(&format!("mcp_servers.{SERVER_NAME}.")))
}

/// The config without zone's tables, and whether any were found.
pub fn without_zone(text: &str) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    let mut found = false;
    for line in text.lines() {
        if line.trim_start().starts_with('[') {
            skipping = is_zone_header(line);
            found |= skipping;
        }
        if !skipping {
            out.push(line);
        }
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    let mut joined = out.join("\n");
    if !joined.is_empty() {
        joined.push('\n');
    }
    (joined, found)
}

/// The file with zone's table set to `launch`, or None when it is already exactly that.
pub fn with_zone(existing: &str, launch: &Launch) -> Option<String> {
    let (mut text, _) = without_zone(existing);
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&block(launch));
    (text != existing).then_some(text)
}

/// A TOML string or bare value, read back.
fn scalar(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('"')
        && let Ok(s) = serde_json::from_str::<String>(value)
    {
        return s;
    }
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return inner.to_string();
    }
    value.to_string()
}

/// Splits at commas outside quotes.
fn split(inner: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in inner.chars() {
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

/// Zone's server as the file has it, if it is there.
pub fn registration(text: &str) -> Option<Launch> {
    let table = format!("mcp_servers.{SERVER_NAME}");
    let env_table = format!("{table}.env");
    let mut section = String::new();
    let mut command = None;
    let mut args = Vec::new();
    let mut env = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = header_name(trimmed).unwrap_or_default();
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if section == table {
            match key {
                "command" => command = Some(scalar(value)),
                "args" => {
                    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                        args = split(inner).iter().map(|a| scalar(a)).collect();
                    }
                }
                "env" => {
                    if let Some(inner) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
                        env = split(inner).iter().filter_map(|p| p.split_once('=').map(|(k, v)| (k.trim().to_string(), scalar(v)))).collect();
                    }
                }
                _ => {}
            }
        } else if section == env_table {
            env.push((key.to_string(), scalar(value)));
        }
    }
    command.map(|command| Launch { command, args, env })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch { command: "/opt/zone/bin/zone".to_string(), args: vec!["mcp".to_string()], env: Vec::new() }
    }

    #[test]
    fn the_table_is_written_replaced_and_taken_out() {
        let first = with_zone("", &launch()).unwrap();
        assert_eq!(first, "[mcp_servers.zone]\ncommand = \"/opt/zone/bin/zone\"\nargs = [\"mcp\"]\n");
        assert_eq!(with_zone(&first, &launch()), None);
        assert_eq!(registration(&first), Some(launch()));

        let existing = "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"other\"\n\n[mcp_servers.zone]\ncommand = \"/old/zone\"\nargs = [\"mcp\"]\n\n[mcp_servers.zone.env]\nZONE_HOME = \"/old\"\n\n[profiles.fast]\nmodel = \"small\"\n";
        assert_eq!(registration(existing).unwrap().env, vec![("ZONE_HOME".to_string(), "/old".to_string())]);
        let windows = Launch {
            command: r"C:\Users\Zed\zone.exe".to_string(),
            args: vec!["mcp".into(), "--read-only".into()],
            env: vec![("ZONE_HOME".into(), r"D:\z".into())],
        };
        let text = with_zone(existing, &windows).unwrap();
        assert!(text.contains("model = \"gpt\"") && text.contains("[mcp_servers.other]") && text.contains("[profiles.fast]\nmodel = \"small\""));
        assert!(!text.contains("/old"));
        assert_eq!(text.matches("[mcp_servers.zone").count(), 1);
        // Backslashes are escaped so the path survives TOML.
        assert!(text.ends_with("[mcp_servers.zone]\ncommand = \"C:\\\\Users\\\\Zed\\\\zone.exe\"\nargs = [\"mcp\", \"--read-only\"]\nenv = { ZONE_HOME = \"D:\\\\z\" }\n"));
        assert_eq!(registration(&text), Some(windows));

        let (text, found) = without_zone(&text);
        assert!(found);
        assert_eq!(text, "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"other\"\n\n[profiles.fast]\nmodel = \"small\"\n");
        assert!(!without_zone(&text).1);
        assert_eq!(registration(&text), None);
    }

    #[test]
    fn quoted_table_names_and_literal_strings() {
        let text = "[mcp_servers.\"zone\"]\ncommand = 'C:\\zone.exe'\nargs = ['mcp']\n[mcp_servers.zoned]\ncommand = \"x\"\n";
        assert_eq!(registration(text).unwrap().command, "C:\\zone.exe");
        let (rest, found) = without_zone(text);
        assert!(found);
        assert_eq!(rest, "[mcp_servers.zoned]\ncommand = \"x\"\n");
        assert_eq!(string("a\u{1}b"), "\"a\\u0001b\"");
    }
}
