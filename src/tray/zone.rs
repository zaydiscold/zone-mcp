//! Asking zone. The tray knows nothing by itself: everything it shows is what the `zone`
//! program beside it prints (`zone status --json`, `zone list --json`, ...), the same way
//! Zone for Mac works. It never opens the library.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::de::DeserializeOwned;

use super::model::{Clients, ClipAnswer, ClipboardSetting, ItemList, LATEST_ASKED, Login, Snapshot, Status};

/// The zone program the tray asks.
#[derive(Clone, Debug)]
pub struct Zone {
    pub program: PathBuf,
}

/// Runs the command without a console window. zone is a console program, and one started by
/// a program that has no console (the tray) would otherwise open a window of its own.
pub fn hide(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

impl Zone {
    /// The zone beside this program: `zone.exe` in the tray's own folder.
    pub fn beside(tray: &Path) -> Zone {
        Zone {
            program: tray.with_file_name(format!("zone{}", std::env::consts::EXE_SUFFIX)),
        }
    }

    pub fn exists(&self) -> bool {
        self.program.is_file()
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(args).stdin(Stdio::null());
        hide(&mut command);
        command
    }

    /// Runs zone and gives what it printed. When zone says it failed, the error is what it
    /// said: its own words are the best ones to show.
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = self
            .command(args)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", self.program.display()))?;
        let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() {
            return Ok(printed);
        }
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(match (said.is_empty(), printed.is_empty()) {
            (false, _) => said.trim_start_matches("zone: ").to_string(),
            (true, false) => printed,
            (true, true) => format!("zone {} did not work", args.join(" ")),
        })
    }

    /// Runs zone and gives everything it printed, whatever it exited with: for commands whose
    /// exit code is part of the answer (`zone doctor`, `zone update --check`).
    pub fn printed(&self, args: &[&str]) -> Result<String, String> {
        let output = self
            .command(args)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", self.program.display()))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let printed = self.run(args)?;
        serde_json::from_str(&printed).map_err(|error| format!("zone {} answered something unexpected ({error})", args.join(" ")))
    }

    /// Like [`Zone::json`], for a command that answers in JSON whatever its exit code.
    pub fn json_whatever<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let printed = self.printed(args)?;
        serde_json::from_str(&printed).map_err(|error| format!("zone {} answered something unexpected ({error})", args.join(" ")))
    }

    /// Starts `zone events --json --follow`: one line for each thing that happens from now on.
    pub fn follow_events(&self) -> std::io::Result<Child> {
        self.command(&["events", "--json", "--follow", "--limit", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }

    /// The version of this zone program: `zone 0.1.4` says `0.1.4`.
    pub fn version(&self) -> Option<String> {
        let printed = self.run(&["--version"]).ok()?;
        Some(printed.strip_prefix("zone ")?.trim().to_string()).filter(|version| !version.is_empty())
    }
}

/// How much to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// Only `zone status`: one program started. Asked every half minute.
    Status,
    /// Everything the menu shows. Asked when something happened.
    Everything,
}

