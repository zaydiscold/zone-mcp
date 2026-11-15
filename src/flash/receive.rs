//! The computer's side of a flash: frames in, items in the library out.
//!
//! [`Receiver`] collects frames, whichever way they arrive (pictures from the local page, or
//! frame bytes directly). When a flash is complete its payload is handed to [`import`], which
//! finds the key that opens it and merges the items like Link items.
//!
//! Nothing here logs or returns any item's text.

use std::collections::HashMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::fountain::{self, Added, Decoder, Shape};
use super::seal;
use crate::db::{Db, Delivery, FlashKeyOwner};
use crate::item::Carrier;

/// Flashes followed at once. More than one only happens when a second phone, or a second
/// flash from the same phone, shows up before the first is finished.
const MAX_SESSIONS: usize = 3;
/// Items stored per transaction.
const BATCH: usize = 500;
/// The page is told which chunks are known, one tile each, up to this many chunks (about
/// 4.6 MB of sealed payload). A larger flash reports counts only.
const MAX_TILES: usize = 16_384;

/// What became of a completed flash.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Stored(Stored),
    /// Why nothing was stored, in words for the person holding the phone.
    Failed(String),
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
            }),
            Outcome::Failed(reason) => json!({ "ok": false, "error": reason }),
        }
    }
}

struct Session {
    /// Dropped once the flash is complete, to give its memory back.
    decoder: Option<Decoder>,
    k: usize,
    rank: usize,
    frames: usize,
    /// The chunks held and solved, as the page gets them, worked out when the rows change.
    tiles: Option<Tiles>,
    /// Order of arrival, to tell which session is the current one.
    touched: u64,
    outcome: Option<Outcome>,
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
        let bytes = k.div_ceil(8);
        (0..bytes).map(|b| format!("{:02x}", (words[b / 8] >> (8 * (b % 8))) & 0xff)).collect()
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
}

/// What one frame did.
pub enum Step {
    /// Not a flash frame at all (some other QR code), or a damaged one.
    Ignored,
    /// Taken. The flash is not complete yet. `useful` when it brought a new row.
    Progress { useful: bool },
    /// A frame of a flash that is already finished: the phone is still showing it.
    AlreadyDone,
    /// This frame completed the flash. The payload is ready for [`import`], and the caller
    /// reports back with [`Receiver::finish`].
    Complete { shape: Shape, payload: Vec<u8> },
}

#[derive(Default)]
pub struct Receiver {
    sessions: HashMap<Shape, Session>,
    clock: u64,
    /// Pictures looked at, and how many of them had a code in them.
    pub pictures: u64,
    pub pictures_with_code: u64,
    /// Frames of a flash read, in any picture, and how many of them were useful.
    pub frames_read: u64,
    pub frames_useful: u64,
    /// What the most recent picture held.
    pub last_picture: Option<PictureRead>,
}

impl Receiver {
    pub fn new() -> Receiver {
        Receiver::default()
    }

    /// Takes the bytes of one QR code.
    pub fn add(&mut self, raw: &[u8]) -> Step {
        let frame = match fountain::parse(raw) {
            Ok(frame) => frame,
            Err(_) => return Step::Ignored,
        };
        self.clock += 1;
        let clock = self.clock;
        if !self.sessions.contains_key(&frame.shape) {
            if self.sessions.len() >= MAX_SESSIONS
                && let Some(oldest) = self.sessions.iter().min_by_key(|(_, s)| s.touched).map(|(shape, _)| *shape)
            {
                self.sessions.remove(&oldest);
            }
            let decoder = Decoder::new(frame.shape);
            let tiles = Tiles::of(&decoder);
            self.sessions.insert(
                frame.shape,
                Session { k: decoder.k(), decoder: Some(decoder), rank: 0, frames: 0, tiles, touched: clock, outcome: None },
            );
        }
        self.frames_read += 1;
        let session = self.sessions.get_mut(&frame.shape).expect("just inserted");
        session.touched = clock;
        let Some(decoder) = session.decoder.as_mut() else {
            return Step::AlreadyDone;
        };
        let added = decoder.add(frame.index, frame.body);
        if added == Added::Duplicate {
            return Step::Progress { useful: false };
        }
        session.rank = decoder.rank();
        session.frames = decoder.frames();
        if added == Added::Useful {
            self.frames_useful += 1;
            session.tiles = Tiles::of(decoder);
        }
        if !decoder.is_complete() {
            return Step::Progress { useful: added == Added::Useful };
        }
        let payload = decoder.solve().unwrap_or_default();
        session.decoder = None;
        session.tiles = Tiles::all(session.k);
        Step::Complete { shape: frame.shape, payload }
    }

    /// Records what became of a completed flash.
    pub fn finish(&mut self, shape: Shape, outcome: Outcome) {
        if let Some(session) = self.sessions.get_mut(&shape) {
            session.outcome = Some(outcome);
        }
    }

