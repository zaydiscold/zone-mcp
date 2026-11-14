//! `zone mcp install|uninstall|status`: registering this program with AI clients.
//!
//! Each client keeps its MCP servers in its own file. Only the `zone` entry is ever added or
//! removed. Everything else in the file is kept, and a copy of the original is saved beside
//! it the first time zone changes it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::paths;

/// The name the server is registered under.
pub const SERVER_NAME: &str = "zone";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Cursor,
}

impl Client {
    pub const ALL: [Client; 4] = [Client::ClaudeCode, Client::ClaudeDesktop, Client::Codex, Client::Cursor];

    pub fn parse(name: &str) -> Result<Client> {
        match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "claude-code" | "claude" | "claudecode" => Ok(Client::ClaudeCode),
            "claude-desktop" | "claudedesktop" | "desktop" => Ok(Client::ClaudeDesktop),
            "codex" => Ok(Client::Codex),
            "cursor" => Ok(Client::Cursor),
            other => bail!("unknown client \"{other}\". Choose one of: claude-code, claude-desktop, codex, cursor."),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeDesktop => "claude-desktop",
            Client::Codex => "codex",
            Client::Cursor => "cursor",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Codex => "Codex",
            Client::Cursor => "Cursor",
        }
    }

    /// The file this client reads its MCP servers from.
    pub fn config_path(self) -> Result<PathBuf> {
        let home = paths::home_dir()?;
        Ok(match self {
            Client::ClaudeCode => match paths::env_path("CLAUDE_CONFIG_DIR") {
                Some(dir) => dir.join(".claude.json"),
                None => home.join(".claude.json"),
            },
            Client::ClaudeDesktop => claude_desktop_dir(&home).join("claude_desktop_config.json"),
            Client::Codex => match paths::env_path("CODEX_HOME") {
                Some(dir) => dir.join("config.toml"),
                None => home.join(".codex").join("config.toml"),
            },
            Client::Cursor => home.join(".cursor").join("mcp.json"),
        })
    }
}

#[cfg(target_os = "macos")]
fn claude_desktop_dir(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join("Claude")
}

#[cfg(windows)]
fn claude_desktop_dir(home: &Path) -> PathBuf {
    match paths::env_path("APPDATA") {
        Some(dir) => dir.join("Claude"),
        None => home.join("AppData").join("Roaming").join("Claude"),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn claude_desktop_dir(home: &Path) -> PathBuf {
    match paths::env_path("XDG_CONFIG_HOME") {
        Some(dir) => dir.join("Claude"),
        None => home.join(".config").join("Claude"),
    }
}

/// How a client should start the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    /// Extra environment, only when the data directory is overridden.
    pub env: Vec<(String, String)>,
}

impl Launch {
    pub fn current(read_only: bool) -> Result<Launch> {
        let exe = std::env::current_exe().context("cannot find where zone is installed")?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let mut args = vec!["mcp".to_string()];
        if read_only {
            args.push("--read-only".to_string());
        }
        let env = match paths::env_path(paths::ENV_HOME) {
            Some(home) => vec![(paths::ENV_HOME.to_string(), home.to_string_lossy().to_string())],
            None => Vec::new(),
        };
        Ok(Launch { command: exe.to_string_lossy().to_string(), args, env })
    }

    fn json_entry(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("command".into(), json!(self.command));
        entry.insert("args".into(), json!(self.args));
        if !self.env.is_empty() {
            let env: Map<String, Value> = self.env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            entry.insert("env".into(), Value::Object(env));
        }
        Value::Object(entry)
    }
}

// -------------------------------------------------------------------- JSON configs

fn read_json_config(path: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => bail!("{} does not hold a JSON object, so zone will not change it.", path.display()),
        Err(e) => bail!("{} is not valid JSON ({e}), so zone will not change it. Fix or remove the file, then try again.", path.display()),
    }
}

/// Keeps one copy of the file as it was before zone first touched it.
fn keep_original(path: &Path) {
    let backup = PathBuf::from(format!("{}.before-zone", path.display()));
    if path.exists() && !backup.exists() {
        let _ = std::fs::copy(path, &backup);
    }
}

fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let tmp = PathBuf::from(format!("{}.zone-tmp", path.display()));
    std::fs::write(&tmp, contents).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// Adds or replaces `mcpServers.zone` in a JSON config. Returns true when the file changed.
