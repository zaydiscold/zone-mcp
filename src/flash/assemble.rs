//! Putting one flash together from what the camera reads, and catching codes that were misread.
//!
//! [`Assembler`] sits between the frames and the GF(2) [`Decoder`]. It keeps every different
//! reading of a code (its number and its body), counts which code numbers were caught, and
//! watches the checks the decoder makes: once a frame brings no new row, its body must equal
//! the sum of the rows it reduces to (see [`Reduced`]). When every reading is what the phone
//! showed, every check comes out even.
//!
//! A misread code (rare: QR error correction rejects nearly all damage, but a picture caught
//! half drawn, or a color channel unmixed wrongly, can still decode as a valid code with the
//! wrong bytes) makes checks come out uneven. Each uneven check names a set of readings that
//! holds a misread one: the check itself and the readings its rows were made from. Each even
//! check clears the readings it names, because the errors of different misreadings do not
//! cancel. The misread reading is in every uneven set and in no even one, so a few dozen
//! codes after the first uneven check it is the only reading left under suspicion. It is then
//! set aside for good, the rows are rebuilt from every other reading, and reading goes on.
//!
//! The seal (ChaCha20-Poly1305) is the final judge: a payload that opens is exactly what the
//! phone sealed. When a complete payload does not open, the assembler keeps reading checks.
//! If they all come out even, every reading agrees and the payload is what the phone showed:
//! then it was sealed with a key this computer does not have, which is a different thing to
//! tell the person than "a code was misread".
//!
//! Tracking which readings each row was made of costs about as much memory as the rows
//! themselves, so it is done for flashes of up to [`TRACK_LIMIT`] chunks. A larger flash that
//! goes wrong starts over from new readings instead, which costs time but never stores a
//! wrong payload.

use std::collections::{BTreeMap, HashMap};

use super::fountain::{Decoder, Reduced, Shape, fingerprint};

/// Flashes of up to this many chunks remember which readings made each row.
pub const TRACK_LIMIT: usize = 16_384;
/// Even checks in a row that make a complete payload which no key opens believable: with a
/// misread reading among the rows, each check would come out uneven half the time.
pub const CONFIRMATIONS: usize = 16;
/// At most this many readings are set aside at once.
const SET_ASIDE_AT_MOST: usize = 8;
/// Readings to wait, while a few suspects stop narrowing, before setting them all aside.
const PATIENCE: usize = 24;
/// Uneven checks kept before giving up on finding the culprits and starting over, beyond
/// four for every chunk.
const MAX_CONFLICTS: usize = 256;
/// Rebuilds before starting over from nothing.
const MAX_REBUILDS: usize = 32;
/// Code number ranges named in a tally.
pub const RANGES_NAMED: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    /// In use, at this position among the readings.
    Kept,
    /// Found to be misread. Seeing it again changes nothing.
    SetAside,
}

struct Reading {
    index: u32,
    fingerprint: u64,
    /// The body, kept while tracking so the rows can be rebuilt without it. Empty otherwise.
    body: Vec<u8>,
}

struct Conflict {
    /// The reading whose check came out uneven.
    reading: usize,
    /// The rows it was checked against, by slot.
    support: Vec<u64>,
    /// Which errors it adds up (see [`Reduced::Disagreed`]).
    residual: u64,
}

/// What one reading did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Took {
    /// A code read before, body for body: the camera saw it twice.
    Repeat,
    /// A reading set aside as misread, seen again.
    SetAside,
    /// A new reading. `useful` when it brought a new row.
    New { useful: bool },
}

/// Why a flash is not simply progressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// Some checks came out uneven: a code was misread. It is being looked for.
    Misread,
    /// The pieces came together but did not open. More codes are read to tell a misread code
    /// from a flash sealed for another computer.
    Checking,
}

impl Trouble {
    pub fn as_str(self) -> &'static str {
        match self {
            Trouble::Misread => "misread",
            Trouble::Checking => "checking",
        }
    }
}

