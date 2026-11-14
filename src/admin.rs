//! Commands about the setup itself: pair, status, devices, unpair, doctor, import, open.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::daemon_info::{self, DaemonInfo};
use crate::db::Db;
use crate::timeparse::{Clock, human_ago};
use crate::{backup, link, pairing, paths, service, tools};

const START_HINT: &str = "Start it with `zone serve`, or run `zone install` once to start it every time you log in.";

/// `zone pair`: a one-use pairing offer, as a link and a QR code.
pub fn pair(show_qr: Option<bool>, with_relay: bool, as_json: bool) -> Result<()> {
    let dir = paths::data_dir()?;
    let Some(daemon) = DaemonInfo::read_if_running(&dir) else {
        bail!(
            "zone is not running, so a phone would have nothing to pair with.\n{START_HINT}\nThen run `zone pair` again."
        );
    };
    let db = Db::open_default()?;
    let name = paths::computer_name();
    let relay = if with_relay {
        Some(daemon.relay_url.clone().context("the daemon has not connected to a relay yet. Try again in a few seconds, or leave out --relay.")?)
    } else {
        // A relay set by hand is one the phone cannot guess, so it always goes in the link.
        link::custom_relay()?.map(|url| url.to_string())
    };
    let offer = pairing::create_offer(&db, &daemon.endpoint_id, &name, relay)?;
    let url = offer.link.to_url();
    let clock = Clock::system();
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "url": url,
                "endpoint_id": daemon.endpoint_id,
                "name": name,
                "expires_at": offer.expires_at,
                "expires_local": clock.rfc3339(offer.expires_at),
            }))?
        );
        return Ok(());
    }
    let qr = show_qr.unwrap_or_else(|| std::io::stdout().is_terminal());
    println!("Pair your iPhone with {name}");
    println!();
    if qr {
        print!("{}", pairing::qr_terminal(&url)?);
        println!();
        println!("In Zone on your iPhone, open the Link settings and scan this code.");
    }
    println!("Or paste this link into Zone:");
    println!();
    println!("  {url}");
    println!();
    println!("It works once and expires at {} (ten minutes).", clock.zoned(offer.expires_at).strftime("%H:%M"));
    if !daemon.relay_connected {
        println!();
        println!("Note: zone has not reached an iroh relay yet. Pairing works on the same network. Run `zone doctor` if the phone cannot connect from elsewhere.");
    }
    Ok(())
}

