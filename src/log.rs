//! The daemon's log: one line per event, to stderr and to `zone.log` in the data directory.
//!
//! Lines describe what happened (a phone connected, 120 items stored). They never contain
//! item text, pairing secrets or the web token.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::timeparse::Clock;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: Option<File>,
    stderr: bool,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Starts logging to `path` (and to stderr when `stderr` is true).
pub fn init(path: &Path, stderr: bool) {
    let file = open(path);
    let _ = SINK.set(Mutex::new(Sink { path: path.to_path_buf(), file, stderr }));
}

fn open(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).ok()
}

pub fn write(level: &str, message: &str) {
    let now = Clock::system();
    let line = format!("{} {level:5} {message}\n", now.rfc3339(crate::paths::now_unix()));
    let Some(sink) = SINK.get() else {
        // Not a daemon: plain commands report problems on stderr.
        eprint!("{line}");
        return;
    };
    let Ok(mut sink) = sink.lock() else { return };
    if sink.stderr {
        eprint!("{line}");
    }
    // Keep one previous file, so the log cannot grow without bound.
    if let Some(file) = &sink.file
        && file.metadata().map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
            let old = sink.path.with_extension("log.1");
            let _ = std::fs::rename(&sink.path, old);
            sink.file = open(&sink.path.clone());
        }
    if let Some(file) = sink.file.as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write("info", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write("warn", &format!($($arg)*)) };
}
