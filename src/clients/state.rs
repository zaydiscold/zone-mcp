//! What zone remembers about registering itself, in `mcp-clients.json` in the data folder.
//!
//! - `registered`: the places zone has added itself to (or found itself in).
//! - `removed`: the places the person took zone out of, with `zone mcp uninstall`, or by hand
//!   after zone had added itself. Nothing automatic ever adds zone back to these.
//! - `auto`: set by `zone install`. When set, `zone serve` adds zone to clients installed
//!   since, but only when it is the program `zone install` was run from.
//!
//! A place is named `<client>:<config file>`, for example `zed:/home/me/.config/zed/settings.json`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const FILE: &str = "mcp-clients.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub auto: bool,
    /// The program `zone install` was run from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// `PATH` when `zone install` ran, so the daemon (which starts with a short one) can still
    /// find a client's own command, like `claude`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_path: Option<String>,
    #[serde(default)]
    pub registered: BTreeSet<String>,
    #[serde(default)]
    pub removed: BTreeSet<String>,
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

impl State {
    /// The saved state, or an empty one when there is none (or it cannot be read).
    pub fn load(dir: &Path) -> State {
        std::fs::read(path(dir)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let text = format!("{}\n", serde_json::to_string_pretty(self)?);
        crate::paths::write_private(&path(dir), text.as_bytes())
    }

    pub fn added(&mut self, id: &str) {
        self.removed.remove(id);
        self.registered.insert(id.to_string());
    }

    pub fn taken_out(&mut self, id: &str) {
        self.registered.remove(id);
        self.removed.insert(id.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()), State::default());
        let mut state = State { auto: true, exe: Some("/opt/zone".into()), ..State::default() };
        state.added("zed:/a");
        state.added("cursor:/b");
        state.taken_out("cursor:/b");
        state.save(dir.path()).unwrap();
        let loaded = State::load(dir.path());
        assert_eq!(loaded, state);
        assert!(loaded.registered.contains("zed:/a") && loaded.removed.contains("cursor:/b") && !loaded.registered.contains("cursor:/b"));
        // A damaged file reads as empty instead of stopping anything.
        std::fs::write(path(dir.path()), "{ nope").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }
}
