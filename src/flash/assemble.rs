//! Putting one flash together from what the camera reads, and catching codes that were misread.
//!
//! [`Assembler`] sits between the frames and the GF(2) [`Decoder`]. It keeps every different
//! reading of a code (its number and a fingerprint of its body), counts which code numbers were
//! caught, and watches the checks the decoder makes: once a frame brings no new row, its body
//! must equal the sum of the rows it reduces to (see [`Reduced`]). When every reading is what
//! the phone showed, every check comes out even.
//!
//! A misread code (rare: QR error correction rejects nearly all damage, but a picture caught
//! half drawn, or a color channel unmixed wrongly, can still decode as a valid code with the
//! wrong bytes) makes checks come out uneven. Each uneven check names a set of readings that
//! holds a misread one: the check itself and the readings its rows were made from. Each even
//! check clears the readings it names, because the errors of different misreadings do not
//! cancel. The misread reading is in every uneven set and in no even one, so a few dozen
//! codes after the first uneven check it is the only reading left under suspicion. It is then
//! set aside for good and taken out of the rows ([`Decoder::remove`]), the uneven checks that
//! named it are checked again against the rows as they are now, and reading goes on.
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
//!
//! Every step is bounded: taking a misread reading out of the rows costs one pass over them,
//! a reading looks for misreads at most [`ROUNDS`] times and checks at most [`RECHECK`] old
//! checks again each time, and after [`MAX_REMOVALS`] takings out, [`MAX_UNOPENED`] payloads
//! that did not open, or too many uneven checks, the flash starts over. Making the rows again
//! from every reading, the one long step, stops part way when asked to.
//!
//! And the flash as a whole is bounded. A whole loop of the phone's codes that does not move it
//! forward is helped along a step further each time ([`Assembler::unstick`]), and after
//! [`GIVE_UP_LOOPS`] loops a flash that has not come together is given up, so the person is
//! told, instead of being asked to hold the phone up for ever.

use std::collections::{BTreeMap, HashMap, VecDeque};

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
/// Uneven checks since reading last began, before giving up on finding the culprits and
/// starting over: this many, plus four for every chunk up to 2048 more. A flash with a few
/// misread codes sees a few hundred at most; one with misread rows so many that no check
/// names just one of them sees more and more, and never narrows.
const MAX_CONFLICTS: usize = 256;
/// Payloads that did not open since reading last began, before starting over. A misread code
/// or two cost one or two; more means the search is not getting anywhere.
pub const MAX_UNOPENED: usize = 8;
/// Times misread readings are taken out of the rows before starting over from nothing.
pub const MAX_REMOVALS: usize = 32;
/// Rounds of looking for misreads after one reading: each round sets aside at least one.
pub const ROUNDS: usize = 8;
/// Uneven checks checked again after the rows they named lose a reading. Older ones are
/// dropped: new codes bring new checks.
pub const RECHECK: usize = 64;
/// Code number ranges named in a tally.
pub const RANGES_NAMED: usize = 6;
/// Beyond the phone's loop of `3k + 60` codes, a code number counts in the tally only this
/// close above another one: a sender that does not loop shows them in order, and a code
/// number misread into another is almost never near any.
pub const FOLLOWS_WITHIN: u64 = 256;

/// Different code numbers taken to be on the phone's screen now: the ones read most recently.
/// A color picture holds three, and one caught while the screen changes holds the three going
/// and the three coming. A code number not among them is one the phone moved on to show (a
/// move, see `moves`).
pub const IN_VIEW: usize = 8;

/// Moves between two readings of the same code, body for body, for the second to vouch for the
/// first: at least this many other codes were shown in between, so they are two showings of it
/// (the phone shows each code once a loop), not one showing read again and again, which repeats
/// a misreading as often as it is read. A phone that stalls on one picture, and the color
/// schedule's opening and closing pictures (up for about half a second each), are read so.
pub const CONFIRM_APART: u64 = 16;

/// Loops of the phone's codes, counted in moves (one loop is `3k + 60`), after which a flash
/// that has not come together is given up, whatever it is doing.
pub const GIVE_UP_LOOPS: u64 = 12;

/// Readings made into rows again between two askings of whether to stop.
const STOP_EVERY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    /// In use, at this position among the readings.
    Kept(usize),
    /// Found to be misread. Seeing it again changes nothing. `sure` when it was the only
    /// suspect left of its uneven checks. Otherwise it was an uneven check whose rows had all
    /// been vouched for, or one of a few suspects set aside together for lack of patience,
    /// and may have been right: a start over forgets those.
    SetAside { sure: bool },
}

/// What a reading told about the rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    /// It made a new row.
    Row,
    /// A check that came out even.
    Even,
    /// A check that did not.
    Uneven,
}

struct Reading {
    index: u32,
    fingerprint: u64,
    /// `moves` when it was first read.
    seen_at: u64,
    /// The row it made, by slot.
    slot: Option<usize>,
    /// Read again, body for body, in another showing of the code. A misreading is random and
    /// does not come out the same twice, so a reading seen twice is taken to be right, as if an
    /// even check had vouched for it.
    confirmed: bool,
    /// The body, kept while tracking so the rows can be made again without a misread reading
    /// (and an uneven check checked again). Empty otherwise.
    body: Vec<u8>,
}

struct Conflict {
    /// The reading whose check came out uneven.
    reading: usize,
    /// The frames it was checked against, by slot.
    support: Vec<u64>,
    /// Which errors it adds up (see [`Reduced::Disagreed`]).
    residual: u64,
}

fn has_bit(bits: &[u64], n: usize) -> bool {
    bits.get(n / 64).is_some_and(|word| word >> (n % 64) & 1 == 1)
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
    /// Not taken: the flash was given up, or it was asked to stop (see [`Assembler::take`]).
    Ignored,
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
///
/// Only code numbers the phone can have shown count: those in its loop of `3k + 60` codes,
/// and past that only ones that follow others closely (see [`FOLLOWS_WITHIN`]). A code whose
/// number was misread into one far beyond is left out, so it cannot stretch the counts to
/// "codes 1 to 1,509,950,965".
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
    /// Whole loops of the phone's codes that went by without the flash moving forward (see
    /// [`Assembler::unstick`]).
    pub stalls: usize,
    /// Code numbers caught that the phone cannot have shown, left out of the counts above.
    pub out_of_range: usize,
}

