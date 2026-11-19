//! `zone agents on|off|status`: one switch for every AI agent on this computer.
//!
//! The setting is kept in the library (the `meta` table), like the clipboard setting, so every
//! `zone` program reads the same one: the daemon, the page, Zone for Mac, and each `zone mcp`
//! server a client started. It is on until the person turns it off, so a library from before
//! the switch existed keeps working exactly as it did.
//!
//! Off does not take zone out of any client. Each client still starts `zone mcp`, the server
//! still answers the handshake (so the client does not show a broken server), and it offers one
//! tool that says agent access is off on this computer and how to turn it back on. Every other
//! tool, prompt and resource answers with that same sentence. A server that is already running
//! reads the setting on every request, and tells its client within a few seconds that its
//! lists changed (see `mcp.rs`).
//!
//! Turning it on or off is recorded in the feed (`zone events`) as `agents_switched`.

use anyhow::Result;
use serde_json::{Value, json};

use crate::db::Db;
use crate::events;
use crate::paths;
use crate::timeparse::Clock;

/// The `meta` key. Absent, or anything but `off`, is on.
const SETTING: &str = "agents";
/// When it was last switched, Unix seconds.
const CHANGED_AT: &str = "agents_changed_at";

/// What the person runs to turn it back on, said the same way everywhere.
pub const HOW_TO_TURN_ON: &str =
    "Turn it back on with `zone agents on` in a terminal, the AI agents switch in Zone for Mac's Settings, or the switch under Settings, AI agents on the page `zone open` shows.";

/// The sentence an agent gets for everything while the switch is off.
pub fn off_message() -> String {
    format!("Agent access to Zone is turned off on this computer, so nothing from the person's library can be read through this connection. Tell the person. {HOW_TO_TURN_ON}")
}

/// Whether agents may use the library. A library that cannot be read counts as on: the tools
/// then report what is wrong themselves.
pub fn enabled(db: &Db) -> bool {
    !matches!(db.meta_get(SETTING), Ok(Some(value)) if value == "off")
}

/// Turns agent access on or off. `by` says who did it (`cli`, `page`), for the feed. Returns
/// whether anything changed: switching to what it already is records nothing.
pub fn set_enabled(db: &Db, on: bool, by: &str) -> Result<bool> {
    if enabled(db) == on {
        return Ok(false);
    }
    if on {
        db.meta_delete(SETTING)?;
    } else {
        db.meta_set(SETTING, "off")?;
    }
    db.meta_set(CHANGED_AT, &paths::now_unix().to_string())?;
    events::record(db, "agents_switched", json!({ "on": on, "by": by }));
    Ok(true)
}

/// The setting as `zone agents status --json` prints it, and as the page's settings carry it.
pub fn status_json(db: &Db) -> Value {
    let on = enabled(db);
    let changed = db.meta_f64(CHANGED_AT).ok().flatten();
    let clock = Clock::system();
    json!({
        "agents": if on { "on" } else { "off" },
        "enabled": on,
        "changed_at": changed,
        "changed_local": changed.map(|t| clock.rfc3339(t)),
        "message": if on {
            "AI agents with zone registered can use the library, each within the view it was registered with.".to_string()
        } else {
            format!("AI agents cannot read the library: every zone server answers that access is off. {HOW_TO_TURN_ON}")
        },
    })
}

/// What `zone agents` was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    On,
    Off,
    Status,
}

/// `zone agents on|off|status [--json]`
pub fn run(action: Action, as_json: bool) -> Result<()> {
    let db = Db::open_default()?;
    match action {
        Action::On => {
            set_enabled(&db, true, "cli")?;
        }
        Action::Off => {
            set_enabled(&db, false, "cli")?;
        }
        Action::Status => {}
    }
    let status = status_json(&db);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    if status["enabled"] == true {
        println!("AI agents: on. Agents with zone registered can use your library, each within the view it was registered with.");
        println!("`zone agents off` stops every one of them at once, without unregistering anything.");
    } else {
        println!("AI agents: off. Every client still starts zone, and each is told that access is turned off on this computer.");
        println!("`zone agents on` lets them back in.");
    }
    if action != Action::Status {
        println!("Servers that are already running notice within a few seconds.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_until_turned_off_and_each_switch_is_in_the_feed() {
        let db = Db::open_in_memory().unwrap();
        assert!(enabled(&db), "a library that never heard of the switch is on");
        assert_eq!(status_json(&db)["agents"], "on");
        assert_eq!(status_json(&db)["changed_at"], Value::Null);

        assert!(set_enabled(&db, false, "cli").unwrap());
        assert!(!enabled(&db));
        let status = status_json(&db);
        assert_eq!((status["agents"].as_str(), status["enabled"].as_bool()), (Some("off"), Some(false)));
        assert!(status["changed_at"].as_f64().is_some());
        assert!(status["message"].as_str().unwrap().contains("zone agents on"));
        // The same again changes nothing and records nothing.
        assert!(!set_enabled(&db, false, "cli").unwrap());
        assert!(set_enabled(&db, true, "page").unwrap());
        assert!(enabled(&db));
        assert_eq!(db.meta_get(SETTING).unwrap(), None, "on is stored as nothing at all, as before the switch existed");

        let events = db.events(None, None, 10).unwrap();
        let switched: Vec<Value> = events.iter().filter(|e| e.event == "agents_switched").map(|e| serde_json::from_str(&e.data).unwrap()).collect();
        assert_eq!(switched, vec![json!({ "on": false, "by": "cli" }), json!({ "on": true, "by": "page" })]);
        assert!(!off_message().contains('\u{2014}') && off_message().contains("turned off"));
    }
}
