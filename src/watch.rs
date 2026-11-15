//! The daemon's watch on the backup folder.
//!
//! Reading the folder is the one thing the daemon does that can block without limit. Under
//! launchd, macOS holds the first `opendir` of a folder in iCloud Drive (or Desktop, or
//! Documents) until the person answers a permission prompt, and it says nothing while it
//! waits. So the reading happens on a thread of its own that owns no lock, and the rest of
//! the daemon only ever asks "has it answered yet?":
//!
//! - a read that is stuck never holds the library lock, so Link and the web page carry on;
//! - while one read is in flight no other is started, so blocked threads do not pile up
//!   every thirty seconds;
//! - when a read began is published in `daemon.json`, so `zone status` and `zone doctor` can
//!   say "waiting for macOS permission to read the backup folder" instead of nothing.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};

use crate::backup::{self, Scanned, Seen};
use crate::daemon_info::{BackupWatch, DaemonInfo};
use crate::db::Db;
use crate::{info, warn};

/// How often the folder is read again to look for a changed `library.json`, unless
/// `ZONE_BACKUP_POLL_SECONDS` says otherwise.
pub const POLL_SECONDS: f64 = 30.0;
/// A read in flight for longer than this is reported as waiting.
pub const WAITING_AFTER_SECONDS: f64 = 5.0;
/// Stuck reads that may be left behind because the folder setting changed under them. After
/// that the watcher waits for one of them rather than adding another blocked thread.
const MAX_ABANDONED: usize = 3;

/// The file system half of a pass. Replaced in tests by something that can be made to block.
pub type ScanFn = Arc<dyn Fn(&Path, &Seen) -> Result<Vec<Scanned>> + Send + Sync>;

struct InFlight {
    folder: String,
    started_at: f64,
    result: Receiver<Result<Vec<Scanned>>>,
    /// Whether the log has said that this read is taking long.
    nagged: bool,
}

pub struct Watcher {
    db: Arc<Mutex<Db>>,
    info: Arc<Mutex<DaemonInfo>>,
    /// The data directory, for `daemon.json`.
    dir: PathBuf,
    scan: ScanFn,
    in_flight: Option<InFlight>,
    abandoned: usize,
    /// The folder the log has announced.
    announced: Option<String>,
    /// The folder that has answered a read at least once.
    answered: Option<String>,
    /// When a read last began or came back. The next one is due a poll period after that.
    last_activity: Option<f64>,
    last_error: Option<String>,
    /// What `daemon.json` currently says: folder, whether a read is in flight, last error.
    persisted: (Option<String>, bool, Option<String>),
    said_full: bool,
    poll_seconds: f64,
}

impl Watcher {
    pub fn new(db: Arc<Mutex<Db>>, info: Arc<Mutex<DaemonInfo>>, dir: PathBuf) -> Watcher {
        Watcher::with_scan(db, info, dir, Arc::new(|folder: &Path, seen: &Seen| backup::scan(folder, seen)))
    }

    pub fn with_scan(db: Arc<Mutex<Db>>, info: Arc<Mutex<DaemonInfo>>, dir: PathBuf, scan: ScanFn) -> Watcher {
        Watcher {
            db,
            info,
            dir,
            scan,
            in_flight: None,
            abandoned: 0,
            announced: None,
            answered: None,
            last_activity: None,
            last_error: None,
            persisted: (None, false, None),
            said_full: false,
            poll_seconds: std::env::var("ZONE_BACKUP_POLL_SECONDS")
                .ok()
                .and_then(|text| text.trim().parse::<f64>().ok())
                .filter(|seconds| seconds.is_finite() && *seconds >= 1.0)
                .unwrap_or(POLL_SECONDS),
        }
    }

    pub fn poll_seconds(&self) -> f64 {
        self.poll_seconds
    }

    /// True while a read has not come back. The caller looks again sooner then.
    pub fn reading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Reads that were left behind, still blocked, because the folder setting changed.
    #[cfg(test)]
    pub fn abandoned(&self) -> usize {
        self.abandoned
    }

