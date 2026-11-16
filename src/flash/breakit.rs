//! The break-it harness: a flash made by the iPhone's own encoder, pushed through what a camera
//! does to it, into the real receiver and library, and the library compared with what the
//! phone held, field for field, times to the bit.
//!
//! `tests/fixtures/flash-iphone.json` is written by the phone's tests
//! (ZoneCore, `LinkFlashCheckTests.testWriteAFixtureForTheComputerSide`): synthetic items that
//! are hard to carry exactly (times either side of daylight saving changes, several at one
//! instant, before 1970 and in the year 9999, every binary digit of a Date; emoji, right to
//! left scripts, NUL, escapes, a 60,000 character line, a meeting, a clip, a deletion), sealed
//! with the published test secret, with each field as the phone holds it in memory.
//!
//! Damage: codes dropped, read twice, read out of order, misread with a changed byte (valid
//! QR, wrong data), misread with a wrong code number, misread so they name a flash nobody
//! shows; color pictures with their channels in any order and read through a camera model;
//! another phone's flash in view at the same time; the page started over half way; a camera
//! that starts reading in the middle of the phone's loop.

use serde_json::{Map, Value, json};

use crate::db::{Db, Delivery, Filter, Order, Row};
use crate::flash::fountain::{Encoder, HEADER_LEN};
use crate::flash::receive::{self, Outcome, Receiver, Step};
use crate::item::{Carrier, Item};
use crate::tools::Access;

const FIXTURE: &str = include_str!("../../tests/fixtures/flash-iphone.json");
const NOW: f64 = 1_794_500_000.0;

fn unbase64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {c}"),
    };
    let digits: Vec<u8> = text.bytes().filter(|c| *c != b'=').map(value).collect();
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for group in digits.chunks(4) {
        let n = group.iter().enumerate().fold(0u32, |n, (i, d)| n | (u32::from(*d) << (18 - 6 * i)));
        out.extend_from_slice(&n.to_be_bytes()[1..group.len()]);
    }
    out
}

struct Fixture {
    value: Value,
    sealed: Vec<u8>,
    session: u32,
    chunk: usize,
    cycle: u32,
}

fn fixture() -> Fixture {
    let value: Value = serde_json::from_str(FIXTURE).expect("the fixture is JSON");
    Fixture {
        sealed: unbase64(value["sealed_base64"].as_str().unwrap()),
        session: value["session"].as_u64().unwrap() as u32,
        chunk: value["chunk"].as_u64().unwrap() as usize,
        cycle: value["cycle"].as_u64().unwrap() as u32,
        value,
    }
}

impl Fixture {
    fn encoder(&self) -> Encoder {
        Encoder::new(self.session, &self.sealed, self.chunk).unwrap()
    }

    fn secret(&self) -> &str {
        self.value["secret"].as_str().unwrap()
    }

    fn expected(&self) -> &Vec<Value> {
        self.value["expected"].as_array().unwrap()
    }

    /// The items exactly as the phone encoded them, by id.
    fn wire_items(&self) -> Vec<Value> {
        serde_json::from_str::<Value>(self.value["items_json"].as_str().unwrap()).unwrap().as_array().unwrap().clone()
    }
}

/// splitmix64, for damage that is random but the same on every run.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    /// True `percent` times in a hundred (fractions allowed: 0.5 is one in two hundred).
    fn chance(&mut self, percent: f64) -> bool {
        (self.next() % 1_000_000) as f64 / 10_000.0 < percent
    }
}

#[derive(Clone, Copy, Default)]
struct Damage {
    /// The frame the camera starts at: it can come in mid-loop.
    start: u32,
    /// How many times round the phone's loop the camera watches.
    passes: f64,
    drop: f64,
    twice: f64,
    /// How far a reading can be moved out of order.
    shuffle: usize,
    /// Body byte changed, still a valid frame.
    misread: f64,
    /// Code number changed.
    misnumbered: f64,
    /// Session changed: names a flash nobody is showing.
    stray: f64,
}

