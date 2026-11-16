//! The computer's side of a flash: frames in, items in the library out.
//!
//! [`Receiver`] collects frames, whichever way they arrive (pictures from the local page, or
//! frame bytes directly), one [`Assembler`] per flash. When a flash is complete its payload is
//! handed to [`import`], which finds the key that opens it, merges the items like Link items,
//! and reads them back to check them, all in one transaction.
//!
//! A complete payload that no key opens is not the end of the flash: a misread code gives a
//! wrong payload, which no key opens either. The receiver keeps reading. Uneven checks lead the
//! assembler to the misread code, which is set aside, and the payload is solved again. If
//! instead every later code agrees, the payload is what the phone showed, sealed for another
//! computer, and only then does the flash fail, saying so.
//!
//! A stored flash is recorded in the library by its header (and the digest of its items), so
//! its frames, still on a phone's screen, never store it again: not after the page starts
//! over and not after zone restarts. Storing a flash again would put back the phone's older
//! version of anything Link has updated since.
//!
//! Nothing here logs or returns any item's text.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::assemble::{Assembler, Tally, Took, Trouble};
use super::check::Check;
use super::fountain::{self, Decoder, Shape};
use super::seal;
use crate::db::{Db, Delivery, FlashKeyOwner, StoredFlash};
use crate::item::Carrier;

/// Flashes followed at once. More than one only happens when a second phone, or a second
/// flash from the same phone, shows up before the first is finished, or when a misread code
/// names a flash that does not exist.
const MAX_SESSIONS: usize = 3;
/// Items stored per batch, inside the one transaction of a flash.
const BATCH: usize = 500;
/// The page is told which chunks are known, one tile each, up to this many chunks (about
/// 4.6 MB of sealed payload). A larger flash reports counts only.
const MAX_TILES: usize = 16_384;
/// Flashes that failed for good remembered after their session is dropped, so that one still
/// on a phone's screen does not take the page over again.
const FAILED_REMEMBERED: usize = 64;
/// After the library could not take an opened flash, how long to wait before trying again.
pub const STORE_RETRY: Duration = Duration::from_secs(3);

/// What became of a completed flash.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Stored(Box<Stored>),
    /// This library stored this very flash before. Nothing was stored again.
    AlreadyStored(Box<StoredFlash>),
    /// Why nothing was stored, for good, in words for the person holding the phone.
    Failed(String),
    /// No key opened the payload. Either a code was misread or the flash was sealed for
    /// another computer: the receiver keeps reading codes to tell which (see the top of this
    /// file). The words are what to say if it turns out to be the second.
    NotOpened(String),
    /// It opened, but the library could not take it just now. Nothing of it was kept, and the
    /// receiver tries again.
    NotStored(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stored {
    pub phone_name: Option<String>,
    /// Items in the flash that could be read.
    pub items: usize,
    pub inserted: usize,
    pub replaced: usize,
    pub unchanged: usize,
    /// Of the inserted and replaced, how many were deletions.
    pub deletions: usize,
    /// Items without an id, kind or time.
    pub rejected: usize,
    /// True when this flash also paired the phone (it was sealed with a pairing offer's key).
    pub paired: bool,
    /// The id of the phone's row among the paired phones.
    pub device_id: Option<String>,
    /// The id of the pairing offer this flash used up, when it paired the phone.
    pub offer: Option<String>,
    /// Of the inserted, how many arrived already deleted.
    pub tombstones: usize,
    /// Of the replaced, how many took an item that was here out of the library.
    pub removed: usize,
    /// The items checked against the phone's summary and read back from the library.
    pub check: Check,
}

impl Stored {
    /// What goes in the feed for this flash: names and counts.
    pub fn event_fields(&self, bytes: usize, color: bool) -> Value {
        json!({
            "device": self.phone_name,
            "device_id": self.device_id,
            "items": self.items,
            "new": self.inserted - self.tombstones,
            "changed": self.replaced - (self.deletions - self.tombstones),
            "deleted": self.removed,
            "unchanged": self.unchanged,
            "rejected": self.rejected,
            "paired": self.paired,
            "bytes": bytes,
            "color": color,
            "checked": self.check.all_checked(),
            "declared": self.check.declared.as_ref().and_then(|d| d.items),
            "duplicates": self.check.duplicates,
            "check_problem": if self.check.problems.is_empty() { Value::Null } else { json!(self.check.problems.join(" ")) },
        })
    }
}

/// Puts what became of a completed flash in the feed: the flash itself, and the pairing when
/// it also paired the phone. `bytes` is the size of the sealed payload. A flash that is still
/// being checked or stored is not in the feed yet, and one stored before is not in it twice.
pub fn record_events(db: &Db, outcome: &Outcome, bytes: usize, color: bool) {
    match outcome {
        Outcome::Stored(stored) => {
            if stored.paired {
                crate::events::record(db, "paired", json!({ "device": stored.phone_name, "device_id": stored.device_id, "via": "flash", "offer": stored.offer }));
            }
            crate::events::record(db, "flash_received", stored.event_fields(bytes, color));
        }
        Outcome::Failed(reason) => crate::events::record(db, "flash_failed", json!({ "reason": reason, "bytes": bytes })),
        Outcome::AlreadyStored(_) | Outcome::NotOpened(_) | Outcome::NotStored(_) => {}
    }
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::Stored(s) => json!({
                "ok": true,
                "phone": s.phone_name,
                "items": s.items,
                "new": s.inserted,
                "changed": s.replaced,
                "unchanged": s.unchanged,
                "deletions": s.deletions,
                "rejected": s.rejected,
                "paired": s.paired,
                "check": s.check.to_json(),
            }),
            Outcome::AlreadyStored(earlier) => json!({
                "ok": true,
                "already_stored": true,
                "phone": earlier.phone_name,
                "items": earlier.items,
                "stored_at": earlier.stored_at,
            }),
            Outcome::Failed(reason) => json!({ "ok": false, "error": reason }),
            Outcome::NotOpened(reason) => json!({ "ok": false, "error": reason, "checking": true }),
            Outcome::NotStored(reason) => json!({ "ok": false, "error": reason, "retrying": true }),
        }
    }
}