/// What the code numbers say, for the page. Numbers are counted from 1, as a person would:
/// frame `i` is code number `i + 1`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Different code numbers caught (and not set aside).
    pub caught: usize,
    /// The highest code number caught: the phone has shown at least this many.
    pub shown: u64,
    /// Code numbers up to `shown` that were never caught.
    pub missed: u64,
    /// The first of those, as inclusive ranges of code numbers.
    pub missed_ranges: Vec<(u64, u64)>,
    /// Readings of a code already caught.
    pub repeats: u64,
    /// Readings found to be misread and set aside.
    pub set_aside: usize,
    /// Times everything was dropped and reading began again.
    pub restarts: usize,
}

pub struct Assembler {
    shape: Shape,
    decoder: Decoder,
    tracking: bool,
    /// Every new reading, in order of arrival. None once set aside (or dropped by a restart).
    readings: Vec<Option<Reading>>,
    known: HashMap<(u32, u64), Known>,
    /// Code number to how many kept readings of it there are.
    caught: BTreeMap<u32, u32>,
    /// The reading that made each row, by slot.
    slots: Vec<usize>,
    /// Slots that an even check vouched for.
    cleared: Vec<u64>,
    conflicts: Vec<Conflict>,
    /// An uneven check while not tracking.
    uneven: bool,
    /// Even checks since the payload last failed to open.
    agreements: usize,
    /// The rows solve to a payload that did not open.
    unopened: bool,
    /// The rows just became complete: a payload is ready to solve.
    ready: bool,
    rebuilds: usize,
    repeats: u64,
    set_aside: usize,
    restarts: usize,
    /// The fewest suspects so far, and readings since that number last fell.
    suspects: usize,
    patience: usize,
}

impl Assembler {
    pub fn new(shape: Shape) -> Assembler {
        let tracking = shape.k as usize <= TRACK_LIMIT;
        Assembler {
            shape,
            decoder: Assembler::decoder_for(shape, tracking),
            tracking,
            readings: Vec::new(),
            known: HashMap::new(),
            caught: BTreeMap::new(),
            slots: Vec::new(),
            cleared: Vec::new(),
            conflicts: Vec::new(),
            uneven: false,
            agreements: 0,
            unopened: false,
            ready: false,
            rebuilds: 0,
            repeats: 0,
            set_aside: 0,
            restarts: 0,
            suspects: usize::MAX,
            patience: 0,
        }
    }

    fn decoder_for(shape: Shape, tracking: bool) -> Decoder {
        if tracking { Decoder::tracking(shape) } else { Decoder::new(shape) }
    }

    pub fn k(&self) -> usize {
        self.decoder.k()
    }

    pub fn rank(&self) -> usize {
        self.decoder.rank()
    }

    pub fn decoder(&self) -> &Decoder {
        &self.decoder
    }

    /// Takes one reading: a frame of this flash, `body` of the flash's chunk size.
    pub fn take(&mut self, index: u32, body: &[u8]) -> Took {
        let print = fingerprint(body);
        match self.known.get(&(index, print)) {
            Some(Known::Kept) => {
                self.repeats += 1;
                return Took::Repeat;
            }
            Some(Known::SetAside) => {
                self.repeats += 1;
                return Took::SetAside;
            }
            None => {}
        }
        let position = self.readings.len();
        let kept = if self.tracking { body.to_vec() } else { Vec::new() };
        self.readings.push(Some(Reading { index, fingerprint: print, body: kept }));
        self.known.insert((index, print), Known::Kept);
        *self.caught.entry(index).or_default() += 1;
        let useful = self.reduce(position, index, body);
        self.look_for_misreads(!useful);
        Took::New { useful }
    }

    /// Reduces one reading. True when it made a new row.
    fn reduce(&mut self, position: usize, index: u32, body: &[u8]) -> bool {
        match self.decoder.reduce(index, body) {
            Reduced::Useful { slot } => {
                debug_assert_eq!(slot, self.slots.len());
                self.slots.push(position);
                if self.decoder.is_complete() {
                    self.ready = true;
                }
                true
            }
            Reduced::Agreed { support } => {
                self.agreements += 1;
                if self.cleared.len() < support.len() {
                    self.cleared.resize(support.len(), 0);
                }
                for (cleared, vouched) in self.cleared.iter_mut().zip(&support) {
                    *cleared |= vouched;
                }
                false
            }
            Reduced::Disagreed { support, residual } => {
                if self.tracking {
                    self.conflicts.push(Conflict { reading: position, support, residual });
                } else {
                    self.uneven = true;
                }
                false
            }
        }
    }