    /// One step. Never touches the backup folder itself and never waits for a read, so it
    /// always returns promptly. `now` is Unix seconds.
    pub fn tick(&mut self, now: f64) -> Result<()> {
        let (folder, seen) = {
            let db = self.db.lock().map_err(|_| anyhow!("the library is unavailable"))?;
            (db.meta_get("backup_folder")?, read_seen(&db)?)
        };

        if let Some(flight) = self.in_flight.as_mut() {
            match flight.result.try_recv() {
                Ok(result) => {
                    let flight = self.in_flight.take().expect("checked above");
                    if folder.as_deref() == Some(flight.folder.as_str()) {
                        self.finish(&flight.folder, result, now)?;
                    }
                    // Otherwise the person chose another folder (or none) while this one was
                    // being read. What it held is not imported.
                }
                Err(TryRecvError::Empty) => {
                    let waited = now - flight.started_at;
                    if folder.as_deref() == Some(flight.folder.as_str()) {
                        if !flight.nagged && waited >= POLL_SECONDS {
                            flight.nagged = true;
                            warn!(
                                "backup folder: still {} after {} seconds. {}",
                                backup::waiting_reason(),
                                waited.round(),
                                backup::waiting_fix().replace('\n', " ")
                            );
                        }
                        // Written once it is clear the read is not an ordinary quick one.
                        let view = flight_view(flight);
                        self.publish(Some(view), waited >= 1.0);
                        return Ok(());
                    }
                    // The setting changed under a read that is stuck. It cannot be cancelled.
                    if self.abandoned >= MAX_ABANDONED {
                        if !self.said_full {
                            self.said_full = true;
                            warn!(
                                "backup folder: {} reads of earlier folders are still blocked, so no new read is started \
                                 until one of them answers or zone is restarted",
                                self.abandoned + 1
                            );
                        }
                        return Ok(());
                    }
                    self.abandoned += 1;
                    warn!("backup folder: the read of {} never answered and is left behind", flight.folder);
                    self.in_flight = None;
                }
                Err(TryRecvError::Disconnected) => {
                    warn!("backup folder: the read of {} ended without an answer", flight.folder);
                    self.in_flight = None;
                }
            }
        }

        let Some(folder) = folder else {
            self.announced = None;
            self.last_error = None;
            self.publish(None, true);
            return Ok(());
        };
        let changed = self.announced.as_deref() != Some(folder.as_str());
        let due = self.last_activity.map(|t| now - t >= self.poll_seconds).unwrap_or(true);
        if !changed && !due {
            return Ok(());
        }
        self.start(folder, seen, now);
        Ok(())
    }