pub fn install_json(path: &Path, launch: &Launch) -> Result<bool> {
    let mut root = read_json_config(path)?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        bail!("\"mcpServers\" in {} is not an object, so zone will not change it.", path.display());
    };
    let entry = launch.json_entry();
    if servers.get(SERVER_NAME) == Some(&entry) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), entry);
    keep_original(path);
    write_atomically(path, &format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?))?;
    Ok(true)
}

/// Removes `mcpServers.zone` from a JSON config. Returns true when it was there.
pub fn uninstall_json(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut root = read_json_config(path)?;
    let removed = root.get_mut("mcpServers").and_then(Value::as_object_mut).map(|servers| servers.remove(SERVER_NAME).is_some()).unwrap_or(false);
    if removed {
        keep_original(path);
        write_atomically(path, &format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?))?;
    }
    Ok(removed)
}

fn json_registration(path: &Path) -> Option<Launch> {
    let root = read_json_config(path).ok()?;
    let entry = root.get("mcpServers")?.get(SERVER_NAME)?;
    Some(Launch {
        command: entry.get("command")?.as_str()?.to_string(),
        args: entry.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
        env: Vec::new(),
    })
}

// -------------------------------------------------------------------- Codex (TOML)

fn toml_string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn codex_block(launch: &Launch) -> String {
    let args: Vec<String> = launch.args.iter().map(|a| toml_string(a)).collect();
    let mut block = format!("[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [{}]\n", toml_string(&launch.command), args.join(", "));
    if !launch.env.is_empty() {
        let pairs: Vec<String> = launch.env.iter().map(|(k, v)| format!("{k} = {}", toml_string(v))).collect();
        block.push_str(&format!("env = {{ {} }}\n", pairs.join(", ")));
    }
    block
}

fn is_zone_header(line: &str) -> bool {
    let line = line.trim();
    let header = format!("[mcp_servers.{SERVER_NAME}]");
    let sub_table = format!("[mcp_servers.{SERVER_NAME}.");
    line == header || line.starts_with(&sub_table)
}

/// The config without zone's tables, and whether any were found.
pub fn codex_without_zone(text: &str) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    let mut found = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            skipping = is_zone_header(trimmed);
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

pub fn install_codex(path: &Path, launch: &Launch) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let (mut text, _) = codex_without_zone(&existing);
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&codex_block(launch));
    if text == existing {
        return Ok(false);
    }
    keep_original(path);
    write_atomically(path, &text)?;
    Ok(true)
}

pub fn uninstall_codex(path: &Path) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let (text, found) = codex_without_zone(&existing);
    if found {
        keep_original(path);
        write_atomically(path, &text)?;
    }
    Ok(found)
}

fn codex_registration(path: &Path) -> Option<Launch> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut inside = false;
    let mut command = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == format!("[mcp_servers.{SERVER_NAME}]");
            continue;
        }
        if inside
            && let Some(value) = trimmed.strip_prefix("command") {
                let value = value.trim_start().strip_prefix('=')?.trim();
                let unquoted = value.trim_matches('"').replace("\\\\", "\\").replace("\\\"", "\"");
                command = Some(unquoted);
            }
    }
    command.map(|command| Launch { command, args: Vec::new(), env: Vec::new() })
}

// -------------------------------------------------------------------- Claude Code

