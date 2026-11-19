//! Clips: text the person sent from the phone to this computer on purpose (PROTOCOL.md, Clips).
//!
//! A clip is an ordinary item of kind `clip`, so it is stored, listed and searched like any
//! other. This module adds what is special about it:
//!
//! - `zone clip` prints the newest one, `zone clip --copy` also puts it on the clipboard, and
//!   `zone clip list` shows the recent ones as short cards.
//! - `zone clipboard on|off|status`: a setting kept in the library, off until the person turns
//!   it on. With it on, the daemon puts a clip on this computer's clipboard as it arrives, only
//!   when it came over Link and was sent less than two minutes ago. Anything older (a resend,
//!   a Flash, a phone that was offline) is stored and never touches the clipboard.
//! - Every clip that arrives goes in the feed as a `clip` event: its length and source, never
//!   its text. The log never holds a clip's text either.
//!
//! Writing the clipboard is done by the tool the system already has, with the text on its
//! standard input: `pbcopy` on macOS and `wl-copy`, `xclip` or `xsel` on Linux. On Windows zone
//! writes the clipboard itself (src/win.rs), as Unicode text with Windows line ends, and only
//! falls back to `clip.exe`, then PowerShell's `Set-Clipboard`, if that fails.
//! `ZONE_CLIPBOARD_COMMAND` names another command to use instead (run by the shell, the text on
//! its standard input).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::daemon_info::DaemonInfo;
use crate::db::{Db, Filter, Order, Row};
use crate::item::{CLIP, Carrier, Item, preview};
use crate::timeparse::Clock;
use crate::{events, paths};
use crate::{info, warn};

/// A clip goes on the clipboard by itself only when it was sent less than this long ago.
pub const FRESH_SECONDS: f64 = 120.0;
/// A phone's clock may run a little ahead of the computer's: a clip may seem to be from this
/// far in the future and still count as just sent.
const AHEAD_SECONDS: f64 = 30.0;
/// Where the setting lives in the library's `meta` table. Absent means off.
const SETTING: &str = "clipboard";
/// Another command to write the clipboard with. It is run by the shell (`sh -c`, on Windows
/// `cmd /C`) and gets the text, as UTF-8, on its standard input.
pub const ENV_COMMAND: &str = "ZONE_CLIPBOARD_COMMAND";
/// The longest a clipboard tool may take.
const PATIENCE: Duration = Duration::from_secs(5);
const FIRST_LINE_CHARS: usize = 80;

// -------------------------------------------------------------------- the clipboard

/// One way of writing the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tool {
    /// What it is called in `zone clipboard status` and in the feed.
    name: String,
    program: PathBuf,
    args: Vec<String>,
    /// `clip.exe` reads UTF-16 with a byte order mark. Everything else reads UTF-8.
    utf16: bool,
    /// A command line for the shell, instead of a program and its arguments.
    shell: Option<String>,
    /// Not a program at all: zone writes the clipboard itself (Windows).
    native: bool,
}

/// What `zone clipboard status` calls zone writing the Windows clipboard itself.
const NATIVE: &str = "Windows clipboard";

impl Tool {
    fn program(name: &str, program: PathBuf, args: &[&str]) -> Tool {
        Tool { name: name.to_string(), program, args: args.iter().map(|a| a.to_string()).collect(), utf16: false, shell: None, native: false }
    }
}

/// A program in the folders of PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|full| full.is_file())
}

/// `clip.exe`, which also exists inside Windows Subsystem for Linux.
fn clip_exe() -> Option<Tool> {
    let found = on_path("clip.exe").or_else(|| {
        let root = std::env::var_os("SystemRoot")?;
        Some(PathBuf::from(root).join("System32").join("clip.exe")).filter(|full| full.is_file())
    })?;
    Some(Tool { utf16: true, ..Tool::program("clip.exe", found, &[]) })
}