/// `zone status`
pub fn status(as_json: bool) -> Result<()> {
    let dir = paths::data_dir()?;
    let mut db = Db::open_default()?;
    let daemon = DaemonInfo::read_if_running(&dir);
    let mut value = tools::sync_status(&mut db)?;
    value["data_directory"] = json!(dir);
    value["database"] = json!(paths::database(&dir));
    value["daemon"] = match &daemon {
        Some(d) => json!({
            "running": true,
            "pid": d.pid,
            "endpoint_id": d.endpoint_id,
            "web_page": format!("http://127.0.0.1:{}/", d.http_port),
            "relay_connected": d.relay_connected,
            "relay_url": d.relay_url,
        }),
        None => json!({ "running": false, "endpoint_id": stored_endpoint_id(&dir) }),
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let clock = Clock::system();
    let now = paths::now_unix();
    match &daemon {
        Some(d) => {
            println!("Zone is running (pid {}).", d.pid);
            println!("  Endpoint id   {}", d.endpoint_id);
            println!("  Web page      http://127.0.0.1:{}/  (open with `zone open`)", d.http_port);
            match (&d.relay_url, d.relay_connected) {
                (Some(url), true) => println!("  Relay         connected to {url}"),
                _ => println!("  Relay         not connected (phones on other networks cannot reach this computer)"),
            }
        }
        None => {
            println!("Zone is not running. {START_HINT}");
            if let Some(id) = stored_endpoint_id(&dir) {
                println!("  Endpoint id   {id}");
            }
        }
    }
    println!();
    let devices = db.devices()?;
    if devices.is_empty() {
        println!("No phone is paired. Run `zone pair`.");
    } else {
        println!("Paired phones");
        for device in &devices {
            let synced = match device.last_sync {
                Some(t) => format!("last sync {} ({})", clock.short(t), human_ago(now - t)),
                None => "never synced".to_string(),
            };
            println!("  {}  {synced}, {} items received", device.name.as_deref().unwrap_or("iPhone"), device.items_received);
        }
    }
    println!();
    let total = db.total_items()?;
    let kinds: Vec<String> = db.counts_by_kind()?.iter().map(|(kind, count)| format!("{count} {kind}")).collect();
    println!("Library       {total} items{}", if kinds.is_empty() { String::new() } else { format!(" ({})", kinds.join(", ")) });
    if let Some((oldest, newest)) = db.time_span()? {
        println!("              from {} to {}", clock.short(oldest), clock.short(newest));
    }
    for (label, key) in [("Link", "last_sync_link"), ("Backup folder", "last_sync_backup")] {
        if let Some(t) = db.meta_f64(key)? {
            println!("{label:13} last delivered {} ({})", clock.short(t), human_ago(now - t));
        }
    }
    let waiting = db.open_offers(now)?;
    if waiting > 0 {
        println!("Pairing       {waiting} unused pairing {} still valid", if waiting == 1 { "code is" } else { "codes are" });
    }
    match db.meta_get("backup_folder")? {
        Some(folder) => println!("Backup folder {folder}"),
        None => println!("Backup folder not set (`zone backup-folder <path>` to watch one)"),
    }
    println!("Database      {}", paths::database(&dir).display());
    Ok(())
}

fn stored_endpoint_id(dir: &Path) -> Option<String> {
    link::read_key(dir).ok().flatten().map(|key| link::endpoint_id_text(&key))
}

/// `zone devices`
pub fn devices(as_json: bool) -> Result<()> {
    let db = Db::open_default()?;
    let clock = Clock::system();
    let now = paths::now_unix();
    let devices = db.devices()?;
    if as_json {
        let list: Vec<Value> = devices
            .iter()
            .map(|d| {
                json!({
                    "name": d.name, "phone_id": d.phone_id, "endpoint_id": d.endpoint_id, "app": d.app,
                    "paired_at": d.paired_at, "last_seen": d.last_seen, "last_sync": d.last_sync,
                    "items_received": d.items_received, "last_path": d.last_path,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if devices.is_empty() {
        println!("No phone is paired. Run `zone pair`.");
        return Ok(());
    }
    for device in &devices {
        println!("{}", device.name.as_deref().unwrap_or("iPhone"));
        println!("  id         {}", device.endpoint_id);
        if let Some(app) = &device.app {
            println!("  app        Zone {app}");
        }
        println!("  paired     {}", clock.short(device.paired_at));
        match device.last_sync {
            Some(t) => println!("  last sync  {} ({})", clock.short(t), human_ago(now - t)),
            None => println!("  last sync  never"),
        }
        println!("  received   {} items", device.items_received);
        if let Some(path) = &device.last_path {
            println!("  connection {path}");
        }
    }
    Ok(())
}

/// `zone unpair <device>`
pub fn unpair(spec: &str) -> Result<()> {
    let db = Db::open_default()?;
    let device = db.find_device(spec)?;
    db.remove_device(&device.endpoint_id)?;
    println!(
        "Unpaired {}. It can no longer sync to this computer. The items it already sent stay in the library.",
        device.name.as_deref().unwrap_or("the phone")
    );
    Ok(())
}

/// `zone resync [device]`
pub fn resync(spec: Option<&str>) -> Result<()> {
    let db = Db::open_default()?;
    let count = match spec {
        Some(spec) => {
            let device = db.find_device(spec)?;
            db.request_resend(Some(&device.endpoint_id))?
        }
        None => db.request_resend(None)?,
    };
    if count == 0 {
        println!("No phone is paired.");
    } else {
        println!("At its next sync, {} will send its whole library again.", if count == 1 { "the phone".to_string() } else { format!("each of the {count} phones") });
    }
    Ok(())
}

/// `zone import <path>`
pub fn import(path: &Path) -> Result<()> {
    let mut db = Db::open_default()?;
    let reports = backup::import_path(&mut db, path, paths::now_unix())?;
    let clock = Clock::system();
    for report in &reports {
        println!("Read {}", report.manifest.display());
        if let Some(written) = report.written_at {
            println!("  written by the phone {}", clock.short(written));
        }
        println!("  {} notes, {} dictations, {} meetings", report.notes, report.dictations, report.meetings);
        println!(
            "  {} new, {} changed, {} already in the library",
            report.applied.inserted,
            report.applied.replaced,
            report.applied.unchanged + report.applied.kept_newer
        );
        if report.skipped > 0 {
            println!("  {} records had no id or date and were skipped", report.skipped);
        }
    }
    println!("The library now holds {} items.", db.total_items()?);
    Ok(())
}

/// `zone backup-folder [path] [--clear]`
pub fn backup_folder(path: Option<&Path>, clear: bool) -> Result<()> {
    let db = Db::open_default()?;
    let dir = paths::data_dir()?;
    if clear {
        db.meta_delete("backup_folder")?;
        db.meta_delete("backup_seen")?;
        println!("No backup folder is watched any more. Items already imported stay in the library.");
        return Ok(());
    }
    let Some(path) = path else {
        match db.meta_get("backup_folder")? {
            Some(folder) => println!("{folder}"),
            None => println!("No backup folder is set. Give a path: `zone backup-folder <path>`."),
        }
        return Ok(());
    };
    let absolute = std::fs::canonicalize(path).with_context(|| format!("cannot find {}", path.display()))?;
    // Fail now, with a clear message, rather than silently in the daemon.
    let manifests = backup::locate(&absolute)?;
    db.meta_set("backup_folder", &absolute.to_string_lossy())?;
    db.meta_delete("backup_seen")?;
    println!("Watching {}", absolute.display());
    for manifest in &manifests {
        println!("  found {}", manifest.display());
    }
    if daemon_info::is_running(&dir) {
        println!("The running daemon will read it within half a minute, and again whenever it changes.");
    } else {
        println!("Zone is not running, so nothing is read yet. {START_HINT}");
        println!("To read it once right now: `zone import \"{}\"`.", absolute.display());
    }
    Ok(())
}

/// `zone open`
pub fn open(print_only: bool) -> Result<()> {
    let dir = paths::data_dir()?;
    let Some(daemon) = DaemonInfo::read_if_running(&dir) else {
        bail!("zone is not running, so there is no page to open.\n{START_HINT}");
    };
    let token = std::fs::read_to_string(paths::web_token(&dir)).context("the web token is missing. Restart `zone serve`.")?;
    let url = format!("http://127.0.0.1:{}/?t={}", daemon.http_port, token.trim());
    if print_only {
        println!("{url}");
        return Ok(());
    }
    open_in_browser(&url)?;
    println!("Opened http://127.0.0.1:{}/ in your browser.", daemon.http_port);
    Ok(())
}

fn open_in_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(windows)]
    let mut command = {
        // rundll32 takes the URL as one argument, so nothing in it is read by a shell.
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let status = command.status().context("cannot start the browser. Run `zone open --print` and paste the link yourself.")?;
    if !status.success() {
        bail!("the browser did not open. Run `zone open --print` and paste the link yourself.");
    }
    Ok(())
}

// -------------------------------------------------------------------- doctor

struct Check {
    name: &'static str,
    /// `ok`, `warn` or `fail`.
    level: &'static str,
    detail: String,
    fix: Option<String>,
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check { name, level: "ok", detail: detail.into(), fix: None }
}
fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check { name, level: "warn", detail: detail.into(), fix: Some(fix.into()) }
}
fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check { name, level: "fail", detail: detail.into(), fix: Some(fix.into()) }
}

/// `zone doctor`: checks the setup and says in plain words how to fix what is wrong.
/// Returns false when something is broken (warnings alone do not count).
pub fn doctor(as_json: bool) -> Result<bool> {
    let checks = run_checks();
    let healthy = checks.iter().all(|c| c.level != "fail");
    if as_json {
        let list: Vec<Value> = checks.iter().map(|c| json!({ "check": c.name, "result": c.level, "detail": c.detail, "fix": c.fix })).collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "healthy": healthy, "checks": list }))?);
        return Ok(healthy);
    }
    for check in &checks {
        let mark = match check.level {
            "ok" => "ok  ",
            "warn" => "note",
            _ => "FAIL",
        };
        println!("[{mark}] {}: {}", check.name, check.detail);
        if let Some(fix) = &check.fix {
            for line in fix.lines() {
                println!("       {line}");
            }
        }
    }
    println!();
    let problems = checks.iter().filter(|c| c.level == "fail").count();
    let notes = checks.iter().filter(|c| c.level == "warn").count();
    match (problems, notes) {
        (0, 0) => println!("Everything looks right."),
        (0, n) => println!("Nothing is broken. {n} {} above may be worth a look.", if n == 1 { "note" } else { "notes" }),
        (p, _) => println!("{p} {} to fix. Start with the first FAIL.", if p == 1 { "thing" } else { "things" }),
    }
    Ok(healthy)
}