struct Session {
    /// Dropped once the flash is finished, to give its memory back.
    assembler: Option<Assembler>,
    k: usize,
    rank: usize,
    /// Different code numbers caught, as of the last frame.
    caught: usize,
    /// What the code numbers said when the assembler was dropped.
    tally: Tally,
    trouble: Option<Trouble>,
    /// The chunks held and solved, as the page gets them, worked out when the rows change.
    tiles: Option<Tiles>,
    /// Order of arrival of the last frame that was not of a finished flash, to tell which
    /// session is the current one.
    touched: u64,
    /// Stored, stored before, or failed for good.
    outcome: Option<Outcome>,
    /// A payload that opened but that the library could not take yet, and when it was tried.
    unstored: Option<(Vec<u8>, Instant)>,
    /// What to say if the payload that did not open turns out to be sealed for another computer.
    unopened: Option<String>,
    /// Codes of this flash misread so that their header named another one (see [`near`]).
    misnamed: usize,
}

impl Session {
    fn reading(shape: Shape, touched: u64) -> Session {
        let assembler = Assembler::new(shape);
        Session {
            k: assembler.k(),
            rank: 0,
            caught: 0,
            tally: Tally::default(),
            trouble: None,
            tiles: Tiles::of(assembler.decoder()),
            assembler: Some(assembler),
            touched,
            outcome: None,
            unstored: None,
            unopened: None,
            misnamed: 0,
        }
    }

    fn tally(&self) -> Tally {
        self.assembler.as_ref().map_or_else(|| self.tally.clone(), Assembler::tally)
    }

    /// Done with: the outcome is kept, the assembler's memory given back.
    fn close(&mut self, outcome: Outcome) {
        if let Some(assembler) = self.assembler.take() {
            self.tally = assembler.tally();
        }
        self.outcome = Some(outcome);
        self.unstored = None;
        self.trouble = None;
        self.rank = self.k;
        self.tiles = Tiles::all(self.k);
    }
}

/// True when one header is a misreading of the other: the same chunk size, and the session,
/// the payload length and the chunk count differ by two bits at most in all. Two flashes made
/// apart have random sessions, about 16 bits apart.
fn near(a: &Shape, b: &Shape) -> bool {
    a.chunk == b.chunk && (a.session ^ b.session).count_ones() + (a.total_len ^ b.total_len).count_ones() + (a.k ^ b.k).count_ones() <= 2
}

/// The header of a stored flash.
fn shape_of(flash: &StoredFlash) -> Shape {
    Shape { session: flash.session, total_len: flash.total_len, k: flash.k, chunk: flash.chunk }
}

/// Which chunks are known, as hexadecimal bit sets: chunk `n` is bit `n % 8` (the lowest
/// first) of byte `n / 8`.
#[derive(Clone, Debug)]
struct Tiles {
    held: String,
    solved: String,
    solved_count: usize,
}

impl Tiles {
    fn hex(words: &[u64], k: usize) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let bytes = k.div_ceil(8);
        let mut out = String::with_capacity(bytes * 2);
        for b in 0..bytes {
            let byte = (words[b / 8] >> (8 * (b % 8))) as u8;
            out.push(DIGITS[usize::from(byte >> 4)] as char);
            out.push(DIGITS[usize::from(byte & 15)] as char);
        }
        out
    }

    fn of(decoder: &Decoder) -> Option<Tiles> {
        let k = decoder.k();
        if k > MAX_TILES {
            return None;
        }
        let (held, solved) = decoder.chunk_states();
        let solved_count = solved.iter().map(|w| w.count_ones() as usize).sum();
        Some(Tiles { held: Tiles::hex(&held, k), solved: Tiles::hex(&solved, k), solved_count })
    }

    /// Every chunk known: a finished flash.
    fn all(k: usize) -> Option<Tiles> {
        if k > MAX_TILES {
            return None;
        }
        let mut words = vec![u64::MAX; k.div_ceil(64)];
        if !k.is_multiple_of(64) {
            *words.last_mut().expect("k is at least 1") = (1u64 << (k % 64)) - 1;
        }
        let hex = Tiles::hex(&words, k);
        Some(Tiles { held: hex.clone(), solved: hex, solved_count: k })
    }
}

/// What the last camera picture held, for the page to react to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PictureRead {
    /// QR codes found in it, of any kind.
    pub codes: usize,
    /// Of those, frames of a flash.
    pub frames: usize,
    /// Of those, frames that brought the decoder a new row.
    pub useful: usize,
    /// True when it was a color picture, read channel by channel.
    pub color: bool,
}

/// What one frame did.
pub enum Step {
    /// Not a flash frame at all (some other QR code), or a damaged one.
    Ignored,
    /// Taken. The flash is not complete yet. `useful` when it brought a new row.
    Progress { useful: bool },
    /// A frame of a flash that is already finished: the phone is still showing it.
    AlreadyDone,
    /// From [`Receiver::take`]: the rows of this flash are complete, and the caller rebuilds
    /// the payload with [`Receiver::solve`] (which takes a while for a large flash).
    Ready { shape: Shape },
    /// The flash is complete (or a payload that could not be stored is due another try).
    /// The payload is ready for [`import`], and the caller reports back with
    /// [`Receiver::finish`].
    Complete { shape: Shape, payload: Vec<u8> },
    /// Every code agrees, and still no key opens the flash: it was sealed for another
    /// computer. The flash has failed for good; the caller records it.
    GaveUp { reason: String, bytes: usize },
}

#[derive(Default)]
pub struct Receiver {
    sessions: HashMap<Shape, Session>,
    /// Flashes this library stored, by header: the ones it had when the receiver started and
    /// every one stored since. Kept when the page starts over.
    stored: HashMap<Shape, StoredFlash>,
    /// Flashes that failed for good since the page last started over, newest last.
    failed: VecDeque<Shape>,
    clock: u64,
    /// Pictures looked at, and how many of them had a code in them.
    pub pictures: u64,
    pub pictures_with_code: u64,
    /// Pictures read as color pictures, channel by channel.
    pub color_pictures: u64,
    /// Frames of a flash read, in any picture, and how many of them were useful.
    pub frames_read: u64,
    pub frames_useful: u64,
    /// What the most recent picture held.
    pub last_picture: Option<PictureRead>,
    /// How long to wait before storing an opened flash again ([`STORE_RETRY`]; tests shorten it).
    store_retry: Option<Duration>,
}

impl Receiver {
    #[cfg(test)]
    pub fn new() -> Receiver {
        Receiver::default()
    }

    /// A receiver that knows the flashes a library has stored ([`Db::stored_flashes`]).
    pub fn with_stored(stored: impl IntoIterator<Item = StoredFlash>) -> Receiver {
        Receiver { stored: stored.into_iter().map(|flash| (shape_of(&flash), flash)).collect(), ..Receiver::default() }
    }