/// What the camera reads while the phone shows the flash: one entry per code read.
fn readings(encoder: &Encoder, cycle: u32, damage: Damage, dice: &mut Dice) -> Vec<Vec<u8>> {
    let shown = (damage.passes * cycle as f64) as u32;
    let mut out = Vec::new();
    for n in 0..shown {
        let index = (damage.start + n) % cycle;
        if dice.chance(damage.drop) {
            continue;
        }
        let times = if dice.chance(damage.twice) { 2 } else { 1 };
        for _ in 0..times {
            let mut frame = encoder.frame(index);
            if dice.chance(damage.misread) {
                let at = HEADER_LEN + dice.below((frame.len() - HEADER_LEN) as u64) as usize;
                frame[at] ^= 1 << dice.below(8);
            }
            if dice.chance(damage.misnumbered) {
                let other = dice.below(u64::from(cycle)) as u32;
                frame[13..17].copy_from_slice(&other.to_be_bytes());
            }
            if dice.chance(damage.stray) {
                frame[3 + dice.below(4) as usize] ^= 1 << dice.below(8);
            }
            out.push(frame);
        }
    }
    // Pictures in flight come back in either order.
    if damage.shuffle > 1 {
        for start in (0..out.len()).step_by(damage.shuffle) {
            let end = (start + damage.shuffle).min(out.len());
            for i in (start + 1..end).rev() {
                let j = start + dice.below((i - start + 1) as u64) as usize;
                out.swap(i, j);
            }
        }
    }
    out
}

/// A library with this computer's pairing code waiting, as before a first flash.
fn library(secret: &str) -> Db {
    let db = Db::open_in_memory().unwrap();
    db.create_offer(secret, NOW - 10.0, 600.0).unwrap();
    // Clips on the clipboard is on: a Flash must still never put one there.
    db.meta_set("clipboard", "on").unwrap();
    db
}

struct Run {
    stored: Vec<receive::Stored>,
    failed: Vec<String>,
}

/// Feeds codes as the page's route does: each complete payload is imported and reported back.
fn feed(receiver: &mut Receiver, db: &mut Db, codes: impl IntoIterator<Item = Vec<u8>>) -> Run {
    let mut run = Run { stored: Vec::new(), failed: Vec::new() };
    for code in codes {
        match receiver.add(&code) {
            Step::Complete { shape, payload } => {
                let outcome = receive::import(db, &payload, NOW);
                match &outcome {
                    Outcome::Stored(stored) => run.stored.push((**stored).clone()),
                    Outcome::Failed(reason) => run.failed.push(reason.clone()),
                    Outcome::NotOpened(_) | Outcome::NotStored(_) => {}
                }
                receiver.finish(shape, &payload, outcome);
            }
            Step::GaveUp { reason, .. } => run.failed.push(reason),
            Step::Ignored | Step::Progress { .. } | Step::AlreadyDone => {}
        }
    }
    run
}

fn bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::paths::hex(Sha256::digest(text.as_bytes()).as_slice())
}

/// Every item the phone sent is in the library exactly as the phone held it.
fn assert_library_is_the_phones(db: &Db, fixture: &Fixture, context: &str) {
    for expected in fixture.expected() {
        let id = expected["id"].as_str().unwrap();
        let row: Row = db.get(id).unwrap().unwrap_or_else(|| panic!("{context}: {id} is missing"));
        let field = |key: &str| expected[key].as_str().map(str::to_string);
        assert_eq!(row.kind, expected["kind"].as_str().unwrap(), "{context}: {id} kind");
        assert_eq!(row.deleted, expected["deleted"].as_bool().unwrap(), "{context}: {id} deleted");
        assert_eq!(Some(bits(row.updated_at)), field("updated_bits"), "{context}: {id} updated_at");
        if row.deleted {
            assert!(row.text.is_empty(), "{context}: {id} a deletion keeps no text");
            continue;
        }
        assert_eq!(Some(bits(row.created_at)), field("created_bits"), "{context}: {id} created_at");
        assert_eq!(Some(sha256_hex(&row.text)), field("text_sha256"), "{context}: {id} text");
        assert_eq!(row.source, field("source"), "{context}: {id} source");
        assert_eq!(row.title, field("title"), "{context}: {id} title");
        assert_eq!(row.duration_s.map(bits), field("duration_bits"), "{context}: {id} duration_s");
        assert_eq!(row.engine, field("engine"), "{context}: {id} engine");
        assert_eq!(row.status, field("status"), "{context}: {id} status");
        assert_eq!(row.rev, field("rev"), "{context}: {id} rev");
        assert_eq!(row.carrier, "flash", "{context}: {id} carrier");
    }
    // Nothing else came in: the rows are the phone's items and only those.
    assert_eq!(db.list(&Filter::default(), Order::Newest, 10_000, None).unwrap().len(), fixture.expected().iter().filter(|e| e["deleted"] == false).count(), "{context}");
}

