//! `zone serve`: the daemon. It accepts paired phones over iroh, watches the backup folder,
//! and serves the local web page.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use iroh::{Endpoint, Watcher};

use crate::daemon_info::{self, DaemonInfo};
use crate::db::Db;
use crate::link::{self, LinkState};
use crate::web::{self, WebState};
use crate::{backup, info, paths, warn};

/// How often the backup folder is checked for a changed `library.json`.
const BACKUP_POLL: Duration = Duration::from_secs(30);
/// How often the daemon refreshes what it says about itself in `daemon.json`.
const INFO_POLL: Duration = Duration::from_secs(5);

pub struct Options {
    /// Port for the local web page. Falls back to a free port when taken.
    pub port: Option<u16>,
    /// Do not echo the log to stderr (the log file is still written).
    pub quiet: bool,
}

pub async fn run(options: Options) -> Result<()> {
    let dir = paths::ensure_data_dir()?;
    let Some(_lock) = daemon_info::acquire(&dir)? else {
        bail!("zone is already running for {} (see `zone status`).", dir.display());
    };
    crate::log::init(&paths::log_file(&dir), !options.quiet);

    let db = Db::open_default()?;
    let database_id = db.database_id()?;
    let db = Arc::new(Mutex::new(db));

    let key = link::load_or_create_key(&dir)?;
    let endpoint_id = link::endpoint_id_text(&key);
    let endpoint = link::bind(key).await?;
    let computer_name = paths::computer_name();

    let token = web::load_or_create_token(&dir)?;
    let preferred = options.port.or_else(env_port).unwrap_or(web::DEFAULT_PORT);
    let (listener, port) = web::bind(preferred).await?;

    let info = Arc::new(Mutex::new(DaemonInfo {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: paths::now_unix(),
        updated_at: paths::now_unix(),
        endpoint_id: endpoint_id.clone(),
        http_port: port,
        relay_url: None,
        relay_connected: false,
        bound_sockets: endpoint.bound_sockets().iter().map(|s| s.to_string()).collect(),
        direct_addrs: Vec::new(),
    }));
    info.lock().map_err(|_| anyhow!("daemon state is unavailable"))?.write(&dir)?;

    info!("zone {} started for {} (library {database_id})", env!("CARGO_PKG_VERSION"), dir.display());
    info!("link: endpoint id {endpoint_id}");
    info!("web: http://127.0.0.1:{port}/ (open it with `zone open`)");
    if port != preferred {
        warn!("web: port {preferred} was taken, using {port} instead");
    }

    let web_state = Arc::new(WebState {
        db: db.clone(),
        token,
        port,
        endpoint_id: endpoint_id.clone(),
        computer_name: computer_name.clone(),
        info: info.clone(),
    });
    let web_task = tokio::spawn(web::serve(listener, web_state));
    let link_task = tokio::spawn(link::accept_loop(endpoint.clone(), LinkState::new(db.clone(), computer_name)));
    let info_task = tokio::spawn(keep_info_fresh(endpoint.clone(), info.clone(), dir.clone()));
    let backup_task = tokio::spawn(watch_backup_folder(db.clone()));

    wait_for_stop().await;
    info!("zone is stopping");
    for task in [web_task, link_task, info_task, backup_task] {
        task.abort();
    }
    // Lets open sessions finish their last frame and tells peers the endpoint is gone.
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.close()).await;
    let _ = std::fs::remove_file(paths::daemon_info(&dir));
    Ok(())
}

fn env_port() -> Option<u16> {
    std::env::var("ZONE_PORT").ok()?.trim().parse().ok()
}