    /// Makes room for one more flash. A flash that has caught one code at most goes first
    /// (often one that only a misread code named), then a finished one, then the one heard
    /// from longest ago.
    fn make_room(&mut self) {
        if self.sessions.len() < MAX_SESSIONS {
            return;
        }
        let barely = |s: &Session| s.outcome.is_none() && s.caught <= 1;
        let victim = self
            .sessions
            .iter()
            .filter(|(_, s)| barely(s))
            .min_by_key(|(_, s)| s.touched)
            .or_else(|| self.sessions.iter().filter(|(_, s)| s.outcome.is_some()).min_by_key(|(_, s)| s.touched))
            .or_else(|| self.sessions.iter().min_by_key(|(_, s)| s.touched))
            .map(|(shape, _)| *shape);
        if let Some(shape) = victim {
            self.sessions.remove(&shape);
        }
    }

    fn remember_failed(&mut self, shape: Shape) {
        if !self.failed.contains(&shape) {
            if self.failed.len() == FAILED_REMEMBERED {
                self.failed.pop_front();
            }
            self.failed.push_back(shape);
        }
    }

    /// True when some flash is being read: it has caught a few codes and is not finished.
    fn reading_one(&self) -> bool {
        self.sessions.values().any(|s| s.outcome.is_none() && s.caught >= 2)
    }

    /// Takes the bytes of one QR code. When they complete a flash, the payload is rebuilt
    /// before this returns: see [`Receiver::take`] for doing that separately.
    #[cfg(test)]
    pub fn add(&mut self, raw: &[u8]) -> Step {
        match self.take(raw) {
            Step::Ready { shape } => match self.solve(shape, &|| false) {
                Some(payload) => Step::Complete { shape, payload },
                None => Step::Progress { useful: true },
            },
            step => step,
        }
    }

    /// Takes the bytes of one QR code. A frame that completes the rows of a flash gives
    /// [`Step::Ready`]: the payload is then rebuilt with [`Receiver::solve`].
    pub fn take(&mut self, raw: &[u8]) -> Step {
        let frame = match fountain::parse(raw) {
            Ok(frame) => frame,
            Err(_) => return Step::Ignored,
        };
        self.clock += 1;
        let clock = self.clock;
        if !self.sessions.contains_key(&frame.shape) {
            if let Some(earlier) = self.stored.get(&frame.shape).cloned() {
                // Stored before, perhaps before the page started over or zone restarted: say
                // so, without taking the page from a flash being read.
                self.frames_read += 1;
                self.make_room();
                let touched = if self.reading_one() { 0 } else { clock };
                let mut session = Session::reading(frame.shape, touched);
                session.close(Outcome::AlreadyStored(Box::new(earlier)));
                self.sessions.insert(frame.shape, session);
                return Step::AlreadyDone;
            }
            if self.failed.contains(&frame.shape) {
                self.frames_read += 1;
                return Step::AlreadyDone;
            }
            // A code of a flash already being read (or finished), whose header was misread.
            let parent = self
                .sessions
                .iter_mut()
                .filter(|(shape, s)| near(shape, &frame.shape) && (s.outcome.is_some() || s.caught >= 2))
                .max_by_key(|(_, s)| s.caught)
                .map(|(_, s)| s);
            if let Some(parent) = parent {
                self.frames_read += 1;
                parent.misnamed += 1;
                return if parent.outcome.is_some() { Step::AlreadyDone } else { Step::Progress { useful: false } };
            }
            if self.failed.iter().chain(self.stored.keys()).any(|shape| near(shape, &frame.shape)) {
                self.frames_read += 1;
                return Step::AlreadyDone;
            }
            self.make_room();
            self.sessions.insert(frame.shape, Session::reading(frame.shape, clock));
        }
        self.frames_read += 1;
        let session = self.sessions.get_mut(&frame.shape).expect("just inserted");
        // A finished flash still on the phone's screen changes nothing, not even which flash
        // the page shows.
        if session.outcome.is_some() {
            return Step::AlreadyDone;
        }
        session.touched = clock;
        if let Some((payload, tried)) = &session.unstored {
            if tried.elapsed() >= self.store_retry.unwrap_or(STORE_RETRY) {
                return Step::Complete { shape: frame.shape, payload: payload.clone() };
            }
            return Step::Progress { useful: false };
        }
        let Some(assembler) = session.assembler.as_mut() else {
            return Step::AlreadyDone;
        };
        let set_aside = assembler.set_aside_count();
        let took = assembler.take(frame.index, frame.body);
        let useful = took == Took::New { useful: true };
        if useful {
            self.frames_useful += 1;
        }
        session.caught = assembler.caught_count();
        session.trouble = assembler.trouble();
        // Rows come with useful frames, and change when a misread one is set aside.
        if useful || assembler.rank() != session.rank || assembler.set_aside_count() != set_aside {
            session.tiles = Tiles::of(assembler.decoder());
        }
        session.rank = assembler.rank();
        if assembler.is_ready() {
            return Step::Ready { shape: frame.shape };
        }
        if assembler.confirmed_unopenable() {
            let reason = session.unopened.take().unwrap_or_else(not_paired_words);
            session.close(Outcome::Failed(reason.clone()));
            self.remember_failed(frame.shape);
            return Step::GaveUp { reason, bytes: frame.shape.total_len as usize };
        }
        Step::Progress { useful }
    }

    /// Rebuilds the payload of a flash whose rows are complete ([`Step::Ready`]), for
    /// [`import`]. None when `stop` said to give up first (the flash then stays ready), or
    /// when the flash is not waiting for this.
    pub fn solve(&mut self, shape: Shape, stop: &dyn Fn() -> bool) -> Option<Vec<u8>> {
        self.sessions.get_mut(&shape)?.assembler.as_mut()?.solve_unless(stop)
    }

    /// Records what became of a complete payload: final, or to be checked, or to be stored
    /// again shortly.
    pub fn finish(&mut self, shape: Shape, payload: &[u8], outcome: Outcome) {
        match &outcome {
            Outcome::Stored(stored) => {
                self.stored.insert(shape, stored_flash(&shape, stored, crate::paths::now_unix()));
            }
            Outcome::AlreadyStored(earlier) => {
                self.stored.insert(shape, (**earlier).clone());
            }
            _ => {}
        }
        let Some(session) = self.sessions.get_mut(&shape) else { return };
        match outcome {
            Outcome::NotOpened(reason) => {
                session.unopened = Some(reason);
                if let Some(assembler) = session.assembler.as_mut() {
                    assembler.did_not_open();
                    session.trouble = assembler.trouble();
                }
            }
            Outcome::NotStored(_) => session.unstored = Some((payload.to_vec(), Instant::now())),
            outcome => {
                let failed = matches!(outcome, Outcome::Failed(_));
                session.close(outcome);
                if failed {
                    self.remember_failed(shape);
                }
            }
        }
    }