fn assert_checked(stored: &receive::Stored, fixture: &Fixture, context: &str) {
    let check = &stored.check;
    assert!(check.all_checked(), "{context}: {:?}", check.problems);
    let declared = fixture.value["check"]["items"].as_u64().unwrap() as usize;
    assert_eq!(check.declared.as_ref().and_then(|d| d.items), Some(declared as u64), "{context}");
    assert_eq!((check.items, check.distinct, check.duplicates), (declared, declared, 0), "{context}");
    assert_eq!((check.read_back, check.read_back_matching), (declared, declared), "{context}");
    assert_eq!(check.oldest.map(bits), fixture.value["check"]["oldest_bits"].as_str().map(str::to_string), "{context}");
    assert_eq!(check.newest.map(bits), fixture.value["check"]["newest_bits"].as_str().map(str::to_string), "{context}");
}

/// The frames this computer makes from the phone's sealed payload are the phone's frames.
#[test]
fn the_phones_frames_are_the_reference_frames() {
    use sha2::{Digest, Sha256};
    let fixture = fixture();
    let encoder = fixture.encoder();
    assert_eq!(encoder.k(), fixture.value["k"].as_u64().unwrap() as usize);
    assert_eq!(fixture.cycle as usize, 3 * encoder.k() + 60);
    let mut all = Sha256::new();
    for index in 0..fixture.cycle {
        all.update(encoder.frame(index));
    }
    assert_eq!(crate::paths::hex(all.finalize().as_slice()), fixture.value["frames_sha256"].as_str().unwrap());
}

/// The phone's own summary checks out against what this computer parses: count, the oldest
/// and newest times to the bit, and the digest over ids, times and texts.
#[test]
fn the_phones_summary_checks_out() {
    let fixture = fixture();
    let key = crate::flash::seal::key_bytes(&crate::flash::flash_key(fixture.secret()).unwrap()).unwrap();
    let flash = receive::open_with_key(&key, &fixture.sealed).unwrap();
    assert!(flash.check.all_checked(), "{:?}", flash.check.problems);
    assert_eq!(flash.check.declared.as_ref().and_then(|d| d.digest.clone()), fixture.value["check"]["digest"].as_str().map(str::to_string));
    assert_eq!(flash.rejected, 0);
}

/// Each kind of damage on its own, and all of them at once, over many runs.
#[test]
fn every_kind_of_damage_still_stores_exactly_the_phones_library() {
    let fixture = fixture();
    let encoder = fixture.encoder();
    let plain = Damage { passes: 1.0, ..Damage::default() };
    let cases: Vec<(&str, Damage)> = vec![
        ("clean", plain),
        ("starts mid-loop and wraps", Damage { start: fixture.cycle - 30, passes: 1.2, ..plain }),
        ("loses most codes", Damage { passes: 3.0, drop: 70.0, ..plain }),
        ("reads codes twice and out of order", Damage { twice: 50.0, shuffle: 6, ..plain }),
        ("misreads a byte now and then", Damage { passes: 3.0, misread: 4.0, ..plain }),
        ("misreads code numbers", Damage { passes: 3.0, misnumbered: 4.0, ..plain }),
        ("misreads which flash", Damage { passes: 2.0, stray: 5.0, ..plain }),
        ("all of it", Damage { start: 17, passes: 6.0, drop: 40.0, twice: 30.0, shuffle: 5, misread: 3.0, misnumbered: 2.0, stray: 2.0 }),
    ];
    let mut misread_runs = 0;
    for (name, damage) in cases {
        for seed in 0..12u64 {
            let context = format!("{name}, seed {seed}");
            let mut dice = Dice(seed * 7919 + 1);
            let mut db = library(fixture.secret());
            let mut receiver = Receiver::new();
            let run = feed(&mut receiver, &mut db, readings(&encoder, fixture.cycle, damage, &mut dice));
            assert!(run.failed.is_empty(), "{context}: {:?}", run.failed);
            assert_eq!(run.stored.len(), 1, "{context}: stored once");
            assert_checked(&run.stored[0], &fixture, &context);
            assert_library_is_the_phones(&db, &fixture, &context);
            let status = receiver.status();
            assert_eq!(status["state"], "done", "{context}: {status}");
            misread_runs += usize::from(status["codes"]["set_aside"].as_u64().unwrap() > 0);
            // A Flash never puts a clip on the clipboard, even with the setting on.
            let events = db.events(None, None, 100).unwrap();
            let clips: Vec<Value> = events.iter().filter(|e| e.event == "clip").map(|e| serde_json::from_str(&e.data).unwrap()).collect();
            assert_eq!(clips.len(), 1, "{context}");
            assert_eq!((clips[0]["copied"].as_bool(), clips[0]["not_copied"].as_str()), (Some(false), Some("carrier")), "{context}");
        }
    }
    assert!(misread_runs > 10, "misread codes were found and set aside in {misread_runs} runs");
}