/// The ways of writing the clipboard on this computer, best first. Empty when there is none.
fn tools() -> Vec<Tool> {
    if let Some(command) = std::env::var(ENV_COMMAND).ok().filter(|c| !c.trim().is_empty()) {
        return vec![Tool { name: ENV_COMMAND.to_string(), program: PathBuf::new(), args: Vec::new(), utf16: false, shell: Some(command), native: false }];
    }
    let mut found = Vec::new();
    if cfg!(target_os = "macos") {
        // A login item's PATH is short, so the usual place is tried by name too.
        if let Some(pbcopy) = on_path("pbcopy").or_else(|| Some(PathBuf::from("/usr/bin/pbcopy")).filter(|p| p.is_file())) {
            found.push(Tool::program("pbcopy", pbcopy, &[]));
        }
    } else if cfg!(windows) {
        found.push(Tool { native: true, ..Tool::program(NATIVE, PathBuf::new(), &[]) });
        found.extend(clip_exe());
        let powershell = Some(crate::update::windows_powershell()).filter(|full| full.is_file()).or_else(|| on_path("powershell.exe")).or_else(|| on_path("pwsh.exe"));
        if let Some(powershell) = powershell {
            // Read as UTF-8 whatever the console's code page is.
            let script = "[Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false; Set-Clipboard -Value ([Console]::In.ReadToEnd())";
            found.push(Tool::program("Set-Clipboard", powershell, &["-NoProfile", "-NonInteractive", "-Command", script]));
        }
    } else {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let wayland = set("WAYLAND_DISPLAY");
        let x11 = set("DISPLAY");
        // With no sign of either (a background service started without the session's
        // environment), every tool there is gets its try.
        let unknown = !wayland && !x11;
        if (wayland || unknown)
            && let Some(tool) = on_path("wl-copy") {
                found.push(Tool::program("wl-copy", tool, &[]));
            }
        if x11 || unknown {
            if let Some(tool) = on_path("xclip") {
                found.push(Tool::program("xclip", tool, &["-selection", "clipboard", "-in"]));
            }
            if let Some(tool) = on_path("xsel") {
                found.push(Tool::program("xsel", tool, &["--clipboard", "--input"]));
            }
        }
        found.extend(clip_exe());
    }
    found
}

/// The name of the tool that would write the clipboard, when there is one.
pub fn tool_name() -> Option<String> {
    tools().first().map(|tool| tool.name.clone())
}

/// What to do when no tool was found, for this system.
pub fn how_to_get_a_tool() -> String {
    if cfg!(target_os = "macos") {
        "pbcopy comes with macOS at /usr/bin/pbcopy, and it is missing here.".to_string()
    } else if cfg!(windows) {
        "Windows always has a clipboard, so this should not happen. Set ZONE_CLIPBOARD_COMMAND to a command that reads the text on its standard input.".to_string()
    } else {
        format!("Install wl-clipboard (Wayland), xclip or xsel (X11), or set {ENV_COMMAND} to a command that reads the text on its standard input.")
    }
}

