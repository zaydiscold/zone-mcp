//! Keeping zone running without a service manager, and stopping it on purpose.
//!
//! macOS and Linux keep zone running through launchd and systemd (service.rs). Windows has no
//! per-user service manager that works without admin rights, so there zone looks after itself:
//!
//! - `zone serve --background` starts a keeper with no window and returns. The keeper starts
//!   `zone serve --quiet`, also without a window, and starts it again after a pause whenever it
//!   ends without being asked to (Task Manager's End task, a crash). The keeper ends when zone
//!   is stopped on purpose (`zone stop`, `zone uninstall`), and when an update replaced the
//!   program: then it starts the keeper of the new version, which takes over.
//! - A stop on purpose is the file `stop.request`, holding the time it was asked. A zone or a
//!   keeper started before that time ends, one started after it carries on. So nothing has to
//!   remember to delete the file, and a stop asked while zone is starting is not lost.
//! - One keeper per data folder (`supervisor.lock`), as there is one daemon (`daemon.lock`).
//!
//! Every program zone starts in the background is started without a console window: on
//! Windows a console program started by one that has no console would otherwise open a new,
//! visible window of its own. See [`hide`].

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::daemon_info::{self, DaemonInfo};
use crate::{info, paths, warn};

/// Set for a zone that was started without a window (by `zonew.exe`, or by `zone serve
/// --background` starting its keeper), so it does not start itself once more.
pub const ENV_HIDDEN: &str = "ZONE_HIDDEN";
/// Set by the keeper for the zone it starts: the time the keeper started, so a stop asked
/// after that time and before the zone itself started still stops it.
pub const ENV_SUPERVISED: &str = "ZONE_SUPERVISED";
/// Shortens the keeper's first pause before starting zone again, for tests.
const ENV_RESTART_SECONDS: &str = "ZONE_RESTART_SECONDS";
/// What `zone serve` exits with when an update replaced its program.
pub const REPLACED_EXIT: i32 = 75;

/// The first pause before zone is started again. It doubles while zone keeps ending within a
/// minute of starting, up to five minutes.
const RESTART_PAUSE: Duration = Duration::from_secs(10);
const RESTART_PAUSE_MAX: Duration = Duration::from_secs(300);
/// A zone that ran this long before ending did not fail at starting.
const RAN_WELL: Duration = Duration::from_secs(60);
/// How long a zone asked to stop has to do so before it is ended.
const STOP_PATIENCE: Duration = Duration::from_secs(10);
/// How long a new keeper waits for the one it replaces to let go.
const HANDOVER_PATIENCE: Duration = Duration::from_secs(20);

#[cfg(windows)]
mod flags {
    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
}

/// Runs the command without a console window. On Windows a console program gets a console of
/// its own that is never shown (its output still goes where it is sent). Elsewhere nothing
/// changes.
pub fn hide(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(flags::CREATE_NO_WINDOW);
    }
    command
}

/// Starts a program that should outlive this one: no window, no standard input or output,
/// and not ended with the terminal (or the SSH session) this one runs in, where the system
/// allows that.
pub fn spawn_detached(command: &mut Command) -> std::io::Result<Child> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let base = flags::CREATE_NO_WINDOW | flags::CREATE_NEW_PROCESS_GROUP;
        crate::win::without_handing_down_stdio(|| {
            // A job that does not allow leaving refuses the whole start, so then without.
            command.creation_flags(base | flags::CREATE_BREAKAWAY_FROM_JOB);
            if let Ok(child) = command.spawn() {
                return Ok(child);
            }
            command.creation_flags(base);
            command.spawn()
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A group of its own: Ctrl-C in the terminal it was started from does not reach it.
        command.process_group(0);
        command.spawn()
    }
}

// -------------------------------------------------------------------- stopping on purpose

pub fn stop_request_path(dir: &Path) -> PathBuf {
    dir.join("stop.request")
}

fn keeper_lock_path(dir: &Path) -> PathBuf {
    dir.join("supervisor.lock")
}

fn keeper_info_path(dir: &Path) -> PathBuf {
    dir.join("supervisor.json")
}

/// Asks every zone and keeper of this data folder that started before now to stop.
pub fn request_stop(dir: &Path) -> Result<()> {
    let path = stop_request_path(dir);
    std::fs::write(&path, format!("{}\n", paths::now_unix())).with_context(|| format!("cannot write {}", path.display()))
}