/// Color pictures, three codes each with the channels in any order, read through the camera
/// models of `zone dev flash-sweep` and the real color reader.
#[test]
fn color_pictures_with_their_channels_in_any_order() {
    use crate::flash::color;
    use crate::flash::dev::{BenchOptions, CAMERAS, webcam_color_picture};
    let fixture = fixture();
    let encoder = fixture.encoder();
    for camera in CAMERAS.iter().take(2) {
        let mut dice = Dice(0xC010 + camera.noise as u64);
        let options = BenchOptions {
            chunk: fixture.chunk,
            code_px: 520,
            picture: 720,
            frames: 0,
            tilt: camera.tilt,
            blur: camera.blur,
            noise: camera.noise,
            sample: None,
            color: true,
            crosstalk: camera.crosstalk,
            levels: color::Levels::FULL,
            shade: camera.shade,
        };
        let mut db = library(fixture.secret());
        let mut receiver = Receiver::new();
        let mut stored = Vec::new();
        for picture in 0..fixture.cycle.div_ceil(3) * 2 {
            let mut frames: Vec<Vec<u8>> = (0..3).map(|c| encoder.frame((picture * 3 + c) % fixture.cycle)).collect();
            // The channels in any order.
            for i in (1..3).rev() {
                frames.swap(i, dice.below(i as u64 + 1) as usize);
            }
            let rgb = webcam_color_picture(&frames, &options, dice.next()).unwrap();
            let read = color::read_picture(&rgb, true);
            let run = feed(&mut receiver, &mut db, read.codes);
            assert!(run.failed.is_empty(), "{}: {:?}", camera.name, run.failed);
            stored.extend(run.stored);
            if receiver.status()["state"] == "done" {
                break;
            }
        }
        assert_eq!(stored.len(), 1, "{}: {}", camera.name, receiver.status());
        assert_checked(&stored[0], &fixture, camera.name);
        assert_library_is_the_phones(&db, &fixture, camera.name);
    }
}

/// Another phone's flash in front of the camera at the same time, the page started over half
/// way through, and the same flash shown again after it was stored.
#[test]
fn another_flash_at_the_same_time_and_the_page_started_over() {
    let fixture = fixture();
    let encoder = fixture.encoder();
    let mut db = library(fixture.secret());
    // A second phone, paired, with items of its own.
    let other_secret = "cd".repeat(32);
    db.create_offer(&other_secret, NOW - 5.0, 600.0).unwrap();
    let other_items = crate::fakephone::synthetic_items(5000, 200);
    let other_key = crate::flash::seal::key_bytes(&crate::flash::flash_key(&other_secret).unwrap()).unwrap();
    let phone = crate::db::PhoneInfo { id: Some("OTHER-PHONE".into()), name: Some("Other".into()), app: None };
    let other_sealed = crate::flash::seal::seal(&other_key, &crate::flash::seal::flash_json(&phone, &other_items)).unwrap();
    let other = Encoder::new(0x8000_0bad, &other_sealed, 900).unwrap();
    let other_cycle = 3 * other.k() as u32 + 60;

    let mut receiver = Receiver::new();
    // Half of the phone's codes, then the page's Start over.
    feed(&mut receiver, &mut db, (0..fixture.cycle / 4).map(|i| encoder.frame(i)));
    receiver.reset();
    // Both phones at once, codes interleaved, some lost.
    let mut dice = Dice(42);
    let mut codes = Vec::new();
    for n in 0..fixture.cycle.max(other_cycle) * 2 {
        if !dice.chance(30.0) {
            codes.push(encoder.frame(n % fixture.cycle));
        }
        if !dice.chance(30.0) {
            codes.push(other.frame(n % other_cycle));
        }
        // Now and then a QR code that is not a flash at all.
        if n % 17 == 0 {
            codes.push(b"https://example.invalid/menu".to_vec());
        }
    }
    let run = feed(&mut receiver, &mut db, codes);
    assert!(run.failed.is_empty(), "{:?}", run.failed);
    assert_eq!(run.stored.len(), 2, "each flash stored once");
    let ours = run.stored.iter().find(|s| s.items == fixture.expected().len()).unwrap();
    assert_checked(ours, &fixture, "two phones");
    for expected in fixture.expected() {
        assert!(db.get(expected["id"].as_str().unwrap()).unwrap().is_some());
    }
    assert_eq!(db.total_items().unwrap() as usize, fixture.expected().iter().filter(|e| e["deleted"] == false).count() + 200);
    // Shown again later: the phone still has it up. Nothing changes.
    let again = feed(&mut receiver, &mut db, (0..fixture.cycle).map(|i| encoder.frame(i)));
    assert!(again.stored.is_empty() && again.failed.is_empty());
}

