//! `zone update`: is there a newer release, and if so, install it.
//!
//! Nothing here runs by itself. `zone update` and `zone doctor` ask GitHub once, when a person
//! runs them, which release is the latest: one request, a few seconds at most, and when it
//! cannot be answered (no network, no `curl`) zone says so and carries on. The daemon never
//! asks.
//!
//! The request is made with the `curl` every supported system has, rather than with a second
//! TLS stack inside zone. Installing is done by the same script that installed zone
//! (`install.sh`, `install.ps1`), which is built into the program, so an update downloads the
//! release, checks it against the release's checksums and swaps the program exactly the way a
//! first install does.
//!
//! - `ZONE_NO_UPDATE_CHECK=1` turns the question off everywhere.
//! - `ZONE_UPDATE_URL` asks another address (a mirror, or a test server) for the latest release.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::paths;

/// The repository releases are published in.
pub const REPO: &str = "zaydiscold/zone-mcp";
const ENV_OFF: &str = "ZONE_NO_UPDATE_CHECK";
const ENV_URL: &str = "ZONE_UPDATE_URL";
/// The longest the question may take.
const PATIENCE_SECONDS: u32 = 6;

const INSTALL_SH: &str = include_str!("../install.sh");
const INSTALL_PS1: &str = include_str!("../install.ps1");

/// What asking for the latest release came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// This is the latest release (or newer than it: a build from source).
    Latest { latest: String },
    /// A newer release exists.
    Newer { latest: String, page: String },
    /// The question was not answered, and why.
    Unknown(String),
    /// Asking is turned off.
    Off,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The numbers of a version: `v0.12.3` and `0.12.3-beta.1` are both `[0, 12, 3]`.
fn numbers(version: &str) -> Vec<u64> {
    let core = version.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next().unwrap_or("");
    core.split('.').map(|part| part.parse::<u64>().unwrap_or(0)).collect()
}

/// True when `latest` is a later version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (mut a, mut b) = (numbers(latest), numbers(current));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// Reads GitHub's answer about the latest release.
fn read_release(body: &str) -> Result<(String, String), String> {
    let value: Value = serde_json::from_str(body).map_err(|_| "the answer was not a release".to_string())?;
    let tag = value["tag_name"].as_str().filter(|tag| tag.chars().any(|c| c.is_ascii_digit()));
    let Some(tag) = tag else {
        return Err(match value["message"].as_str() {
            // What GitHub says for a repository without a published release.
            Some("Not Found") | None => "no release has been published yet".to_string(),
            Some(message) => format!("GitHub answered: {message}"),
        });
    };
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    let page = value["html_url"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://github.com/{REPO}/releases/tag/{tag}"));
    Ok((version, page))
}

/// Asks once which release is the latest. Never fails and never takes long.
pub fn check() -> Check {
    if std::env::var(ENV_OFF).is_ok_and(|value| !value.trim().is_empty() && value.trim() != "0") {
        return Check::Off;
    }
    let url = std::env::var(ENV_URL).ok().filter(|url| !url.trim().is_empty()).unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let asked = Command::new("curl")
        .args(["--silent", "--show-error", "--location", "--max-time", &PATIENCE_SECONDS.to_string()])
        .args(["--header", "Accept: application/vnd.github+json", "--user-agent", &format!("zone/{}", current_version())])
        .arg(&url)
        .stdin(Stdio::null())
        .output();
    let output = match asked {
        Ok(output) => output,
        Err(_) => return Check::Unknown("curl is not on this computer, so zone cannot ask".to_string()),
    };
    if !output.status.success() {
        // No network, a name that does not resolve, a server that does not answer.
        return Check::Unknown("no answer from GitHub (is this computer online?)".to_string());
    }
    match read_release(&String::from_utf8_lossy(&output.stdout)) {
        Ok((latest, page)) if is_newer(&latest, current_version()) => Check::Newer { latest, page },
        Ok((latest, _)) => Check::Latest { latest },
        Err(why) => Check::Unknown(why),
    }
}

/// How this copy of zone got here, which decides how it is updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Put there by the install script (or by hand): the script replaces it.
    Script,
    Homebrew,
    /// `cargo install`.
    Cargo,
    /// Inside the Zone for Mac app, which brings its own.
    MacApp,
    /// A build in a checkout of the source.
    Checkout,
}