/// When stopping was last asked, if ever.
fn stop_asked_at(dir: &Path) -> Option<f64> {
    std::fs::read_to_string(stop_request_path(dir)).ok()?.trim().parse().ok()
}

/// True when a stop was asked after `since`: the time the asking zone (or its keeper) started.
pub fn stop_requested_since(dir: &Path, since: f64) -> bool {
    stop_asked_at(dir).is_some_and(|asked| asked >= since)
}

/// Since when a stop request applies to the zone that is starting now: its keeper's start, when
/// a keeper started it, else now.
pub fn stop_horizon() -> f64 {
    let now = paths::now_unix();
    std::env::var(ENV_SUPERVISED).ok().and_then(|value| value.trim().parse::<f64>().ok()).filter(|t| *t <= now).unwrap_or(now)
}

// -------------------------------------------------------------------- the keeper

/// What a running keeper says about itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeeperInfo {
    pub pid: u32,
    pub version: String,
    pub started_at: f64,
}

impl KeeperInfo {
    /// The running keeper of this data folder, if there is one.
    pub fn read_if_running(dir: &Path) -> Option<KeeperInfo> {
        if !daemon_info::is_locked(&keeper_lock_path(dir)) {
            return None;
        }
        let bytes = std::fs::read(keeper_info_path(dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

pub fn keeper_running(dir: &Path) -> bool {
    daemon_info::is_locked(&keeper_lock_path(dir))
}

/// The program, and the arguments a keeper is started with.
fn keeper_command(program: &Path, port: Option<u16>) -> Command {
    let mut command = Command::new(program);
    command.args(["serve", "--background"]);
    if let Some(port) = port {
        command.arg("--port").arg(port.to_string());
    }
    command.env(ENV_HIDDEN, "1");
    command
}

/// Starts a keeper for this data folder (the `ZONE_HOME` of this process goes with it) and
/// returns at once.
pub fn start_keeper(port: Option<u16>) -> Result<()> {
    let program = paths::current_exe()?;
    spawn_detached(&mut keeper_command(&program, port)).with_context(|| format!("cannot start {}", program.display()))?;
    Ok(())
}

/// Waits until zone is running, up to `patience`. True when it is.
pub fn wait_until_running(dir: &Path, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    loop {
        if DaemonInfo::read_if_running(dir).is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `zone serve --background`. Started with a window (from a terminal, or a login entry that
/// names `zone.exe` itself), it starts the keeper without one and ends. Started without one, it
/// is the keeper.
pub fn serve_background(port: Option<u16>) -> Result<ExitCode> {
    if std::env::var(ENV_HIDDEN).is_ok_and(|value| value == "1") {
        return keep(port);
    }
    let dir = paths::ensure_data_dir()?;
    if let Some(keeper) = KeeperInfo::read_if_running(&dir) {
        println!("zone is already running in the background (pid {}).", keeper.pid);
        return Ok(ExitCode::SUCCESS);
    }
    start_keeper(port)?;
    if wait_until_running(&dir, Duration::from_secs(20)) {
        println!("zone is running in the background. `zone status` shows it, `zone stop` stops it.");
    } else {
        println!("zone is starting in the background. If `zone status` does not show it soon, the log says why: {}", paths::log_file(&dir).display());
    }
    Ok(ExitCode::SUCCESS)
}

/// The keeper itself.
fn keep(port: Option<u16>) -> Result<ExitCode> {
    // Asked once, now: an update may move this very file aside while the keeper runs, and the
    // path is where the new version will be.
    let program = paths::current_exe()?;
    let dir = paths::ensure_data_dir()?;
    crate::log::init(&paths::log_file(&dir), false);
    let Some(lock) = daemon_info::acquire_waiting(&keeper_lock_path(&dir), HANDOVER_PATIENCE)? else {
        info!("keeper: another keeper is already looking after zone for {}", dir.display());
        return Ok(ExitCode::SUCCESS);
    };
    let started_at = paths::now_unix();
    let me = KeeperInfo { pid: std::process::id(), version: env!("CARGO_PKG_VERSION").to_string(), started_at };
    let _ = paths::write_private(&keeper_info_path(&dir), &serde_json::to_vec_pretty(&me)?);
    // What an update left beside the program. A `zone update` that is still finishing holds its
    // own old file a little longer, so this is done again a minute later.
    let tidy = |program: &Path| {
        let removed = remove_old_programs(program);
        if removed > 0 {
            info!("keeper: removed {removed} program file(s) an update left beside {}", program.display());
        }
    };
    tidy(&program);
    std::thread::spawn({
        let program = program.clone();
        move || {
            std::thread::sleep(Duration::from_secs(60));
            tidy(&program);
        }
    });
    info!("keeper: zone {} started (pid {}), keeping zone running for {}", me.version, me.pid, dir.display());

    let first_pause = std::env::var(ENV_RESTART_SECONDS).ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| *s > 0.0).map(Duration::from_secs_f64).unwrap_or(RESTART_PAUSE);
    let mut quick_endings = 0u32;
    let mut waiting_for_other = false;
    loop {
        if stop_requested_since(&dir, started_at) {
            info!("keeper: zone was stopped on purpose, so the keeper ends too");
            break;
        }
        // A zone this keeper did not start (one in a terminal) has the library: wait for it.
        if daemon_info::is_running(&dir) {
            if !waiting_for_other {
                info!("keeper: another zone is running for this folder; starting none until it ends");
                waiting_for_other = true;
            }
            pause(&dir, started_at, Duration::from_secs(5));
            continue;
        }
        waiting_for_other = false;

        let mut command = Command::new(&program);
        command.args(["serve", "--quiet"]);
        if let Some(port) = port {
            command.arg("--port").arg(port.to_string());
        }
        command.env(ENV_SUPERVISED, started_at.to_string()).env(ENV_HIDDEN, "1").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        hide(&mut command);
        let begun = Instant::now();
        let status = match command.spawn() {
            Ok(mut child) => wait_for(&mut child, &dir, started_at),
            Err(error) => {
                warn!("keeper: cannot start {}: {error}", program.display());
                None
            }
        };
        let code = status.and_then(|status| status.code());
        if stop_requested_since(&dir, started_at) {
            info!("keeper: zone was stopped on purpose, so the keeper ends too");
            break;
        }
        match code {
            Some(0) => {
                info!("keeper: zone ended by itself, cleanly, so it is left stopped");
                break;
            }
            Some(REPLACED_EXIT) => {
                // The program at this path is now another version: its keeper takes over.
                info!("keeper: an update replaced zone, starting the keeper of the new version");
                if let Err(error) = spawn_detached(&mut keeper_command(&program, port)) {
                    warn!("keeper: cannot start the new version ({error}); starting this path again instead");
                    continue;
                }
                break;
            }
            _ => {}
        }
        if begun.elapsed() >= RAN_WELL {
            quick_endings = 0;
        }
        quick_endings += 1;
        let wait = restart_pause(first_pause, quick_endings);
        let how = match code {
            Some(code) => format!("exit code {code}"),
            None => "no exit code".to_string(),
        };
        warn!("keeper: zone stopped without being asked to ({how}, after {:.0} s); starting it again in {} s", begun.elapsed().as_secs_f64(), wait.as_secs());
        if pause(&dir, started_at, wait) {
            info!("keeper: zone was stopped on purpose, so the keeper ends too");
            break;
        }
    }
    let _ = std::fs::remove_file(keeper_info_path(&dir));
    drop(lock);
    Ok(ExitCode::SUCCESS)
}

/// The pause before the `n`th quick start in a row: the first pause, doubled each time.
fn restart_pause(first: Duration, n: u32) -> Duration {
    let factor = 1u32 << n.saturating_sub(1).min(8);
    (first * factor).min(RESTART_PAUSE_MAX.max(first))
}

/// Sleeps, unless a stop is asked meanwhile. True when it was.
fn pause(dir: &Path, since: f64, how_long: Duration) -> bool {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        if stop_requested_since(dir, since) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    }
    stop_requested_since(dir, since)
}

/// Waits for the zone the keeper started. When a stop is asked, zone has a while to stop by
/// itself, then it is ended.
fn wait_for(child: &mut Child, dir: &Path, since: f64) -> Option<ExitStatus> {
    let mut asked: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return child.wait().ok(),
        }
        if asked.is_none() && stop_requested_since(dir, since) {
            asked = Some(Instant::now());
        }
        if asked.is_some_and(|at| at.elapsed() > STOP_PATIENCE) {
            warn!("keeper: zone did not stop within {} s of being asked, ending it", STOP_PATIENCE.as_secs());
            let _ = child.kill();
            return child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// What an earlier update could not delete because it was running: `zone.exe.old-<n>` (and
/// `zonew.exe.old-<n>`) beside the program. Returns how many were removed.
pub fn remove_old_programs(program: &Path) -> usize {
    let Some(folder) = program.parent() else { return 0 };
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let old = ["zone.exe.old-", "zonew.exe.old-"].iter().any(|prefix| name.starts_with(prefix) && name[prefix.len()..].chars().all(|c| c.is_ascii_digit()) && name.len() > prefix.len());
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

// -------------------------------------------------------------------- `zone stop`

/// What stopping came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Nothing was running.
    NotRunning,
    /// It stopped when asked.
    Asked,
    /// It had to be ended (an older zone that does not know about stop requests, or one that
    /// did not answer).
    Ended,
    /// It is still running.
    StillRunning,
}

/// Ends a process by its id.
pub fn end_process(pid: u32) {
    #[cfg(windows)]
    {
        let _ = hide(Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"])).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(pid) {
            // SAFETY: kill only sends a signal; the id comes from a zone that holds its lock.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
}

fn anything_running(dir: &Path) -> bool {
    daemon_info::is_running(dir) || keeper_running(dir)
}

fn wait_until_stopped(dir: &Path, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while anything_running(dir) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Stops zone and its keeper for this data folder, on purpose. A zone too old to know about
/// stop requests, or one that does not answer, is ended by its process id.
pub fn stop(dir: &Path) -> Result<Stopped> {
    let daemon = DaemonInfo::read_if_running(dir);
    let keeper = KeeperInfo::read_if_running(dir);
    if daemon.is_none() && keeper.is_none() && !anything_running(dir) {
        return Ok(Stopped::NotRunning);
    }
    request_stop(dir)?;
    // A zone from before stop requests existed would wait in vain: it is ended at once.
    let listens = daemon.as_ref().is_none_or(|d| d.stops_on_request);
    if listens && wait_until_stopped(dir, STOP_PATIENCE + Duration::from_secs(5)) {
        return Ok(Stopped::Asked);
    }
    // The keeper first, so it does not start zone again.
    if let Some(keeper) = KeeperInfo::read_if_running(dir) {
        end_process(keeper.pid);
    }
    if let Some(daemon) = DaemonInfo::read_if_running(dir) {
        end_process(daemon.pid);
    }
    Ok(if wait_until_stopped(dir, Duration::from_secs(5)) { Stopped::Ended } else { Stopped::StillRunning })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_request_reaches_only_what_started_before_it() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!stop_requested_since(dir.path(), 0.0));
        let before = paths::now_unix() - 1.0;
        request_stop(dir.path()).unwrap();
        let after = paths::now_unix() + 1.0;
        assert!(stop_requested_since(dir.path(), before));
        assert!(!stop_requested_since(dir.path(), after));
        // Something that is not a time asks nothing.
        std::fs::write(stop_request_path(dir.path()), "soon").unwrap();
        assert!(!stop_requested_since(dir.path(), 0.0));
    }

    #[test]
    fn the_pause_doubles_and_has_a_ceiling() {
        let first = Duration::from_secs(10);
        let pauses: Vec<u64> = (1..=8).map(|n| restart_pause(first, n).as_secs()).collect();
        assert_eq!(pauses, [10, 20, 40, 80, 160, 300, 300, 300]);
        // A test's short first pause still doubles.
        assert_eq!(restart_pause(Duration::from_millis(200), 3), Duration::from_millis(800));
    }

    #[test]
    fn files_left_by_an_update_are_removed_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["zone.exe", "zonew.exe", "zone.exe.old-638123", "zonew.exe.old-1", "zone.exe.old-", "zone.exe.old-12x", "notes.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        assert_eq!(remove_old_programs(&dir.path().join("zone.exe")), 2);
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "zone.exe", "zone.exe.old-", "zone.exe.old-12x", "zonew.exe"]);
    }
}