    /// Forgets everything, for the page's "start over".
    pub fn reset(&mut self) {
        *self = Receiver::default();
    }

    /// Where the most recent flash stands, for the page.
    ///
    /// `blocks` has one bit per chunk of the payload (see [`Tiles`]): `held` where the decoder
    /// has a row, `solved` where it can already rebuild the chunk. It is null for a flash of
    /// more than 16384 chunks. `last` is what the most recent camera picture held.
    pub fn status(&self) -> Value {
        let pictures = json!({
            "seen": self.pictures,
            "with_code": self.pictures_with_code,
            "frames": self.frames_read,
            "useful": self.frames_useful,
        });
        let last = self.last_picture.map(|p| json!({ "codes": p.codes, "frames": p.frames, "useful": p.useful }));
        let Some((shape, session)) = self.sessions.iter().max_by_key(|(_, s)| s.touched) else {
            return json!({ "state": "waiting", "pictures": pictures, "last": last });
        };
        let state = match (&session.outcome, session.decoder.is_some()) {
            (Some(Outcome::Stored(_)), _) => "done",
            (Some(Outcome::Failed(_)), _) => "failed",
            (None, true) => "receiving",
            (None, false) => "storing",
        };
        json!({
            "state": state,
            "session": format!("{:08x}", shape.session),
            "frames": session.frames,
            "rows": session.rank,
            "k": session.k,
            "bytes": shape.total_len,
            "solved": session.tiles.as_ref().map(|t| t.solved_count),
            "blocks": session.tiles.as_ref().map(|t| json!({ "held": t.held, "solved": t.solved })),
            "result": session.outcome.as_ref().map(Outcome::to_json),
            "pictures": pictures,
            "last": last,
        })
    }
}

/// Opens a complete payload with whichever known key fits and merges its items.
///
/// The keys tried are each paired phone's, then each pairing offer's that is still valid. An
/// offer's key pairs the phone and uses the offer up.
pub fn import(db: &mut Db, sealed: &[u8], now: f64) -> Outcome {
    match import_inner(db, sealed, now) {
        Ok(stored) => Outcome::Stored(stored),
        Err(error) => Outcome::Failed(format!("{error:#}")),
    }
}

/// Opens a payload with one given key and reads it, without storing anything.
pub fn open_with_key(key: &[u8; 32], sealed: &[u8]) -> Result<seal::Flash> {
    let Some(packed) = seal::unseal(key, sealed) else {
        bail!("this key does not open the flash");
    };
    seal::parse(&seal::inflate(&packed)?)
}

