//! `zone serve`: the daemon. It accepts paired phones over iroh, watches the backup folder,
//! and serves the local web page.

use std::path::{Path, PathBuf};
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
/// How often a running zone looks at its own program file, to see whether an update replaced
/// it. `ZONE_SELF_CHECK_SECONDS` shortens it for tests.
#[cfg_attr(not(unix), allow(dead_code))]
const SELF_POLL: Duration = Duration::from_secs(10);

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
    // Only Unix delivers these; on Windows a console close ends the process without telling it.
    #[cfg_attr(not(unix), allow(dead_code))]
    Signal(i32),
    /// The program file was replaced by another version of zone (`zone update`, the install
    /// script, a package manager). The zone that is running ends and starts the one that is
    /// now on disk, in its place: same process, same arguments. Without this an update that
    /// nothing restarts (a `zone serve` in a terminal, a zone started by Zone for Mac) would
    /// leave the old version running until the next login.
    // Only on Unix can a running program be replaced by another in the same process.
    #[cfg_attr(not(unix), allow(dead_code))]
    Replaced,
}

impl Stop {
    pub fn exit_code(self) -> std::process::ExitCode {
        match self {
            Stop::Interrupt => std::process::ExitCode::SUCCESS,
            Stop::Signal(number) => std::process::ExitCode::from((128 + number).clamp(1, 255) as u8),
            // Only reached when the new program could not be started: not a success, so a
            // login item starts zone again.
            Stop::Replaced => std::process::ExitCode::from(75),
        }
    }
}

pub async fn run(options: Options) -> Result<std::process::ExitCode> {
    // Where this program is, as it was started: asked now, before anything can replace it.
    let program = std::env::current_exe().ok();
    let dir = paths::ensure_data_dir()?;
    let Some(lock) = daemon_info::acquire(&dir)? else {
        bail!("zone is already running for {} (see `zone status`).", dir.display());
    };
    let mut stops = StopSignals::listen();
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
    // Words removed on the page are kept thirty days, then erased, whether or not the page is open.
    let erase_db = db.clone();
    let erase_task = tokio::spawn(async move {
        loop {
            if let Ok(db) = erase_db.lock() {
                let _ = db.erase_old_removed(paths::now_unix());
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
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

    let stop = tokio::select! {
        stop = stops.wait() => stop,
        _ = program_replaced(program.clone()) => Stop::Replaced,
    };
    match stop {
        Stop::Interrupt => info!("zone is stopping"),
        Stop::Signal(number) => info!("zone is stopping (signal {number})"),
        Stop::Replaced => info!("zone is stopping, to start the version that replaced it"),
    }
    for task in [web_task, link_task, info_task, backup_task, erase_task] {
        task.abort();
        // Until the task is gone its port is still held, and the next zone needs it.
        let _ = task.await;
    }
    // Lets open sessions finish their last frame and tells peers the endpoint is gone.
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.close()).await;
    if let Ok(db) = db.lock() {
        events::record(&db, "daemon_stopped", json!({ "uptime_s": started.elapsed().as_secs_f64().round() }));
    }
    let _ = std::fs::remove_file(paths::daemon_info(&dir));
    drop(lock);
    #[cfg(unix)]
    if stop == Stop::Replaced
        && let Some(program) = program
    {
        use std::os::unix::process::CommandExt;
        // Returns only when the new program could not be started.
        let error = std::process::Command::new(&program).args(std::env::args_os().skip(1)).exec();
        warn!("could not start {}: {error}", program.display());
    }
    Ok(stop.exit_code())
}

/// What `program --version` says its version is.
#[cfg_attr(not(unix), allow(dead_code))]
fn version_of(program: &Path) -> Option<String> {
    let output = std::process::Command::new(program).arg("--version").stdin(std::process::Stdio::null()).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim().strip_prefix("zone ")?.trim().to_string()).filter(|version| !version.is_empty())
}

/// Returns once the program file has been replaced by another version of zone that runs.
/// The same version put there again (a reinstall, a rebuild) changes nothing.
#[cfg(unix)]
async fn program_replaced(program: Option<PathBuf>) {
    use std::os::unix::fs::MetadataExt;
    let Some(program) = program else { return std::future::pending().await };
    let poll = std::env::var("ZONE_SELF_CHECK_SECONDS").ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| *s > 0.0).map(Duration::from_secs_f64).unwrap_or(SELF_POLL);
    let look = |path: &Path| std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino(), m.size(), m.mtime(), m.mtime_nsec()));
    let Some(mut seen) = look(&program) else { return std::future::pending().await };
    loop {
        tokio::time::sleep(poll).await;
        // Gone for a moment (an installer that removes before it moves) is not replaced yet.
        let Some(now) = look(&program) else { continue };
        if now == seen {
            continue;
        }
        // Look twice, so a file still being written is not started half made.
        tokio::time::sleep(poll.min(Duration::from_secs(2))).await;
        if look(&program) != Some(now) {
            continue;
        }
        seen = now;
        let asked = tokio::task::spawn_blocking({
            let program = program.clone();
            move || version_of(&program)
        })
        .await;
        match asked {
            Ok(Some(version)) if version != env!("CARGO_PKG_VERSION") => {
                info!("zone {version} replaced this zone {} at {}", env!("CARGO_PKG_VERSION"), program.display());
                return;
            }
            Ok(Some(_)) => {}
            _ => warn!("the program at {} changed and does not run as zone. This zone {} keeps running.", program.display(), env!("CARGO_PKG_VERSION")),
        }
    }
}

#[cfg(not(unix))]
async fn program_replaced(_program: Option<PathBuf>) {
    // Windows cannot replace a running program: the installer stops zone, swaps the file and
    // starts it again (install.ps1).
    std::future::pending().await
}

fn env_port() -> Option<u16> {
    std::env::var("ZONE_PORT").ok()?.trim().parse().ok()
}

/// The signals that stop zone, listened for from before zone says it is running: a signal
/// that arrives in zone's first moments is then a stop like any other (clean up, and the exit
/// status that says which signal), not the end of a program that was not listening yet.
struct StopSignals {
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    hangup: Option<tokio::signal::unix::Signal>,
}

impl StopSignals {
    fn listen() -> StopSignals {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            StopSignals { interrupt: signal(SignalKind::interrupt()).ok(), terminate: signal(SignalKind::terminate()).ok(), hangup: signal(SignalKind::hangup()).ok() }
        }
        #[cfg(not(unix))]
        StopSignals {}
    }

    async fn wait(&mut self) -> Stop {
        #[cfg(unix)]
        {
            async fn got(signal: &mut Option<tokio::signal::unix::Signal>) {
                match signal.as_mut() {
                    Some(signal) => {
                        signal.recv().await;
                    }
                    None => std::future::pending().await,
                }
            }
            tokio::select! {
                _ = got(&mut self.interrupt) => Stop::Interrupt,
                _ = got(&mut self.terminate) => Stop::Signal(libc::SIGTERM),
                _ = got(&mut self.hangup) => Stop::Signal(libc::SIGHUP),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            Stop::Interrupt
        }
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