async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut hangup = signal(SignalKind::hangup()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match terminate.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {}
            _ = async { match hangup.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Keeps `daemon.json` describing how this computer can be reached right now.
async fn keep_info_fresh(endpoint: Endpoint, info: Arc<Mutex<DaemonInfo>>, dir: PathBuf) {
    let mut was_connected = false;
    let mut polls = 0u32;
    loop {
        let addr = endpoint.addr();
        let relay_url = addr.relay_urls().next().map(|url| url.to_string());
        let relay_connected = endpoint.home_relay_status().get().iter().any(|status| status.is_connected());
        let direct_addrs: Vec<String> = addr.ip_addrs().map(|a| a.to_string()).collect();
        let bound: Vec<String> = endpoint.bound_sockets().iter().map(|s| s.to_string()).collect();
        if relay_connected != was_connected {
            match (&relay_url, relay_connected) {
                (Some(url), true) => info!("link: reachable through the relay {url}"),
                (_, false) => warn!("link: no relay connection. Phones on other networks cannot reach this computer until it returns."),
                (None, true) => {}
            }
            was_connected = relay_connected;
        }
        if let Ok(mut current) = info.lock() {
            let changed = current.relay_url != relay_url
                || current.relay_connected != relay_connected
                || current.direct_addrs != direct_addrs
                || current.bound_sockets != bound;
            current.relay_url = relay_url;
            current.relay_connected = relay_connected;
            current.direct_addrs = direct_addrs;
            current.bound_sockets = bound;
            // Rewritten at least once a minute so `zone doctor` can tell it is alive.
            if changed || paths::now_unix() - current.updated_at > 60.0 {
                current.updated_at = paths::now_unix();
                let _ = current.write(&dir);
            }
        }
        // Look often while the relay connection is still coming up, then settle down.
        let young = polls < 30;
        polls += 1;
        tokio::time::sleep(if young { Duration::from_secs(1) } else { INFO_POLL }).await;
    }
}

/// Re-imports the backup folder whenever a `library.json` in it changes.
async fn watch_backup_folder(db: Arc<Mutex<Db>>) {
    let mut announced: Option<String> = None;
    loop {
        let db = db.clone();
        let previous = announced.clone();
        let outcome = tokio::task::spawn_blocking(move || backup_pass(&db, previous)).await;
        match outcome {
            Ok(Ok(folder)) => announced = folder,
            Ok(Err(error)) => warn!("backup folder: {error:#}"),
            Err(error) => warn!("backup folder: the check failed: {error}"),
        }
        tokio::time::sleep(BACKUP_POLL).await;
    }
}

/// One look at the backup folder. Returns the folder being watched, if any.
fn backup_pass(db: &Arc<Mutex<Db>>, announced: Option<String>) -> Result<Option<String>> {
    let folder = {
        let db = db.lock().map_err(|_| anyhow!("the library is unavailable"))?;
        db.meta_get("backup_folder")?
    };
    let Some(folder) = folder else {
        return Ok(None);
    };
    if announced.as_deref() != Some(folder.as_str()) {
        info!("backup folder: watching {folder}");
    }
    let manifests = match backup::locate(Path::new(&folder)) {
        Ok(manifests) => manifests,
        Err(error) => {
            // Said once per folder, not every thirty seconds.
            if announced.as_deref() != Some(folder.as_str()) {
                warn!("backup folder: {error:#}");
            }
            return Ok(Some(folder));
        }
    };
    let mut db = db.lock().map_err(|_| anyhow!("the library is unavailable"))?;
    let mut seen: BTreeMap<String, String> =
        db.meta_get("backup_seen")?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    let mut dirty = false;
    for manifest in manifests {
        let key = manifest.to_string_lossy().to_string();
        let Some(signature) = backup::signature(&manifest) else { continue };
        if seen.get(&key) == Some(&signature) {
            continue;
        }
        match backup::import_manifest(&mut db, &manifest, paths::now_unix()) {
            Ok(report) => {
                info!(
                    "backup folder: read {} notes, {} dictations, {} meetings: {} new, {} changed, {} already here",
                    report.notes,
                    report.dictations,
                    report.meetings,
                    report.applied.inserted,
                    report.applied.replaced,
                    report.applied.unchanged + report.applied.kept_newer
                );
                seen.insert(key, signature);
                dirty = true;
            }
            // A file caught halfway through a sync is tried again on the next pass.
            Err(error) => warn!("backup folder: could not read {}: {error:#}", manifest.display()),
        }
    }
    if dirty {
        db.meta_set("backup_seen", &serde_json::to_string(&seen).context("cannot record the backup state")?)?;
    }
    Ok(Some(folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn backup_folder_is_imported_once_per_change() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("Zone library.zonebackup");
        std::fs::create_dir_all(&package).unwrap();
        let manifest = package.join("library.json");
        let write = |notes: Vec<serde_json::Value>| {
            let body = json!({ "formatVersion": 1, "createdAt": 816317953.0, "notes": notes, "dictations": [], "meetings": [] });
            std::fs::write(&manifest, serde_json::to_vec(&body).unwrap()).unwrap();
        };
        write(vec![json!({ "id": "N1", "text": "first", "createdAt": 816200000.0 })]);

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        // No folder configured: nothing happens.
        assert_eq!(backup_pass(&db, None).unwrap(), None);
        db.lock().unwrap().meta_set("backup_folder", &dir.path().to_string_lossy()).unwrap();

        let folder = backup_pass(&db, None).unwrap();
        assert_eq!(db.lock().unwrap().total_items().unwrap(), 1);
        let first_sync = db.lock().unwrap().meta_f64("last_sync_backup").unwrap().unwrap();

        // Unchanged file: not read again.
        std::thread::sleep(Duration::from_millis(20));
        backup_pass(&db, folder.clone()).unwrap();
        assert_eq!(db.lock().unwrap().meta_f64("last_sync_backup").unwrap().unwrap(), first_sync);

        // Changed file: read again, new item appears.
        write(vec![
            json!({ "id": "N1", "text": "first", "createdAt": 816200000.0 }),
            json!({ "id": "N2", "text": "second, longer", "createdAt": 816200500.0 }),
        ]);
        backup_pass(&db, folder.clone()).unwrap();
        assert_eq!(db.lock().unwrap().total_items().unwrap(), 2);
        assert!(db.lock().unwrap().meta_f64("last_sync_backup").unwrap().unwrap() > first_sync);

        // A folder that went away is reported, not fatal.
        db.lock().unwrap().meta_set("backup_folder", "/nonexistent/zone-backups").unwrap();
        assert!(backup_pass(&db, folder).is_ok());
    }
}