pub struct Assembler {
    shape: Shape,
    decoder: Decoder,
    tracking: bool,
    /// Every new reading, in order of arrival. None once set aside (or dropped by a restart).
    readings: Vec<Option<Reading>>,
    known: HashMap<(u32, u64), Known>,
    /// The readings set aside on doubtful grounds since reading last began, kept (while
    /// tracking) so that they can be checked again (see [`Assembler::unstick`]).
    doubtful: Vec<Reading>,
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
    removals: usize,
    /// The clock of what the phone shows: readings of a code number not among the last
    /// [`IN_VIEW`] different ones read, because the phone moved on to show it. A still screen
    /// read again and again does not move it, and neither does a code read twice in one
    /// picture. Never set back, so it also says how far into the flash the camera is.
    moves: u64,
    /// The code numbers read last, the most recent first, [`IN_VIEW`] at most.
    in_view: VecDeque<u32>,
    /// `moves` when the flash last moved forward: a new reading made a row or came out even, or
    /// the rows became complete.
    progress_at: u64,
    /// Whole loops of the phone's codes that went by without the flash moving forward.
    stalls: usize,
    /// The flash did not come together, and was given up.
    gave_up: bool,
    /// Making the rows again was stopped part way, so the rows are not yet what the readings
    /// make. The next reading makes them again first.
    interrupted: bool,
    /// Searching the thorough way (see [`Assembler::look_for_misreads`]).
    thorough: bool,
    /// Uneven checks, and payloads that did not open, since reading last began.
    uneven_seen: usize,
    unopened_seen: usize,
    repeats: u64,
    set_aside: usize,
    restarts: usize,
    /// The fewest suspects so far, and readings since that number last fell.
    suspects: usize,
    patience: usize,
    /// Bodies added by the decoders replaced so far (see [`Assembler::body_ops`]).
    #[cfg(test)]
    retired_ops: u64,
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
            doubtful: Vec::new(),
            caught: BTreeMap::new(),
            slots: Vec::new(),
            cleared: Vec::new(),
            conflicts: Vec::new(),
            uneven: false,
            agreements: 0,
            unopened: false,
            ready: false,
            removals: 0,
            moves: 0,
            in_view: VecDeque::with_capacity(IN_VIEW),
            progress_at: 0,
            stalls: 0,
            gave_up: false,
            interrupted: false,
            thorough: false,
            uneven_seen: 0,
            unopened_seen: 0,
            repeats: 0,
            set_aside: 0,
            restarts: 0,
            suspects: usize::MAX,
            patience: 0,
            #[cfg(test)]
            retired_ops: 0,
        }
    }

    fn decoder_for(shape: Shape, tracking: bool) -> Decoder {
        if tracking { Decoder::tracking(shape) } else { Decoder::new(shape) }
    }

    /// Replaces the decoder with an empty one.
    fn fresh_decoder(&mut self) {
        let _old = std::mem::replace(&mut self.decoder, Assembler::decoder_for(self.shape, self.tracking));
        #[cfg(test)]
        {
            self.retired_ops += _old.body_ops();
        }
    }

    /// Bodies added into others so far, by this decoder and every one it replaced.
    #[cfg(test)]
    pub fn body_ops(&self) -> u64 {
        self.retired_ops + self.decoder.body_ops()
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

    /// Different code numbers caught, out of range or not. Cheap, unlike [`Assembler::tally`].
    pub fn caught_count(&self) -> usize {
        self.caught.len()
    }

    /// Readings set aside as misread so far.
    pub fn set_aside_count(&self) -> usize {
        self.set_aside
    }

    /// Takes one reading: a frame of this flash, `body` of the flash's chunk size.
    ///
    /// `stop` says to stop at once (the page started over, or zone is closing). Making the rows
    /// again from every reading kept is the one step here that can take long, a second or more
    /// for a flash of thousands of pieces, and it asks `stop` as it goes. Stopped part way, it
    /// is done again from the start at the next reading.
    pub fn take(&mut self, index: u32, body: &[u8], stop: &dyn Fn() -> bool) -> Took {
        if self.gave_up || (self.interrupted && !self.rebuild(stop)) {
            return Took::Ignored;
        }
        let print = fingerprint(body);
        self.look(index);
        if self.moves >= GIVE_UP_LOOPS * self.in_loop() {
            self.gave_up = true;
            return Took::Ignored;
        }
        if self.moves - self.progress_at >= self.in_loop() {
            self.unstick(stop);
            if self.gave_up || self.interrupted {
                return Took::Ignored;
            }
        }
        match self.known.get(&(index, print)) {
            Some(Known::Kept(position)) => {
                self.repeats += 1;
                if self.confirm(*position) && !self.conflicts.is_empty() {
                    self.look_for_misreads(false, stop);
                }
                return Took::Repeat;
            }
            Some(Known::SetAside { .. }) => {
                self.repeats += 1;
                return Took::SetAside;
            }
            None => {}
        }
        let position = self.readings.len();
        let kept = if self.tracking { body.to_vec() } else { Vec::new() };
        self.readings.push(Some(Reading { index, fingerprint: print, seen_at: self.moves, slot: None, confirmed: false, body: kept }));
        self.known.insert((index, print), Known::Kept(position));
        *self.caught.entry(index).or_default() += 1;
        let check = self.reduce(position, index, body);
        if check != Check::Uneven {
            self.progress_at = self.moves;
        }
        let useful = check == Check::Row;
        // A new row changes no check, so only a check can move the search for a misread code.
        if !useful {
            self.look_for_misreads(true, stop);
        }
        Took::New { useful }
    }

    /// The codes the phone shows in one loop: `3k + 60`, then it starts again.
    fn in_loop(&self) -> u64 {
        3 * self.k() as u64 + 60
    }

    /// Notes the code number of a reading, and moves the clock when the phone has moved on to
    /// show it (see `moves`).
    fn look(&mut self, index: u32) {
        match self.in_view.iter().position(|seen| *seen == index) {
            Some(at) => {
                self.in_view.remove(at);
            }
            None => {
                self.moves += 1;
                self.in_view.truncate(IN_VIEW - 1);
            }
        }
        self.in_view.push_front(index);
    }

    /// A whole loop of the phone's codes went by and the flash did not move forward. Every code
    /// it shows is known (a row, a check, or set aside), so no new reading brings evidence, and
    /// what is known does not open, or does not come together. Each time it is helped along a
    /// step further:
    ///
    /// 1. It forgets which readings vouched for themselves by being read again, takes back the
    ///    readings set aside on doubtful grounds (see [`Known::SetAside`]), and makes the rows
    ///    again from every reading kept, which checks every check again against the rows as
    ///    they are now. A misreading read again in another showing (against the odds) vouched
    ///    for a wrong row, after which the right reading of that code looked misread, and so
    ///    did every check that would have said so.
    /// 2. It starts over, and forgets the readings set aside on doubtful grounds (see
    ///    [`Known::SetAside`]): good codes set aside by mistake can count again.
    /// 3. It starts over as if new, forgetting every reading set aside.
    /// 4. It gives up.
    ///
    /// A flash too large to track has nothing to check again, and starts over at the first step.
    fn unstick(&mut self, stop: &dyn Fn() -> bool) {
        self.stalls += 1;
        self.progress_at = self.moves;
        match self.stalls {
            1 if self.tracking => {
                for reading in self.readings.iter_mut().flatten() {
                    reading.confirmed = false;
                }
                self.take_back_doubtful();
                if self.rebuild(stop) {
                    self.look_for_misreads(false, stop);
                }
            }
            1 | 2 => self.start_over(false),
            3 => self.start_over(true),
            _ => self.gave_up = true,
        }
    }

    /// True once the flash did not come together and was given up: see [`Assembler::unstick`]
    /// and [`GIVE_UP_LOOPS`]. Nothing more is taken.
    pub fn gave_up(&self) -> bool {
        self.gave_up
    }

    /// A reading read again, body for body. True when that confirms it now.
    fn confirm(&mut self, position: usize) -> bool {
        let moves = self.moves;
        let Some(reading) = self.readings[position].as_mut() else { return false };
        // More than CONFIRM_APART moves: this reading's own, and at least CONFIRM_APART other
        // codes shown in between.
        if reading.confirmed || moves - reading.seen_at <= CONFIRM_APART {
            return false;
        }
        reading.confirmed = true;
        if let Some(slot) = reading.slot {
            self.vouch(slot);
        }
        true
    }

    fn vouch(&mut self, slot: usize) {
        if self.cleared.len() <= slot / 64 {
            self.cleared.resize(slot / 64 + 1, 0);
        }
        self.cleared[slot / 64] |= 1 << (slot % 64);
    }

    fn confirmed(&self, position: usize) -> bool {
        self.readings[position].as_ref().is_some_and(|r| r.confirmed)
    }

    /// Reduces one reading.
    fn reduce(&mut self, position: usize, index: u32, body: &[u8]) -> Check {
        match self.decoder.reduce(index, body) {
            Reduced::Useful { slot } => {
                debug_assert_eq!(slot, self.slots.len());
                self.slots.push(position);
                if let Some(reading) = self.readings[position].as_mut() {
                    reading.slot = Some(slot);
                    if reading.confirmed {
                        self.vouch(slot);
                    }
                }
                if self.decoder.is_complete() {
                    self.ready = true;
                    self.progress_at = self.moves;
                }
                Check::Row
            }
            Reduced::Agreed { support } => {
                self.agreements += 1;
                if self.cleared.len() < support.len() {
                    self.cleared.resize(support.len(), 0);
                }
                for (cleared, vouched) in self.cleared.iter_mut().zip(&support) {
                    *cleared |= vouched;
                }
                Check::Even
            }
            Reduced::Disagreed { support, residual } => {
                self.uneven_seen += 1;
                // The even checks that make an unopened payload believable come in a row.
                self.agreements = 0;
                if self.tracking {
                    self.conflicts.push(Conflict { reading: position, support, residual });
                } else {
                    self.uneven = true;
                }
                Check::Uneven
            }
        }
    }

    /// After a new reading: narrows down the suspects of the uneven checks so far, and sets a
    /// suspect aside once it is the only one left (or once the few left stop narrowing).
    ///
    /// `after_check` says the newest reading was a check: only checks are evidence, so only
    /// they count toward the patience with suspects that stop narrowing.
    ///
    /// `stop` is for making the rows again (see [`Assembler::take`]): stopped, it returns.
    fn look_for_misreads(&mut self, mut after_check: bool, stop: &dyn Fn() -> bool) {
        if !self.tracking {
            if self.uneven {
                self.start_over(false);
            }
            return;
        }
        // Each round sets aside at least one reading, or returns.
        for _ in 0..ROUNDS {
            if self.interrupted {
                return;
            }
            if self.conflicts.is_empty() {
                self.patience = 0;
                self.suspects = usize::MAX;
                return;
            }
            let limit = MAX_CONFLICTS + (4 * self.k()).min(2048);
            if self.thorough {
                if self.conflicts.len() > limit || self.removals >= MAX_REMOVALS {
                    self.start_over(false);
                    return;
                }
            } else if self.uneven_seen > limit || self.unopened_seen >= MAX_UNOPENED || self.removals >= MAX_REMOVALS {
                // Taking misread readings out one at a time is not getting anywhere: misread
                // rows are so many that no check names just one of them. From now on every
                // reading is checked again after each setting aside, which shuffles what the
                // checks say until they narrow (what zone 0.1.0 always did, at the cost of a
                // pass over every reading each time). A fresh start goes back to the quick way.
                self.thorough = true;
                self.removals = 0;
                self.rebuild(stop);
                continue;
            }
            if !self.narrow(after_check, stop) {
                return;
            }
            after_check = false;
        }
    }

    /// One round of [`Assembler::look_for_misreads`]. True when it set something aside.
    fn narrow(&mut self, after_check: bool, stop: &dyn Fn() -> bool) -> bool {
        let words = self.slots.len().div_ceil(64).max(1);
        let open = |conflict: &Conflict, cleared: &[u64]| -> Vec<u64> {
            (0..words).map(|w| conflict.support.get(w).copied().unwrap_or(0) & !cleared.get(w).copied().unwrap_or(0)).collect()
        };
        let count = |bits: &[u64]| bits.iter().map(|w| w.count_ones() as usize).sum::<usize>();
        // An uneven check whose rows have all been vouched for was itself misread.
        // (Unless it was read again: then a row taken to be right was not, and the next checks
        // will say which.)
        let lone: Vec<usize> =
            self.conflicts.iter().filter(|c| count(&open(c, &self.cleared)) == 0 && !self.confirmed(c.reading)).map(|c| c.reading).collect();
        if !lone.is_empty() {
            for position in &lone {
                self.set_reading_aside(*position, false);
            }
            self.conflicts.retain(|c| !lone.contains(&c.reading));
            if self.conflicts.is_empty() {
                return true;
            }
        }
        // Uneven checks with the same residual add up the same errors: most often those of one
        // misread reading. For each such group, the rows every check in it names are suspects,
        // and with only one check in the group, so is its own reading. Several misread readings
        // are then found side by side, each by its own group.
        // In order of residual, so that the same codes always lead to the same choices.
        let mut groups: BTreeMap<u64, (Vec<u64>, Vec<usize>)> = BTreeMap::new();
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
        // A check that was read again is not a suspect.
        let suspects_of = |(rows, readings): &(Vec<u64>, Vec<usize>)| -> (Vec<u64>, Vec<usize>) {
            (rows.clone(), if readings.len() == 1 && !self.confirmed(readings[0]) { readings.clone() } else { Vec::new() })
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
        let sure = !settled.is_empty();
        if settled.is_empty() && fewest <= SET_ASIDE_AT_MOST && self.patience >= PATIENCE {
            settled.extend(candidates.iter().filter(|c| size(c) == fewest).take(1));
        }
        if settled.is_empty() {
            return !lone.is_empty();
        }
        let mut checks: Vec<usize> = Vec::new();
        let mut slots: Vec<usize> = Vec::new();
        for (rows, own) in settled {
            checks.extend(own);
            slots.extend((0..self.slots.len()).filter(|slot| has_bit(rows, *slot)));
        }
        checks.sort_unstable();
        checks.dedup();
        slots.sort_unstable();
        slots.dedup();
        for position in &checks {
            self.set_reading_aside(*position, sure);
        }
        for slot in &slots {
            self.set_reading_aside(self.slots[*slot], sure);
        }
        self.patience = 0;
        self.suspects = usize::MAX;
        if slots.is_empty() {
            // Only checks were misread. The rows stand; their uneven checks go.
            self.conflicts.retain(|c| !checks.contains(&c.reading));
        } else if self.thorough {
            self.rebuild(stop);
        } else {
            self.take_out(&slots);
        }
        true
    }

    fn set_reading_aside(&mut self, position: usize, sure: bool) {
        let Some(reading) = self.readings[position].take() else { return };
        self.known.insert((reading.index, reading.fingerprint), Known::SetAside { sure });
        if let Some(count) = self.caught.get_mut(&reading.index) {
            *count -= 1;
            if *count == 0 {
                self.caught.remove(&reading.index);
            }
        }
        self.set_aside += 1;
        if !sure && self.tracking {
            self.doubtful.push(reading);
        }
    }

    /// Takes back the readings set aside on doubtful grounds, as if new: they are kept again
    /// and checked again at the next rebuild.
    fn take_back_doubtful(&mut self) {
        for mut reading in std::mem::take(&mut self.doubtful) {
            let position = self.readings.len();
            self.known.insert((reading.index, reading.fingerprint), Known::Kept(position));
            *self.caught.entry(reading.index).or_default() += 1;
            reading.slot = None;
            reading.confirmed = false;
            self.readings.push(Some(reading));
        }
    }

    /// Takes the rows' misread readings out of them, by slot, and checks again the uneven
    /// checks that named them (at most [`RECHECK`], the newest): with the misread reading
    /// gone, one of them may now bring the row that left, and the rest are checks again.
    fn take_out(&mut self, slots: &[usize]) {
        self.removals += 1;
        for slot in slots {
            self.decoder.remove(*slot);
        }
        self.agreements = 0;
        self.unopened = false;
        self.ready = false;
        self.suspects = usize::MAX;
        self.patience = 0;
        let readings = &self.readings;
        let (stale, kept): (Vec<Conflict>, Vec<Conflict>) =
            std::mem::take(&mut self.conflicts).into_iter().partition(|c| readings[c.reading].is_none() || slots.iter().any(|slot| has_bit(&c.support, *slot)));
        self.conflicts = kept;
        let skip = stale.len().saturating_sub(RECHECK);
        for conflict in stale.into_iter().skip(skip) {
            self.reduce_again(conflict.reading);
        }
    }

    /// Reduces a kept reading again, from its kept body.
    fn reduce_again(&mut self, position: usize) {
        let Some(reading) = self.readings[position].as_mut() else { return };
        let (index, body) = (reading.index, std::mem::take(&mut reading.body));
        self.reduce(position, index, &body);
        if let Some(reading) = self.readings[position].as_mut() {
            reading.body = body;
        }
    }

    /// Makes the rows again from every reading still kept, in the order they arrived: the
    /// thorough way, once the quick one is not getting anywhere. It costs what the whole flash
    /// did so far, so it asks `stop` every [`STOP_EVERY`] readings, and when told to stop it
    /// leaves the rows half made (`interrupted`) and returns false.
    fn rebuild(&mut self, stop: &dyn Fn() -> bool) -> bool {
        // A rebuild stopped part way and done again is still one.
        if !self.interrupted {
            self.removals += 1;
        }
        self.interrupted = false;
        self.fresh_decoder();
        self.slots.clear();
        self.cleared.clear();
        self.conflicts.clear();
        self.agreements = 0;
        self.unopened = false;
        self.ready = false;
        self.suspects = usize::MAX;
        self.patience = 0;
        for position in 0..self.readings.len() {
            if position % STOP_EVERY == 0 && stop() {
                self.interrupted = true;
                self.ready = false;
                return false;
            }
            if let Some(reading) = self.readings[position].as_mut() {
                reading.slot = None;
            }
            self.reduce_again(position);
        }
        true
    }

    /// Drops every reading and begins again, keeping only the readings surely misread (or none,
    /// with `forget_all`). The phone keeps showing the codes, so they come round again.
    fn start_over(&mut self, forget_all: bool) {
        self.restarts += 1;
        self.removals = 0;
        self.thorough = false;
        self.uneven_seen = 0;
        self.unopened_seen = 0;
        self.fresh_decoder();
        self.readings.clear();
        self.doubtful.clear();
        self.known.retain(|_, known| !forget_all && *known == Known::SetAside { sure: true });
        self.progress_at = self.moves;
        self.interrupted = false;
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

    /// True when the rows have just become complete and their payload has not been asked for.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// The payload, once the rows have just become complete, unless `stop` says to give up
    /// (then it is still ready). Each set of rows is offered once.
    pub fn solve_unless(&mut self, stop: &dyn Fn() -> bool) -> Option<Vec<u8>> {
        if !self.ready {
            return None;
        }
        let payload = self.decoder.solve_unless(stop)?;
        self.ready = false;
        Some(payload)
    }

    /// [`Assembler::solve_unless`] without a way to stop.
    #[cfg(test)]
    pub fn take_payload(&mut self) -> Option<Vec<u8>> {
        self.solve_unless(&|| false)
    }

    /// The payload the rows solve to now, ready or not. For tools that read a whole recording
    /// at once.
    pub fn payload(&self) -> Option<Vec<u8>> {
        self.decoder.solve()
    }

    /// The payload these rows solve to did not open with any key: keep checking.
    pub fn did_not_open(&mut self) {
        self.unopened_seen += 1;
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
        // The phone shows codes 1 to 3k + 60, then starts again.
        let in_loop = 3 * self.k() as u64 + 60;
        let mut caught = 0;
        let mut last: Option<u64> = None;
        let mut missed_ranges = Vec::new();
        for index in self.caught.keys() {
            let index = u64::from(*index);
            if index >= in_loop && last.is_none_or(|last| index - last > FOLLOWS_WITHIN) {
                continue;
            }
            let next = last.map_or(0, |last| last + 1);
            if index > next && missed_ranges.len() < RANGES_NAMED {
                missed_ranges.push((next + 1, index));
            }
            caught += 1;
            last = Some(index);
        }
        let shown = last.map_or(0, |last| last + 1);
        Tally {
            caught,
            shown,
            missed: shown - caught as u64,
            missed_ranges,
            repeats: self.repeats,
            set_aside: self.set_aside,
            restarts: self.restarts,
            stalls: self.stalls,
            out_of_range: self.caught.len() - caught,
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
        assembler.take(parsed.index, parsed.body, &|| false)
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

    /// A code whose number was misread into one the phone never showed does not stretch the
    /// tally: zone 0.1.0 said "Of codes 1 to 1,509,950,965" on the Done page.
    #[test]
    fn a_misread_code_number_stays_out_of_the_tally() {
        let data = payload(6000, 11);
        let encoder = Encoder::new(0x8000_0010, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let shape = parse(&encoder.frame(0)).unwrap().shape;
        let mut assembler = Assembler::new(shape);
        for index in 0..k - 2 {
            take(&mut assembler, &encoder.frame(index));
        }
        let mut wild = encoder.frame(k);
        wild[13..17].copy_from_slice(&1_509_950_964u32.to_be_bytes());
        take(&mut assembler, &wild);
        let tally = assembler.tally();
        let (caught, shown) = (k as usize - 2, u64::from(k) - 2);
        assert_eq!((tally.caught, tally.shown, tally.missed, tally.out_of_range), (caught, shown, 0, 1), "{tally:?}");
        assert!(tally.missed_ranges.is_empty());
        // A sender that does not loop goes on past 3k + 60, in order: those codes count.
        let mut onward = Assembler::new(shape);
        let in_loop = 3 * k + 60;
        for index in (0..k - 2).chain(in_loop..in_loop + 40) {
            take(&mut onward, &encoder.frame(index));
        }
        let tally = onward.tally();
        assert_eq!((tally.shown, tally.out_of_range), (u64::from(in_loop + 40), 0), "{tally:?}");
        assert_eq!(tally.missed_ranges, vec![(u64::from(k) - 1, u64::from(in_loop))]);
        assert_eq!(tally.caught as u64 + tally.missed, tally.shown);
    }

    /// A misread plain code among the rows, and the only evidence is the phone's loop: the
    /// right code read on the next pass is a check that names just that row, which leaves two
    /// suspects, the row and the check. Read once more in another showing, the check is taken to
    /// be right, so the row is the one set aside, and the payload comes out right.
    #[test]
    fn a_code_read_right_in_two_showings_vouches_for_itself() {
        let data = payload(6000, 12);
        let encoder = Encoder::new(0x8000_0011, &data, 300).unwrap();
        let k = encoder.k() as u32;
        assert!(k as u64 >= CONFIRM_APART, "the passes are far enough apart");
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        for index in 0..k {
            let frame = encoder.frame(index);
            take(&mut assembler, &if index == 5 { misread(&frame, 40) } else { frame });
        }
        assert_ne!(assembler.take_payload().unwrap(), data);
        assembler.did_not_open();
        // The next pass: only code 6 (frame 5) brings anything, a check naming its row.
        for index in 0..k {
            take(&mut assembler, &encoder.frame(index));
        }
        assert_eq!(assembler.trouble(), Some(Trouble::Checking));
        assert_eq!(assembler.take_payload(), None);
        // The pass after: frame 5, right again, vouches for the check.
        let mut solved = None;
        for index in 0..k {
            take(&mut assembler, &encoder.frame(index));
            if let Some(payload) = assembler.take_payload() {
                solved = Some(payload);
                break;
            }
        }
        assert_eq!(solved.as_deref(), Some(data.as_slice()), "{:?}", assembler.tally());
        assert_eq!(assembler.tally().set_aside, 1);
    }

    /// Misread codes so many (one in twelve) that no check names just one misread row: the quick
    /// search gives way to the thorough one, and the right payload still comes out.
    #[test]
    fn dense_misreads_are_survived() {
        let data = payload(37_200, 13);
        let encoder = Encoder::new(0x8000_0012, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let cycle = 3 * k + 60;
        let mut dice = 0x5eed_u64;
        let mut roll = move |n: u64| {
            dice = dice.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (dice >> 33) % n
        };
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let mut thorough = false;
        let mut solved = None;
        for n in 0..cycle * 8 {
            if roll(100) < 30 {
                continue;
            }
            let frame = encoder.frame(n % cycle);
            let frame = if roll(1000) < 80 { misread(&frame, roll(300) as usize) } else { frame };
            take(&mut assembler, &frame);
            thorough |= assembler.thorough;
            if let Some(payload) = assembler.take_payload() {
                if payload == data {
                    solved = Some(payload);
                    break;
                }
                assembler.did_not_open();
            }
        }
        assert_eq!(solved.as_deref(), Some(data.as_slice()), "{:?}", assembler.tally());
        assert!(thorough, "the thorough search was needed");
    }

    /// A camera reading a flash of 100 pieces round the phone's loop, losing one code in five
    /// (the same ones each run), with a still picture the first time code number 3 is shown:
    /// `picture` (frame 2, or frames 1 to 3 for a color picture's three) stays on the screen and
    /// is read `times` times, frame 2 misread the same way each time. Returns whether the
    /// payload came out right, after how many codes read, and the tally.
    fn a_still_picture(picture: &[u32], times: usize) -> (bool, usize, Tally) {
        let data = payload(30_000, 14);
        let encoder = Encoder::new(0x8000_0013, &data, 300).unwrap();
        assert_eq!(encoder.k(), 100);
        let cycle = 3 * encoder.k() as u32 + 60;
        let mut dice = 0x5eed_u64;
        let mut roll = move |n: u64| {
            dice = dice.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (dice >> 33) % n
        };
        let bad = misread(&encoder.frame(2), 11);
        let codes = (0..cycle * 60).flat_map(|shown| {
            let index = shown % cycle;
            let still: Vec<Vec<u8>> = picture.iter().map(|i| if *i == 2 { bad.clone() } else { encoder.frame(*i) }).collect();
            if shown == picture[0] {
                (0..times).flat_map(|_| still.iter().cloned()).collect()
            } else if (shown < cycle && picture.contains(&index)) || roll(100) < 20 {
                // Shown in the still picture, or lost.
                Vec::new()
            } else {
                vec![encoder.frame(index)]
            }
        });
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        for (read, frame) in codes.enumerate() {
            take(&mut assembler, &frame);
            if let Some(payload) = assembler.take_payload() {
                if payload == data {
                    return (true, read + 1, assembler.tally());
                }
                assembler.did_not_open();
            }
        }
        (false, 0, assembler.tally())
    }

    /// The phone stalls on one picture, or the color schedule holds its first picture up for
    /// half a second, and the camera reads a misreading of it again and again. Those readings
    /// are one showing, not two, so they do not vouch for the misread code, however many there
    /// are. zone 0.1.1 as first built counted every reading toward "two showings": 17 readings
    /// of one still code (7 pictures of a color one) vouched for the misread row, the right
    /// reading of that code was then set aside as misread, then good checks after it, and the
    /// flash was never finished: after 60 loops every row was in and it was still "checking".
    #[test]
    fn a_misreading_read_again_and_again_from_a_still_screen_vouches_for_nothing() {
        for (picture, times) in [(&[2u32][..], 16), (&[2], 17), (&[2], 400), (&[1, 2, 3], 7), (&[1, 2, 3], 150)] {
            let (solved, codes, tally) = a_still_picture(picture, times);
            let context = format!("{picture:?} read {times} times: {codes} codes, {tally:?}");
            assert!(solved, "{context}");
            // About one pass of the 100 pieces, besides the still picture's readings.
            assert!(codes < picture.len() * times + 200, "{context}");
            assert_eq!((tally.set_aside, tally.stalls, tally.restarts), (1, 0, 0), "{context}");
        }
    }

    struct Got {
        solved: bool,
        /// Codes read after the first two passes.
        codes: u64,
        cycle: u64,
        tally: Tally,
        /// Bodies added into others by the reading that did the most, and readings not taken.
        most_ops: u64,
        ignored: usize,
        /// Bodies added by the readings not taken.
        ignored_ops: u64,
    }

    /// The camera catches only the plain codes on the first two passes, and code 4 (frame 3)
    /// both times with the same misreading. Then it reads everything, round the loop, at most
    /// [`GIVE_UP_LOOPS`] times.
    fn read_in_two_showings(stop: &dyn Fn() -> bool) -> Got {
        let data = payload(30_000, 15);
        let encoder = Encoder::new(0x8000_0014, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let cycle = 3 * k + 60;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let bad = misread(&encoder.frame(3), 21);
        let passes = (0..2).flat_map(|_| 0..k).map(|index| (true, if index == 3 { bad.clone() } else { encoder.frame(index) }));
        let loops = (0..GIVE_UP_LOOPS as u32 * cycle).map(|n| (false, encoder.frame(n % cycle)));
        let mut got = Got { solved: false, codes: 0, cycle: u64::from(cycle), tally: Tally::default(), most_ops: 0, ignored: 0, ignored_ops: 0 };
        for (early, frame) in passes.chain(loops) {
            got.codes += u64::from(!early);
            let parsed = parse(&frame).unwrap();
            let ops = assembler.body_ops();
            let took = assembler.take(parsed.index, parsed.body, stop);
            let spent = assembler.body_ops() - ops;
            if took == Took::Ignored {
                got.ignored += 1;
                got.ignored_ops += spent;
            }
            got.most_ops = got.most_ops.max(spent);
            if let Some(payload) = assembler.take_payload() {
                if payload == data {
                    got.solved = true;
                    break;
                }
                assert!(!early || payload != data);
                assembler.did_not_open();
            }
        }
        got.tally = assembler.tally();
        got
    }

    /// Against the odds, the same misreading of a code is read in two showings, before any check
    /// of it arrives. That vouches for the wrong row, so the right reading of that code, when it
    /// comes, looks misread, and so does every check that would say otherwise: all are set
    /// aside. Every code is then known, nothing opens, and no reading brings anything new. zone
    /// 0.1.1 as first built asked for more codes for ever. Now, a whole loop later, the flash is
    /// helped along: what was set aside on doubtful grounds is checked again with nobody
    /// vouching for anybody, and the misread row is found.
    #[test]
    fn a_misreading_read_in_two_showings_is_got_past() {
        let got = read_in_two_showings(&|| false);
        assert!(got.solved, "{:?}", got.tally);
        // Helped along once, without starting over: within the loop after the one it got stuck.
        assert_eq!((got.tally.stalls, got.tally.restarts, got.ignored), (1, 0, 0), "{:?}", got.tally);
        assert!(got.codes <= 2 * got.cycle, "{} codes", got.codes);
    }

    /// Start over asks the flash thread to stop, and making the rows again from every reading,
    /// the one long step, stops then, at the start or part way: the reading that was being taken
    /// is not, and costs a few bodies added at most. The rows are made again at the next
    /// reading, and the flash comes out the same. zone 0.1.1 as first built did not ask while
    /// making the rows again, and one code could make them again up to eight times, each costing
    /// what the whole flash had so far: about five seconds at 16,384 pieces.
    #[test]
    fn making_the_rows_again_stops_when_asked() {
        let whole = read_in_two_showings(&|| false);
        for stop_on in [1, 3] {
            let asked = std::cell::Cell::new(0);
            let got = read_in_two_showings(&|| {
                asked.set(asked.get() + 1);
                asked.get() == stop_on
            });
            assert!(got.solved, "stopped at asking {stop_on}: {:?}", got.tally);
            assert_eq!(got.ignored, 1, "stopped at asking {stop_on}");
            assert!(asked.get() > stop_on, "made again after stopping");
            // The same, but for the one reading not taken.
            assert_eq!((got.codes, got.tally.stalls, got.tally.set_aside), (whole.codes + 1, whole.tally.stalls, whole.tally.set_aside));
            // Stopped, it had made a few rows at most of what the rows made again cost in all.
            assert!(got.ignored_ops * 8 < whole.most_ops, "{} bodies before stopping, against {}", got.ignored_ops, whole.most_ops);
        }
        // Stopped a few times, at the start of making the rows again and part way, in a search
        // that makes them again many times. (In zone a stop is for good: the flash is dropped.)
        let data = payload(37_200, 13);
        let encoder = Encoder::new(0x8000_0012, &data, 300).unwrap();
        let cycle = 3 * encoder.k() as u32 + 60;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let (asked, stopped) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let stop = || {
            asked.set(asked.get() + 1);
            let now = [2, 9, 30, 60].contains(&asked.get());
            stopped.set(stopped.get() + usize::from(now));
            now
        };
        let mut dice = 0x5eed_u64;
        let mut roll = move |n: u64| {
            dice = dice.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (dice >> 33) % n
        };
        let mut solved = None;
        for n in 0..cycle * 8 {
            if roll(100) < 30 {
                continue;
            }
            let frame = encoder.frame(n % cycle);
            let frame = if roll(1000) < 80 { misread(&frame, roll(300) as usize) } else { frame };
            let parsed = parse(&frame).unwrap();
            assembler.take(parsed.index, parsed.body, &stop);
            if let Some(payload) = assembler.take_payload() {
                if payload == data {
                    solved = Some(payload);
                    break;
                }
                assembler.did_not_open();
            }
        }
        assert_eq!(solved.as_deref(), Some(data.as_slice()), "{:?}", assembler.tally());
        assert_eq!(stopped.get(), 4, "asked {} times", asked.get());
    }

    /// A flash whose codes can never come together ends, given up, within [`GIVE_UP_LOOPS`]
    /// loops of the phone's codes. Here code 4 is misread the same way every time it is shown,
    /// and so is every coded frame, each time differently, so the right payload cannot be made.
    #[test]
    fn a_flash_that_cannot_come_together_is_given_up() {
        let data = payload(6000, 16);
        let encoder = Encoder::new(0x8000_0015, &data, 300).unwrap();
        let k = encoder.k() as u32;
        let cycle = 3 * k + 60;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let mut codes = 0u64;
        for n in 0..cycle * 2 * GIVE_UP_LOOPS as u32 {
            let index = n % cycle;
            let frame = match index {
                3 => misread(&encoder.frame(3), 7),
                i if i >= k => misread(&encoder.frame(i), (n as usize * 31) % 300),
                i => encoder.frame(i),
            };
            codes += 1;
            take(&mut assembler, &frame);
            if let Some(payload) = assembler.take_payload() {
                assert_ne!(payload, data);
                assembler.did_not_open();
            }
            if assembler.gave_up() {
                break;
            }
        }
        assert!(assembler.gave_up(), "{:?}", assembler.tally());
        assert!(codes <= GIVE_UP_LOOPS * u64::from(cycle), "{codes} codes, {:?}", assembler.tally());
        assert_eq!(take(&mut assembler, &encoder.frame(5)), Took::Ignored);
        assert_eq!(assembler.take_payload(), None);
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

/// A camera reading a flash, for the tests and timings: the phone shows frames 0, 1, 2, ...
/// round its loop of `3k + 60`, the camera loses 30% of them, and `misreads` of the first `k`
/// codes it reads come out with one byte changed. The seal is played by comparing with the
/// payload.
#[cfg(test)]
pub(crate) mod simulate {
    use super::*;
    use crate::flash::fountain::{Encoder, HEADER_LEN, parse};
    use std::time::{Duration, Instant};

    pub struct Run {
        pub solved: bool,
        pub codes: usize,
        /// Bodies added into others, in all, and by the reading that did the most.
        pub body_ops: u64,
        pub most_body_ops: u64,
        /// Uneven checks and payloads that did not open, since the last start.
        pub uneven: usize,
        pub unopened: usize,
        pub elapsed: Duration,
        pub longest: Duration,
        pub tally: Tally,
    }

    struct Dice(u64);

    impl Dice {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    pub fn payload(k: usize, chunk: usize) -> Vec<u8> {
        let mut dice = Dice(k as u64);
        (0..k * chunk).map(|_| dice.next() as u8).collect()
    }

    /// Reads `data` as a flash of `chunk` byte pieces until it comes out right, or `limit`.
    pub fn run(data: &[u8], chunk: usize, misreads: usize, limit: Duration) -> Run {
        let encoder = Encoder::new(0x8765_4321, data, chunk).unwrap();
        let k = encoder.k();
        let mut dice = Dice(7 + misreads as u64);
        let mut bad = std::collections::HashSet::new();
        while bad.len() < misreads {
            bad.insert((dice.next() % k as u64) as usize);
        }
        let cycle = 3 * k as u32 + 60;
        let mut assembler = Assembler::new(parse(&encoder.frame(0)).unwrap().shape);
        let started = Instant::now();
        let mut run = Run { solved: false, codes: 0, body_ops: 0, most_body_ops: 0, uneven: 0, unopened: 0, elapsed: Duration::ZERO, longest: Duration::ZERO, tally: Tally::default() };
        for n in 0..cycle * 8 {
            if dice.next() % 100 < 30 {
                continue;
            }
            let mut frame = encoder.frame(n % cycle);
            if bad.contains(&run.codes) {
                let at = HEADER_LEN + (dice.next() % chunk as u64) as usize;
                frame[at] ^= 1 << (dice.next() % 8);
            }
            run.codes += 1;
            let frame = parse(&frame).unwrap();
            let (ops, at) = (assembler.body_ops(), Instant::now());
            assembler.take(frame.index, frame.body, &|| false);
            let payload = assembler.take_payload();
            run.longest = run.longest.max(at.elapsed());
            run.most_body_ops = run.most_body_ops.max(assembler.body_ops() - ops);
            if let Some(payload) = payload {
                if payload == data {
                    run.solved = true;
                    break;
                }
                assembler.did_not_open();
            }
            if started.elapsed() > limit {
                break;
            }
        }
        run.elapsed = started.elapsed();
        run.body_ops = assembler.body_ops();
        run.tally = assembler.tally();
        run.uneven = assembler.uneven_seen;
        run.unopened = assembler.unopened_seen;
        run
    }
}

/// The times quoted in PROTOCOL.md: `cargo test --release flash_assembly_times -- --ignored
/// --nocapture`. `ZONE_TIMES_K` and `ZONE_TIMES_MISREADS` pick the sizes and misread counts
/// (default 1024,4096,16384 and 0,4,8), `ZONE_TIMES_LIMIT` the seconds allowed for each.
#[cfg(test)]
#[test]
#[ignore]
fn flash_assembly_times() {
    let list = |name: &str, default: &str| -> Vec<usize> { std::env::var(name).unwrap_or(default.into()).split(',').map(|s| s.trim().parse().unwrap()).collect() };
    let limit = std::time::Duration::from_secs(std::env::var("ZONE_TIMES_LIMIT").ok().and_then(|s| s.parse().ok()).unwrap_or(1800));
    let chunk = 900;
    println!("| Pieces | Misread codes | Codes read | Set aside | All the work | Longest single step | Bodies added |");
    println!("| --- | --- | --- | --- | --- | --- | --- |");
    for k in list("ZONE_TIMES_K", "1024,4096,16384") {
        let data = simulate::payload(k, chunk);
        for misreads in list("ZONE_TIMES_MISREADS", "0,4,8") {
            let run = simulate::run(&data, chunk, misreads, limit);
            println!(
                "| {k} | {misreads} | {} ({:.2} k) | {} | {:.2?}{} | {:.2?} | {} |",
                run.codes,
                run.codes as f64 / k as f64,
                run.tally.set_aside,
                run.elapsed,
                if run.solved { "" } else { " (not finished)" },
                run.longest,
                run.body_ops
            );
        }
    }
}

#[cfg(test)]
mod work {
    use super::simulate;
    use std::time::Duration;

    /// Bounds on the work of putting a flash together, counted in bodies added into others,
    /// so that a slow or busy computer cannot make them fail. A misread code costs one pass
    /// over the rows to take it out and a few dozen checks to look at again, never a pass over
    /// every reading: that is the work of the whole flash, which zone 0.1.0 did again for each
    /// misread code, in one step that grew with the square of the flash's size.
    #[test]
    fn a_misread_costs_a_pass_over_the_rows_not_the_whole_flash() {
        let k = 4096;
        let data = simulate::payload(k, 64);
        let clean = simulate::run(&data, 64, 0, Duration::from_secs(600));
        let misread = simulate::run(&data, 64, 8, Duration::from_secs(600));
        assert!(clean.solved && misread.solved);
        assert!(misread.tally.set_aside >= 8, "{:?}", misread.tally);
        let square = (k * k) as u64;
        // A clean flash: about k * k / 10 bodies in all, and the largest step is the solve at
        // the end, about a quarter of that.
        assert!(clean.body_ops < square / 8, "{} bodies, {:.3} k^2", clean.body_ops, clean.body_ops as f64 / square as f64);
        assert!(clean.most_body_ops < square / 25, "{} bodies in one step", clean.most_body_ops);
        // Eight misread codes not much more, and no step much larger than that solve.
        assert!(misread.body_ops < clean.body_ops * 5 / 2, "{} bodies against {}", misread.body_ops, clean.body_ops);
        assert!(misread.most_body_ops < square / 16, "{} bodies in one step, {:.3} k^2", misread.most_body_ops, misread.most_body_ops as f64 / square as f64);
        // And the clock, with room for any computer: an unoptimized build takes about two
        // seconds here, and zone 0.1.0 optimized took six.
        assert!(misread.elapsed < Duration::from_secs(120), "{:?}", misread.elapsed);
    }
}

