//! The tray's icons: one `.ico` file per face and taskbar color, built into the program.
//!
//! An `.ico` file is a short list of pictures of one drawing at several sizes. Windows wants
//! the picture that fits the taskbar exactly (16 pixels at 100% scaling, 20 at 125%, 24 at
//! 150%, 32 at 200%), so the list is read here and the right picture handed over as it is.
//! Drawn by `scripts/make-tray-icons.swift`.

use super::model::State;

/// Which taskbar the icon sits on. Windows does not recolor a tray icon the way macOS does a
/// menu bar icon, so there is one drawing for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Taskbar {
    /// A dark taskbar (the usual one): the Z is drawn light.
    Dark,
    /// A light taskbar: the Z is drawn dark.
    Light,
}

/// A second drawing of "receiving", shown in turn with the first so the ring moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Beat {
    First,
    Second,
}

/// The whole `.ico` file for a face.
pub fn file(state: State, taskbar: Taskbar, beat: Beat) -> &'static [u8] {
    match (taskbar, state, beat) {
        (Taskbar::Dark, State::Running, _) => include_bytes!("../../assets/windows/tray-running-dark.ico"),
        (Taskbar::Dark, State::Receiving, Beat::First) => include_bytes!("../../assets/windows/tray-receiving-dark.ico"),
        (Taskbar::Dark, State::Receiving, Beat::Second) => include_bytes!("../../assets/windows/tray-receiving2-dark.ico"),
        (Taskbar::Dark, State::Stopped, _) => include_bytes!("../../assets/windows/tray-stopped-dark.ico"),
        (Taskbar::Dark, State::Attention, _) => include_bytes!("../../assets/windows/tray-attention-dark.ico"),
        (Taskbar::Light, State::Running, _) => include_bytes!("../../assets/windows/tray-running-light.ico"),
        (Taskbar::Light, State::Receiving, Beat::First) => include_bytes!("../../assets/windows/tray-receiving-light.ico"),
        (Taskbar::Light, State::Receiving, Beat::Second) => include_bytes!("../../assets/windows/tray-receiving2-light.ico"),
        (Taskbar::Light, State::Stopped, _) => include_bytes!("../../assets/windows/tray-stopped-light.ico"),
        (Taskbar::Light, State::Attention, _) => include_bytes!("../../assets/windows/tray-attention-light.ico"),
    }
}

/// One picture in an `.ico` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture<'a> {
    /// Its side in pixels (the pictures are square).
    pub side: u32,
    /// The picture itself, as Windows takes it.
    pub bytes: &'a [u8],
}

/// The pictures in an `.ico` file. Empty when the bytes are not one.
pub fn pictures(file: &[u8]) -> Vec<Picture<'_>> {
    let word = |at: usize| file.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let long = |at: usize| file.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    // Reserved 0, type 1 (an icon), then how many.
    let (Some(0), Some(1), Some(count)) = (word(0), word(2), word(4)) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for index in 0..count {
        let entry = 6 + index * 16;
        let (Some(width), Some(size), Some(offset)) = (file.get(entry), long(entry + 8), long(entry + 12)) else {
            return Vec::new();
        };
        let Some(bytes) = offset.checked_add(size).and_then(|end| file.get(offset..end)) else {
            return Vec::new();
        };
        // A width of 0 stands for 256.
        found.push(Picture {
            side: if *width == 0 { 256 } else { u32::from(*width) },
            bytes,
        });
    }
    found
}

/// The picture for a taskbar that wants icons `wanted` pixels wide: that size exactly when
/// the file has it, else the smallest larger one (Windows scales down well, up badly), else
/// the largest.
pub fn best(file: &[u8], wanted: u32) -> Option<Picture<'_>> {
    let all = pictures(file);
    all.iter()
        .filter(|picture| picture.side >= wanted)
        .min_by_key(|picture| picture.side)
        .or_else(|| all.iter().max_by_key(|picture| picture.side))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [State; 4] = [State::Running, State::Receiving, State::Stopped, State::Attention];

    #[test]
    fn every_face_has_every_taskbar_size() {
        for taskbar in [Taskbar::Dark, Taskbar::Light] {
            for state in STATES {
                for beat in [Beat::First, Beat::Second] {
                    let sides: Vec<u32> = pictures(file(state, taskbar, beat)).iter().map(|picture| picture.side).collect();
                    assert_eq!(sides, [16, 20, 24, 32, 40, 48], "{state:?} {taskbar:?}");
                    for picture in pictures(file(state, taskbar, beat)) {
                        // Each picture is a PNG, which Windows has taken in icons since Vista.
                        assert!(picture.bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{state:?} {taskbar:?} {}", picture.side);
                    }
                }
            }
        }
    }

    #[test]
    fn the_faces_differ_and_each_taskbar_has_its_own() {
        let mut seen = std::collections::HashSet::new();
        for taskbar in [Taskbar::Dark, Taskbar::Light] {
            for state in STATES {
                assert!(seen.insert(file(state, taskbar, Beat::First)), "{state:?} {taskbar:?} is drawn twice");
            }
            assert!(seen.insert(file(State::Receiving, taskbar, Beat::Second)));
        }
        // Only "receiving" moves.
        assert_eq!(
            file(State::Running, Taskbar::Dark, Beat::First),
            file(State::Running, Taskbar::Dark, Beat::Second)
        );
    }

    #[test]
    fn the_picture_that_fits_is_chosen() {
        let file = file(State::Running, Taskbar::Dark, Beat::First);
        let side = |wanted: u32| best(file, wanted).unwrap().side;
        assert_eq!([side(16), side(20), side(24), side(32)], [16, 20, 24, 32]);
        // 175% scaling wants 28: the next one up is scaled down.
        assert_eq!(side(28), 32);
        assert_eq!(side(64), 48);
        assert_eq!(best(b"not an icon", 16), None);
        assert!(pictures(&file[..30]).is_empty(), "a file cut short is not read past its end");
    }
}