fn run_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    let dir = match paths::data_dir() {
        Ok(dir) => dir,
        Err(error) => {
            checks.push(fail("Data folder", format!("{error:#}"), "Set the HOME environment variable, or set ZONE_HOME to the folder zone should use."));
            return checks;
        }
    };

    // Data folder
    if !dir.exists() {
        checks.push(warn(
            "Data folder",
            format!("{} does not exist yet", dir.display()),
            "Nothing has been set up on this computer. Run `zone serve` (or `zone install`), then `zone pair`.",
        ));
        return checks;
    }
    let probe = dir.join(format!(".write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            checks.push(ok("Data folder", dir.display().to_string()));
        }
        Err(error) => checks.push(fail(
            "Data folder",
            format!("cannot write to {} ({error})", dir.display()),
            "Make the folder writable by your user, or point ZONE_HOME at a folder that is.",
        )),
    }

    // Library
    let db = match Db::open(&paths::database(&dir)) {
        Ok(db) => Some(db),
        Err(error) => {
            checks.push(fail(
                "Library",
                format!("{error:#}"),
                "If the file is damaged, move zone.db aside and start `zone serve` again: the phone sends everything again to a new library.",
            ));
            None
        }
    };
    if let Some(db) = &db {
        match (db.integrity_ok(), db.fts_available()) {
            (Ok(true), true) => {
                let total = db.total_items().unwrap_or(0);
                checks.push(ok("Library", format!("{total} items, SQLite {} with full-text search", db.sqlite_version())));
            }
            (Ok(true), false) => checks.push(fail(
                "Library",
                "the full-text search index does not answer",
                "This build of zone has an SQLite without FTS5. Use an official build, or rebuild with the bundled SQLite.",
            )),
            _ => checks.push(fail(
                "Library",
                "SQLite reports damage in zone.db",
                "Stop zone, move zone.db aside, start `zone serve`, then run `zone resync` so the phone sends everything again.",
            )),
        }
    }

    // Identity
    let key_path = paths::secret_key(&dir);
    match link::read_key(&dir) {
        Ok(Some(key)) => {
            if paths::is_shared_with_others(&key_path) {
                checks.push(warn(
                    "Identity key",
                    format!("{} can be read by other users of this computer", key_path.display()),
                    format!("Run: chmod 600 \"{}\"", key_path.display()),
                ));
            } else {
                checks.push(ok("Identity key", format!("endpoint id {}", link::endpoint_id_text(&key))));
            }
        }
        Ok(None) => checks.push(warn("Identity key", "not created yet", "It is created the first time `zone serve` runs.")),
        Err(error) => checks.push(fail("Identity key", format!("{error:#}"), "Delete the file and start `zone serve`. Every phone must then pair again.")),
    }

    // Daemon
    let daemon = DaemonInfo::read_if_running(&dir);
    match &daemon {
        Some(d) => {
            checks.push(ok("Daemon", format!("running, pid {}", d.pid)));
            match (&d.relay_url, d.relay_connected) {
                (Some(url), true) => checks.push(ok("Relay", format!("connected to {url}"))),
                _ if paths::now_unix() - d.started_at < 20.0 => {
                    checks.push(warn("Relay", "still connecting (zone started a few seconds ago)", "Run `zone doctor` again in a moment."))
                }
                _ => checks.push(warn(
                    "Relay",
                    "zone has not reached an iroh relay",
                    "Syncing still works when the phone and this computer are on the same network.\n\
                     Away from home the phone needs the relay: check that this computer is online and that\n\
                     outgoing HTTPS (TCP port 443) is not blocked by a firewall or VPN.",
                )),
            }
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], d.http_port));
            match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)) {
                Ok(_) => checks.push(ok("Web page", format!("http://127.0.0.1:{}/ answers (open it with `zone open`)", d.http_port))),
                Err(error) => checks.push(fail("Web page", format!("port {} does not answer ({error})", d.http_port), "Restart zone: stop `zone serve` and start it again.")),
            }
        }
        None => checks.push(fail("Daemon", "zone is not running, so phones cannot sync", START_HINT)),
    }

    // Start at login
    match service::status() {
        Ok(service::Installed::Yes(path)) => checks.push(ok("Start at login", format!("installed ({})", path.display()))),
        Ok(service::Installed::Stale(path, why)) => checks.push(warn("Start at login", format!("{} {why}", path.display()), "Run `zone install` again to refresh it.")),
        Ok(service::Installed::No) => checks.push(warn("Start at login", "not installed", "Run `zone install` so zone starts by itself when you log in.")),
        Err(error) => checks.push(warn("Start at login", format!("could not check ({error:#})"), "Run `zone install`.")),
    }

    // Phones
    if let Some(db) = &db {
        let now = paths::now_unix();
        match db.devices() {
            Ok(devices) if devices.is_empty() => {
                let has_backup = db.meta_get("backup_folder").ok().flatten().is_some();
                checks.push(warn(
                    "Paired phones",
                    "none",
                    if has_backup { "Items arrive from the backup folder only. Run `zone pair` to sync directly from the phone." } else { "Run `zone pair` and scan the code with Zone on your iPhone." },
                ));
            }
            Ok(devices) => {
                for device in devices {
                    let name = device.name.clone().unwrap_or_else(|| "iPhone".to_string());
                    match device.last_sync {
                        Some(t) if now - t > 3.0 * 86400.0 => checks.push(warn(
                            "Paired phones",
                            format!("{name} last synced {}", human_ago(now - t)),
                            "Open Zone on the phone and tap Sync now. If it cannot connect, check that zone is running here and that both devices are online.",
                        )),
                        Some(t) => checks.push(ok("Paired phones", format!("{name} synced {}", human_ago(now - t)))),
                        None => checks.push(warn(
                            "Paired phones",
                            format!("{name} is paired but has never sent anything"),
                            "Open Zone on the phone and tap Sync now.",
                        )),
                    }
                }
            }
            Err(error) => checks.push(fail("Paired phones", format!("{error:#}"), "See the Library check above.")),
        }

        // Backup folder
        match db.meta_get("backup_folder") {
            Ok(Some(folder)) => match backup::locate(&PathBuf::from(&folder)) {
                Ok(manifests) => {
                    let read = db.meta_f64("last_sync_backup").ok().flatten();
                    let detail = match read {
                        Some(t) => format!("{folder} ({} backup, last read {})", manifests.len(), human_ago(now - t)),
                        None => format!("{folder} ({} backup, not read yet)", manifests.len()),
                    };
                    checks.push(ok("Backup folder", detail));
                }
                Err(error) => checks.push(warn(
                    "Backup folder",
                    format!("{error:#}"),
                    "Check that the folder still exists and has finished syncing, or choose another with `zone backup-folder <path>`.\n\
                     On macOS, if the folder is in iCloud Drive, the system may ask once whether zone may read it.",
                )),
            },
            Ok(None) => checks.push(ok("Backup folder", "not used")),
            Err(_) => {}
        }
    }

    let clock = Clock::system();
    checks.push(ok("Clock", format!("{} ({})", clock.full(paths::now_unix()), clock.zone_name())));
    checks
}