/// The text as a tool wants it.
fn encode(text: &str, utf16: bool) -> Vec<u8> {
    if !utf16 {
        return text.as_bytes().to_vec();
    }
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Text with Windows line ends: every line feed that has no carriage return before it gets one.
/// What the phone sends has line feeds only, and some Windows programs show such text as one
/// line.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_line_ends(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.matches('\n').count());
    let mut previous = None;
    for c in text.chars() {
        if c == '\n' && previous != Some('\r') {
            out.push('\r');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

fn run_tool(tool: &Tool, text: &str) -> Result<()> {
    if tool.native {
        #[cfg(windows)]
        return crate::win::set_clipboard_text(&windows_line_ends(text)).map_err(|e| anyhow!("{e}"));
        #[cfg(not(windows))]
        bail!("only on Windows");
    }
    let mut command = match &tool.shell {
        Some(line) => shell(line),
        None => {
            let mut command = Command::new(&tool.program);
            command.args(&tool.args);
            command
        }
    };
    if tool.name == "pbcopy" {
        // pbcopy reads its input in the locale's encoding, and a login item has no locale:
        // without this, every letter outside ASCII arrives as something else.
        command.env_remove("LC_ALL").env("LC_CTYPE", "UTF-8");
    }
    if tool.name == "Set-Clipboard" {
        // Under a PowerShell 7 terminal, Windows PowerShell would look for its commands in
        // that one's module folders and not find Set-Clipboard.
        command.env_remove("PSModulePath");
    }
    // Some of these tools stay behind to serve the clipboard, so nothing of theirs is waited on
    // but the program itself.
    // A console program started by a zone that has no window would open a window of its own.
    crate::background::hide(&mut command);
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| anyhow!("cannot start it: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let bytes = encode(text, tool.utf16);
    let writer = std::thread::spawn(move || stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_ok());
    let deadline = Instant::now() + PATIENCE;
    loop {
        match child.try_wait()? {
            Some(status) => {
                let written = writer.join().unwrap_or(false);
                if !status.success() {
                    bail!("it ended with {status}");
                }
                if !written {
                    bail!("it did not read the text");
                }
                return Ok(());
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("it did not finish within {} seconds", PATIENCE.as_secs());
            }
            None => std::thread::sleep(Duration::from_millis(15)),
        }
    }
}

#[cfg(windows)]
fn shell(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    // The line goes to cmd as it is written, quotes and all.
    command.arg("/C").raw_arg(line);
    command
}

#[cfg(not(windows))]
fn shell(line: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(line);
    command
}

/// Puts text on this computer's clipboard and returns the name of the tool that did it.
/// Errors name tools and reasons, never the text.
pub fn copy(text: &str) -> Result<String> {
    let tools = tools();
    if tools.is_empty() {
        bail!("there is no clipboard tool on this computer. {}", how_to_get_a_tool());
    }
    let mut failures = Vec::new();
    for tool in &tools {
        match run_tool(tool, text) {
            Ok(()) => return Ok(tool.name.clone()),
            Err(error) => failures.push(format!("{}: {error}", tool.name)),
        }
    }
    bail!("could not write the clipboard ({})", failures.join("; "))
}

// -------------------------------------------------------------------- the setting

/// Whether clips go on the clipboard as they arrive.
pub fn enabled(db: &Db) -> bool {
    matches!(db.meta_get(SETTING), Ok(Some(value)) if value == "on")
}

pub fn set_enabled(db: &Db, on: bool) -> Result<()> {
    if on { db.meta_set(SETTING, "on") } else { db.meta_delete(SETTING) }
}

/// The setting and whether it can work, as `zone clipboard status --json` prints it.
fn setting_json(db: &Db) -> Result<Value> {
    let on = enabled(db);
    let tool = tool_name();
    let dir = paths::data_dir()?;
    Ok(json!({
        "clipboard": if on { "on" } else { "off" },
        "enabled": on,
        "available": tool.is_some(),
        "tool": tool,
        "fix": if tool.is_none() { Some(how_to_get_a_tool()) } else { None },
        "max_age_s": FRESH_SECONDS,
        "daemon_running": DaemonInfo::read_if_running(&dir).is_some(),
    }))
}

/// What `zone clipboard` was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    On,
    Off,
    Status,
}

/// `zone clipboard on|off|status`
pub fn clipboard(action: Setting, as_json: bool) -> Result<()> {
    let db = Db::open_default()?;
    match action {
        Setting::On => set_enabled(&db, true)?,
        Setting::Off => set_enabled(&db, false)?,
        Setting::Status => {}
    }
    let status = setting_json(&db)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    let on = status["enabled"] == true;
    match (action, on) {
        (Setting::On, _) => println!("Clipboard: on. Text you send from Zone on your iPhone goes on this computer's clipboard when it arrives."),
        (Setting::Off, _) => println!("Clipboard: off. Clips are still kept: `zone clip` prints the newest, `zone clip --copy` copies it."),
        (Setting::Status, true) => println!("Clipboard: on. Text you send from Zone on your iPhone goes on this computer's clipboard when it arrives."),
        (Setting::Status, false) => println!("Clipboard: off. `zone clipboard on` puts text you send from Zone on your iPhone on this computer's clipboard."),
    }
    if on {
        println!("Only a clip that comes over Link and was sent in the last {} minutes. Older ones are kept and left alone.", FRESH_SECONDS as u64 / 60);
    }
    match status["tool"].as_str() {
        Some(tool) => println!("Clipboard tool: {tool}"),
        None => println!("Clipboard tool: none found, so nothing can be copied. {}", how_to_get_a_tool()),
    }
    if on && status["daemon_running"] != true {
        println!("zone is not running, so nothing arrives until it is: `zone install` or `zone serve`.");
    }
    Ok(())
}

/// The line `zone doctor` shows: (is it fine, what it says, what to do).
pub fn doctor_line(db: &Db) -> (bool, String, Option<String>) {
    let on = enabled(db);
    match (on, tool_name()) {
        (true, Some(tool)) => (true, format!("on: a clip from the phone goes on the clipboard as it arrives (with {tool})"), None),
        (true, None) => (false, "on, but there is no clipboard tool, so nothing can be copied".to_string(), Some(how_to_get_a_tool())),
        (false, Some(tool)) => (true, format!("off (`zone clipboard on` puts clips from the phone on the clipboard, with {tool})"), None),
        (false, None) => (true, format!("off, and there is no clipboard tool. {}", how_to_get_a_tool()), None),
    }
}

// -------------------------------------------------------------------- arriving

/// Who brought a batch that may hold clips.
pub struct Arrival<'a> {
    pub carrier: Carrier,
    /// The phone's name, for the feed.
    pub device: Option<&'a str>,
    /// The phone's row among the paired phones.
    pub device_id: Option<&'a str>,
    pub now: f64,
}

/// True when a clip made at `created_at` counts as just sent at `now`.
pub fn is_fresh(created_at: f64, now: f64) -> bool {
    let age = now - created_at;
    age < FRESH_SECONDS && age > -AHEAD_SECONDS
}

/// Called after a batch is stored. `changed` holds the positions in `items` of those that were
/// new or different (see [`Db::apply_items_noting`]). Each clip among them goes in the feed, and
/// the newest one goes on the clipboard when the setting is on and it is fresh off Link.
pub fn arrived(db: &Db, items: &[Item], changed: &[usize], arrival: &Arrival<'_>) {
    arrived_with(db, items, changed, arrival, &copy)
}

fn arrived_with(db: &Db, items: &[Item], changed: &[usize], arrival: &Arrival<'_>, copy: &dyn Fn(&str) -> Result<String>) {
    let clips: Vec<&Item> = changed.iter().filter_map(|index| items.get(*index)).filter(|item| item.kind == CLIP && !item.deleted).collect();
    if clips.is_empty() {
        return;
    }
    let on = enabled(db);
    let over_link = arrival.carrier == Carrier::Link;
    // Of several fresh clips in one batch, only the newest is what the person just sent.
    let newest_fresh = clips
        .iter()
        .filter(|clip| is_fresh(clip.created_at, arrival.now))
        .max_by(|a, b| a.created_at.total_cmp(&b.created_at))
        .map(|clip| clip.id.clone());
    for clip in clips {
        let length = clip.text.chars().count();
        let who = arrival.device.unwrap_or("a phone");
        let source = clip.source.as_deref().unwrap_or("unknown");
        let (copied, tool, why_not) = if !on {
            (false, None, Some("off"))
        } else if !over_link {
            (false, None, Some("carrier"))
        } else if !is_fresh(clip.created_at, arrival.now) {
            (false, None, Some("old"))
        } else if newest_fresh.as_deref() != Some(clip.id.as_str()) {
            (false, None, Some("newer_clip"))
        } else {
            match copy(&clip.text) {
                Ok(tool) => (true, Some(tool), None),
                Err(error) => {
                    warn!("clip: could not put a clip on the clipboard: {error:#}");
                    (false, None, Some("failed"))
                }
            }
        };
        match (&tool, why_not) {
            (Some(tool), _) => info!("clip: {length} characters from {who} ({source}), put on the clipboard with {tool}"),
            (None, Some("off") | None) => info!("clip: {length} characters from {who} ({source}), kept"),
            (None, Some(why)) => info!("clip: {length} characters from {who} ({source}), kept and not copied ({why})"),
        }
        events::record(
            db,
            "clip",
            json!({
                "id": clip.id,
                "device": arrival.device,
                "device_id": arrival.device_id,
                "carrier": arrival.carrier.as_str(),
                "source": clip.source,
                "length": length,
                "bytes": clip.text.len(),
                "copied": copied,
                "not_copied": why_not,
                "tool": tool,
            }),
        );
    }
}

// -------------------------------------------------------------------- reading

/// The first line that says something, cut to a card's width.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    preview(line, FIRST_LINE_CHARS)
}

/// A clip as JSON: a card, and with `with_text` the text too.
fn clip_json(row: &Row, clock: &Clock, now: f64, with_text: bool) -> Value {
    let mut value = json!({
        "id": row.id,
        "source": row.source,
        "created_at": row.created_at,
        "created_local": clock.rfc3339(row.created_at),
        "received_at": row.received_at,
        "age_s": (now - row.created_at).max(0.0).round(),
        "carrier": row.carrier,
        "device_id": row.device_id,
        "length": row.text.chars().count(),
        "bytes": row.text.len(),
        "lines": row.text.lines().count(),
        "first_line": first_line(&row.text),
    });
    if with_text {
        value["text"] = json!(row.text);
    }
    value
}

fn clips_filter() -> Filter {
    Filter { kinds: vec![CLIP.to_string()], ..Filter::default() }
}

/// What `zone clip` came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Ok,
    /// There is no clip to show.
    Nothing,
    /// The clip was printed, and copying it failed.
    NotCopied,
}

