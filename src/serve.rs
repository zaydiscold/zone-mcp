//! `zone serve`: the daemon. It accepts paired phones over iroh, watches the backup folder,
//! and serves the local web page.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use iroh::{Endpoint, Watcher};
use serde_json::json;

use crate::daemon_info::{self, DaemonInfo};
use crate::db::Db;
use crate::link::{self, LinkState};
use crate::watch::Watcher as BackupWatcher;
use crate::web::{self, WebState};
use crate::{events, info, paths, warn};

/// How often the daemon looks at which backup folder is set. Reading the folder itself
/// happens every half minute (see watch.rs).
const SETTING_POLL: Duration = Duration::from_secs(5);
/// How often the daemon refreshes what it says about itself in `daemon.json`.
const INFO_POLL: Duration = Duration::from_secs(5);

pub struct Options {
    /// Port for the local web page. Falls back to a free port when taken.
    pub port: Option<u16>,
    /// Do not echo the log to stderr (the log file is still written).
    pub quiet: bool,
}

/// Why `zone serve` stopped, which decides its exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Ctrl-C in a terminal: the person stopped it on purpose.
    Interrupt,
    /// SIGTERM or SIGHUP. The service manager sends these when it stops zone itself
    /// (`launchctl bootout`, `kickstart -k`, logging out, `systemctl stop`), and then the exit
    /// status does not matter. Anything else that sends one (a stray `pkill -f "zone serve"`
    /// meant for a test copy) is not the person asking zone to stop, so zone exits the way a
    /// process ended by that signal reports it: 128 plus the signal. The login item's
    /// "restart unless it exited successfully" then brings zone back ten seconds later.
    Signal(i32),
}

impl Stop {
    pub fn exit_code(self) -> std::process::ExitCode {
        match self {
            Stop::Interrupt => std::process::ExitCode::SUCCESS,
            Stop::Signal(number) => std::process::ExitCode::from((128 + number).clamp(1, 255) as u8),
        }
    }
}

pub async fn run(options: Options) -> Result<std::process::ExitCode> {
    let dir = paths::ensure_data_dir()?;
    let Some(_lock) = daemon_info::acquire(&dir)? else {
        bail!("zone is already running for {} (see `zone status`).", dir.display());
    };
    crate::log::init(&paths::log_file(&dir), !options.quiet);

    let db = Db::open_default()?;
    let database_id = db.database_id()?;
    // Pairing codes that ran out while zone was not running.
    db.delete_expired_offers(paths::now_unix())?;
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
        backup: Default::default(),
    }));
    info.lock().map_err(|_| anyhow!("daemon state is unavailable"))?.write(&dir)?;
    let started = std::time::Instant::now();
    if let Ok(db) = db.lock() {
        events::record(&db, "daemon_started", json!({ "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id(), "port": port }));
    }

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
        flash: crate::flash::worker::FlashWorker::start(db.clone()),
    });
    let web_task = tokio::spawn(web::serve(listener, web_state));
    let link_task = tokio::spawn(link::accept_loop(endpoint.clone(), LinkState::new(db.clone(), computer_name)));
    let info_task = tokio::spawn(keep_info_fresh(endpoint.clone(), info.clone(), dir.clone(), db.clone()));
    let backup_task = tokio::spawn(watch_backup_folder(db.clone(), info.clone(), dir.clone()));
    // When `zone install` asked for it: register with AI clients installed since. This only
    // adds zone where it never was; see clients.rs.
    tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || {
            for line in crate::clients::register_on_start(&dir) {
                info!("mcp: {line}");
            }
        }
    });

    let stop = wait_for_stop().await;
    match stop {
        Stop::Interrupt => info!("zone is stopping"),
        Stop::Signal(number) => info!("zone is stopping (signal {number})"),
    }
    for task in [web_task, link_task, info_task, backup_task] {
        task.abort();
    }
    // Lets open sessions finish their last frame and tells peers the endpoint is gone.
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.close()).await;
    if let Ok(db) = db.lock() {
        events::record(&db, "daemon_stopped", json!({ "uptime_s": started.elapsed().as_secs_f64().round() }));
    }
    let _ = std::fs::remove_file(paths::daemon_info(&dir));
    Ok(stop.exit_code())
}

fn env_port() -> Option<u16> {
    std::env::var("ZONE_PORT").ok()?.trim().parse().ok()
}

async fn wait_for_stop() -> Stop {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut hangup = signal(SignalKind::hangup()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => Stop::Interrupt,
            _ = async { match terminate.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => Stop::Signal(libc::SIGTERM),
            _ = async { match hangup.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => Stop::Signal(libc::SIGHUP),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        Stop::Interrupt
    }
}

/// Keeps `daemon.json` describing how this computer can be reached right now.
async fn keep_info_fresh(endpoint: Endpoint, info: Arc<Mutex<DaemonInfo>>, dir: PathBuf, db: Arc<Mutex<Db>>) {
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
            // In the feed too. Off the async threads: the library may be busy with an import.
            let (db, url) = (db.clone(), relay_url.clone());
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(db) = db.lock() {
                    match relay_connected {
                        true => events::record(&db, "relay_connected", json!({ "relay_url": url })),
                        false => events::record(&db, "relay_lost", json!({})),
                    }
                }
            })
            .await;
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

/// Re-imports the backup folder whenever a `library.json` in it changes. See watch.rs for
/// why none of this ever waits for the folder.
async fn watch_backup_folder(db: Arc<Mutex<Db>>, info: Arc<Mutex<DaemonInfo>>, dir: PathBuf) {
    let watcher = Arc::new(Mutex::new(BackupWatcher::new(db, info, dir)));
    // Never slower than the folder poll itself, when that was shortened.
    let setting_poll = SETTING_POLL.min(Duration::from_secs_f64(watcher.lock().map(|w| w.poll_seconds()).unwrap_or(30.0)));
    loop {
        let stepped = tokio::task::spawn_blocking({
            let watcher = watcher.clone();
            move || {
                let Ok(mut watcher) = watcher.lock() else { return false };
                if let Err(error) = watcher.tick(paths::now_unix()) {
                    warn!("backup folder: {error:#}");
                }
                watcher.reading()
            }
        })
        .await;
        // While a read is in flight, look for its answer every second. Otherwise the setting
        // is checked every few seconds, and the folder itself every half minute.
        let reading = stepped.unwrap_or(false);
        tokio::time::sleep(if reading { Duration::from_secs(1) } else { setting_poll }).await;
    }
}