    /// Forgets everything but the flashes this library stored, for the page's "start over".
    pub fn reset(&mut self) {
        *self = Receiver { stored: std::mem::take(&mut self.stored), store_retry: self.store_retry, ..Receiver::default() };
    }

    /// The flash the page shows: the most recent one that is finished or has more than one
    /// code caught, so a stray code from a misreading does not take over; else the most
    /// recent of any.
    fn current(&self) -> Option<(&Shape, &Session)> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.outcome.is_some() || s.caught >= 2 || s.k == 1)
            .max_by_key(|(_, s)| s.touched)
            .or_else(|| self.sessions.iter().max_by_key(|(_, s)| s.touched))
    }

    /// Where the most recent flash stands, for the page.
    ///
    /// `blocks` has one bit per chunk of the payload (see [`Tiles`]): `held` where the decoder
    /// has a row, `solved` where it can already rebuild the chunk. It is null for a flash of
    /// more than 16384 chunks. `codes` is the tally of code numbers (see [`Tally`]), `needed`
    /// the pieces still missing and `trouble` what is being sorted out: `misread` while a
    /// misread code is looked for, `checking` while a complete flash that did not open is
    /// checked. `last` is what the most recent camera picture held.
    pub fn status(&self) -> Value {
        let pictures = json!({
            "seen": self.pictures,
            "with_code": self.pictures_with_code,
            "frames": self.frames_read,
            "useful": self.frames_useful,
            "color": self.color_pictures,
        });
        let last = self.last_picture.map(|p| json!({ "codes": p.codes, "frames": p.frames, "useful": p.useful, "color": p.color }));
        let Some((shape, session)) = self.current() else {
            return json!({ "state": "waiting", "pictures": pictures, "last": last });
        };
        // Storing: an opened flash waiting for the library, or a complete one being opened.
        let storing = session.unstored.is_some() || (session.rank == session.k && session.trouble.is_none());
        let state = match (&session.outcome, storing) {
            (Some(Outcome::Stored(_) | Outcome::AlreadyStored(_)), _) => "done",
            (Some(_), _) => "failed",
            (None, true) => "storing",
            (None, false) => "receiving",
        };
        let tally = session.tally();
        json!({
            "state": state,
            "session": format!("{:08x}", shape.session),
            "name": flash_name(shape.session),
            "frames": tally.caught,
            "rows": session.rank,
            "k": session.k,
            "needed": session.k - session.rank,
            "bytes": shape.total_len,
            "solved": session.tiles.as_ref().map(|t| t.solved_count),
            "blocks": session.tiles.as_ref().map(|t| json!({ "held": t.held, "solved": t.solved })),
            "codes": {
                "caught": tally.caught,
                "shown": tally.shown,
                "missed": tally.missed,
                "missed_ranges": tally.missed_ranges.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
                "repeats": tally.repeats,
                "set_aside": tally.set_aside + tally.out_of_range + session.misnamed,
            },
            "trouble": session.trouble.map(Trouble::as_str),
            "result": session.outcome.as_ref().map(Outcome::to_json),
            "pictures": pictures,
            "last": last,
        })
    }
}

/// The name a flash goes by on the phone and on the page: its session as eight hexadecimal
/// digits in two groups of four, `8f3a 21c4`.
pub fn flash_name(session: u32) -> String {
    format!("{:04x} {:04x}", session >> 16, session & 0xffff)
}

fn not_paired_words() -> String {
    "Every code agrees, but the flash is not from a phone paired with this computer: it is sealed with a key this computer does not have. Choose Pair a phone in your library, scan the code with the phone, then flash again.".to_string()
}

/// What the library keeps of a stored flash.
fn stored_flash(shape: &Shape, stored: &Stored, now: f64) -> StoredFlash {
    StoredFlash {
        session: shape.session,
        total_len: shape.total_len,
        k: shape.k,
        chunk: shape.chunk,
        digest: stored.check.digest.clone(),
        device_id: stored.device_id.clone(),
        phone_name: stored.phone_name.clone(),
        items: stored.items,
        stored_at: now,
    }
}

/// Why a complete payload was not stored.
enum Problem {
    /// This library stored this flash before.
    Repeat(StoredFlash),
    /// No key opened it.
    NotOpened(String),
    /// The library could not take it.
    Storage(anyhow::Error),
    /// Anything else: final.
    Final(anyhow::Error),
}

/// Opens a complete payload with whichever known key fits, merges its items and reads them
/// back, and records the flash as stored, all in one transaction: when anything fails, the
/// library is as it was, pairing included, so trying again later is the same as trying for
/// the first time. `shape` is the flash's header; a flash with a header this library has
/// stored before is not stored again.
///
/// The keys tried are each paired phone's, then each pairing offer's that is still valid. An
/// offer's key pairs the phone and uses the offer up.
pub fn import(db: &mut Db, shape: &Shape, sealed: &[u8], now: f64) -> Outcome {
    let result = match db.atomically(|db| import_inner(db, shape, sealed, now)) {
        Ok(result) => result,
        Err(error) => Err(Problem::Storage(error)),
    };
    match result {
        Ok(stored) => Outcome::Stored(Box::new(stored)),
        Err(Problem::Repeat(earlier)) => Outcome::AlreadyStored(Box::new(earlier)),
        Err(Problem::NotOpened(reason)) => Outcome::NotOpened(reason),
        Err(Problem::Storage(error)) => Outcome::NotStored(format!("The library could not take the flash just now ({error:#}). Trying again.")),
        Err(Problem::Final(error)) => Outcome::Failed(format!("{error:#}")),
    }
}

/// Opens a payload with one given key and reads it, without storing anything.
pub fn open_with_key(key: &[u8; 32], sealed: &[u8]) -> Result<seal::Flash> {
    let Some(packed) = seal::unseal(key, sealed) else {
        bail!("this key does not open the flash");
    };
    seal::parse(&seal::inflate(&packed)?)
}