/// Finds a program on PATH.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    #[cfg(windows)]
    let candidates = [format!("{name}.exe"), format!("{name}.cmd"), name.to_string()];
    #[cfg(not(windows))]
    let candidates = [name.to_string()];
    for dir in std::env::split_paths(&path) {
        for candidate in &candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

fn claude_code_manual(launch: &Launch) -> String {
    let mut command = format!("claude mcp add --scope user {SERVER_NAME}");
    for (key, value) in &launch.env {
        command.push_str(&format!(" --env {key}=\"{value}\""));
    }
    command.push_str(&format!(" -- \"{}\" {}", launch.command, launch.args.join(" ")));
    command
}

fn install_claude_code(launch: &Launch) -> Result<()> {
    let Some(claude) = find_on_path("claude") else {
        println!("The `claude` command is not on your PATH, so zone could not register itself.");
        println!("Install Claude Code, then run `zone mcp install claude-code` again, or run this yourself:");
        println!();
        println!("  {}", claude_code_manual(launch));
        bail!("Claude Code was not found");
    };
    // Replace an older registration rather than fail on "already exists".
    let _ = std::process::Command::new(&claude).args(["mcp", "remove", "--scope", "user", SERVER_NAME]).output();
    let mut command = std::process::Command::new(&claude);
    command.args(["mcp", "add", "--scope", "user", SERVER_NAME]);
    for (key, value) in &launch.env {
        command.arg("--env").arg(format!("{key}={value}"));
    }
    command.arg("--").arg(&launch.command).args(&launch.args);
    let output = command.output().with_context(|| format!("cannot run {}", claude.display()))?;
    if !output.status.success() {
        bail!(
            "`claude mcp add` failed: {}\nYou can run it yourself:\n  {}",
            String::from_utf8_lossy(&output.stderr).trim(),
            claude_code_manual(launch)
        );
    }
    Ok(())
}

fn uninstall_claude_code() -> Result<bool> {
    let Some(claude) = find_on_path("claude") else {
        bail!("The `claude` command is not on your PATH. To remove zone by hand, delete \"{SERVER_NAME}\" from mcpServers in ~/.claude.json.");
    };
    let output = std::process::Command::new(&claude)
        .args(["mcp", "remove", "--scope", "user", SERVER_NAME])
        .output()
        .with_context(|| format!("cannot run {}", claude.display()))?;
    Ok(output.status.success())
}

// -------------------------------------------------------------------- commands

pub fn install(client: Client, read_only: bool) -> Result<()> {
    let launch = Launch::current(read_only)?;
    match client {
        Client::ClaudeCode => {
            install_claude_code(&launch)?;
            println!("Registered zone with Claude Code for your user. New sessions will have it.");
        }
        Client::ClaudeDesktop | Client::Cursor => {
            let path = client.config_path()?;
            let changed = install_json(&path, &launch)?;
            println!("{} zone in {}", if changed { "Registered" } else { "Already registered:" }, path.display());
            println!("Restart {} to pick it up.", client.label());
        }
        Client::Codex => {
            let path = client.config_path()?;
            let changed = install_codex(&path, &launch)?;
            println!("{} zone in {}", if changed { "Registered" } else { "Already registered:" }, path.display());
            println!("New Codex sessions will have it.");
        }
    }
    println!("It runs: {} {}", launch.command, launch.args.join(" "));
    if read_only {
        println!("Read-only: agents cannot flag items as processed.");
    }
    Ok(())
}

pub fn uninstall(client: Client) -> Result<()> {
    let removed = match client {
        Client::ClaudeCode => uninstall_claude_code()?,
        Client::ClaudeDesktop | Client::Cursor => uninstall_json(&client.config_path()?)?,
        Client::Codex => uninstall_codex(&client.config_path()?)?,
    };
    if removed {
        println!("Removed zone from {}.", client.label());
    } else {
        println!("zone was not registered with {}.", client.label());
    }
    Ok(())
}

/// Where zone is registered, read without changing anything.
pub fn status() -> Result<()> {
    let current = Launch::current(false)?;
    println!("This zone: {}", current.command);
    println!();
    for client in Client::ALL {
        let path = client.config_path()?;
        let registered = match client {
            Client::Codex => codex_registration(&path),
            _ => json_registration(&path),
        };
        match registered {
            Some(launch) if launch.command == current.command => {
                let mode = if launch.args.iter().any(|a| a == "--read-only") { " (read-only)" } else { "" };
                println!("{:15} registered{mode}  [{}]", client.label(), path.display());
            }
            Some(launch) => {
                println!("{:15} registered, but it starts another copy: {}", client.label(), launch.command);
                println!("{:15} run `zone mcp install {}` to point it here", "", client.name());
            }
            None => println!("{:15} not registered  (`zone mcp install {}`)", client.label(), client.name()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch { command: "/opt/zone/bin/zone".to_string(), args: vec!["mcp".to_string()], env: Vec::new() }
    }

    #[test]
    fn json_config_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Claude").join("claude_desktop_config.json");
        // A missing file and folder are created.
        assert!(install_json(&path, &launch()).unwrap());
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root, json!({ "mcpServers": { "zone": { "command": "/opt/zone/bin/zone", "args": ["mcp"] } } }));
        // Installing again changes nothing.
        assert!(!install_json(&path, &launch()).unwrap());

        // An existing config with other servers and settings is preserved, in order.
        let existing = "{\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"files\": { \"command\": \"npx\", \"args\": [\"-y\", \"files\"] }\n  },\n  \"last\": true\n}\n";
        std::fs::write(&path, existing).unwrap();
        let read_only = Launch { args: vec!["mcp".into(), "--read-only".into()], env: vec![("ZONE_HOME".into(), "/tmp/z".into())], ..launch() };
        assert!(install_json(&path, &read_only).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["theme"], "dark");
        assert_eq!(root["mcpServers"]["files"]["command"], "npx");
        assert_eq!(root["mcpServers"]["zone"]["args"], json!(["mcp", "--read-only"]));
        assert_eq!(root["mcpServers"]["zone"]["env"]["ZONE_HOME"], "/tmp/z");
        assert!(text.find("\"theme\"").unwrap() < text.find("\"mcpServers\"").unwrap());
        assert!(text.find("\"mcpServers\"").unwrap() < text.find("\"last\"").unwrap());
        // The original is kept beside it.
        assert_eq!(std::fs::read_to_string(format!("{}.before-zone", path.display())).unwrap(), existing);
        assert_eq!(json_registration(&path).unwrap().command, "/opt/zone/bin/zone");

        // Removing takes out only zone.
        assert!(uninstall_json(&path).unwrap());
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root, json!({ "theme": "dark", "mcpServers": { "files": { "command": "npx", "args": ["-y", "files"] } }, "last": true }));
        assert!(!uninstall_json(&path).unwrap());
        assert!(json_registration(&path).is_none());
    }

    #[test]
    fn broken_json_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(install_json(&path, &launch()).unwrap_err().to_string().contains("not valid JSON"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        std::fs::write(&path, "[1, 2]").unwrap();
        assert!(install_json(&path, &launch()).is_err());
        std::fs::write(&path, "{ \"mcpServers\": 5 }").unwrap();
        assert!(install_json(&path, &launch()).is_err());
        assert!(!uninstall_json(&dir.path().join("missing.json")).unwrap());
    }

    #[test]
    fn codex_toml_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".codex").join("config.toml");
        assert!(install_codex(&path, &launch()).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[mcp_servers.zone]\ncommand = \"/opt/zone/bin/zone\"\nargs = [\"mcp\"]\n");
        assert!(!install_codex(&path, &launch()).unwrap());

        let existing = "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"other\"\n\n[mcp_servers.zone]\ncommand = \"/old/zone\"\nargs = [\"mcp\"]\n\n[mcp_servers.zone.env]\nZONE_HOME = \"/old\"\n\n[profiles.fast]\nmodel = \"small\"\n";
        std::fs::write(&path, existing).unwrap();
        let windows = Launch {
            command: r"C:\Users\Zed\zone.exe".to_string(),
            args: vec!["mcp".into(), "--read-only".into()],
            env: vec![("ZONE_HOME".into(), r"D:\z".into())],
        };
        assert!(install_codex(&path, &windows).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model = \"gpt\"") && text.contains("[mcp_servers.other]") && text.contains("[profiles.fast]\nmodel = \"small\""));
        assert!(!text.contains("/old"));
        assert_eq!(text.matches("[mcp_servers.zone").count(), 1);
        // Backslashes are escaped so the path survives TOML.
        assert!(text.ends_with("[mcp_servers.zone]\ncommand = \"C:\\\\Users\\\\Zed\\\\zone.exe\"\nargs = [\"mcp\", \"--read-only\"]\nenv = { ZONE_HOME = \"D:\\\\z\" }\n"));
        assert_eq!(codex_registration(&path).unwrap().command, r"C:\Users\Zed\zone.exe");

        assert!(uninstall_codex(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"other\"\n\n[profiles.fast]\nmodel = \"small\"\n");
        assert!(!uninstall_codex(&path).unwrap());
    }

    #[test]
    fn client_names() {
        assert_eq!(Client::parse("Claude-Code").unwrap(), Client::ClaudeCode);
        assert_eq!(Client::parse("claude_desktop").unwrap(), Client::ClaudeDesktop);
        assert_eq!(Client::parse("codex").unwrap(), Client::Codex);
        assert_eq!(Client::parse("cursor").unwrap(), Client::Cursor);
        assert!(Client::parse("emacs").unwrap_err().to_string().contains("claude-code, claude-desktop, codex, cursor"));
        assert_eq!(
            claude_code_manual(&launch()),
            "claude mcp add --scope user zone -- \"/opt/zone/bin/zone\" mcp"
        );
    }
}