/// The same library sent over Link (in the order a Link session sends it) and over Flash
/// lists in the same order, with the same cards, in `zone list` and the MCP `list_items`.
#[test]
fn a_flashed_library_lists_like_the_same_library_sent_over_link() {
    let fixture = fixture();
    let encoder = fixture.encoder();
    let wire = fixture.wire_items();
    // The deleted record was on the computer before either.
    let deleted = wire.iter().find(|v| v["deleted"] == true).unwrap()["id"].as_str().unwrap().to_string();
    let earlier = Item::from_json(&json!({ "id": deleted, "kind": "dictation", "created_at": 1.0, "updated_at": 1.0, "text": "gone" })).unwrap();
    let before = Delivery { carrier: Carrier::Link, device_id: None, received_at: NOW - 100.0 };

    let mut flashed = library(fixture.secret());
    flashed.apply_items(std::slice::from_ref(&earlier), &before).unwrap();
    let mut receiver = Receiver::new();
    let run = feed(&mut receiver, &mut flashed, (0..fixture.cycle).map(|i| encoder.frame(i)));
    assert_eq!(run.stored.len(), 1);

    // Over Link: the waiting clip first (as the phone's LinkStore.pending puts it), then the
    // session's order, in frames of up to 500 items.
    let by_id = |id: &str| wire.iter().find(|v| v["id"] == id).unwrap().clone();
    let mut order = vec![wire.iter().find(|v| v["kind"] == "clip").unwrap().clone()];
    order.extend(fixture.value["link_order"].as_array().unwrap().iter().map(|id| by_id(id.as_str().unwrap())));
    let mut linked = Db::open_in_memory().unwrap();
    linked.apply_items(&[earlier], &before).unwrap();
    let items: Vec<Item> = order.iter().map(|v| Item::from_json(v).unwrap()).collect();
    for frame in items.chunks(500) {
        linked.apply_items(frame, &Delivery { carrier: Carrier::Link, device_id: Some("PHONE"), received_at: NOW }).unwrap();
    }
    assert!(linked.get(&deleted).unwrap().unwrap().deleted && flashed.get(&deleted).unwrap().unwrap().deleted);

    let ids = |db: &Db, order: Order| db.list(&Filter::default(), order, 10_000, None).unwrap().into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(ids(&flashed, Order::Newest), ids(&linked, Order::Newest));
    assert_eq!(ids(&flashed, Order::Oldest), ids(&linked, Order::Oldest));
    // The three items made at the same instant keep one order, newest first and oldest first.
    let access = Access { read_only: true, scope: Default::default() };
    for args in [json!({ "limit": 200 }), json!({ "limit": 200, "order": "oldest" }), json!({ "limit": 5, "timezone": "America/Los_Angeles" })] {
        let args: Map<String, Value> = args.as_object().unwrap().clone();
        let a = crate::tools::call(&mut flashed, "list_items", &args, &access).unwrap();
        let b = crate::tools::call(&mut linked, "list_items", &args, &access).unwrap();
        assert_eq!(a, b, "{args:?}");
    }
    // Every row matches across the two, apart from how it came.
    for id in ids(&linked, Order::Newest) {
        let (a, b) = (flashed.get(&id).unwrap().unwrap(), linked.get(&id).unwrap().unwrap());
        assert_eq!(
            (bits(a.created_at), bits(a.updated_at), &a.text, &a.kind, &a.source, &a.title, &a.extra, &a.rev),
            (bits(b.created_at), bits(b.updated_at), &b.text, &b.kind, &b.source, &b.title, &b.extra, &b.rev),
            "{id}"
        );
    }
}

