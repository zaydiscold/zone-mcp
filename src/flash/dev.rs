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
use super::{color, qr, seal};
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
    /// Color Flash: three frames to a picture, one per channel.
    pub color: bool,
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
    let per_picture = if options.color { 3 } else { 1 };
    let pictures = frames.div_ceil(per_picture);
    for p in 0..pictures {
        let first = options.first_frame + (p * per_picture) as u32;
        let path = options.out.join(format!("frame-{first:06}.png"));
        if options.color {
            // Frames first, first + 1 and first + 2 in red, green and blue.
            let three: Vec<Vec<u8>> = (0..3).map(|c| encoder.frame(first + c)).collect();
            let refs: Vec<&[u8]> = three.iter().map(|f| f.as_slice()).collect();
            color::write_png(&path, &color::draw(&refs, options.scale, options.quiet)?)?;
        } else {
            qr::write_png(&path, &qr::draw(&encoder.frame(first), options.scale, options.quiet)?)?;
        }
    }
    Ok(json!({
        "dir": options.out,
        "frames": pictures * per_picture,
        "pictures": pictures,
        "color": options.color,
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

/// The codes in one picture file: a color picture's channels, or a black and white one's
/// codes at whatever scale they were saved.
fn read_file(path: &Path, unmix: bool) -> Result<color::ColorRead> {
    let rgb = color::read_png(path)?;
    let read = color::read_picture_with(&rgb, true, unmix);
    if read.color || !read.codes.is_empty() {
        return Ok(read);
    }
    Ok(color::ColorRead { codes: qr::read_codes(&rgb.luminance()), color: false })
}

/// Reads the codes of many pictures, on every processor.
fn read_all(files: &[PathBuf], unmix: bool) -> Vec<Result<color::ColorRead>> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(files.len().max(1));
    let per = files.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files.chunks(per).map(|part| scope.spawn(move || part.iter().map(|path| read_file(path, unmix)).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|handle| handle.join().unwrap_or_default()).collect()
    })
}

/// `zone dev flash-read`: the codes in each picture of a folder, as hexadecimal, one JSON
/// object per line. Flash frames are encrypted, so this never shows an item's words.
pub fn read_folder(dir: &Path, unmix: bool) -> Result<Vec<Value>> {
    let files = png_files(dir)?;
    let reads = read_all(&files, unmix);
    Ok(files
        .iter()
        .zip(reads)
        .map(|(path, read)| {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            match read {
                Ok(read) => json!({ "file": name, "color": read.color, "codes": read.codes.iter().map(|c| paths::hex(c)).collect::<Vec<_>>() }),
                Err(error) => json!({ "file": name, "error": format!("{error:#}") }),
            }
        })
        .collect())
}

pub fn decode(options: &DecodeOptions) -> Result<Value> {
    let files = png_files(&options.dir)?;
    if files.is_empty() {
        bail!("there are no PNG pictures in {}", options.dir.display());
    }
    let mut unreadable = 0usize;
    let mut without_code = 0usize;
    let mut not_flash = 0usize;
    let mut color_pictures = 0usize;
    let mut sessions: HashMap<Shape, Decoder> = HashMap::new();
    for read in read_all(&files, true) {
        let Ok(read) = read else {
            unreadable += 1;
            continue;
        };
        if read.codes.is_empty() {
            without_code += 1;
        }
        color_pictures += usize::from(read.color);
        for raw in read.codes {
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
        "color_pictures": color_pictures,
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
                let outcome = receive::import(&mut db, &payload, now);
                receive::record_events(&db, &outcome, payload.len(), color_pictures > 0);
                match outcome {
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

// -------------------------------------------------------------------- flash-bench

#[derive(Clone)]
pub struct BenchOptions {
    /// Bytes per frame body: what the phone chooses.
    pub chunk: usize,
    /// How wide the code is in the picture, in pixels, quiet zone included.
    pub code_px: usize,
    /// The side of the square picture the page posts.
    pub picture: usize,
    pub frames: usize,
    /// Degrees the phone is turned.
    pub tilt: f64,
    /// Box blur radius in pixels: a camera slightly out of focus.
    pub blur: usize,
    /// Sensor noise, plus or minus this much on each pixel.
    pub noise: u8,
    /// Writes the first picture here, to look at.
    pub sample: Option<PathBuf>,
    /// Color Flash: three frames to a picture.
    pub color: bool,
    /// How much of each channel's light the camera sees in its neighbours (color only).
    pub crosstalk: f64,
}

/// A frame as a camera sees it: the code on a phone's lit screen, turned a little, in a dim
/// room, slightly soft and noisy, scaled to `code_px` with smooth (not whole pixel) steps.
pub fn webcam_picture(code: &qr::Gray, options: &BenchOptions, seed: u64) -> qr::Gray {
    let side = options.picture;
    let center = side as f64 / 2.0;
    let scale = code.width as f64 / options.code_px as f64;
    let (sin, cos) = options.tilt.to_radians().sin_cos();
    // The phone's body reaches a little past the screen.
    let body = options.code_px as f64 * 0.62;
    let mut pixels = vec![0u8; side * side];
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as f64 - center, y as f64 - center);
            // Into the phone's own frame of reference.
            let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            let room = 38.0 + 30.0 * (y as f64 / side as f64);
            let half = options.code_px as f64 / 2.0;
            let value = if u.abs() <= half && v.abs() <= half {
                // The share of this pixel the dark modules cover (three by three samples),
                // on a screen where white is 228 and black 32.
                let mut ink = 0.0;
                for sy in 0..3 {
                    for sx in 0..3 {
                        let (ou, ov) = ((sx as f64 - 1.0) / 3.0, (sy as f64 - 1.0) / 3.0);
                        let (pu, pv) = (u + ou * cos - ov * sin, v + ou * sin + ov * cos);
                        let (mx, my) = (((pu + half) * scale) as usize, ((pv + half) * scale) as usize);
                        ink += code.pixels[my.min(code.height - 1) * code.width + mx.min(code.width - 1)] as f64 / 255.0;
                    }
                }
                32.0 + ink / 9.0 * 196.0
            } else if u.abs() <= body && v.abs() <= body * 1.6 {
                14.0
            } else {
                room
            };
            pixels[y * side + x] = value as u8;
        }
    }
    let mut picture = qr::Gray { width: side, height: side, pixels };
    if options.blur > 0 {
        picture = box_blur(&picture, options.blur);
    }
    if options.noise > 0 {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let span = 2 * options.noise as u64 + 1;
        for pixel in picture.pixels.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let delta = (state % span) as i32 - options.noise as i32;
            *pixel = (*pixel as i32 + delta).clamp(0, 255) as u8;
        }
    }
    picture
}

/// A color picture as a camera sees it: the same phone and room as [`webcam_picture`], each
/// channel leaking into the others by `crosstalk` (red into green fully, into blue by half,
/// and so on), a slight gamma, noise and blur, and color stored at half resolution (4:2:0),
/// as webcams and their video formats do.
pub fn webcam_color_picture(frames: &[Vec<u8>], options: &BenchOptions, seed: u64) -> Result<color::Rgb> {
    let clean = BenchOptions { blur: 0, noise: 0, sample: None, ..*options };
    let layers: Vec<qr::Gray> = frames.iter().map(|f| qr::draw(f, 1, 4).map(|code| webcam_picture(&code, &clean, seed))).collect::<Result<_>>()?;
    let side = options.picture;
    let x = options.crosstalk as f32;
    let mix = [[1.0, x, x * 0.5], [x, 1.0, x], [x * 0.5, x, 1.0]];
    let mut channels: Vec<Vec<u8>> = (0..3)
        .map(|c| {
            let row: [f32; 3] = mix[c];
            let sum: f32 = row.iter().sum();
            (0..side * side)
                .map(|i| {
                    let v: f32 = (0..3).map(|j| row[j] * layers[j].pixels[i] as f32 / 255.0).sum::<f32>() / sum;
                    (v.powf(1.1) * 255.0) as u8
                })
                .collect()
        })
        .collect();
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for channel in channels.iter_mut() {
        if options.noise > 0 {
            let span = 2 * options.noise as u64 + 1;
            for pixel in channel.iter_mut() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *pixel = (*pixel as i32 + (state % span) as i32 - options.noise as i32).clamp(0, 255) as u8;
            }
        }
        if options.blur > 0 {
            *channel = box_blur(&qr::Gray { width: side, height: side, pixels: std::mem::take(channel) }, options.blur).pixels;
        }
    }
    // 4:2:0: brightness at full resolution, the two color differences averaged over 2 by 2.
    let mut pixels = vec![0u8; side * side * 3];
    for by in (0..side).step_by(2) {
        for bx in (0..side).step_by(2) {
            let block: Vec<usize> = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().map(|(dx, dy)| (by + dy).min(side - 1) * side + (bx + dx).min(side - 1)).collect();
            let ycc = |i: usize| {
                let (r, g, b) = (channels[0][i] as f32, channels[1][i] as f32, channels[2][i] as f32);
                let y = 0.299 * r + 0.587 * g + 0.114 * b;
                (y, b - y, r - y)
            };
            let (mut cb, mut cr) = (0.0, 0.0);
            for &i in &block {
                let (_, b, r) = ycc(i);
                cb += b / 4.0;
                cr += r / 4.0;
            }
            for &i in &block {
                let (y, _, _) = ycc(i);
                let (r, b) = (y + cr, y + cb);
                let g = (y - 0.299 * r - 0.114 * b) / 0.587;
                pixels[i * 3] = r.clamp(0.0, 255.0) as u8;
                pixels[i * 3 + 1] = g.clamp(0.0, 255.0) as u8;
                pixels[i * 3 + 2] = b.clamp(0.0, 255.0) as u8;
            }
        }
    }
    color::Rgb::new(side, side, pixels)
}

