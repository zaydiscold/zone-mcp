//! Commands about the setup itself: pair, status, devices, unpair, doctor, import, open.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::daemon_info::{self, DaemonInfo};
use crate::db::Db;
use crate::timeparse::{Clock, human_ago, human_duration};
use crate::{backup, events, link, pairing, paths, service, tools};

const START_HINT: &str = "Start it with `zone serve`, or run `zone install` once to start it every time you log in.";

/// How often a waiting `zone pair` looks at whether a phone has used the code.
const PAIR_POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// Where a pairing offer stands, as JSON: for `zone pair --json --wait` and `zone pair --check`.
fn offer_json(id: &str, status: Option<&crate::db::OfferStatus>, clock: &Clock) -> Value {
    let Some(status) = status else {
        // Never made here, or it ran out long enough ago to have been cleared away.
        return json!({ "id": id, "state": "unknown", "claimed": false });
    };
    json!({
        "id": status.id,
        "state": status.state.as_str(),
        "claimed": status.state == crate::db::OfferState::Claimed,
        "expires_at": status.expires_at,
        "expires_local": clock.rfc3339(status.expires_at),
        "claimed_at": status.claimed_at,
        "device": status.device_id.as_ref().map(|device_id| json!({
            "name": status.device.as_ref().and_then(|d| d.name.clone()),
            "endpoint_id": device_id,
            "app": status.device.as_ref().and_then(|d| d.app.clone()),
        })),
    })
}

/// `zone pair`: a one-use pairing offer, as a link and a QR code. With `wait`, stays until a
/// phone has used the code or it has run out, and returns false when it ran out.
pub fn pair(show_qr: Option<bool>, with_relay: bool, as_json: bool, wait: Option<bool>) -> Result<bool> {
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
    let offer = pairing::create_offer(&db, &daemon.endpoint_id, &name, relay, "cli")?;
    let url = offer.link.to_url();
    let clock = Clock::system();
    let interactive = std::io::stdout().is_terminal();
    // A person at a terminal gets told when the phone has paired. A script gets the link and
    // carries on, unless it asks to wait.
    let wait = wait.unwrap_or(interactive && !as_json);
    if as_json {
        let value = json!({
            "id": offer.id,
            "url": url,
            "endpoint_id": daemon.endpoint_id,
            "name": name,
            "expires_at": offer.expires_at,
            "expires_local": clock.rfc3339(offer.expires_at),
            "state": "waiting",
            "claimed": false,
            "qr_svg": pairing::qr_svg(&url)?,
        });
        if !wait {
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(true);
        }
        // Two lines when waiting: the offer now, what became of it later.
        println!("{value}");
        let status = wait_for_offer(&db, &offer.id);
        println!("{}", offer_json(&offer.id, status.as_ref(), &clock));
        return Ok(status.is_some_and(|s| s.state == crate::db::OfferState::Claimed));
    }
    let qr = show_qr.unwrap_or(interactive);
    println!("Pair your iPhone with {name}");
    println!();
    if qr {
        print!("{}", pairing::qr_terminal(&url)?);
        println!();
        println!("In Zone on your iPhone, open Settings > Computers and scan this code.");
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
    if !wait {
        return Ok(true);
    }
    println!();
    println!("Waiting for the phone. Press Ctrl-C to stop waiting: the code stays valid.");
    match wait_for_offer(&db, &offer.id) {
        Some(status) if status.state == crate::db::OfferState::Claimed => {
            let phone = status.device.and_then(|d| d.name).unwrap_or_else(|| "The phone".to_string());
            println!("{phone} is paired. Its library arrives in a moment: `zone status` shows it, `zone open` shows the page.");
            Ok(true)
        }
        _ => {
            println!("The code ran out before a phone used it. Run `zone pair` for a new one.");
            Ok(false)
        }
    }
}

/// Waits until an offer has been used or has run out, and returns where it stands then.
fn wait_for_offer(db: &Db, id: &str) -> Option<crate::db::OfferStatus> {
    loop {
        let status = db.offer_status(id, paths::now_unix()).ok().flatten()?;
        if status.state != crate::db::OfferState::Waiting {
            return Some(status);
        }
        std::thread::sleep(PAIR_POLL);
    }
}

/// `zone pair --check <id>`: whether a phone has used a pairing code. Returns false when the
/// code was not used (still waiting, run out, or unknown).
pub fn pair_check(id: &str, as_json: bool) -> Result<bool> {
    let db = Db::open_default()?;
    let clock = Clock::system();
    let status = db.offer_status(id, paths::now_unix())?;
    let claimed = status.as_ref().is_some_and(|s| s.state == crate::db::OfferState::Claimed);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&offer_json(id, status.as_ref(), &clock))?);
        return Ok(claimed);
    }
    match status {
        None => println!("No pairing code with that id is known here. Codes that ran out are forgotten."),
        Some(status) => match status.state {
            crate::db::OfferState::Waiting => println!("No phone has used this code yet. It is valid until {}.", clock.zoned(status.expires_at).strftime("%H:%M")),
            crate::db::OfferState::Expired => println!("This code ran out unused. Run `zone pair` for a new one."),
            crate::db::OfferState::Claimed => match status.device {
                Some(device) => println!("{} paired with this code.", device.name.as_deref().unwrap_or("A phone")),
                None => println!("A phone paired with this code, and has been unpaired since."),
            },
        },
    }
    Ok(claimed)
}