fn import_inner(db: &mut Db, shape: &Shape, sealed: &[u8], now: f64) -> Result<Stored, Problem> {
    if let Some(earlier) = db.stored_flash(shape.session, shape.total_len, shape.k, shape.chunk).map_err(Problem::Storage)? {
        return Err(Problem::Repeat(earlier));
    }
    let keys = db.flash_keys(now).map_err(Problem::Storage)?;
    if keys.is_empty() {
        return Err(Problem::Final(anyhow::anyhow!(
            "No phone is paired with this computer, so the flash cannot be opened. Choose Pair a phone in your library, scan the code with the phone, then flash again."
        )));
    }
    let Some((packed, owner)) = keys.iter().find_map(|k| seal::unseal(&k.key, sealed).map(|packed| (packed, &k.owner))) else {
        return Err(Problem::NotOpened(not_paired_words()));
    };
    let flash = seal::inflate(&packed).and_then(|json| seal::parse(&json)).map_err(Problem::Final)?;
    let (device, paired, offer) = match owner {
        FlashKeyOwner::Device(endpoint_id) => {
            db.touch_device(endpoint_id, &flash.phone, now).map_err(Problem::Storage)?;
            (endpoint_id.clone(), false, None)
        }
        FlashKeyOwner::Offer(hash) => match db.pair_by_flash(hash, &flash.phone, now).map_err(Problem::Storage)? {
            Some(device) => (device, true, Some(crate::db::offer_id_of_hash(hash))),
            None => return Err(Problem::Final(anyhow::anyhow!("The pairing code this flash was made with ran out. Choose Pair a phone in your library for a new one, scan it with the phone, then flash again."))),
        },
    };
    let delivery = Delivery { carrier: Carrier::Flash, device_id: flash.phone.id.as_deref(), received_at: now };
    let mut stored = Stored {
        phone_name: flash.phone.name.clone(),
        items: flash.items.len(),
        rejected: flash.rejected,
        paired,
        device_id: Some(device.clone()),
        offer,
        check: flash.check.clone(),
        ..Stored::default()
    };
    // An empty flash still counts as a sync.
    let empty: &[crate::item::Item] = &[];
    let batches: Vec<&[crate::item::Item]> = if flash.items.is_empty() { vec![empty] } else { flash.items.chunks(BATCH).collect() };
    let mut changed = 0;
    for batch in batches {
        let (applied, arrived) = db.apply_items_noting(batch, &delivery).map_err(Problem::Storage)?;
        // Clips go in the feed. A Flash never puts one on the clipboard.
        let arrival = crate::clip::Arrival { carrier: Carrier::Flash, device: flash.phone.name.as_deref(), device_id: Some(&device), now };
        crate::clip::arrived(db, batch, &arrived, &arrival);
        stored.inserted += applied.inserted;
        stored.replaced += applied.replaced;
        stored.unchanged += applied.unchanged;
        stored.deletions += applied.deletions;
        stored.tombstones += applied.tombstones;
        stored.removed += applied.removed;
        changed += applied.changed();
    }
    db.record_delivery(&device, changed, Some("flash"), now).map_err(Problem::Storage)?;
    db.record_flash(&stored_flash(shape, &stored, now)).map_err(Problem::Storage)?;
    stored.check.read_back(&flash.items, |id| db.get(id).ok().flatten());
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PhoneInfo;
    use crate::fakephone::synthetic_items;
    use crate::flash::fountain::Encoder;

    const SECRET: &str = "abababababababababababababababababababababababababababababababab";

    fn key_of(secret: &str) -> [u8; 32] {
        seal::key_bytes(&crate::flash::flash_key(secret).unwrap()).unwrap()
    }

    fn phone(id: &str) -> PhoneInfo {
        PhoneInfo { id: Some(id.to_string()), name: Some("Test phone".to_string()), app: Some("1.0.3 (1)".to_string()) }
    }

    /// Imports a payload as a flash of its own: a header made up from it, so every payload
    /// is a new flash.
    fn import_once(db: &mut Db, payload: &[u8], now: f64) -> Outcome {
        let shape = Shape { session: fountain::fingerprint(payload) as u32, total_len: payload.len() as u32, k: payload.len().div_ceil(300) as u16, chunk: 300 };
        import(db, &shape, payload, now)
    }

    fn sealed(secret: &str, id: &str, items: &[Value]) -> Vec<u8> {
        seal::seal(&key_of(secret), &seal::flash_json(&phone(id), items)).unwrap()
    }

    /// Feeds frames until the flash completes, skipping every third one.
    fn flash_through(receiver: &mut Receiver, payload: &[u8], session: u32) -> (Shape, Vec<u8>) {
        let encoder = Encoder::new(session, payload, 300).unwrap();
        for index in 0u32.. {
            if index % 3 == 1 {
                continue;
            }
            match receiver.add(&encoder.frame(index)) {
                Step::Complete { shape, payload } => return (shape, payload),
                Step::Progress { .. } => {}
                _ => panic!("unexpected step"),
            }
            assert!(index < 10_000);
        }
        unreachable!()
    }

    #[test]
    fn an_offer_key_pairs_the_phone_and_is_used_up() {
        let mut db = Db::open_in_memory().unwrap();
        // Nothing paired and nothing offered: the flash cannot be opened.
        let payload = sealed(SECRET, "PHONE-A", &synthetic_items(0, 40));
        assert!(matches!(import_once(&mut db, &payload, 100.0), Outcome::Failed(why) if why.contains("Pair a phone") && !why.contains("zone pair")));

        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let Outcome::Stored(first) = import_once(&mut db, &payload, 110.0) else { panic!("not stored") };
        assert_eq!((first.items, first.inserted, first.paired, first.rejected), (40, 40, true, 0));
        assert_eq!(db.total_items().unwrap(), 40);
        assert_eq!(db.open_offers(110.0).unwrap(), 0);

        // The phone is now paired, by its install id, and its key keeps working after the
        // offer's ten minutes.
        let devices = db.devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].endpoint_id, "flash:PHONE-A");
        assert_eq!((devices[0].items_received, devices[0].last_path.as_deref()), (40, Some("flash")));
        let more = sealed(SECRET, "PHONE-A", &synthetic_items(30, 30));
        let Outcome::Stored(second) = import_once(&mut db, &more, 5000.0) else { panic!("not stored") };
        assert_eq!((second.inserted, second.unchanged, second.paired), (20, 10, false));
        assert_eq!(db.devices().unwrap()[0].items_received, 60);
        assert_eq!(db.get("FA4E0000-0000-4000-8000-000000000000").unwrap().unwrap().carrier, "flash");

        // A flash sealed with any other secret is not opened (the receiver then checks whether
        // a code was misread before it says so).
        let stranger = sealed(&"cd".repeat(32), "PHONE-B", &synthetic_items(500, 5));
        assert!(matches!(import_once(&mut db, &stranger, 5001.0), Outcome::NotOpened(why) if why.contains("not from a phone paired")));
        assert_eq!(db.total_items().unwrap(), 60);
    }

    #[test]
    fn an_expired_offer_opens_nothing() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let payload = sealed(SECRET, "PHONE-A", &synthetic_items(0, 3));
        assert!(matches!(import_once(&mut db, &payload, 701.0), Outcome::Failed(_)));
        assert_eq!((db.total_items().unwrap(), db.devices().unwrap().len()), (0, 0));
    }

    #[test]
    fn a_phone_paired_over_link_can_flash_and_a_flash_paired_phone_can_link() {
        let mut db = Db::open_in_memory().unwrap();
        // Paired over Link first.
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        assert_eq!(db.admit("endpoint-a", Some(SECRET), &phone("PHONE-A"), 101.0).unwrap(), crate::db::Admission::Paired);
        let payload = sealed(SECRET, "PHONE-A", &synthetic_items(0, 12));
        let Outcome::Stored(stored) = import_once(&mut db, &payload, 200.0) else { panic!("not stored") };
        assert_eq!((stored.inserted, stored.paired), (12, false));
        assert_eq!(db.device("endpoint-a").unwrap().unwrap().items_received, 12);

        // Another phone pairs by Flash while offline, then comes online with the same secret.
        let other = "ef".repeat(32);
        db.create_offer(&other, 300.0, 600.0).unwrap();
        let payload = sealed(&other, "PHONE-B", &synthetic_items(100, 7));
        let Outcome::Stored(stored) = import_once(&mut db, &payload, 310.0) else { panic!("not stored") };
        assert!(stored.paired);
        // Somebody else with the secret but another install id is refused.
        assert_eq!(db.admit("endpoint-x", Some(&other), &phone("PHONE-X"), 320.0).unwrap(), crate::db::Admission::BadSecret);
        // Without the secret it is not paired over Link yet.
        assert_eq!(db.admit("endpoint-b", None, &phone("PHONE-B"), 321.0).unwrap(), crate::db::Admission::NotPaired);
        assert_eq!(db.admit("endpoint-b", Some(&other), &phone("PHONE-B"), 9000.0).unwrap(), crate::db::Admission::Known);
        let ids: Vec<String> = db.devices().unwrap().into_iter().map(|d| d.endpoint_id).collect();
        assert_eq!(ids, vec!["endpoint-a".to_string(), "endpoint-b".to_string()]);
        // The row kept what the flash delivered, and its key: it can still flash.
        assert_eq!(db.device("endpoint-b").unwrap().unwrap().items_received, 7);
        assert_eq!(db.admit("endpoint-b", None, &phone("PHONE-B"), 9001.0).unwrap(), crate::db::Admission::Known);
        let again = sealed(&other, "PHONE-B", &synthetic_items(100, 9));
        assert!(matches!(import_once(&mut db, &again, 9002.0), Outcome::Stored(s) if s.inserted == 2 && !s.paired));
    }

    #[test]
    fn flash_follows_the_link_merge_rule() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let mut items = synthetic_items(0, 3);
        assert!(matches!(import_once(&mut db, &sealed(SECRET, "P", &items), 110.0), Outcome::Stored(_)));
        // An older time still replaces, a deletion deletes, and a live item restores.
        items[0]["text"] = json!("changed on the phone");
        items[0]["updated_at"] = json!(5.0);
        let id1 = items[1]["id"].as_str().unwrap().to_string();
        items[1] = json!({ "id": id1, "kind": "dictation", "deleted": true, "updated_at": 6.0 });
        let Outcome::Stored(stored) = import_once(&mut db, &sealed(SECRET, "P", &items), 120.0) else { panic!("not stored") };
        assert_eq!((stored.replaced, stored.unchanged, stored.deletions), (2, 1, 1));
        assert!(db.get(&id1).unwrap().unwrap().deleted);
        let restored = synthetic_items(1, 1);
        assert!(matches!(import_once(&mut db, &sealed(SECRET, "P", &restored), 130.0), Outcome::Stored(s) if s.replaced == 1));
        assert!(!db.get(&id1).unwrap().unwrap().deleted);
    }

    #[test]
    fn the_receiver_follows_a_flash_and_reports_it() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let mut receiver = Receiver::new();
        assert_eq!(receiver.status()["state"], "waiting");
        assert!(matches!(receiver.add(b"zone://pair?v=1"), Step::Ignored));

        let payload = sealed(SECRET, "PHONE-A", &synthetic_items(0, 200));
        let encoder = Encoder::new(7, &payload, 300).unwrap();
        assert!(encoder.k() > 10);
        assert!(matches!(receiver.add(&encoder.frame(2)), Step::Progress { useful: true }));
        assert!(matches!(receiver.add(&encoder.frame(2)), Step::Progress { useful: false }));
        let status = receiver.status();
        assert_eq!((status["state"].as_str(), status["rows"].as_u64(), status["frames"].as_u64()), (Some("receiving"), Some(1), Some(1)));
        // The name the phone shows for this flash: its session, 7, in two groups of four.
        assert_eq!((status["session"].as_str(), status["name"].as_str()), (Some("00000007"), Some("0000 0007")));
        assert_eq!(flash_name(0x8f3a_21c4), "8f3a 21c4");
        assert_eq!(status["k"].as_u64(), Some(encoder.k() as u64));
        // One tile per chunk: chunk 2 is held and solved (bit 2 of the first byte).
        let k = encoder.k();
        let held = status["blocks"]["held"].as_str().unwrap();
        assert_eq!(held.len(), k.div_ceil(8) * 2);
        assert!(held.starts_with("04") && held[2..].chars().all(|c| c == '0'), "{held}");
        assert_eq!(status["blocks"]["solved"], held);
        assert_eq!(status["solved"], 1);
        assert_eq!((status["pictures"]["frames"].as_u64(), status["pictures"]["useful"].as_u64()), (Some(2), Some(1)));

        let (shape, rebuilt) = flash_through(&mut receiver, &payload, 7);
        assert_eq!(rebuilt, payload);
        let status = receiver.status();
        assert_eq!(status["state"], "storing");
        // Every tile is lit once the flash is whole, and no bit past the last chunk is set.
        assert_eq!(status["solved"].as_u64(), Some(k as u64));
        let solved = status["blocks"]["solved"].as_str().unwrap();
        let ones: u32 = (0..solved.len()).step_by(2).map(|i| u8::from_str_radix(&solved[i..i + 2], 16).unwrap().count_ones()).sum();
        assert_eq!(ones as usize, k);
        let outcome = import(&mut db, &shape, &rebuilt, 110.0);
        receiver.finish(shape, &rebuilt, outcome);
        let status = receiver.status();
        assert_eq!((status["state"].as_str(), status["result"]["new"].as_u64(), status["result"]["paired"].as_bool()), (Some("done"), Some(200), Some(true)));
        // The phone keeps showing frames. They change nothing.
        assert!(matches!(receiver.add(&encoder.frame(900)), Step::AlreadyDone));
        assert_eq!(db.total_items().unwrap(), 200);

        // A second flash (a new session) takes over the status.
        let next = sealed(SECRET, "PHONE-A", &synthetic_items(200, 20));
        let (shape, rebuilt) = flash_through(&mut receiver, &next, 8);
        receiver.finish(shape, &rebuilt, import(&mut db, &shape, &rebuilt, 120.0));
        assert_eq!(receiver.status()["result"]["new"], 20);
        assert_eq!(db.total_items().unwrap(), 220);

        // A flash that cannot be opened is checked first: it might have been a misread code.
        let stranger = sealed(&"cd".repeat(32), "PHONE-B", &synthetic_items(0, 2));
        let (shape, rebuilt) = flash_through(&mut receiver, &stranger, 9);
        let outcome = import(&mut db, &shape, &rebuilt, 130.0);
        assert!(matches!(outcome, Outcome::NotOpened(_)));
        receiver.finish(shape, &rebuilt, outcome);
        let status = receiver.status();
        assert_eq!((status["state"].as_str(), status["trouble"].as_str()), (Some("receiving"), Some("checking")));
        // Every later code agrees, so it is believed: sealed for another computer.
        let encoder = Encoder::new(9, &stranger, 300).unwrap();
        let mut gave_up = None;
        for index in 1000u32..1100 {
            if let Step::GaveUp { reason, .. } = receiver.add(&encoder.frame(index)) {
                gave_up = Some((index, reason));
                break;
            }
        }
        let (index, reason) = gave_up.expect("gave up");
        assert_eq!(index, 1000 + crate::flash::assemble::CONFIRMATIONS as u32 - 1);
        assert!(reason.contains("not from a phone paired with this computer"));
        let status = receiver.status();
        assert_eq!(status["state"], "failed");
        assert!(status["result"]["error"].as_str().unwrap().contains("Choose Pair a phone in your library"));
        assert!(matches!(receiver.add(&encoder.frame(2000)), Step::AlreadyDone));
        // No text of any item is in a status.
        assert!(!receiver.status().to_string().contains("amber"));

        receiver.reset();
        assert_eq!(receiver.status()["state"], "waiting");
    }

    fn sealed_with_check(secret: &str, id: &str, items: &[Value]) -> Vec<u8> {
        let live: Vec<f64> = items.iter().filter(|i| i["deleted"] != json!(true)).filter_map(|i| i["created_at"].as_f64()).collect();
        let json = json!({
            "t": "flash", "v": 1,
            "phone": { "id": id, "name": "Test phone", "app": "1.0.3 (1)" },
            "items": items,
            "check": {
                "items": items.len(),
                "oldest": live.iter().copied().reduce(f64::min),
                "newest": live.iter().copied().reduce(f64::max),
                "digest": crate::flash::check::digest(items),
            },
        });
        seal::seal(&key_of(secret), json.to_string().as_bytes()).unwrap()
    }

    /// Runs frames through the receiver as the web route does, storing what completes.
    fn run(receiver: &mut Receiver, db: &mut Db, frames: impl IntoIterator<Item = Vec<u8>>, now: f64) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        for frame in frames {
            if let Step::Complete { shape, payload } = receiver.add(&frame) {
                let outcome = import(db, &shape, &payload, now);
                outcomes.push(outcome.clone());
                receiver.finish(shape, &payload, outcome);
            }
        }
        outcomes
    }

    /// A misread code gives a payload that does not open. The receiver keeps reading, finds
    /// the code, sets it aside and stores the right payload, and the page is never told the
    /// flash failed or is from another computer.
    #[test]
    fn a_misread_code_is_found_and_the_flash_still_stored() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let items = synthetic_items(0, 120);
        let payload = sealed_with_check(SECRET, "PHONE-M", &items);
        let encoder = Encoder::new(0x8000_00aa, &payload, 300).unwrap();
        let k = encoder.k() as u32;
        let mut receiver = Receiver::new();
        // Frame 3 (code number 4) arrives with a byte changed, as if misread.
        let frames = (0..3 * k + 60).map(|i| {
            let mut frame = encoder.frame(i);
            if i == 3 {
                frame[fountain::HEADER_LEN + 10] ^= 0x40;
            }
            frame
        });
        let mut states = Vec::new();
        let mut outcomes = Vec::new();
        for frame in frames {
            outcomes.extend(run(&mut receiver, &mut db, [frame], 110.0));
            let status = receiver.status();
            states.push((status["state"].as_str().unwrap().to_string(), status["trouble"].as_str().map(str::to_string)));
            if status["state"] == "done" {
                break;
            }
        }
        assert!(matches!(outcomes.first(), Some(Outcome::NotOpened(_))), "the first payload was wrong");
        assert!(matches!(outcomes.last(), Some(Outcome::Stored(_))), "{outcomes:?}");
        assert!(states.iter().all(|(state, _)| state != "failed"), "never failed");
        assert!(states.iter().any(|(_, trouble)| trouble.as_deref() == Some("checking")));
        let status = receiver.status();
        assert_eq!(status["state"], "done");
        assert_eq!(status["codes"]["set_aside"], 1);
        assert_eq!(status["result"]["check"]["all_checked"], true, "{status}");
        assert_eq!(status["result"]["check"]["declared"], 120);
        assert_eq!(db.total_items().unwrap(), 120);
    }

    #[test]
    fn the_status_counts_codes_from_one_and_names_the_missed() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let items = synthetic_items(0, 600);
        let payload = sealed_with_check(SECRET, "PHONE-T", &items);
        let encoder = Encoder::new(0x8000_00ab, &payload, 64).unwrap();
        let k = encoder.k();
        assert!(k > 110, "k = {k}");
        let mut receiver = Receiver::new();
        // Code number 70 (frame 69) and 101 to 103 are missed; the rest arrive in order.
        let wanted: Vec<u32> = (0..(k as u32 + 200)).filter(|i| *i != 69 && !(100..103).contains(i)).collect();
        let mut last = Value::Null;
        for index in &wanted {
            run(&mut receiver, &mut db, [encoder.frame(*index)], 110.0);
            last = receiver.status();
            if *index == 104 {
                assert_eq!(last["codes"]["missed_ranges"], json!([[70, 70], [101, 103]]), "{last}");
                assert_eq!((last["codes"]["shown"].as_u64(), last["codes"]["missed"].as_u64()), (Some(105), Some(4)));
                assert_eq!(last["needed"].as_u64(), Some(k as u64 - last["rows"].as_u64().unwrap()));
            }
            if last["state"] == "done" {
                break;
            }
        }
        assert_eq!(last["state"], "done");
        assert_eq!(last["needed"], 0);
        assert_eq!(last["result"]["check"]["items"], 600);
        assert!(last["result"]["check"]["oldest"].as_f64().unwrap() < last["result"]["check"]["newest"].as_f64().unwrap());
    }

    /// The library could not take the flash: it is tried again a little later, while the phone
    /// still shows it, instead of failing for good.
    #[test]
    fn a_flash_the_library_could_not_take_is_stored_on_a_later_try() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let payload = sealed(SECRET, "PHONE-R", &synthetic_items(0, 30));
        let encoder = Encoder::new(0x8000_00ac, &payload, 300).unwrap();
        let mut receiver = Receiver { store_retry: Some(Duration::ZERO), ..Receiver::new() };
        let mut index = 0u32;
        let (shape, rebuilt) = loop {
            if let Step::Complete { shape, payload } = receiver.add(&encoder.frame(index)) {
                break (shape, payload);
            }
            index += 1;
        };
        receiver.finish(shape, &rebuilt, Outcome::NotStored("busy".to_string()));
        assert_eq!(receiver.status()["state"], "storing");
        // The next frame brings the payload back to be stored.
        let Step::Complete { shape, payload: again } = receiver.add(&encoder.frame(index + 1)) else { panic!("not retried") };
        assert_eq!(again, rebuilt);
        receiver.finish(shape, &again, import(&mut db, &shape, &again, 110.0));
        assert_eq!(receiver.status()["state"], "done");
        assert_eq!(db.total_items().unwrap(), 30);
    }

    /// The library fails part way through a flash (here a trigger refuses one item of its
    /// second batch of 500). Nothing of it is kept and its pairing code is not used up, so the
    /// next try is the same as the first: it pairs the phone, says so once, and counts every
    /// item as new. zone 0.1.0 kept the first batch and the pairing, then on the next try opened
    /// the flash with the phone's new key, never said it paired, and counted the first 500 as
    /// already here.
    #[test]
    fn a_flash_the_library_failed_part_way_through_is_stored_whole_on_the_next_try() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let items = synthetic_items(0, 1200);
        let refused = items[700]["id"].as_str().unwrap().to_string();
        db.execute_for_tests(&format!(
            "CREATE TEMP TRIGGER refuse BEFORE INSERT ON items WHEN new.id = '{refused}' BEGIN SELECT RAISE(ABORT, 'database is locked'); END;"
        ))
        .unwrap();
        let payload = sealed_with_check(SECRET, "PHONE-P", &items);
        let encoder = Encoder::new(0x8000_00ad, &payload, 900).unwrap();
        let mut receiver = Receiver { store_retry: Some(Duration::ZERO), ..Receiver::new() };
        let mut index = 0u32;
        let mut tries = Vec::new();
        while tries.len() < 2 {
            if let Step::Complete { shape, payload } = receiver.add(&encoder.frame(index)) {
                let outcome = import(&mut db, &shape, &payload, 110.0);
                record_events(&db, &outcome, payload.len(), false);
                tries.push(outcome.clone());
                if tries.len() == 1 {
                    assert!(matches!(&outcome, Outcome::NotStored(why) if why.contains("database is locked")), "{outcome:?}");
                    // Nothing of the first try is kept: no items, no pairing, the code unused.
                    assert_eq!((db.total_items().unwrap(), db.devices().unwrap().len(), db.open_offers(110.0).unwrap()), (0, 0, 1));
                    assert!(db.stored_flashes().unwrap().is_empty());
                    db.execute_for_tests("DROP TRIGGER refuse;").unwrap();
                }
                receiver.finish(shape, &payload, outcome);
                if tries.len() == 1 {
                    assert_eq!(receiver.status()["state"], "storing");
                }
            }
            index += 1;
            assert!(index < 1000, "{}", receiver.status());
        }
        let Outcome::Stored(stored) = &tries[1] else { panic!("{:?}", tries[1]) };
        assert_eq!((stored.inserted, stored.unchanged, stored.paired, stored.check.all_checked()), (1200, 0, true, true), "{stored:?}");
        let status = receiver.status();
        assert_eq!((status["state"].as_str(), status["result"]["paired"].as_bool(), status["result"]["new"].as_u64()), (Some("done"), Some(true), Some(1200)));
        let events = db.events(None, None, 100).unwrap();
        let kinds: Vec<&str> = events.iter().map(|e| e.event.as_str()).filter(|e| *e == "paired" || e.starts_with("flash")).collect();
        assert_eq!(kinds, vec!["paired", "flash_received"]);
        assert_eq!((db.total_items().unwrap(), db.open_offers(110.0).unwrap(), db.devices().unwrap()[0].items_received), (1200, 0, 1200));
    }

    /// A finished flash still on a phone's screen, and a stray code naming a flash that does
    /// not exist, do not take the page away from the flash being read.
    #[test]
    fn the_page_follows_the_flash_being_read() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let first = sealed(SECRET, "PHONE-A", &synthetic_items(0, 10));
        let first_frames = Encoder::new(0x8000_0001, &first, 300).unwrap();
        let mut receiver = Receiver::new();
        run(&mut receiver, &mut db, (0..40).map(|i| first_frames.frame(i)), 110.0);
        assert_eq!(receiver.status()["state"], "done");
        let done_session = receiver.status()["session"].clone();

        let second = sealed(SECRET, "PHONE-A", &synthetic_items(100, 300));
        let second_frames = Encoder::new(0x8000_0002, &second, 300).unwrap();
        run(&mut receiver, &mut db, (0..5).map(|i| second_frames.frame(i)), 120.0);
        let reading = receiver.status()["session"].clone();
        assert_ne!(reading, done_session);
        // The first phone is still showing its finished flash.
        run(&mut receiver, &mut db, (40..60).map(|i| first_frames.frame(i)), 121.0);
        assert_eq!(receiver.status()["session"], reading);
        assert_eq!(receiver.status()["state"], "receiving");
        // A code whose header was misread names a flash nobody is showing.
        let mut stray = second_frames.frame(7);
        stray[5] ^= 0x01;
        run(&mut receiver, &mut db, [stray], 122.0);
        assert_eq!(receiver.status()["session"], reading);
        let outcomes = run(&mut receiver, &mut db, (5..400).map(|i| second_frames.frame(i)), 123.0);
        assert!(matches!(outcomes.as_slice(), [Outcome::Stored(s)] if s.inserted == 300));
        assert_eq!(receiver.status()["session"], reading);
    }
}
