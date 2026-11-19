//! How other `zone` commands know whether the daemon is running and where it listens.
//!
//! The daemon holds an exclusive lock on `daemon.lock` for its whole life. The operating
//! system drops the lock when the process ends, however it ends, so a stale file can never
//! make a dead daemon look alive. What the daemon knows about itself is in `daemon.json`.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub started_at: f64,
    pub updated_at: f64,
    /// This computer's iroh endpoint id, 64 lowercase hex characters.
    pub endpoint_id: String,
    /// Port of the local web page on 127.0.0.1.
    pub http_port: u16,
    /// The relay this endpoint is reachable through, once known.
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub relay_connected: bool,
    /// Local socket addresses the endpoint is bound to.
    #[serde(default)]
    pub bound_sockets: Vec<String>,
    /// Addresses the endpoint believes it can be reached at directly.
    #[serde(default)]
    pub direct_addrs: Vec<String>,
    /// How the watch on the backup folder is going.
    #[serde(default)]
    pub backup: BackupWatch,
    /// This zone ends when asked through `stop.request` (see background.rs). Zones from before
    /// that existed leave it out, and are ended by their process id instead.
    #[serde(default)]
    pub stops_on_request: bool,
}

/// The state of the daemon's watch on the backup folder. It lives here, not in the library,
/// so that it vanishes with the daemon: a read that "began" in a daemon that has since died
/// must not look like one that is still waiting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupWatch {
    pub folder: Option<String>,
    /// When the read now in flight began (Unix seconds). None when no read is in flight. A
    /// read that stays in flight is waiting for something: on macOS, usually for the person
    /// to answer a permission prompt.
    #[serde(default)]
    pub read_started_at: Option<f64>,
    /// When the last read that came back did so.
    #[serde(default)]
    pub read_finished_at: Option<f64>,
    /// What the last read that came back said, when it failed.
    #[serde(default)]
    pub error: Option<String>,
}

impl BackupWatch {
    /// Seconds the read in flight has been waiting, once that is long enough to mention.
    pub fn waiting_for(&self, now: f64) -> Option<f64> {
        let waited = now - self.read_started_at?;
        (waited > crate::watch::WAITING_AFTER_SECONDS).then_some(waited)
    }
}

impl DaemonInfo {
    pub fn read(dir: &Path) -> Option<DaemonInfo> {
        let bytes = std::fs::read(paths::daemon_info(dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The daemon's description of itself, only when a daemon is actually running.
    pub fn read_if_running(dir: &Path) -> Option<DaemonInfo> {
        if is_running(dir) { DaemonInfo::read(dir) } else { None }
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        paths::write_private(&paths::daemon_info(dir), &serde_json::to_vec_pretty(self)?)
    }
}

/// The daemon's hold on the data directory. Dropping it releases the lock.
pub struct DaemonLock {
    _file: File,
}

/// Takes the daemon lock. Returns none when another daemon already holds it.
///
/// [`is_running`] holds the lock for an instant while it looks, and `zone status` is asked
/// every few seconds by Zone for Mac. A daemon starting in that instant must not conclude that
/// another daemon is running, so a refusal is asked about a few more times before it counts.
pub fn acquire(dir: &Path) -> Result<Option<DaemonLock>> {
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        if let Some(lock) = acquire_once(dir)? {
            return Ok(Some(lock));
        }
    }
    Ok(None)
}

fn acquire_once(dir: &Path) -> Result<Option<DaemonLock>> {
    acquire_file(&paths::daemon_lock(dir))
}

/// Takes the lock held in this file, waiting up to `patience` for whoever holds it to let go.
pub fn acquire_waiting(path: &Path, patience: std::time::Duration) -> Result<Option<DaemonLock>> {
    let deadline = std::time::Instant::now() + patience;
    loop {
        if let Some(lock) = acquire_file(path)? {
            return Ok(Some(lock));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn acquire_file(path: &Path) -> Result<Option<DaemonLock>> {
    let path = path.to_path_buf();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(DaemonLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

/// True when a daemon holds the lock right now.
pub fn is_running(dir: &Path) -> bool {
    is_locked(&paths::daemon_lock(dir))
}

/// True when some process holds the lock in this file right now.
pub fn is_locked(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_look_at_the_lock_does_not_turn_a_starting_daemon_away() {
        let dir = tempfile::tempdir().unwrap();
        drop(acquire(dir.path()).unwrap());
        let path = dir.path().to_path_buf();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let looking = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    is_running(&path);
                }
            })
        };
        for _ in 0..300 {
            let lock = acquire(dir.path()).unwrap();
            assert!(lock.is_some(), "nobody else is running");
            drop(lock);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        looking.join().unwrap();
    }

    #[test]
    fn lock_shows_a_running_daemon() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_running(dir.path()));
        let lock = acquire(dir.path()).unwrap().expect("first daemon gets the lock");
        assert!(is_running(dir.path()));
        assert!(acquire(dir.path()).unwrap().is_none(), "a second daemon is refused");
        let info = DaemonInfo { pid: 7, endpoint_id: "ab".repeat(32), http_port: 47813, ..Default::default() };
        info.write(dir.path()).unwrap();
        assert_eq!(DaemonInfo::read_if_running(dir.path()).unwrap().http_port, 47813);
        drop(lock);
        // Another test starting a program at this moment holds a copy of the lock's file
        // until that program is running (a fork copies every open file), so the lock can
        // outlive the drop by an instant.
        for _ in 0..200 {
            if !is_running(dir.path()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!is_running(dir.path()));
        assert!(DaemonInfo::read_if_running(dir.path()).is_none());
        assert!(acquire(dir.path()).unwrap().is_some());
    }
}