/// `zone status`
pub fn status(as_json: bool) -> Result<()> {
    let dir = paths::data_dir()?;
    let mut db = Db::open_default()?;
    let daemon = DaemonInfo::read_if_running(&dir);
    let mut value = tools::sync_status(&mut db, &Default::default())?;
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
    for (label, key) in [("Link", "last_sync_link"), ("Flash", "last_sync_flash"), ("Backup folder", "last_sync_backup")] {
        if let Some(t) = db.meta_f64(key)? {
            println!("{label:13} last delivered {} ({})", clock.short(t), human_ago(now - t));
        }
    }
    let waiting = db.open_offers(now)?;
    if waiting > 0 {
        println!("Pairing       {waiting} unused pairing {} still valid", if waiting == 1 { "code is" } else { "codes are" });
    }
    match db.meta_get("backup_folder")? {
        Some(folder) => {
            println!("Backup folder {folder}");
            let state = &value["backup_folder_state"];
            match state["state"].as_str() {
                Some("waiting") => {
                    println!(
                        "              {} (for {})",
                        backup::waiting_reason(),
                        human_duration(state["waiting_s"].as_f64().unwrap_or(0.0))
                    );
                    for line in backup::waiting_fix().lines() {
                        println!("              {line}");
                    }
                }
                Some("error") => {
                    println!("              cannot be read: {}", state["message"].as_str().unwrap_or(""));
                    println!("              Run `zone doctor` for how to fix it.");
                }
                Some("not_watching") => println!("              not watched while zone is not running"),
                _ => {}
            }
        }
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
    events::record(&db, "unpaired", json!({ "device": device.name, "device_id": device.endpoint_id }));
    println!(
        "Unpaired {}. It can no longer sync to this computer. The items it already sent stay in the library.",
        device.name.as_deref().unwrap_or("the phone")
    );
    Ok(())
}

/// `zone resync [device]`
pub fn resync(spec: Option<&str>) -> Result<()> {
    let db = Db::open_default()?;
    let (count, name) = match spec {
        Some(spec) => {
            let device = db.find_device(spec)?;
            (db.request_resend(Some(&device.endpoint_id))?, Some(device.name.unwrap_or_else(|| "iPhone".to_string())))
        }
        None => (db.request_resend(None)?, None),
    };
    if count > 0 {
        events::record(&db, "resync_requested", json!({ "device": name, "phones": count }));
    }
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
        report.record_event(&db, "import");
        print_import_report(report, &clock);
    }
    println!("The library now holds {} items.", db.total_items()?);
    Ok(())
}

/// How long a foreground command waits for a folder before saying that it is waiting.
const FOLDER_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);