pub fn origin_of(exe: &Path) -> Origin {
    let text = exe.to_string_lossy().replace('\\', "/");
    if text.contains(".app/Contents/") {
        Origin::MacApp
    } else if text.contains("/Cellar/") || text.contains("/homebrew/") || text.contains("/linuxbrew/") {
        Origin::Homebrew
    } else if text.contains("/.cargo/bin/") {
        Origin::Cargo
    } else if text.contains("/target/release/") || text.contains("/target/debug/") {
        Origin::Checkout
    } else {
        Origin::Script
    }
}

/// The one line that updates zone, for this copy of it.
pub fn how_to_update(origin: Origin) -> String {
    match origin {
        Origin::Script => "zone update".to_string(),
        Origin::Homebrew => "brew upgrade zone".to_string(),
        Origin::Cargo => format!("cargo install --locked --force --git https://github.com/{REPO} zone"),
        Origin::MacApp => "update Zone for Mac: it brings its own zone".to_string(),
        Origin::Checkout => "git pull && cargo build --release".to_string(),
    }
}

/// The check as `zone update --check --json` prints it.
pub fn check_json(check: &Check, origin: Origin) -> Value {
    let (checked, latest, newer, page, note) = match check {
        Check::Latest { latest } => (true, Some(latest.clone()), false, None, None),
        Check::Newer { latest, page } => (true, Some(latest.clone()), true, Some(page.clone()), None),
        Check::Unknown(why) => (false, None, false, None, Some(why.clone())),
        Check::Off => (false, None, false, None, Some(format!("turned off by {ENV_OFF}"))),
    };
    json!({
        "current": current_version(),
        "latest": latest,
        "newer": newer,
        "checked": checked,
        "page": page,
        "note": note,
        "update_with": how_to_update(origin),
    })
}

/// `zone update`. With `check_only`, only says whether a newer release exists. Returns false
/// when the question could not be answered or the update did not happen.
pub fn run(check_only: bool, force: bool, as_json: bool) -> Result<bool> {
    let exe = paths::current_exe()?;
    let origin = origin_of(&exe);
    let found = check();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&check_json(&found, origin))?);
    }
    let latest = match &found {
        Check::Off => {
            if !as_json {
                println!("Looking for updates is turned off ({ENV_OFF} is set).");
            }
            return Ok(false);
        }
        Check::Unknown(why) => {
            if !as_json {
                println!("Could not find out whether a newer zone exists: {why}.");
                println!("This is zone {}. Releases are at https://github.com/{REPO}/releases", current_version());
            }
            return Ok(false);
        }
        Check::Latest { latest } => {
            if !as_json {
                match is_newer(current_version(), latest) {
                    true => println!("This is zone {}, newer than the latest release ({latest}).", current_version()),
                    false => println!("zone {} is the latest.", current_version()),
                }
            }
            if !force || check_only {
                return Ok(true);
            }
            latest.clone()
        }
        Check::Newer { latest, page } => {
            if !as_json {
                println!("zone {latest} is out. This is {}.", current_version());
                println!("What is new: {page}");
            }
            latest.clone()
        }
    };
    if check_only {
        if !as_json {
            println!("To update: {}", how_to_update(origin));
        }
        return Ok(true);
    }
    if as_json {
        // --json reports, it does not install: a program that asks decides what to do next.
        return Ok(true);
    }
    if origin != Origin::Script {
        println!("This copy of zone was not put here by the install script, so zone will not replace it.");
        println!("To update: {}", how_to_update(origin));
        return Ok(false);
    }
    let folder = exe.parent().context("cannot tell which folder zone is in")?;
    println!();
    install(&latest, folder)?;
    Ok(true)
}