fn box_blur(picture: &qr::Gray, radius: usize) -> qr::Gray {
    let (w, h) = (picture.width, picture.height);
    let pass = |source: &[u8], horizontal: bool| -> Vec<u8> {
        let mut out = vec![0u8; w * h];
        for a in 0..(if horizontal { h } else { w }) {
            for b in 0..(if horizontal { w } else { h }) {
                let (lo, hi) = (b.saturating_sub(radius), (b + radius).min(if horizontal { w } else { h } - 1));
                let sum: u32 = (lo..=hi).map(|c| source[if horizontal { a * w + c } else { c * w + a }] as u32).sum();
                out[if horizontal { a * w + b } else { b * w + a }] = (sum / (hi - lo + 1) as u32) as u8;
            }
        }
        out
    };
    let pixels = pass(&pass(&picture.pixels, true), false);
    qr::Gray { width: w, height: h, pixels }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Times the page's QR reader on webcam-like pictures of frames of a given size.
pub fn bench(options: &BenchOptions) -> Result<Value> {
    if options.code_px + 8 > options.picture {
        bail!("the code ({} px) must fit inside the picture ({} px)", options.code_px, options.picture);
    }
    let frames = options.frames.max(4);
    // A payload of about two thirds as many chunks as frames, so the flash completes.
    let k = (frames * 2 / 3).max(2);
    // Random-looking bytes, as a sealed payload is (a regular pattern can make neighbouring
    // chunks look alike, which would flatter a color picture).
    let mut state = 0x5EED_5EED_5EED_5EEDu64;
    let payload: Vec<u8> = (0..k * options.chunk)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect();
    let encoder = Encoder::new(0x5EED_0001, &payload, options.chunk).map_err(|e| anyhow!(e))?;
    let first = encoder.frame(0);
    let modules = qr::modules(&first)?.len();
    let shape = fountain::parse(&first).map_err(|e| anyhow!("{e}"))?.shape;
    let mut decoder = Decoder::new(shape);
    let mut times = Vec::with_capacity(frames);
    let mut read = 0usize;
    let mut complete_after = None;
    let per_picture = if options.color { 3 } else { 1 };
    let mut pictures = 0usize;
    let mut index = 0u32;
    while (index as usize) < frames {
        let shown: Vec<Vec<u8>> = (0..per_picture as u32).map(|c| encoder.frame(index + c)).collect();
        let started;
        let codes = if options.color {
            let picture = webcam_color_picture(&shown, options, index as u64 + 1)?;
            if index == 0
                && let Some(path) = &options.sample
            {
                color::write_png(path, &picture)?;
            }
            started = std::time::Instant::now();
            color::read_picture(&picture, true).codes
        } else {
            let picture = webcam_picture(&qr::draw(&shown[0], 1, 4)?, options, index as u64 + 1);
            if index == 0
                && let Some(path) = &options.sample
            {
                qr::write_png(path, &picture)?;
            }
            started = std::time::Instant::now();
            qr::read_camera(&picture, true)
        };
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        pictures += 1;
        for bytes in &shown {
            if codes.contains(bytes) {
                read += 1;
                let frame = fountain::parse(bytes).map_err(|e| anyhow!("{e}"))?;
                decoder.add(frame.index, frame.body);
                if decoder.is_complete() && complete_after.is_none() {
                    complete_after = Some(pictures);
                }
            }
        }
        index += per_picture as u32;
    }
    let frames = pictures * per_picture;
    // A picture with no code at all: what every picture costs while the phone is not up yet
    // (the quick look), and right after codes were in view (the thorough one).
    let empty = qr::Gray { width: options.picture, height: options.picture, pixels: vec![60; options.picture * options.picture] };
    let time_empty = |thorough: bool| {
        let mut times: Vec<f64> = (0..10)
            .map(|_| {
                let started = std::time::Instant::now();
                let _ = qr::read_camera(&empty, thorough);
                started.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        (percentile(&times, 0.5) * 10.0).round() / 10.0
    };
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let median = percentile(&sorted, 0.5);
    let success = read as f64 / frames as f64;
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    // Codes a second: pictures a second, times the frames each picture carries.
    let rate = |fps: f64| (fps * per_picture as f64 * options.chunk as f64 * success).round();
    Ok(json!({
        "chunk": options.chunk,
        "frame_bytes": first.len(),
        "qr_version": (modules - 17) / 4,
        "modules": modules,
        "picture_px": options.picture,
        "code_px": options.code_px,
        "px_per_module": ((options.code_px as f64 / (modules + 8) as f64) * 100.0).round() / 100.0,
        "tilt_degrees": options.tilt,
        "blur_px": options.blur,
        "noise": options.noise,
        "color": options.color,
        "crosstalk": if options.color { Some(options.crosstalk) } else { None },
        "pictures": pictures,
        "frames": frames,
        "read": read,
        "read_rate": (success * 1000.0).round() / 1000.0,
        "complete_after_pictures": complete_after,
        "k": k,
        "decode_ms": {
            "min": (sorted[0] * 10.0).round() / 10.0,
            "median": (median * 10.0).round() / 10.0,
            "p95": (percentile(&sorted, 0.95) * 10.0).round() / 10.0,
            "max": (sorted[sorted.len() - 1] * 10.0).round() / 10.0,
        },
        "decode_ms_without_a_code": { "waiting": time_empty(false), "codes_in_view": time_empty(true) },
        "pictures_per_second_one_core": (1000.0 / median).round(),
        "cores": cores,
        "bytes_per_second_at_pictures_a_second": { "12": rate(12.0), "20": rate(20.0), "30": rate(30.0) },
    }))
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
            color: false,
        }
    }

    #[test]
    fn color_pictures_carry_three_frames_each() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("color");
        let mut options = encode_options(&out);
        options.color = true;
        options.chunk = 900;
        options.count = 400;
        let made = encode(&options).unwrap();
        let k = made["k"].as_u64().unwrap() as usize;
        let pictures = made["pictures"].as_u64().unwrap() as usize;
        assert_eq!((made["color"].as_bool(), pictures), (Some(true), (k + 20 + k / 20).div_ceil(3)));
        assert_eq!(png_files(&out).unwrap().len(), pictures);
        // A third as many pictures as a black and white flash, and they open just the same.
        let report = decode(&DecodeOptions { dir: out.clone(), secret: Some(SECRET.to_string()), import: false }).unwrap();
        assert_eq!((report["complete"].as_bool(), report["items"].as_u64()), (Some(true), Some(400)), "{report}");
        assert_eq!(report["color_pictures"].as_u64(), Some(pictures as u64));
        assert!(report["frames"].as_u64().unwrap() as usize >= k);
        // flash-read lists the three codes of each picture.
        let lines = read_folder(&out, true).unwrap();
        assert_eq!(lines.len(), pictures);
        assert!(lines.iter().all(|l| l["color"] == true && l["codes"].as_array().unwrap().len() == 3), "{:?}", lines[0]);
        // Mixed with black and white pictures of the same flash, everything still adds up.
        let mut gray = encode_options(&out);
        gray.chunk = 900;
        gray.count = 400;
        gray.first_frame = 5000;
        gray.frames = Some(4);
        encode(&gray).unwrap();
        let report = decode(&DecodeOptions { dir: out, secret: Some(SECRET.to_string()), import: false }).unwrap();
        assert_eq!((report["complete"].as_bool(), report["color_pictures"].as_u64()), (Some(true), Some(pictures as u64)));
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