    fn start(&mut self, folder: String, seen: Seen, now: f64) {
        if self.announced.as_deref() != Some(folder.as_str()) {
            info!("backup folder: watching {folder}");
            self.announced = Some(folder.clone());
            self.last_error = None;
        }
        if self.answered.as_deref() != Some(folder.as_str()) {
            // Said before the read, because the read may not come back for a long time.
            if cfg!(target_os = "macos") {
                info!(
                    "backup folder: reading it now. macOS may ask for permission to access this folder \
                     (iCloud Drive, Desktop, Documents and network folders are protected). The read waits for the answer. \
                     Syncing and the web page do not wait for it."
                );
            } else {
                info!("backup folder: reading it now. Syncing and the web page do not wait for it.");
            }
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let scan = self.scan.clone();
        let path = PathBuf::from(&folder);
        // A plain thread, not the runtime's blocking pool: a read stuck in the kernel must
        // not keep the daemon from shutting down.
        let spawned = std::thread::Builder::new().name("backup-folder-read".to_string()).spawn(move || {
            let _ = sender.send(scan(&path, &seen));
        });
        if let Err(error) = spawned {
            warn!("backup folder: cannot start a thread to read it: {error}");
            return;
        }
        self.last_activity = Some(now);
        self.in_flight = Some(InFlight { folder, started_at: now, result: receiver, nagged: false });
        let view = self.in_flight.as_ref().map(flight_view);
        self.publish(view, false);
    }

    fn finish(&mut self, folder: &str, result: Result<Vec<Scanned>>, now: f64) -> Result<()> {
        // A read that took ten minutes to answer is not followed by another one at once.
        self.last_activity = Some(now);
        let scanned = match result {
            Ok(scanned) => scanned,
            Err(error) => {
                let text = format!("{error:#}");
                // Said once per problem, not every thirty seconds.
                if self.last_error.as_deref() != Some(text.as_str()) {
                    if backup::is_permission_denied(&error) {
                        warn!("backup folder: {text}. {}", backup::denied_fix().replace('\n', " "));
                    } else {
                        warn!("backup folder: {text}");
                    }
                }
                self.last_error = Some(text.clone());
                self.publish(Some(BackupWatch { folder: Some(folder.to_string()), read_started_at: None, read_finished_at: Some(now), error: Some(text) }), true);
                return Ok(());
            }
        };
        if self.answered.as_deref() != Some(folder) {
            info!("backup folder: it can be read");
            self.answered = Some(folder.to_string());
        }
        self.last_error = None;
        if !scanned.is_empty() {
            let mut db = self.db.lock().map_err(|_| anyhow!("the library is unavailable"))?;
            // Read again under the lock: `zone backup-folder` may have imported meanwhile.
            let mut seen = read_seen(&db)?;
            let mut dirty = false;
            for found in scanned {
                match found.parsed {
                    Ok(parsed) => {
                        let report = backup::apply_parsed(&mut db, &found.manifest, &parsed, now)?;
                        info!(
                            "backup folder: read {} notes, {} dictations, {} meetings: {} new, {} changed, {} already here",
                            report.notes,
                            report.dictations,
                            report.meetings,
                            report.applied.inserted,
                            report.applied.replaced,
                            report.applied.unchanged + report.applied.kept_newer
                        );
                        seen.insert(found.key, found.signature);
                        dirty = true;
                    }
                    // A file caught halfway through a sync is tried again on the next pass.
                    Err(error) => warn!("backup folder: could not read {}: {error:#}", found.manifest.display()),
                }
            }
            if dirty {
                db.meta_set("backup_seen", &serde_json::to_string(&seen).context("cannot record the backup state")?)?;
            }
        }
        self.publish(Some(BackupWatch { folder: Some(folder.to_string()), read_started_at: None, read_finished_at: Some(now), error: None }), true);
        Ok(())
    }

    /// Updates what the daemon says about the folder. `daemon.json` is rewritten when the
    /// part of it a person would notice changed: the folder, waiting or not, the error.
    fn publish(&mut self, view: Option<BackupWatch>, write_if_changed: bool) {
        let view = view.unwrap_or_default();
        let key = (view.folder.clone(), view.read_started_at.is_some(), view.error.clone());
        let Ok(mut info) = self.info.lock() else { return };
        // A read that just began keeps the time the previous one finished.
        let finished = view.read_finished_at.or(info.backup.read_finished_at.filter(|_| info.backup.folder == view.folder));
        info.backup = BackupWatch { read_finished_at: finished, ..view };
        if write_if_changed && key != self.persisted {
            info.updated_at = crate::paths::now_unix();
            if info.write(&self.dir).is_ok() {
                self.persisted = key;
            }
        }
    }
}

fn flight_view(flight: &InFlight) -> BackupWatch {
    BackupWatch { folder: Some(flight.folder.clone()), read_started_at: Some(flight.started_at), read_finished_at: None, error: None }
}

/// Which backups were imported already, as recorded in the library.
pub fn read_seen(db: &Db) -> Result<Seen> {
    Ok(db.meta_get("backup_seen")?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Sender, channel};
    use std::time::Duration;

    struct Rig {
        dir: tempfile::TempDir,
        db: Arc<Mutex<Db>>,
        info: Arc<Mutex<DaemonInfo>>,
    }

    impl Rig {
        fn new() -> Rig {
            Rig {
                dir: tempfile::tempdir().unwrap(),
                db: Arc::new(Mutex::new(Db::open_in_memory().unwrap())),
                info: Arc::new(Mutex::new(DaemonInfo::default())),
            }
        }

        fn watcher(&self) -> Watcher {
            Watcher::new(self.db.clone(), self.info.clone(), self.dir.path().to_path_buf())
        }

        fn set_folder(&self, folder: &str) {
            self.db.lock().unwrap().meta_set("backup_folder", folder).unwrap();
        }

        fn total(&self) -> i64 {
            self.db.lock().unwrap().total_items().unwrap()
        }

        fn published(&self) -> BackupWatch {
            self.info.lock().unwrap().backup.clone()
        }

        fn on_disk(&self) -> Option<BackupWatch> {
            DaemonInfo::read(self.dir.path()).map(|info| info.backup)
        }
    }

    /// Ticks until the read in flight has come back and been dealt with.
    fn settle(watcher: &mut Watcher, now: f64) {
        for _ in 0..500 {
            watcher.tick(now).unwrap();
            if !watcher.reading() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the read did not finish");
    }

    fn write_manifest(manifest: &Path, notes: Vec<serde_json::Value>) {
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let body = json!({ "formatVersion": 1, "createdAt": 816317953.0, "notes": notes, "dictations": [], "meetings": [] });
        std::fs::write(manifest, serde_json::to_vec(&body).unwrap()).unwrap();
    }

    #[test]
    fn backup_folder_is_imported_once_per_change() {
        let rig = Rig::new();
        let folder = tempfile::tempdir().unwrap();
        let manifest = folder.path().join("Zone library.zonebackup").join("library.json");
        write_manifest(&manifest, vec![json!({ "id": "N1", "text": "first", "createdAt": 816200000.0 })]);
        let mut watcher = rig.watcher();

        // No folder configured: nothing happens.
        watcher.tick(1000.0).unwrap();
        assert!(!watcher.reading());
        assert_eq!(rig.total(), 0);

        rig.set_folder(&folder.path().to_string_lossy());
        settle(&mut watcher, 1001.0);
        assert_eq!(rig.total(), 1);
        let first_sync = rig.db.lock().unwrap().meta_f64("last_sync_backup").unwrap().unwrap();
        assert_eq!(rig.published().read_started_at, None);
        assert_eq!(rig.published().read_finished_at, Some(1001.0));

        // Less than half a minute later: the folder is not even looked at.
        watcher.tick(1010.0).unwrap();
        assert!(!watcher.reading());

        // Unchanged file: looked at, not imported again.
        settle(&mut watcher, 1040.0);
        assert_eq!(rig.db.lock().unwrap().meta_f64("last_sync_backup").unwrap().unwrap(), first_sync);

        // Changed file: read again, the new item appears.
        std::thread::sleep(Duration::from_millis(20));
        write_manifest(
            &manifest,
            vec![
                json!({ "id": "N1", "text": "first", "createdAt": 816200000.0 }),
                json!({ "id": "N2", "text": "second, longer", "createdAt": 816200500.0 }),
            ],
        );
        settle(&mut watcher, 1080.0);
        assert_eq!(rig.total(), 2);
        assert_eq!(rig.db.lock().unwrap().meta_f64("last_sync_backup").unwrap().unwrap(), 1080.0);

        // A folder that went away is reported, not fatal, and the daemon says what is wrong.
        rig.set_folder("/nonexistent/zone-backups");
        settle(&mut watcher, 1081.0);
        assert!(rig.published().error.unwrap().contains("cannot find"));
        assert!(rig.on_disk().unwrap().error.is_some());

        // Clearing the folder clears what is published.
        rig.db.lock().unwrap().meta_delete("backup_folder").unwrap();
        watcher.tick(1082.0).unwrap();
        assert_eq!(rig.published(), BackupWatch::default());
        assert_eq!(rig.on_disk().unwrap(), BackupWatch::default());
    }

    /// A scan that blocks until the test lets it go, like `opendir` behind a permission prompt.
    fn blocking_scan(starts: Arc<AtomicUsize>) -> (ScanFn, Sender<()>) {
        let (release, gate) = channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let scan: ScanFn = Arc::new(move |folder: &Path, seen: &Seen| {
            starts.fetch_add(1, Ordering::SeqCst);
            // Blocks here, holding nothing but this gate.
            let _ = gate.lock().unwrap().recv();
            backup::scan(folder, seen)
        });
        (scan, release)
    }

    #[test]
    fn a_stuck_read_holds_no_lock_and_is_not_repeated() {
        let rig = Rig::new();
        let folder = tempfile::tempdir().unwrap();
        let manifest = folder.path().join("Zone library.zonebackup").join("library.json");
        write_manifest(&manifest, vec![json!({ "id": "N1", "text": "first", "createdAt": 816200000.0 })]);
        rig.set_folder(&folder.path().to_string_lossy());

        let starts = Arc::new(AtomicUsize::new(0));
        let (scan, release) = blocking_scan(starts.clone());
        let mut watcher = Watcher::with_scan(rig.db.clone(), rig.info.clone(), rig.dir.path().to_path_buf(), scan);

        watcher.tick(1000.0).unwrap();
        assert!(watcher.reading());
        // The read began, and the daemon says so in memory at once.
        assert_eq!(rig.published().read_started_at, Some(1000.0));

        // Ten minutes of ticks, one a second. The read never answers.
        for second in 1..=600 {
            watcher.tick(1000.0 + second as f64).unwrap();
            // The library is free the whole time: this is what Link and the web page need.
            assert!(rig.db.try_lock().is_ok(), "the library lock is held during a stuck read");
        }
        assert!(watcher.reading());
        // Twenty thirty-second periods went by and still only one thread is blocked.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "a stuck read must not be started again");
        assert_eq!(watcher.abandoned(), 0);
        // `zone status` and `zone doctor` read this from daemon.json.
        let seen_by_status = rig.on_disk().unwrap();
        assert_eq!(seen_by_status.read_started_at, Some(1000.0));
        assert_eq!(rig.total(), 0);

        // The person answers the prompt: the read returns, and the import happens.
        release.send(()).unwrap();
        settle(&mut watcher, 1700.0);
        assert_eq!(rig.total(), 1);
        assert_eq!(rig.published().read_started_at, None);
        assert_eq!(rig.on_disk().unwrap().read_started_at, None);
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        // Watching carries on as usual afterwards.
        watcher.tick(1740.0).unwrap();
        assert!(watcher.reading());
        release.send(()).unwrap();
        settle(&mut watcher, 1741.0);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn changing_the_folder_under_a_stuck_read_is_bounded() {
        let rig = Rig::new();
        let starts = Arc::new(AtomicUsize::new(0));
        let (scan, release) = blocking_scan(starts.clone());
        let mut watcher = Watcher::with_scan(rig.db.clone(), rig.info.clone(), rig.dir.path().to_path_buf(), scan);

        // Each new folder gets a read of its own while the earlier ones are still stuck, up to
        // a limit. Then the watcher waits instead of blocking yet another thread.
        for (index, now) in (0..8).zip((0..).map(|i| 2000.0 + i as f64 * 100.0)) {
            rig.set_folder(&format!("/nonexistent/stuck-{index}"));
            watcher.tick(now).unwrap();
            watcher.tick(now + 50.0).unwrap();
        }
        // The reads start on threads of their own: give a slow computer time to start them all,
        // then a little more to show that no further one starts.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while starts.load(Ordering::SeqCst) < MAX_ABANDONED + 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(starts.load(Ordering::SeqCst), MAX_ABANDONED + 1);
        assert_eq!(watcher.abandoned(), MAX_ABANDONED);
        assert!(watcher.reading());
        assert!(rig.db.try_lock().is_ok());

        // Everything answers at last. The watcher moves on to the folder that is set now.
        for _ in 0..=MAX_ABANDONED {
            release.send(()).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while starts.load(Ordering::SeqCst) < MAX_ABANDONED + 2 {
            assert!(std::time::Instant::now() < deadline, "the current folder was not read once the old read answered");
            watcher.tick(3000.0).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        release.send(()).unwrap();
        settle(&mut watcher, 3002.0);
        assert_eq!(rig.published().folder.as_deref(), Some("/nonexistent/stuck-7"));
        assert!(rig.published().error.is_some());
    }
}