fn import_inner(db: &mut Db, sealed: &[u8], now: f64) -> Result<Stored> {
    let keys = db.flash_keys(now)?;
    if keys.is_empty() {
        bail!("No phone is paired with this computer, so the flash cannot be opened. Run `zone pair`, scan the code with the phone, then flash again.");
    }
    let Some((packed, owner)) = keys.iter().find_map(|k| seal::unseal(&k.key, sealed).map(|packed| (packed, &k.owner))) else {
        bail!(
            "The flash came through whole, but it is not from a phone paired with this computer. Run `zone pair`, scan the code with the phone, then flash again."
        );
    };
    let flash = seal::parse(&seal::inflate(&packed)?)?;
    let (device, paired) = match owner {
        FlashKeyOwner::Device(endpoint_id) => {
            db.touch_device(endpoint_id, &flash.phone, now)?;
            (endpoint_id.clone(), false)
        }
        FlashKeyOwner::Offer(hash) => match db.pair_by_flash(hash, &flash.phone, now)? {
            Some(device) => (device, true),
            None => bail!("The pairing code this flash was made with ran out. Run `zone pair` again."),
        },
    };
    let delivery = Delivery { carrier: Carrier::Flash, device_id: flash.phone.id.as_deref(), received_at: now };
    let mut stored = Stored {
        phone_name: flash.phone.name.clone(),
        items: flash.items.len(),
        rejected: flash.rejected,
        paired,
        ..Stored::default()
    };
    // An empty flash still counts as a sync.
    let empty: &[crate::item::Item] = &[];
    let batches: Vec<&[crate::item::Item]> = if flash.items.is_empty() { vec![empty] } else { flash.items.chunks(BATCH).collect() };
    let mut changed = 0;
    for batch in batches {
        let applied = db.apply_items(batch, &delivery)?;
        stored.inserted += applied.inserted;
        stored.replaced += applied.replaced;
        stored.unchanged += applied.unchanged;
        stored.deletions += applied.deletions;
        changed += applied.changed();
    }
    db.record_delivery(&device, changed, Some("flash"), now)?;
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
        assert!(matches!(import(&mut db, &payload, 100.0), Outcome::Failed(why) if why.contains("zone pair")));

        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let Outcome::Stored(first) = import(&mut db, &payload, 110.0) else { panic!("not stored") };
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
        let Outcome::Stored(second) = import(&mut db, &more, 5000.0) else { panic!("not stored") };
        assert_eq!((second.inserted, second.unchanged, second.paired), (20, 10, false));
        assert_eq!(db.devices().unwrap()[0].items_received, 60);
        assert_eq!(db.get("FA4E0000-0000-4000-8000-000000000000").unwrap().unwrap().carrier, "flash");

        // A flash sealed with any other secret is refused.
        let stranger = sealed(&"cd".repeat(32), "PHONE-B", &synthetic_items(500, 5));
        assert!(matches!(import(&mut db, &stranger, 5001.0), Outcome::Failed(why) if why.contains("not from a phone paired")));
        assert_eq!(db.total_items().unwrap(), 60);
    }

    #[test]
    fn an_expired_offer_opens_nothing() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let payload = sealed(SECRET, "PHONE-A", &synthetic_items(0, 3));
        assert!(matches!(import(&mut db, &payload, 701.0), Outcome::Failed(_)));
        assert_eq!((db.total_items().unwrap(), db.devices().unwrap().len()), (0, 0));
    }

    #[test]
    fn a_phone_paired_over_link_can_flash_and_a_flash_paired_phone_can_link() {
        let mut db = Db::open_in_memory().unwrap();
        // Paired over Link first.
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        assert_eq!(db.admit("endpoint-a", Some(SECRET), &phone("PHONE-A"), 101.0).unwrap(), crate::db::Admission::Paired);
        let payload = sealed(SECRET, "PHONE-A", &synthetic_items(0, 12));
        let Outcome::Stored(stored) = import(&mut db, &payload, 200.0) else { panic!("not stored") };
        assert_eq!((stored.inserted, stored.paired), (12, false));
        assert_eq!(db.device("endpoint-a").unwrap().unwrap().items_received, 12);

        // Another phone pairs by Flash while offline, then comes online with the same secret.
        let other = "ef".repeat(32);
        db.create_offer(&other, 300.0, 600.0).unwrap();
        let payload = sealed(&other, "PHONE-B", &synthetic_items(100, 7));
        let Outcome::Stored(stored) = import(&mut db, &payload, 310.0) else { panic!("not stored") };
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
        assert!(matches!(import(&mut db, &again, 9002.0), Outcome::Stored(s) if s.inserted == 2 && !s.paired));
    }

    #[test]
    fn flash_follows_the_link_merge_rule() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, 100.0, 600.0).unwrap();
        let mut items = synthetic_items(0, 3);
        assert!(matches!(import(&mut db, &sealed(SECRET, "P", &items), 110.0), Outcome::Stored(_)));
        // An older time still replaces, a deletion deletes, and a live item restores.
        items[0]["text"] = json!("changed on the phone");
        items[0]["updated_at"] = json!(5.0);
        let id1 = items[1]["id"].as_str().unwrap().to_string();
        items[1] = json!({ "id": id1, "kind": "dictation", "deleted": true, "updated_at": 6.0 });
        let Outcome::Stored(stored) = import(&mut db, &sealed(SECRET, "P", &items), 120.0) else { panic!("not stored") };
        assert_eq!((stored.replaced, stored.unchanged, stored.deletions), (2, 1, 1));
        assert!(db.get(&id1).unwrap().unwrap().deleted);
        let restored = synthetic_items(1, 1);
        assert!(matches!(import(&mut db, &sealed(SECRET, "P", &restored), 130.0), Outcome::Stored(s) if s.replaced == 1));
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
        let outcome = import(&mut db, &rebuilt, 110.0);
        receiver.finish(shape, outcome);
        let status = receiver.status();
        assert_eq!((status["state"].as_str(), status["result"]["new"].as_u64(), status["result"]["paired"].as_bool()), (Some("done"), Some(200), Some(true)));
        // The phone keeps showing frames. They change nothing.
        assert!(matches!(receiver.add(&encoder.frame(900)), Step::AlreadyDone));
        assert_eq!(db.total_items().unwrap(), 200);

        // A second flash (a new session) takes over the status.
        let next = sealed(SECRET, "PHONE-A", &synthetic_items(200, 20));
        let (shape, rebuilt) = flash_through(&mut receiver, &next, 8);
        receiver.finish(shape, import(&mut db, &rebuilt, 120.0));
        assert_eq!(receiver.status()["result"]["new"], 20);
        assert_eq!(db.total_items().unwrap(), 220);

        // A flash that cannot be opened says so.
        let stranger = sealed(&"cd".repeat(32), "PHONE-B", &synthetic_items(0, 2));
        let (shape, rebuilt) = flash_through(&mut receiver, &stranger, 9);
        receiver.finish(shape, import(&mut db, &rebuilt, 130.0));
        let status = receiver.status();
        assert_eq!(status["state"], "failed");
        assert!(status["result"]["error"].as_str().unwrap().contains("zone pair"));
        // No text of any item is in a status.
        assert!(!receiver.status().to_string().contains("amber"));

        receiver.reset();
        assert_eq!(receiver.status()["state"], "waiting");
    }
}