/// Asks zone and gives the new picture. What was not asked this time is kept from `before`.
/// `login` reads the login entry (the registry, on Windows).
pub fn refresh(zone: &Zone, before: &Snapshot, depth: Depth, now: f64, login: &dyn Fn() -> Login) -> Snapshot {
    let mut next = before.clone();
    next.fetched_at = now;
    next.tray_version = env!("CARGO_PKG_VERSION").to_string();
    next.login = login();
    if !zone.exists() {
        next.status = None;
        next.trouble = Some(format!(
            "{} is not beside the tray",
            zone.program.file_name().unwrap_or_default().to_string_lossy()
        ));
        return next;
    }
    match zone.json::<Status>(&["status", "--json"]) {
        Ok(status) => {
            next.status = Some(status);
            next.trouble = None;
        }
        Err(why) => {
            next.status = None;
            next.trouble = Some(first_line(&why));
            return next;
        }
    }
    // The first time, and whenever the library or the running zone is another than before.
    let changed = |a: &Snapshot, b: &Snapshot| {
        let key = |s: &Snapshot| {
            s.status.as_ref().map(|status| {
                (
                    status.items.total,
                    status.daemon_running,
                    status.daemon.version.clone(),
                    status.phones.len(),
                )
            })
        };
        key(a) != key(b)
    };
    if depth == Depth::Status && before.status.is_some() && !changed(before, &next) {
        return next;
    }
    if next.zone_version.is_none() || depth == Depth::Everything {
        next.zone_version = zone.version().or(next.zone_version);
    }
    let limit = LATEST_ASKED.to_string();
    if let Ok(list) = zone.json::<ItemList>(&["list", "--limit", &limit, "--json"]) {
        next.latest = list.items;
    }
    next.meeting = zone
        .json::<ItemList>(&["list", "--kind", "meeting", "--limit", "1", "--json"])
        .ok()
        .and_then(|list| list.items.into_iter().next());
    next.clip = zone.json::<ClipAnswer>(&["clip", "--json"]).ok().and_then(|answer| answer.clip);
    next.clipboard = zone.json::<ClipboardSetting>(&["clipboard", "status", "--json"]).ok();
    next.apps = zone.json::<Clients>(&["mcp", "status", "--json"]).ok().map(|clients| clients.clients);
    next
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_is_looked_for_beside_the_tray() {
        let zone = Zone::beside(Path::new("/opt/zone/bin/zone-tray"));
        assert_eq!(
            zone.program,
            Path::new("/opt/zone/bin").join(format!("zone{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn a_missing_zone_is_said_plainly_and_what_was_known_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let zone = Zone::beside(&dir.path().join("zone-tray"));
        let mut before = Snapshot {
            clip: Some(Default::default()),
            ..Default::default()
        };
        before.status = Some(Default::default());
        let after = refresh(&zone, &before, Depth::Everything, 5.0, &|| Login::On);
        assert!(after.status.is_none());
        assert!(
            after.trouble.as_deref().unwrap().ends_with("is not beside the tray"),
            "{:?}",
            after.trouble
        );
        assert_eq!((after.fetched_at, after.login.clone(), after.clip.is_some()), (5.0, Login::On, true));
        assert_eq!(after.tray_version, env!("CARGO_PKG_VERSION"));
    }

    /// A stand-in for zone: a script that answers the way zone does.
    #[cfg(unix)]
    #[test]
    fn the_picture_is_put_together_from_what_zone_prints() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("zone");
        let script = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/asked"
case "$*" in
  "--version") echo "zone 9.9.9" ;;
  "status --json") echo '{"daemon_running": true, "now": {"date": "2026-11-19"}, "items": {"total": 2}, "phones": [{"name": "Demo iPhone", "last_sync_age_s": 5}]}' ;;
  "list --limit 24 --json") echo '{"total": 2, "items": [{"id": "A", "kind": "note", "text": "Buy lemons", "created_local": "2026-11-19T09:00:00-08:00"}]}' ;;
  "list --kind meeting --limit 1 --json") echo '{"total": 0, "items": []}' ;;
  "clip --json") echo '{"clip": {"id": "C", "first_line": "a link", "text": "a link"}}' ;;
  "clipboard status --json") echo '{"enabled": true, "available": true}' ;;
  "mcp status --json") echo '{"clients": [{"client": "codex", "label": "Codex", "installed": true, "state": "registered"}]}' ;;
  "agents off") echo "zone: the library cannot be opened" >&2; exit 1 ;;
  *) exit 3 ;;
esac
"#;
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let zone = Zone::beside(&dir.path().join("zone-tray"));
        let asked = || std::fs::read_to_string(dir.path().join("asked")).unwrap_or_default().lines().count();

        let first = refresh(&zone, &Snapshot::default(), Depth::Status, 10.0, &|| Login::Off);
        assert!(first.running() && first.trouble.is_none());
        assert_eq!(first.zone_version.as_deref(), Some("9.9.9"));
        assert_eq!(
            (
                first.latest[0].text.as_str(),
                first.meeting.is_none(),
                first.clip.as_ref().unwrap().text.as_str()
            ),
            ("Buy lemons", true, "a link")
        );
        assert!(first.clipboard.as_ref().unwrap().enabled);
        assert_eq!(first.apps.as_ref().unwrap()[0].label, "Codex");
        // The first asking asks everything, whatever depth was wanted.
        assert_eq!(asked(), 7);

        // Nothing changed: half a minute later only `zone status` is run.
        let second = refresh(&zone, &first, Depth::Status, 40.0, &|| Login::Off);
        assert_eq!((asked(), second.fetched_at, second.latest.len()), (8, 40.0, 1));

        // zone's own words are what a failure says.
        assert_eq!(zone.run(&["agents", "off"]), Err("the library cannot be opened".to_string()));
        assert_eq!(zone.run(&["nonsense"]), Err("zone nonsense did not work".to_string()));
    }
}