/// A larger flash (several hundred pieces) through heavy loss and misreads: misread codes
/// among hundreds of rows are still found, and the library is stored once and checks out.
#[test]
fn a_large_flash_through_loss_and_misreads() {
    let secret = "ef".repeat(32);
    let items = crate::fakephone::synthetic_items(20_000, 2_500);
    let live: Vec<f64> = items.iter().filter_map(|i| i["created_at"].as_f64()).collect();
    let json = json!({
        "t": "flash", "v": 1,
        "phone": { "id": "BIG-PHONE", "name": "Big", "app": "test" },
        "check": {
            "items": items.len(),
            "oldest": live.iter().copied().reduce(f64::min),
            "newest": live.iter().copied().reduce(f64::max),
            "digest": crate::flash::check::digest(&items),
        },
        "items": items,
    });
    let key = crate::flash::seal::key_bytes(&crate::flash::flash_key(&secret).unwrap()).unwrap();
    let sealed = crate::flash::seal::seal(&key, json.to_string().as_bytes()).unwrap();
    let encoder = Encoder::new(0x8123_0000, &sealed, 300).unwrap();
    let cycle = 3 * encoder.k() as u32 + 60;
    assert!(encoder.k() > 200, "k = {}", encoder.k());
    for seed in 0..3u64 {
        let mut dice = Dice(seed + 99);
        let damage = Damage { start: seed as u32 * 50, passes: 3.0, drop: 50.0, twice: 20.0, shuffle: 4, misread: 1.5, misnumbered: 0.5, stray: 0.5 };
        let mut db = library(&secret);
        let mut receiver = Receiver::new();
        let run = feed(&mut receiver, &mut db, readings(&encoder, cycle, damage, &mut dice));
        assert!(run.failed.is_empty(), "seed {seed}: {:?}", run.failed);
        assert_eq!(run.stored.len(), 1, "seed {seed}: {}", receiver.status());
        let check = &run.stored[0].check;
        assert!(check.all_checked(), "seed {seed}: {:?}", check.problems);
        assert_eq!((check.declared.as_ref().and_then(|d| d.items), check.read_back_matching), (Some(2_500), 2_500));
        assert_eq!(db.total_items().unwrap(), 2_500);
        assert!(receiver.status()["codes"]["set_aside"].as_u64().unwrap() > 0, "seed {seed}: misreads were set aside");
    }
}

/// How far misreads can go: `cargo test --release stress_misread_rates -- --ignored --nocapture`.
/// Measured on 2026-11-16 (248 pieces, 30% of codes lost): every run stored the right library
/// at 0.5, 2 and 4 percent of codes misread, after 1.0 to 3.8 times as many codes as pieces;
/// at 8 percent two runs in four did within eight passes. A 1,161 piece flash at 4 percent
/// (about 46 misread rows at once) did not finish. None stored anything wrong. QR error
/// correction lets far fewer misreads through than any of these.
#[test]
#[ignore]
fn stress_misread_rates() {
    for (rate, chunk) in [(0.5f64, 300usize), (2.0, 300), (4.0, 300), (8.0, 300)] {
        let secret = "ef".repeat(32);
        let items = crate::fakephone::synthetic_items(20_000, 2_500);
        let key = crate::flash::seal::key_bytes(&crate::flash::flash_key(&secret).unwrap()).unwrap();
        let sealed = crate::flash::seal::seal(&key, &crate::flash::seal::flash_json(&crate::db::PhoneInfo { id: Some("S".into()), name: None, app: None }, &items)).unwrap();
        let encoder = Encoder::new(0x8123_0001, &sealed, chunk).unwrap();
        let cycle = 3 * encoder.k() as u32 + 60;
        for seed in 0..4u64 {
            let mut dice = Dice(seed + 7);
            let damage = Damage { passes: 8.0, drop: 30.0, misread: rate, ..Damage::default() };
            let mut db = library(&secret);
            let mut receiver = Receiver::new();
            let started = std::time::Instant::now();
            let codes = readings(&encoder, cycle, damage, &mut dice);
            let mut used = 0;
            let mut stored = 0;
            for code in codes {
                used += 1;
                let run = feed(&mut receiver, &mut db, [code]);
                assert!(run.failed.is_empty());
                stored += run.stored.len();
                if stored > 0 { break; }
            }
            let s = receiver.status();
            println!("rate {rate}% k {} seed {seed}: stored {stored} after {used} codes ({:.1} k), set aside {}, {:?}", encoder.k(), used as f64 / encoder.k() as f64, s["codes"]["set_aside"], started.elapsed());
        }
    }
}