/// `zone clip`: the newest clip (or the one with this id), its text and nothing else.
pub fn show(id: Option<&str>, copy_it: bool, as_json: bool) -> Result<Shown> {
    let db = Db::open_default()?;
    let clock = Clock::system();
    let row = match id {
        Some(id) => {
            let row = db.resolve(id)?;
            if row.kind != CLIP {
                bail!("{} is a {}, not a clip. `zone show {}` prints it.", &row.id[..row.id.len().min(8)], row.kind, &row.id[..row.id.len().min(8)]);
            }
            if row.deleted {
                bail!("that clip was deleted");
            }
            Some(row)
        }
        None => db.list(&clips_filter(), Order::Newest, 1, None)?.into_iter().next(),
    };
    let Some(row) = row else {
        if as_json {
            let mut value = json!({ "clip": null });
            if copy_it {
                value["copied"] = json!(false);
                value["tool"] = Value::Null;
                value["error"] = json!("there is no clip to copy");
            }
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            eprintln!("No clips yet. Send one from Zone on your iPhone (it needs the app's Computers feature) and it shows up here.");
        }
        return Ok(Shown::Nothing);
    };
    let copied = if copy_it { Some(copy(&row.text)) } else { None };
    if as_json {
        let mut value = json!({ "clip": clip_json(&row, &clock, paths::now_unix(), true) });
        match &copied {
            Some(Ok(tool)) => {
                value["copied"] = json!(true);
                value["tool"] = json!(tool);
                value["error"] = Value::Null;
            }
            Some(Err(error)) => {
                value["copied"] = json!(false);
                value["tool"] = Value::Null;
                value["error"] = json!(format!("{error:#}"));
            }
            None => {}
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        // The text as it is, so `zone clip | pbcopy` and `zone clip > file` carry exactly it.
        // A terminal gets a line end after it, so the prompt starts on a line of its own.
        let terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
        if terminal && !row.text.ends_with('\n') {
            println!("{}", row.text);
        } else {
            print!("{}", row.text);
        }
        match &copied {
            Some(Ok(tool)) if terminal => eprintln!("Copied ({tool})."),
            Some(Err(error)) => eprintln!("zone: not copied: {error:#}"),
            _ => {}
        }
    }
    Ok(if matches!(copied, Some(Err(_))) { Shown::NotCopied } else { Shown::Ok })
}

/// `zone clip list`: the recent clips as short cards, newest first.
pub fn list(limit: usize, as_json: bool) -> Result<()> {
    let db = Db::open_default()?;
    let clock = Clock::system();
    let filter = clips_filter();
    let total = db.count(&filter)?;
    let rows = db.list(&filter, Order::Newest, limit.max(1), None)?;
    let now = paths::now_unix();
    if as_json {
        let clips: Vec<Value> = rows.iter().map(|row| clip_json(row, &clock, now, false)).collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "total": total, "clips": clips }))?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("No clips yet. Send one from Zone on your iPhone (it needs the app's Computers feature) and it shows up here.");
        return Ok(());
    }
    for row in &rows {
        let length = row.text.chars().count();
        let lines = row.text.lines().count();
        let mut head = format!("{}  {}", clock.short(row.created_at), row.source.as_deref().unwrap_or("clip"));
        head.push_str(&format!("  {length} {}", if length == 1 { "character" } else { "characters" }));
        if lines > 1 {
            head.push_str(&format!("  {lines} lines"));
        }
        head.push_str(&format!("  {}", &row.id[..row.id.len().min(8)]));
        println!("{head}");
        println!("    {}", first_line(&row.text));
    }
    if (rows.len() as i64) < total {
        println!("\nShowing {} of {total}. Use --limit N for more. `zone clip --id <id>` prints one.", rows.len());
    } else {
        println!("\n{total} {}. `zone clip` prints the newest, `zone clip --id <id>` another.", if total == 1 { "clip" } else { "clips" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Delivery;
    use std::cell::RefCell;

    fn clip(id: &str, created_at: f64, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: CLIP.to_string(),
            source: Some("clipboard".to_string()),
            created_at,
            updated_at: created_at,
            text: text.to_string(),
            title: None,
            duration_s: None,
            engine: None,
            status: None,
            extra: "{}".to_string(),
            rev: Some(format!("rev-{id}")),
            deleted: false,
        }
    }

    /// Stores a batch the way a carrier does and runs the arrival hook with a clipboard that
    /// only remembers what it was given.
    fn deliver(db: &mut Db, items: &[Item], carrier: Carrier, now: f64) -> Vec<String> {
        let copied = RefCell::new(Vec::new());
        let (_, changed) = db.apply_items_noting(items, &Delivery { carrier, device_id: Some("PHONE"), received_at: now }).unwrap();
        let arrival = Arrival { carrier, device: Some("Test iPhone"), device_id: Some("endpoint"), now };
        arrived_with(db, items, &changed, &arrival, &|text| {
            copied.borrow_mut().push(text.to_string());
            Ok("fake".to_string())
        });
        copied.into_inner()
    }

    fn clip_events(db: &Db) -> Vec<Value> {
        let clock = Clock::in_zone(Some("UTC")).unwrap();
        db.events(None, None, 100).unwrap().iter().filter(|row| row.event == "clip").map(|row| events::to_json(row, &clock)).collect()
    }

    #[test]
    fn a_clip_is_copied_only_when_on_fresh_and_off_link() {
        let now = 1_800_000_000.0;
        let mut db = Db::open_in_memory().unwrap();

        // Off, which is how it starts: stored, in the feed, and the clipboard is left alone.
        assert!(!enabled(&db));
        assert!(deliver(&mut db, &[clip("A", now - 3.0, "first secret words")], Carrier::Link, now).is_empty());
        let events = clip_events(&db);
        assert_eq!((events[0]["copied"].as_bool(), events[0]["not_copied"].as_str(), events[0]["length"].as_u64()), (Some(false), Some("off"), Some(18)));
        assert_eq!((events[0]["device"].as_str(), events[0]["carrier"].as_str(), events[0]["source"].as_str()), (Some("Test iPhone"), Some("link"), Some("clipboard")));

        set_enabled(&db, true).unwrap();
        assert!(enabled(&db));
        // On and fresh off Link: copied. Non-ASCII text goes through whole.
        assert_eq!(deliver(&mut db, &[clip("B", now - 3.0, "caf\u{e9} \u{1F600} second")], Carrier::Link, now), ["caf\u{e9} \u{1F600} second"]);
        // The same clip sent again changes nothing, so it does not go on the clipboard again.
        assert!(deliver(&mut db, &[clip("B", now - 3.0, "caf\u{e9} \u{1F600} second")], Carrier::Link, now + 1.0).is_empty());
        // Two minutes old: kept, not copied. A phone's clock a little ahead is fine, far ahead is not.
        assert!(deliver(&mut db, &[clip("C", now - FRESH_SECONDS, "old")], Carrier::Link, now).is_empty());
        assert_eq!(deliver(&mut db, &[clip("D", now + 10.0, "ahead")], Carrier::Link, now), ["ahead"]);
        assert!(deliver(&mut db, &[clip("E", now + 600.0, "far ahead")], Carrier::Link, now).is_empty());
        // A Flash never touches the clipboard, however new the clip.
        assert!(deliver(&mut db, &[clip("F", now - 1.0, "flashed")], Carrier::Flash, now).is_empty());
        // Of two fresh clips in one batch, the newer one is the one just sent.
        assert_eq!(deliver(&mut db, &[clip("G", now - 50.0, "earlier"), clip("H", now - 5.0, "later")], Carrier::Link, now), ["later"]);
        // Other kinds and deletions are not clips.
        let mut note = clip("N", now - 1.0, "a note");
        note.kind = "note".to_string();
        let mut gone = clip("B", now, "");
        gone.deleted = true;
        assert!(deliver(&mut db, &[note, gone], Carrier::Link, now).is_empty());

        let events = clip_events(&db);
        let seen: Vec<(&str, bool, Option<&str>)> = events.iter().map(|e| (e["id"].as_str().unwrap(), e["copied"].as_bool().unwrap(), e["not_copied"].as_str())).collect();
        assert_eq!(
            seen,
            [
                ("A", false, Some("off")),
                ("B", true, None),
                ("C", false, Some("old")),
                ("D", true, None),
                ("E", false, Some("old")),
                ("F", false, Some("carrier")),
                ("G", false, Some("newer_clip")),
                ("H", true, None),
            ]
        );
        assert_eq!(events[1]["tool"], "fake");
        // The feed holds lengths, never the words.
        let feed = serde_json::to_string(&events).unwrap();
        for word in ["secret", "second", "flashed", "later", "ahead"] {
            assert!(!feed.contains(word), "the feed holds clip text: {feed}");
        }

        set_enabled(&db, false).unwrap();
        assert!(!enabled(&db) && db.meta_get(SETTING).unwrap().is_none());
    }

    #[test]
    fn a_clipboard_that_fails_is_reported_and_the_clip_is_kept() {
        let now = 1_800_000_000.0;
        let mut db = Db::open_in_memory().unwrap();
        set_enabled(&db, true).unwrap();
        let items = [clip("A", now, "words")];
        let (_, changed) = db.apply_items_noting(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: now }).unwrap();
        arrived_with(&db, &items, &changed, &Arrival { carrier: Carrier::Link, device: None, device_id: None, now }, &|_| bail!("no tool"));
        let events = clip_events(&db);
        assert_eq!((events[0]["copied"].as_bool(), events[0]["not_copied"].as_str(), &events[0]["tool"]), (Some(false), Some("failed"), &Value::Null));
        assert_eq!(db.count(&clips_filter()).unwrap(), 1);
    }

    #[test]
    fn cards_show_the_first_line_and_sizes() {
        let clock = Clock::in_zone(Some("UTC")).unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let text = format!("\n  \nfirst real line {}\nsecond line\n", "x".repeat(100));
        db.apply_items(&[clip("A", 1000.0, &text)], &Delivery { carrier: Carrier::Link, device_id: Some("PHONE"), received_at: 1001.0 }).unwrap();
        let row = db.list(&clips_filter(), Order::Newest, 1, None).unwrap().remove(0);
        let card = clip_json(&row, &clock, 1010.0, false);
        let keys: Vec<&String> = card.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["id", "source", "created_at", "created_local", "received_at", "age_s", "carrier", "device_id", "length", "bytes", "lines", "first_line"]);
        assert_eq!(card["first_line"].as_str().unwrap().chars().count(), FIRST_LINE_CHARS + 1);
        assert!(card["first_line"].as_str().unwrap().starts_with("first real line xxx"));
        assert_eq!((card["lines"].as_u64(), card["age_s"].as_f64(), card["carrier"].as_str(), card["device_id"].as_str()), (Some(4), Some(10.0), Some("link"), Some("PHONE")));
        assert_eq!(card["length"].as_u64(), Some(text.chars().count() as u64));
        let full = clip_json(&row, &clock, 1010.0, true);
        assert_eq!(full["text"], text);
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn text_is_encoded_as_each_tool_reads_it() {
        assert_eq!(encode("h\u{e9}", false), [b'h', 0xC3, 0xA9]);
        // clip.exe: a byte order mark, then little-endian UTF-16, surrogate pairs included.
        assert_eq!(encode("h\u{e9}", true), [0xFF, 0xFE, b'h', 0, 0xE9, 0]);
        assert_eq!(encode("\u{1F600}", true), [0xFF, 0xFE, 0x3D, 0xD8, 0x00, 0xDE]);
    }

    #[test]
    fn windows_line_ends_are_added_once() {
        assert_eq!(windows_line_ends("one\ntwo\r\nthree\n"), "one\r\ntwo\r\nthree\r\n");
        assert_eq!(windows_line_ends("\n\n"), "\r\n\r\n");
        assert_eq!(windows_line_ends("no line end, caf\u{e9} \u{1F600}"), "no line end, caf\u{e9} \u{1F600}");
        assert_eq!(windows_line_ends("a lone \r stays"), "a lone \r stays");
    }

    /// The real Windows clipboard: what goes on is what comes back. Run only where a person's
    /// session is (the clipboard of a service or an SSH session belongs to nobody).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn the_windows_clipboard_holds_the_text_exactly() {
        let text = "line one\ncaf\u{e9} na\u{ef}ve \u{65e5}\u{672c}\u{8a9e} \u{1F600}\n";
        let tool = tools().into_iter().find(|tool| tool.native).unwrap();
        run_tool(&tool, text).unwrap();
        let read = std::process::Command::new("powershell").env_remove("PSModulePath").args(["-NoProfile", "-Command", "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&read.stdout).trim_end_matches(['\r', '\n']), windows_line_ends(text).trim_end_matches(['\r', '\n']));
    }

    /// The real path through a command, with a command that writes a file.
    #[cfg(unix)]
    #[test]
    fn a_command_gets_the_text_on_its_standard_input() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clipboard.txt");
        let tool = Tool { name: "test".into(), program: PathBuf::new(), args: Vec::new(), utf16: false, shell: Some(format!("cat > '{}'", file.display())), native: false };
        let text = "line one\ncaf\u{e9} \u{65e5}\u{672c}\u{8a9e} \u{1F600}\n";
        run_tool(&tool, text).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        // A large clip does not stall on a full pipe.
        let large = "0123456789abcdef".repeat(16 * 1024);
        run_tool(&tool, &large).unwrap();
        assert_eq!(std::fs::read(&file).unwrap().len(), 256 * 1024);
        // A command that fails, and one that does not exist, are errors without the text.
        let failing = Tool { shell: Some("exit 3".into()), ..tool.clone() };
        assert!(run_tool(&failing, "words").unwrap_err().to_string().contains("ended"));
        let missing = Tool { program: PathBuf::from("/nonexistent/clipboard-tool"), shell: None, ..tool };
        assert!(!run_tool(&missing, "words").unwrap_err().to_string().contains("words"));
    }
}
