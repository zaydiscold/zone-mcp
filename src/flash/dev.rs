//! `zone dev flash-encode` and `zone dev flash-decode`: Flash without a phone or a camera.
//!
//! flash-encode draws the frames of a flash as numbered PNG pictures. flash-decode reads a
//! folder of PNG pictures (from flash-encode, or written by the iPhone app's own encoder),
//! rebuilds the payload and opens it. Neither prints any item's text.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::fountain::{self, Decoder, Encoder, Shape};
use super::receive::{self, Outcome};
use super::{qr, seal};
use crate::db::{Db, Delivery, PhoneInfo};
use crate::item::Carrier;
use crate::paths;

pub struct EncodeOptions {
    pub out: PathBuf,
    /// The pairing secret, 64 hex characters.
    pub secret: String,
    /// Items as JSON Lines. Synthetic items when none.
    pub file: Option<PathBuf>,
    pub count: usize,
    pub start_index: usize,
    pub chunk: usize,
    /// How many frames to draw. None means enough to rebuild with some to spare.
    pub frames: Option<usize>,
    /// Index of the first frame drawn.
    pub first_frame: u32,
    pub scale: usize,
    pub quiet: usize,
    pub session: Option<u32>,
    pub phone_id: Option<String>,
    pub name: Option<String>,
}

fn key_for_secret(secret: &str) -> Result<[u8; 32]> {
    super::flash_key(secret).and_then(|key| seal::key_bytes(&key)).context("the secret must be 64 hexadecimal characters (32 bytes)")
}

pub fn encode(options: &EncodeOptions) -> Result<Value> {
    let key = key_for_secret(&options.secret)?;
    let items = match &options.file {
        Some(path) => crate::fakephone::load_jsonl(path)?,
        None => crate::fakephone::synthetic_items(options.start_index, options.count),
    };
    let phone = PhoneInfo {
        id: Some(options.phone_id.clone().unwrap_or_else(|| "F1A50000-0000-4000-8000-000000000001".to_string())),
        name: Some(options.name.clone().unwrap_or_else(|| "Flash test phone".to_string())),
        app: Some(format!("zone dev {}", env!("CARGO_PKG_VERSION"))),
    };
    let json = seal::flash_json(&phone, &items);
    let sealed = seal::seal(&key, &json)?;
    let session = options.session.unwrap_or_else(|| u32::from_be_bytes(paths::random_bytes::<4>()));
    let encoder = Encoder::new(session, &sealed, options.chunk).map_err(|e| anyhow!(e))?;
    let k = encoder.k();
    let frames = options.frames.unwrap_or(k + 20 + k / 20);
    std::fs::create_dir_all(&options.out).with_context(|| format!("cannot create {}", options.out.display()))?;
    for n in 0..frames {
        let index = options.first_frame + n as u32;
        let picture = qr::draw(&encoder.frame(index), options.scale, options.quiet)?;
        qr::write_png(&options.out.join(format!("frame-{index:06}.png")), &picture)?;
    }
    Ok(json!({
        "dir": options.out,
        "frames": frames,
        "first_frame": options.first_frame,
        "k": k,
        "chunk": options.chunk,
        "session": format!("{session:08x}"),
        "items": items.len(),
        "json_bytes": json.len(),
        "sealed_bytes": sealed.len(),
        "picture_side": qr::draw(&encoder.frame(0), options.scale, options.quiet)?.width,
    }))
}

pub struct DecodeOptions {
    pub dir: PathBuf,
    /// Open with the flash key of this pairing secret instead of the library's keys.
    pub secret: Option<String>,
    /// Store the items in the library.
    pub import: bool,
}

/// The PNG files of a folder, in name order. The order does not matter to the decoder: every
/// frame says which one it is.
fn png_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read the folder {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file() && path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false))
        // Not the hidden companions macOS adds when a folder is copied or archived (._name.png).
        .filter(|path| !path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(true))
        .collect();
    files.sort();
    Ok(files)
}

/// Reads the codes of many pictures, on every processor.
fn read_all(files: &[PathBuf]) -> Vec<Result<Vec<Vec<u8>>>> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(files.len().max(1));
    let per = files.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(per)
            .map(|part| scope.spawn(move || part.iter().map(|path| qr::read_png(path).map(|gray| qr::read_codes(&gray))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|handle| handle.join().unwrap_or_default()).collect()
    })
}