    /// After a new reading: narrows down the suspects of the uneven checks so far, and sets a
    /// suspect aside once it is the only one left (or once the few left stop narrowing).
    ///
    /// `after_check` says the newest reading was a check: only checks are evidence, so only
    /// they count toward the patience with suspects that stop narrowing.
    fn look_for_misreads(&mut self, after_check: bool) {
        if !self.tracking {
            if self.uneven {
                self.start_over();
            }
            return;
        }
        if self.conflicts.is_empty() {
            self.patience = 0;
            self.suspects = usize::MAX;
            return;
        }
        if self.conflicts.len() > MAX_CONFLICTS + 4 * self.k() || self.rebuilds >= MAX_REBUILDS {
            self.start_over();
            return;
        }
        let words = self.slots.len().div_ceil(64).max(1);
        let open = |conflict: &Conflict, cleared: &[u64]| -> Vec<u64> {
            (0..words).map(|w| conflict.support.get(w).copied().unwrap_or(0) & !cleared.get(w).copied().unwrap_or(0)).collect()
        };
        let count = |bits: &[u64]| bits.iter().map(|w| w.count_ones() as usize).sum::<usize>();
        // An uneven check whose rows have all been vouched for was itself misread.
        let lone: Vec<usize> = self.conflicts.iter().filter(|c| count(&open(c, &self.cleared)) == 0).map(|c| c.reading).collect();
        if !lone.is_empty() {
            for position in &lone {
                self.set_reading_aside(*position);
            }
            self.conflicts.retain(|c| !lone.contains(&c.reading));
            if self.conflicts.is_empty() {
                return;
            }
        }
        // Uneven checks with the same residual add up the same errors: most often those of one
        // misread reading. For each such group, the rows every check in it names are suspects,
        // and with only one check in the group, so is its own reading. Several misread readings
        // are then found side by side, each by its own group.
        let mut groups: HashMap<u64, (Vec<u64>, Vec<usize>)> = HashMap::new();
        for conflict in &self.conflicts {
            let bits = open(conflict, &self.cleared);
            match groups.get_mut(&conflict.residual) {
                Some((rows, readings)) => {
                    for (w, b) in bits.into_iter().enumerate() {
                        rows[w] &= b;
                    }
                    readings.push(conflict.reading);
                }
                None => {
                    groups.insert(conflict.residual, (bits, vec![conflict.reading]));
                }
            }
        }
        let suspects_of = |(rows, readings): &(Vec<u64>, Vec<usize>)| -> (Vec<u64>, Vec<usize>) {
            (rows.clone(), if readings.len() == 1 { readings.clone() } else { Vec::new() })
        };
        let size = |(rows, checks): &(Vec<u64>, Vec<usize>)| count(rows) + checks.len();
        let candidates: Vec<(Vec<u64>, Vec<usize>)> = groups.values().map(suspects_of).filter(|c| size(c) > 0).collect();
        let fewest = candidates.iter().map(size).min().unwrap_or(usize::MAX);
        if fewest < self.suspects {
            self.suspects = fewest;
            self.patience = 0;
        } else if after_check {
            self.patience += 1;
        }
        // Every group down to one suspect is settled; failing that, the smallest group once it
        // has stopped narrowing.
        let mut settled: Vec<&(Vec<u64>, Vec<usize>)> = candidates.iter().filter(|c| size(c) == 1).collect();
        if settled.is_empty() && fewest <= SET_ASIDE_AT_MOST && self.patience >= PATIENCE {
            settled.extend(candidates.iter().filter(|c| size(c) == fewest).take(1));
        }
        if settled.is_empty() {
            return;
        }
        let mut positions: Vec<usize> = Vec::new();
        let mut rows_affected = false;
        for (rows, checks) in settled {
            positions.extend(checks);
            for slot in 0..self.slots.len() {
                if rows[slot / 64] >> (slot % 64) & 1 == 1 {
                    positions.push(self.slots[slot]);
                    rows_affected = true;
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        for position in &positions {
            self.set_reading_aside(*position);
        }
        self.patience = 0;
        self.suspects = usize::MAX;
        if rows_affected {
            self.rebuild();
        } else {
            // Only checks were misread. The rows stand; their uneven checks go.
            self.conflicts.retain(|c| !positions.contains(&c.reading));
        }
    }

    fn set_reading_aside(&mut self, position: usize) {
        let Some(reading) = self.readings[position].take() else { return };
        self.known.insert((reading.index, reading.fingerprint), Known::SetAside);
        if let Some(count) = self.caught.get_mut(&reading.index) {
            *count -= 1;
            if *count == 0 {
                self.caught.remove(&reading.index);
            }
        }
        self.set_aside += 1;
    }

    /// Makes the rows again from every reading still kept, in the order they arrived.
    fn rebuild(&mut self) {
        self.rebuilds += 1;
        self.decoder = Assembler::decoder_for(self.shape, self.tracking);
        self.slots.clear();
        self.cleared.clear();
        self.conflicts.clear();
        self.agreements = 0;
        self.unopened = false;
        self.ready = false;
        self.suspects = usize::MAX;
        self.patience = 0;
        for position in 0..self.readings.len() {
            let Some(reading) = self.readings[position].take() else { continue };
            self.reduce(position, reading.index, &reading.body);
            self.readings[position] = Some(reading);
        }
        self.look_for_misreads(false);
    }

    /// Drops every reading and begins again, keeping only what was set aside. The phone keeps
    /// showing the codes, so they come round again.
    fn start_over(&mut self) {
        self.restarts += 1;
        self.rebuilds = 0;
        self.decoder = Assembler::decoder_for(self.shape, self.tracking);
        self.readings.clear();
        self.known.retain(|_, known| *known == Known::SetAside);
        self.caught.clear();
        self.slots.clear();
        self.cleared.clear();
        self.conflicts.clear();
        self.uneven = false;
        self.agreements = 0;
        self.unopened = false;
        self.ready = false;
        self.suspects = usize::MAX;
        self.patience = 0;
    }

    /// The payload, once the rows have just become complete. Each set of rows is offered once.
    pub fn take_payload(&mut self) -> Option<Vec<u8>> {
        if !std::mem::take(&mut self.ready) {
            return None;
        }
        self.decoder.solve()
    }

    /// The payload the rows solve to now, ready or not. For tools that read a whole recording
    /// at once.
    pub fn payload(&self) -> Option<Vec<u8>> {
        self.decoder.solve()
    }

    /// The payload these rows solve to did not open with any key: keep checking.
    pub fn did_not_open(&mut self) {
        self.unopened = true;
        self.agreements = 0;
    }

    /// True once a complete payload that did not open has been checked by enough later codes,
    /// all even: every reading agrees, so the payload is what the phone showed.
    pub fn confirmed_unopenable(&self) -> bool {
        self.unopened && self.conflicts.is_empty() && !self.uneven && self.agreements >= CONFIRMATIONS
    }

    pub fn trouble(&self) -> Option<Trouble> {
        if self.unopened {
            Some(Trouble::Checking)
        } else if !self.conflicts.is_empty() || self.uneven {
            Some(Trouble::Misread)
        } else {
            None
        }
    }

    pub fn tally(&self) -> Tally {
        let caught = self.caught.len();
        let shown = self.caught.keys().next_back().map_or(0, |last| u64::from(*last) + 1);
        let mut missed_ranges = Vec::new();
        let mut next = 0u64;
        for index in self.caught.keys() {
            let index = u64::from(*index);
            if index > next {
                missed_ranges.push((next + 1, index));
                if missed_ranges.len() == RANGES_NAMED {
                    break;
                }
            }
            next = index + 1;
        }
        Tally {
            caught,
            shown,
            missed: shown - caught as u64,
            missed_ranges,
            repeats: self.repeats,
            set_aside: self.set_aside,
            restarts: self.restarts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flash::fountain::{Encoder, HEADER_LEN, parse};

    fn payload(len: usize, seed: u32) -> Vec<u8> {
        (0..len as u32).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 13) as u8).collect()
    }

    fn take(assembler: &mut Assembler, frame: &[u8]) -> Took {
        let parsed = parse(frame).unwrap();
        assembler.take(parsed.index, parsed.body)
    }

    /// A frame with one byte of its body changed: a code misread as another valid code.
    fn misread(frame: &[u8], at: usize) -> Vec<u8> {
        let mut bad = frame.to_vec();
        bad[HEADER_LEN + at] ^= 0x5a;
        bad
    }

    #[test]
    fn the_tally_counts_from_one_and_names_what_was_missed() {
        let data = payload(9000, 1);
        let encoder = Encoder::new(5, &data, 300).unwrap();
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        // Frame 69 is code number 70.
        for index in (0..80u32).filter(|i| *i != 69 && !(140..143).contains(i)) {
            take(&mut assembler, &encoder.frame(index));
        }
        take(&mut assembler, &encoder.frame(150));
        take(&mut assembler, &encoder.frame(3));
        let tally = assembler.tally();
        assert_eq!((tally.caught, tally.shown, tally.repeats), (80, 151, 1));
        assert_eq!(tally.missed, 151 - 80);
        assert_eq!(tally.missed_ranges, vec![(70, 70), (81, 150)]);
    }

    #[test]
    fn a_whole_flash_with_nothing_misread_has_no_trouble() {
        let data = payload(30_000, 2);
        let encoder = Encoder::new(6, &data, 300).unwrap();
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let mut index = 0;
        let solved = loop {
            take(&mut assembler, &encoder.frame(index * 2 + 1));
            assert_eq!(assembler.trouble(), None);
            if let Some(solved) = assembler.take_payload() {
                break solved;
            }
            index += 1;
        };
        assert_eq!(solved, data);
        assert_eq!(assembler.take_payload(), None, "offered once");
    }

    /// A misread coded frame among the rows: the payload comes out wrong, later checks find
    /// the frame, it is set aside, and the rebuilt rows give the right payload.
    #[test]
    fn a_misread_row_is_found_set_aside_and_the_rows_rebuilt() {
        for (len, chunk, bad_at) in [(30_000usize, 300usize, 7u32), (300, 300, 0), (2000, 300, 3), (120_000, 900, 40)] {
            let data = payload(len, len as u32);
            let encoder = Encoder::new(0x8000_0007, &data, chunk).unwrap();
            let k = encoder.k();
            let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
            let mut wrong_payloads = 0;
            let mut solved = None;
            for index in 0..(3 * k as u32 + 60) {
                let frame = encoder.frame(index);
                let frame = if index == bad_at { misread(&frame, 5) } else { frame };
                take(&mut assembler, &frame);
                if let Some(payload) = assembler.take_payload() {
                    if payload == data {
                        solved = Some(payload);
                        break;
                    }
                    // What the seal would say: does not open.
                    wrong_payloads += 1;
                    assembler.did_not_open();
                }
            }
            assert_eq!(solved.as_deref(), Some(data.as_slice()), "len {len}: rebuilt to the right payload");
            assert!(wrong_payloads <= 1, "len {len}: {wrong_payloads} wrong payloads");
            let tally = assembler.tally();
            assert!(tally.set_aside >= 1, "len {len}: {tally:?}");
            assert_eq!(tally.restarts, 0, "len {len}: found without starting over");
        }
    }

    /// A misread frame that arrives after the rows are complete only ever fails its own check.
    #[test]
    fn a_misread_check_is_set_aside_and_the_rows_stand() {
        let data = payload(20_000, 3);
        let encoder = Encoder::new(0x8000_0008, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        for index in 0..k {
            take(&mut assembler, &encoder.frame(index));
        }
        let first = assembler.take_payload().unwrap();
        assert_eq!(first, data);
        // Pretend it had not opened, to watch the checks: the misread check is uneven ...
        assembler.did_not_open();
        take(&mut assembler, &misread(&encoder.frame(k + 3), 0));
        assert_eq!(assembler.trouble(), Some(Trouble::Checking));
        // ... and even checks clear the rows it named, leaving it alone under suspicion.
        for index in k + 4..k + 40 {
            take(&mut assembler, &encoder.frame(index));
        }
        let tally = assembler.tally();
        assert_eq!(tally.set_aside, 1, "{tally:?}");
        assert_eq!(assembler.take_payload(), None, "the rows were not rebuilt");
        // The same misreading again is recognised and changes nothing.
        assert_eq!(take(&mut assembler, &misread(&encoder.frame(k + 3), 0)), Took::SetAside);
        // The real frame of that number is a new reading, and agrees.
        assert_eq!(take(&mut assembler, &encoder.frame(k + 3)), Took::New { useful: false });
    }

    /// A consistent flash whose payload does not open: every check agrees, so after enough of
    /// them it is believed, and it was never a misread.
    #[test]
    fn an_unopenable_flash_is_confirmed_by_even_checks() {
        let data = payload(5000, 4);
        let encoder = Encoder::new(0x8000_0009, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let mut index = 0;
        while assembler.take_payload().is_none() {
            take(&mut assembler, &encoder.frame(index));
            index += 1;
        }
        assembler.did_not_open();
        for extra in 0..CONFIRMATIONS as u32 {
            assert!(!assembler.confirmed_unopenable());
            assert_eq!(assembler.trouble(), Some(Trouble::Checking));
            take(&mut assembler, &encoder.frame(k + 100 + extra));
        }
        assert!(assembler.confirmed_unopenable());
        assert_eq!(assembler.tally().set_aside, 0);
    }

    /// The same code number read twice with different bodies: one of the two is wrong.
    #[test]
    fn two_readings_of_one_code_that_differ_are_caught() {
        let data = payload(9000, 5);
        let encoder = Encoder::new(0x8000_000a, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        // Plain frame 4 misread, then read correctly.
        take(&mut assembler, &misread(&encoder.frame(4), 100));
        take(&mut assembler, &encoder.frame(4));
        assert_eq!(assembler.trouble(), Some(Trouble::Misread));
        let mut solved = None;
        for index in (0..3 * k + 60).filter(|i| *i != 4) {
            take(&mut assembler, &encoder.frame(index));
            if let Some(payload) = assembler.take_payload() {
                if payload == data {
                    solved = Some(payload);
                    break;
                }
                assembler.did_not_open();
            }
        }
        assert_eq!(solved.as_deref(), Some(data.as_slice()));
        assert_eq!(assembler.trouble(), None);
        // Code number 5 (frame 4) is still counted as caught: its good reading is kept.
        let tally = assembler.tally();
        assert!(tally.missed_ranges.iter().all(|(a, b)| !(*a <= 5 && 5 <= *b)), "{tally:?}");
    }

    /// Several misread frames at once, some among the rows and some checks.
    #[test]
    fn several_misreads_are_survived() {
        let data = payload(60_000, 6);
        let encoder = Encoder::new(0x8000_000b, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let bad = [3u32, 50, k + 2, k + 9, k + 30, 2 * k + 1];
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let mut solved = None;
        'passes: for _pass in 0..3 {
            for index in 0..3 * k + 60 {
                let frame = encoder.frame(index);
                let frame = if bad.contains(&index) { misread(&frame, index as usize % 300) } else { frame };
                take(&mut assembler, &frame);
                if let Some(payload) = assembler.take_payload() {
                    if payload == data {
                        solved = Some(payload);
                        break 'passes;
                    }
                    assembler.did_not_open();
                }
            }
        }
        assert_eq!(solved.as_deref(), Some(data.as_slice()), "{:?}", assembler.tally());
    }

    /// Past the tracking limit a misread costs a fresh start, never a wrong payload.
    #[test]
    fn without_tracking_a_misread_starts_over() {
        let shape = Shape { session: 1, total_len: 300 * (TRACK_LIMIT as u32 + 1), k: (TRACK_LIMIT + 1) as u16, chunk: 300 };
        let assembler = Assembler::new(shape);
        assert!(!assembler.tracking);
        // The behaviour itself is the same code path as a small flash with tracking off.
        let data = payload(9000, 7);
        let encoder = Encoder::new(0x8000_000c, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let shape = parse(&encoder.frame(0)).unwrap().shape;
        let mut assembler = Assembler::new(shape);
        assembler.tracking = false;
        assembler.decoder = Decoder::new(shape);
        let mut solved = None;
        for index in 0..6 * k + 120 {
            let frame = encoder.frame(index % (3 * k + 60));
            let frame = if index == 2 { misread(&frame, 9) } else { frame };
            take(&mut assembler, &frame);
            if let Some(payload) = assembler.take_payload() {
                if payload == data {
                    solved = Some(payload);
                    break;
                }
                assembler.did_not_open();
            }
        }
        assert_eq!(solved.as_deref(), Some(data.as_slice()));
        assert_eq!(assembler.tally().restarts, 1);
    }
}