fn print_import_report(report: &backup::ImportReport, clock: &Clock) {
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

/// `zone backup-folder [path] [--clear]`
///
/// With a path: reads the folder here and now, in the foreground, then leaves the watching
/// to the daemon. A terminal usually already has access to iCloud Drive, and when it does not,
/// this is where a permission prompt can be seen and answered.
pub fn backup_folder(path: Option<&Path>, clear: bool) -> Result<()> {
    let mut db = Db::open_default()?;
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

    // Everything that touches the folder runs on its own thread, so that a read held up by
    // the operating system can be explained instead of looking like a hang.
    let wanted = path.to_path_buf();
    let work = move || -> Result<(PathBuf, Vec<backup::Scanned>)> {
        let absolute = std::fs::canonicalize(&wanted).with_context(|| format!("cannot find {}", wanted.display()))?;
        // Windows writes a resolved path as \\?\C:\..., which is no good to show or to store.
        let absolute = PathBuf::from(paths::without_verbatim_prefix(&absolute.to_string_lossy()));
        let scanned = backup::scan(&absolute, &backup::Seen::new())?;
        Ok((absolute, scanned))
    };
    let outcome = match backup::with_patience(FOLDER_PATIENCE, work) {
        backup::Patient::Done(outcome) => outcome,
        backup::Patient::Waiting(receiver) => {
            eprintln!("Still reading {} ...", path.display());
            if cfg!(target_os = "macos") {
                eprintln!("If macOS is asking whether your terminal may access the folder, allow it. Press Ctrl-C to give up.");
            }
            receiver.recv().context("the read ended without an answer")?
        }
    };
    let (absolute, scanned) = match outcome {
        Ok(found) => found,
        Err(error) if backup::is_permission_denied(&error) && cfg!(target_os = "macos") => bail!(
            "{error:#}\nmacOS did not let this terminal read the folder. Allow your terminal app in \
             System Settings > Privacy & Security > Files and Folders (or Full Disk Access), then run this again."
        ),
        // Fail now, with a clear message, rather than silently in the daemon.
        Err(error) => return Err(error),
    };

    // Import now, in the foreground.
    let clock = Clock::system();
    let now = paths::now_unix();
    let mut seen = backup::Seen::new();
    let mut failed = 0;
    for found in &scanned {
        match &found.parsed {
            Ok(parsed) => {
                let report = backup::apply_parsed(&mut db, &found.manifest, parsed, now)?;
                report.record_event(&db, "backup-folder");
                print_import_report(&report, &clock);
                seen.insert(found.key.clone(), found.signature.clone());
            }
            Err(error) => {
                failed += 1;
                eprintln!("Could not read {}: {error:#}", found.manifest.display());
            }
        }
    }
    if failed > 0 && seen.is_empty() {
        bail!("no backup in {} could be read, so the folder is not watched.", absolute.display());
    }
    println!("The library now holds {} items.", db.total_items()?);

    // Then hand the watching to the daemon. It skips what was just imported.
    db.meta_set("backup_seen", &serde_json::to_string(&seen)?)?;
    db.meta_set("backup_folder", &absolute.to_string_lossy())?;
    println!();
    println!("Watching {}", absolute.display());
    if daemon_info::is_running(&dir) {
        println!("The running zone reads it again whenever the backup changes (it looks every half minute).");
        if cfg!(target_os = "macos") {
            println!("If macOS asks whether zone may access the folder, allow it. `zone status` says when zone is waiting for that.");
        }
    } else {
        println!("Zone is not running, so later changes are not picked up yet. {START_HINT}");
    }
    Ok(())
}

/// `zone open`: the history page, or with `flash` the Flash page.
pub fn open(print_only: bool, flash: bool) -> Result<()> {
    let dir = paths::data_dir()?;
    let Some(daemon) = DaemonInfo::read_if_running(&dir) else {
        bail!("zone is not running, so there is no page to open.\n{START_HINT}");
    };
    let token = std::fs::read_to_string(paths::web_token(&dir)).context("the web token is missing. Restart `zone serve`.")?;
    let page = if flash { "flash" } else { "" };
    let url = format!("http://127.0.0.1:{}/{page}?t={}", daemon.http_port, token.trim());
    if print_only {
        println!("{url}");
        return Ok(());
    }
    open_in_browser(&url)?;
    println!("Opened http://127.0.0.1:{}/{page} in your browser.", daemon.http_port);
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

/// Whether the backup folder can be read: by the daemon (which is what matters for watching)
/// and from here. Neither question is allowed to hang this command.
fn backup_folder_check(db: &Db, folder: &str, daemon: Option<&DaemonInfo>, now: f64) -> Check {
    // The daemon's own view first: it is the one that has to read the folder.
    if let Some(waited) = daemon.and_then(|d| d.backup.waiting_for(now)) {
        return warn(
            "Backup folder",
            format!("{} (for {}): {folder}", backup::waiting_reason(), human_duration(waited)),
            backup::waiting_fix(),
        );
    }
    let path = PathBuf::from(folder);
    let from_here = match backup::with_patience(FOLDER_PATIENCE, move || backup::locate(&path)) {
        backup::Patient::Done(result) => Some(result),
        backup::Patient::Waiting(_) => None,
    };
    let daemon_error = daemon.and_then(|d| d.backup.error.clone());
    match from_here {
        None => warn(
            "Backup folder",
            format!("{folder} did not answer within {} seconds: {}", FOLDER_PATIENCE.as_secs(), backup::waiting_reason()),
            backup::waiting_fix(),
        ),
        Some(Err(error)) if backup::is_permission_denied(&error) => warn("Backup folder", format!("{error:#}"), backup::denied_fix()),
        Some(Err(error)) => warn(
            "Backup folder",
            format!("{error:#}"),
            "Check that the folder still exists and has finished syncing, or choose another with `zone backup-folder <path>`.",
        ),
        Some(Ok(manifests)) => {
            if let Some(error) = daemon_error {
                // This terminal can read it and the daemon cannot: a permission, almost always.
                return warn("Backup folder", format!("this terminal can read {folder}, but zone itself cannot: {error}"), backup::denied_fix());
            }
            let read = db.meta_f64("last_sync_backup").ok().flatten();
            let count = manifests.len();
            let detail = match read {
                Some(t) => format!("{folder} ({count} backup, last imported {})", human_ago(now - t)),
                None => format!("{folder} ({count} backup, not imported yet)"),
            };
            ok("Backup folder", detail)
        }
    }
}

/// Which AI clients on this computer can use zone, and the one command that fixes the rest.
fn ai_clients_check() -> Check {
    let Some(crate::clients::Summary { registered, missing, other_copy: other, unreadable }) = crate::clients::doctor_summary() else {
        return warn("AI clients", "could not check", "Run `zone mcp status`.");
    };
    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!("installed but without zone: {}", missing.join(", ")));
    }
    if !other.is_empty() {
        problems.push(format!("starting another copy of zone: {}", other.join(", ")));
    }
    if !problems.is_empty() {
        return warn("AI clients", problems.join("; "), "Run `zone mcp install --all` to register zone with every one of them.");
    }
    if !unreadable.is_empty() {
        return warn("AI clients", unreadable.join("; "), "Fix the file named above, then run `zone mcp install --all`.");
    }
    match registered {
        0 => ok("AI clients", "none found on this computer (zone registers with one when you run `zone mcp install --all`)"),
        n => ok("AI clients", format!("zone is registered in {n} {}", if n == 1 { "place" } else { "places" })),
    }
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

    // AI clients
    checks.push(ai_clients_check());

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
        if let Ok(Some(folder)) = db.meta_get("backup_folder") {
            checks.push(backup_folder_check(db, &folder, daemon.as_ref(), now));
        } else {
            checks.push(ok("Backup folder", "not used"));
        }
    }

    let clock = Clock::system();
    checks.push(ok("Clock", format!("{} ({})", clock.full(paths::now_unix()), clock.zone_name())));
    checks.push(version_check());
    checks
}

/// Whether a newer release is out. One question to GitHub, asked because a person ran
/// `zone doctor`. No answer is not a problem with the setup.
fn version_check() -> Check {
    use crate::update::{self, Check as Found};
    let this = update::current_version();
    match update::check() {
        Found::Latest { latest } if update::is_newer(this, &latest) => ok("Version", format!("zone {this}, newer than the latest release ({latest})")),
        Found::Latest { .. } => ok("Version", format!("zone {this}, the latest")),
        Found::Newer { latest, page } => {
            let how = paths::current_exe().map(|exe| update::how_to_update(update::origin_of(&exe))).unwrap_or_else(|_| "zone update".to_string());
            warn("Version", format!("zone {latest} is out, and this is {this}"), format!("To update: {how}\nWhat is new: {page}"))
        }
        Found::Unknown(why) => ok("Version", format!("zone {this} (could not ask whether a newer one is out: {why})")),
        Found::Off => ok("Version", format!("zone {this} (not asking whether a newer one is out)")),
    }
}