pub fn decode(options: &DecodeOptions) -> Result<Value> {
    let files = png_files(&options.dir)?;
    if files.is_empty() {
        bail!("there are no PNG pictures in {}", options.dir.display());
    }
    let mut unreadable = 0usize;
    let mut without_code = 0usize;
    let mut not_flash = 0usize;
    let mut sessions: HashMap<Shape, Decoder> = HashMap::new();
    for codes in read_all(&files) {
        let Ok(codes) = codes else {
            unreadable += 1;
            continue;
        };
        if codes.is_empty() {
            without_code += 1;
        }
        for raw in codes {
            match fountain::parse(&raw) {
                Ok(frame) => {
                    sessions.entry(frame.shape).or_insert_with(|| Decoder::new(frame.shape)).add(frame.index, frame.body);
                }
                Err(_) => not_flash += 1,
            }
        }
    }
    let mut report = json!({
        "dir": options.dir,
        "pictures": files.len(),
        "pictures_unreadable": unreadable,
        "pictures_without_a_code": without_code,
        "codes_that_are_not_flash_frames": not_flash,
        "sessions": sessions.len(),
    });
    // The flash with the most frames is the one meant. Others are strays.
    let Some((shape, decoder)) = sessions.iter().max_by_key(|(_, d)| d.frames()) else {
        report["complete"] = json!(false);
        report["error"] = json!("no picture held a flash frame");
        return Ok(report);
    };
    report["session"] = json!(format!("{:08x}", shape.session));
    report["frames"] = json!(decoder.frames());
    report["k"] = json!(decoder.k());
    report["rows"] = json!(decoder.rank());
    report["chunk"] = json!(shape.chunk);
    report["sealed_bytes"] = json!(shape.total_len);
    let Some(payload) = decoder.solve() else {
        report["complete"] = json!(false);
        report["error"] = json!(format!("{} of {} pieces: more frames are needed", decoder.rank(), decoder.k()));
        return Ok(report);
    };
    report["complete"] = json!(true);

    match &options.secret {
        Some(secret) => {
            let flash = receive::open_with_key(&key_for_secret(secret)?, &payload)?;
            report["opened_with"] = json!("the secret given");
            report["phone"] = json!({ "id": flash.phone.id, "name": flash.phone.name, "app": flash.phone.app });
            report["items"] = json!(flash.items.len());
            report["rejected"] = json!(flash.rejected);
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut deletions = 0usize;
            for item in &flash.items {
                if item.deleted {
                    deletions += 1;
                } else {
                    *kinds.entry(item.kind.clone()).or_default() += 1;
                }
            }
            report["by_kind"] = json!(kinds);
            report["deletions"] = json!(deletions);
            if options.import {
                // Stored as a flash, without pairing anything: the secret was given by hand.
                let mut db = Db::open_default()?;
                let now = paths::now_unix();
                let delivery = Delivery { carrier: Carrier::Flash, device_id: flash.phone.id.as_deref(), received_at: now };
                let (mut new, mut changed, mut unchanged) = (0, 0, 0);
                for batch in flash.items.chunks(500) {
                    let applied = db.apply_items(batch, &delivery)?;
                    new += applied.inserted;
                    changed += applied.replaced;
                    unchanged += applied.unchanged;
                }
                report["imported"] = json!({ "new": new, "changed": changed, "unchanged": unchanged });
            }
        }
        None => {
            let mut db = Db::open_default()?;
            let now = paths::now_unix();
            if options.import {
                report["opened_with"] = json!("a key from the library");
                match receive::import(&mut db, &payload, now) {
                    Outcome::Stored(stored) => {
                        report["phone"] = json!({ "name": stored.phone_name });
                        report["items"] = json!(stored.items);
                        report["rejected"] = json!(stored.rejected);
                        report["deletions"] = json!(stored.deletions);
                        report["imported"] =
                            json!({ "new": stored.inserted, "changed": stored.replaced, "unchanged": stored.unchanged, "paired": stored.paired });
                    }
                    Outcome::Failed(reason) => bail!("{reason}"),
                }
            } else {
                let keys = db.flash_keys(now)?;
                let Some(flash) = keys.iter().find_map(|k| receive::open_with_key(&k.key, &payload).ok()) else {
                    bail!("none of the library's {} keys opens this flash. Give the pairing secret with --secret.", keys.len());
                };
                report["opened_with"] = json!("a key from the library");
                report["phone"] = json!({ "id": flash.phone.id, "name": flash.phone.name, "app": flash.phone.app });
                report["items"] = json!(flash.items.len());
                report["rejected"] = json!(flash.rejected);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "abababababababababababababababababababababababababababababababab";

    fn encode_options(out: &Path) -> EncodeOptions {
        EncodeOptions {
            out: out.to_path_buf(),
            secret: SECRET.to_string(),
            file: None,
            count: 40,
            start_index: 0,
            chunk: 300,
            frames: None,
            first_frame: 0,
            scale: 3,
            quiet: 4,
            session: Some(0x0A0B_0C0D),
            phone_id: None,
            name: None,
        }
    }

    #[test]
    fn pictures_out_and_back_in() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("frames");
        let made = encode(&encode_options(&out)).unwrap();
        let k = made["k"].as_u64().unwrap() as usize;
        assert!(k >= 5, "k = {k}");
        assert_eq!(made["frames"].as_u64().unwrap() as usize, k + 20 + k / 20);
        assert_eq!(made["picture_side"], (61 + 8) * 3);

        let report = decode(&DecodeOptions { dir: out.clone(), secret: Some(SECRET.to_string()), import: false }).unwrap();
        assert_eq!(report["complete"], true);
        assert_eq!((report["items"].as_u64(), report["rejected"].as_u64(), report["k"].as_u64()), (Some(40), Some(0), Some(k as u64)));
        assert_eq!(report["by_kind"]["dictation"], 36);
        assert_eq!(report["by_kind"]["note"], 4);
        assert_eq!(report["session"], "0a0b0c0d");
        assert!(!report.to_string().contains("amber"), "no item text in the report");

        // Any names, in any order, with frames missing and other files lying around.
        let mut files = png_files(&out).unwrap();
        files.sort();
        for (n, path) in files.iter().enumerate() {
            if n % 5 == 1 && n < k {
                std::fs::remove_file(path).unwrap();
            } else {
                std::fs::rename(path, out.join(format!("{:04}-shot.PNG", (n * 7919) % 10_007))).unwrap();
            }
        }
        std::fs::write(out.join("notes.txt"), b"not a picture").unwrap();
        std::fs::write(out.join("._0001-shot.PNG"), b"what macOS leaves beside a copied file").unwrap();
        std::fs::write(out.join("broken.png"), b"not a picture either").unwrap();
        qr::write_png(&out.join("other-code.png"), &qr::draw(b"zone://pair?v=1", 4, 4).unwrap()).unwrap();
        qr::write_png(&out.join("blank.png"), &qr::Gray::new(50, 50, vec![255; 2500]).unwrap()).unwrap();
        let report = decode(&DecodeOptions { dir: out.clone(), secret: Some(SECRET.to_string()), import: false }).unwrap();
        assert_eq!((report["complete"].as_bool(), report["items"].as_u64()), (Some(true), Some(40)));
        assert_eq!(report["pictures_unreadable"], 1);
        assert_eq!(report["pictures_without_a_code"], 1);
        assert_eq!(report["codes_that_are_not_flash_frames"], 1);

        // The wrong secret opens nothing, and says so.
        let wrong = decode(&DecodeOptions { dir: out.clone(), secret: Some("cd".repeat(32)), import: false });
        assert!(wrong.err().unwrap().to_string().contains("does not open"));
        assert!(decode(&DecodeOptions { dir: out.clone(), secret: Some("abc".into()), import: false }).is_err());

        // Too few frames: not complete, and it says how far it got.
        let few = dir.path().join("few");
        let mut options = encode_options(&few);
        options.frames = Some(k - 2);
        encode(&options).unwrap();
        let report = decode(&DecodeOptions { dir: few, secret: Some(SECRET.to_string()), import: false }).unwrap();
        assert_eq!(report["complete"], false);
        assert_eq!(report["rows"].as_u64(), Some(k as u64 - 2));

        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(decode(&DecodeOptions { dir: empty, secret: None, import: false }).is_err());
    }
}
