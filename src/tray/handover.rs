//! Getting out of the way of an update.
//!
//! Windows cannot write over a program that is running, but it can rename it. So the
//! installer (install.ps1, which `zone update` runs) moves the running tray aside and puts
//! the new one at its name. The tray looks at its own name every few seconds: once another
//! file is there, and has stopped changing, it starts that file and ends. The new tray waits
//! for the old one to let go of `tray.lock`, so there is never more than one icon.
//!
//! The same rule as `zone serve` and its keeper follow (serve.rs, background.rs), kept here
//! as a plain decision so it can be tested on any system.

use std::path::Path;

/// What tells one program file from another at the same path: its size and its times.
pub type Identity = (u64, u64, u64);

#[cfg(windows)]
pub fn identity(path: &Path) -> Option<Identity> {
    use std::os::windows::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.file_size(), metadata.creation_time(), metadata.last_write_time()))
}

#[cfg(unix)]
pub fn identity(path: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.size(), metadata.ino(), metadata.mtime() as u64))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The program at this name is still this one (or is missing for a moment).
    Stay,
    /// Another file is there. Look once more before trusting it: it may still be written.
    LookAgain,
    /// Another file is there and has stopped changing: start it and end.
    HandOver,
}

/// Watches the tray's own program file.
#[derive(Clone, Debug)]
pub struct Watch {
    mine: Option<Identity>,
    seen: Option<Identity>,
}

impl Watch {
    /// `mine`: the file as it was when this tray started.
    pub fn new(mine: Option<Identity>) -> Watch {
        Watch { mine, seen: None }
    }

    /// One look. `now` is the file at the tray's name (None when there is none), `busy` is
    /// true while the tray is in the middle of something that must finish first: an update it
    /// started itself is still running the installer.
    pub fn look(&mut self, now: Option<Identity>, busy: bool) -> Step {
        // A tray that could not see its own file at the start has nothing to compare with.
        let (Some(mine), Some(now)) = (self.mine, now) else {
            // Gone for a moment (an installer between moving and copying) is not replaced yet.
            self.seen = None;
            return Step::Stay;
        };
        if now == mine {
            self.seen = None;
            return Step::Stay;
        }
        if self.seen != Some(now) {
            self.seen = Some(now);
            return Step::LookAgain;
        }
        if busy { Step::LookAgain } else { Step::HandOver }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_hands_over_once_the_new_program_has_stopped_changing() {
        let old = (100, 1, 1);
        let mut watch = Watch::new(Some(old));
        assert_eq!(watch.look(Some(old), false), Step::Stay);
        // The installer moved the old one aside: for a moment nothing is at the name.
        assert_eq!(watch.look(None, false), Step::Stay);
        // The new file is being copied: it grows between two looks.
        assert_eq!(watch.look(Some((40, 2, 2)), false), Step::LookAgain);
        assert_eq!(watch.look(Some((120, 2, 3)), false), Step::LookAgain);
        assert_eq!(watch.look(Some((120, 2, 3)), false), Step::HandOver);
    }

    #[test]
    fn an_update_the_tray_started_itself_finishes_first() {
        let mut watch = Watch::new(Some((100, 1, 1)));
        let new = Some((120, 2, 3));
        assert_eq!(watch.look(new, true), Step::LookAgain);
        assert_eq!(watch.look(new, true), Step::LookAgain);
        assert_eq!(watch.look(new, true), Step::LookAgain);
        assert_eq!(watch.look(new, false), Step::HandOver);
    }

    #[test]
    fn the_same_file_put_back_changes_nothing() {
        let old = (100, 1, 1);
        let mut watch = Watch::new(Some(old));
        assert_eq!(watch.look(Some((90, 5, 5)), false), Step::LookAgain);
        // The installer could not write the new one and moved the old one back.
        assert_eq!(watch.look(Some(old), false), Step::Stay);
        assert_eq!(watch.look(Some((90, 5, 5)), false), Step::LookAgain);
        // A tray that never saw its own file does not guess.
        let mut blind = Watch::new(None);
        assert_eq!(blind.look(Some(old), false), Step::Stay);
        assert_eq!(blind.look(Some(old), false), Step::Stay);
    }

    #[test]
    fn a_file_has_an_identity_and_a_missing_one_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zone-tray.exe");
        assert_eq!(identity(&path), None);
        std::fs::write(&path, b"one").unwrap();
        let first = identity(&path).unwrap();
        assert_eq!(identity(&path), Some(first));
        // Moved aside and another put in its place, as the installer does.
        std::fs::rename(&path, dir.path().join("zone-tray.exe.old-1")).unwrap();
        std::fs::write(&path, b"another").unwrap();
        assert_ne!(identity(&path), Some(first));
    }
}