/// Runs the install script built into this program, for one version, into one folder.
fn install(version: &str, folder: &Path) -> Result<()> {
    let mut command;
    let _script_file;
    if cfg!(windows) {
        // PowerShell runs a script from a file more faithfully than from its standard input.
        let path = std::env::temp_dir().join(format!("zone-install-{}.ps1", std::process::id()));
        std::fs::write(&path, INSTALL_PS1).with_context(|| format!("cannot write {}", path.display()))?;
        command = Command::new("powershell");
        command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(&path);
        _script_file = Some(RemoveOnDrop(path));
    } else {
        command = Command::new("sh");
        command.arg("-c").arg(INSTALL_SH);
        _script_file = None;
    }
    let status = command
        .env("ZONE_VERSION", version)
        .env("ZONE_INSTALL_DIR", folder)
        .env("ZONE_INSTALL_UPDATING", "1")
        .stdin(Stdio::null())
        .status()
        .context("cannot start the installer")?;
    if !status.success() {
        bail!("the update did not finish (see above). The zone you have is unchanged.");
    }
    Ok(())
}

struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v0.10.0", "0.9.12"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(is_newer("0.1.1", "0.1"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        // A pre-release of the same numbers is not newer, and odd text is not a version.
        assert!(!is_newer("0.1.0-beta.2", "0.1.0"));
        assert!(!is_newer("latest", "0.1.0"));
        assert_eq!(numbers("v12.0.3+build5"), [12, 0, 3]);
    }

    #[test]
    fn github_answers_are_read() {
        let body = json!({ "tag_name": "v0.4.2", "html_url": "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.4.2", "draft": false }).to_string();
        assert_eq!(read_release(&body), Ok(("0.4.2".to_string(), "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.4.2".to_string())));
        let bare = read_release(&json!({ "tag_name": "0.4.2" }).to_string()).unwrap();
        assert_eq!(bare, ("0.4.2".to_string(), format!("https://github.com/{REPO}/releases/tag/0.4.2")));
        // No release yet, a rate limit, and something that is not JSON at all.
        assert_eq!(read_release(&json!({ "message": "Not Found" }).to_string()), Err("no release has been published yet".to_string()));
        assert!(read_release(&json!({ "message": "API rate limit exceeded" }).to_string()).unwrap_err().contains("rate limit"));
        assert_eq!(read_release("{}"), Err("no release has been published yet".to_string()));
        assert!(read_release("<html>").is_err());
    }

    #[test]
    fn where_zone_is_decides_how_it_updates() {
        let origin = |path: &str| origin_of(Path::new(path));
        assert_eq!(origin("/home/me/.local/bin/zone"), Origin::Script);
        assert_eq!(origin(r"C:\Users\Me\AppData\Local\Zone\bin\zone.exe"), Origin::Script);
        assert_eq!(origin("/opt/homebrew/Cellar/zone/0.2.0/bin/zone"), Origin::Homebrew);
        assert_eq!(origin("/home/linuxbrew/.linuxbrew/bin/zone"), Origin::Homebrew);
        assert_eq!(origin("/home/me/.cargo/bin/zone"), Origin::Cargo);
        assert_eq!(origin("/Applications/Zone.app/Contents/Resources/zone"), Origin::MacApp);
        assert_eq!(origin("/home/me/src/zone-mcp/target/release/zone"), Origin::Checkout);
        assert_eq!(how_to_update(Origin::Script), "zone update");
        assert!(how_to_update(Origin::Homebrew).starts_with("brew upgrade"));

        let value = check_json(&Check::Newer { latest: "9.9.9".into(), page: "https://example.com/r".into() }, Origin::Script);
        assert_eq!((value["current"].as_str(), value["latest"].as_str(), value["newer"].as_bool(), value["checked"].as_bool()), (Some(current_version()), Some("9.9.9"), Some(true), Some(true)));
        let value = check_json(&Check::Unknown("no answer".into()), Origin::Homebrew);
        assert_eq!((value["checked"].as_bool(), value["newer"].as_bool(), &value["latest"], value["note"].as_str()), (Some(false), Some(false), &Value::Null, Some("no answer")));
        assert_eq!(check_json(&Check::Off, Origin::Script)["checked"], false);
    }

    #[test]
    fn the_installers_are_built_in() {
        assert!(INSTALL_SH.starts_with("#!/bin/sh") && INSTALL_SH.contains("ZONE_INSTALL_DIR"));
        assert!(INSTALL_PS1.contains("ZONE_INSTALL_DIR"));
        for script in [INSTALL_SH, INSTALL_PS1] {
            assert!(script.contains(REPO) && script.contains("SHA256SUMS") && !script.contains('\u{2014}'));
        }
    }
}
