//! QR codes for Flash: reading them out of pictures, and drawing them.
//!
//! Reading works on plain grayscale pixels, so the same code serves the camera pictures the
//! local page posts and the PNG files `zone dev flash-decode` reads. The bytes of a code come
//! back exactly as they were encoded: a frame is binary and is never treated as text.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use qrcode::bits::Bits;
use qrcode::{Color, EcLevel, QrCode, Version};

/// The largest picture accepted, in pixels on a side. Camera pictures are far smaller.
pub const MAX_SIDE: usize = 8192;

/// A grayscale picture: one byte per pixel, rows from the top, 0 is black.
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Gray {
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Result<Gray> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            bail!("a picture must be between 1 and {MAX_SIDE} pixels on each side");
        }
        if pixels.len() != width * height {
            bail!("a {width} by {height} picture is {} bytes, not {}", width * height, pixels.len());
        }
        Ok(Gray { width, height, pixels })
    }

    /// Every pixel repeated `factor` times in both directions.
    fn enlarged(&self, factor: usize) -> Gray {
        let (width, height) = (self.width * factor, self.height * factor);
        let mut pixels = Vec::with_capacity(width * height);
        for row in self.pixels.chunks(self.width) {
            let wide: Vec<u8> = row.iter().flat_map(|p| std::iter::repeat_n(*p, factor)).collect();
            for _ in 0..factor {
                pixels.extend_from_slice(&wide);
            }
        }
        Gray { width, height, pixels }
    }

    /// Half the size: each pixel the average of four.
    fn halved(&self) -> Gray {
        let (width, height) = (self.width / 2, self.height / 2);
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            let (top, bottom) = (&self.pixels[2 * y * self.width..], &self.pixels[(2 * y + 1) * self.width..]);
            for x in 0..width {
                let sum = top[2 * x] as u16 + top[2 * x + 1] as u16 + bottom[2 * x] as u16 + bottom[2 * x + 1] as u16;
                pixels.push((sum / 4) as u8);
            }
        }
        Gray { width, height, pixels }
    }
}

/// ZXing's reader, through rxing's port of zxing-cpp's detector: fast, and steady with the
/// larger, turned, soft codes a camera sees. It keeps the decoded bytes of the code exactly as
/// encoded (for a flash frame, its one byte segment), never going through text.
fn read_zxing(gray: &Gray) -> Vec<Vec<u8>> {
    use rxing::common::HybridBinarizer;
    use rxing::qrcode::cpp_port::QrReader;
    use rxing::{BinaryBitmap, DecodeHints, Luma8Source, Reader};
    let Ok(source) = Luma8Source::new_with_slice(&gray.pixels, gray.width as u32, gray.height as u32) else {
        return Vec::new();
    };
    let hints = DecodeHints { TryHarder: Some(true), ..DecodeHints::default() };
    match QrReader.decode_with_hints(&mut BinaryBitmap::new(HybridBinarizer::new(source)), &hints) {
        Ok(result) if !result.getRawBytes().is_empty() => vec![result.getRawBytes().to_vec()],
        _ => Vec::new(),
    }
}

fn read_once(gray: &Gray) -> Vec<Vec<u8>> {
    let found = read_zxing(gray);
    if !found.is_empty() {
        return found;
    }
    read_quirc(gray)
}

/// quirc's reader (through rqrr), the fallback.
fn read_quirc(gray: &Gray) -> Vec<Vec<u8>> {
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(gray.width, gray.height, |x, y| gray.pixels[y * gray.width + x]);
    let mut found = Vec::new();
    for grid in prepared.detect_grids() {
        let mut bytes = Vec::new();
        if grid.decode_to(&mut bytes).is_ok() && !bytes.is_empty() {
            found.push(bytes);
        }
    }
    found
}

/// The bytes of every QR code found in a picture.
///
/// A code drawn very small (a pixel or two per module) or very large is tried again at a
/// friendlier size, so the scale a picture was saved at does not matter.
pub fn read_codes(gray: &Gray) -> Vec<Vec<u8>> {
    let found = read_once(gray);
    if !found.is_empty() {
        return found;
    }
    let longest = gray.width.max(gray.height);
    if longest <= 700 {
        let found = read_once(&gray.enlarged(if longest <= 250 { 4 } else { 2 }));
        if !found.is_empty() {
            return found;
        }
    }
    let mut smaller = gray.halved();
    while smaller.width.max(smaller.height) >= 300 {
        let found = read_once(&smaller);
        if !found.is_empty() {
            return found;
        }
        smaller = smaller.halved();
    }
    Vec::new()
}

/// The bytes of every QR code in a camera picture from the page.
///
/// The page posts many pictures a second, so this is the quick path, at the size the picture
/// came in: ZXing's reader (about 2.5 ms for 720 pixels, under 2 ms when there is no code),
/// then once more at half the size, which smooths out sensor noise.
///
/// `thorough` adds quirc's reader in between, for whatever is left. It costs about 10 ms on a
/// picture with no code in it, so the daemon asks for it only while codes are actually in
/// view, not while the page waits for a phone. [`read_codes`] also tries other sizes, for
/// files.
pub fn read_camera(gray: &Gray, thorough: bool) -> Vec<Vec<u8>> {
    let found = read_zxing(gray);
    if !found.is_empty() {
        return found;
    }
    if thorough {
        let found = read_quirc(gray);
        if !found.is_empty() {
            return found;
        }
    }
    if gray.width.min(gray.height) < 600 {
        return Vec::new();
    }
    read_zxing(&gray.halved())
}

/// Reads a PNG file as grayscale. Transparent pixels count as white paper. (Files are read in
/// color now, see [`super::color::read_png`]; this stays for the tests.)
#[cfg(test)]
pub fn read_png(path: &Path) -> Result<Gray> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().with_context(|| format!("{} is not a PNG picture", path.display()))?;
    let size = reader.output_buffer_size().ok_or_else(|| anyhow!("{} is too large", path.display()))?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width > MAX_SIDE || height > MAX_SIDE {
        bail!("{} is larger than {MAX_SIDE} pixels on a side", path.display());
    }
    let mut buffer = vec![0u8; size];
    let frame = reader.next_frame(&mut buffer).with_context(|| format!("{} is a damaged PNG picture", path.display()))?;
    let data = &buffer[..frame.buffer_size()];
    let luma = |r: u8, g: u8, b: u8| ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
    let over_white = |value: u8, alpha: u8| ((value as u32 * alpha as u32 + 255 * (255 - alpha as u32)) / 255) as u8;
    let pixels: Vec<u8> = match frame.color_type {
        png::ColorType::Grayscale => data.to_vec(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).map(|p| over_white(p[0], p[1])).collect(),
        png::ColorType::Rgb => data.chunks_exact(3).map(|p| luma(p[0], p[1], p[2])).collect(),
        png::ColorType::Rgba => data.chunks_exact(4).map(|p| over_white(luma(p[0], p[1], p[2]), p[3])).collect(),
        png::ColorType::Indexed => bail!("{} uses a palette that could not be expanded", path.display()),
    };
    Gray::new(width, height, pixels).with_context(|| format!("{}", path.display()))
}

/// The modules of a byte mode, level L QR code holding exactly `bytes`: `true` is dark, rows
/// of equal length, no quiet zone. The smallest version that fits is used, as the phone does.
pub fn modules(bytes: &[u8]) -> Result<Vec<Vec<bool>>> {
    for version in 1..=40 {
        let mut bits = Bits::new(Version::Normal(version));
        if bits.push_byte_data(bytes).is_err() || bits.push_terminator(EcLevel::L).is_err() {
            continue;
        }
        let code = QrCode::with_bits(bits, EcLevel::L).map_err(|e| anyhow!("cannot draw the QR code: {e}"))?;
        let width = code.width();
        return Ok(code.to_colors().chunks(width).map(|row| row.iter().map(|c| *c == Color::Dark).collect()).collect());
    }
    bail!("{} bytes do not fit in one QR code", bytes.len())
}

/// A QR code as a grayscale picture: `scale` pixels per module, `quiet` modules of white
/// around it.
pub fn draw(bytes: &[u8], scale: usize, quiet: usize) -> Result<Gray> {
    let modules = modules(bytes)?;
    let scale = scale.max(1);
    let side = (modules.len() + 2 * quiet) * scale;
    let mut pixels = vec![255u8; side * side];
    for (row, line) in modules.iter().enumerate() {
        for (column, dark) in line.iter().enumerate() {
            if !*dark {
                continue;
            }
            let (x0, y0) = ((column + quiet) * scale, (row + quiet) * scale);
            for y in y0..y0 + scale {
                pixels[y * side + x0..y * side + x0 + scale].fill(0);
            }
        }
    }
    Gray::new(side, side, pixels)
}

/// Writes a grayscale picture as a PNG file.
pub fn write_png(path: &Path, gray: &Gray) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), gray.width as u32, gray.height as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&gray.pixels)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every byte value, so a decoder that goes through text would be caught.
    fn binary(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 131 + 7) as u8).collect()
    }

    #[test]
    fn a_frame_sized_code_is_version_11() {
        // 17 bytes of header and 300 of body: 61 modules on a side (PROTOCOL.md, "Sizes").
        assert_eq!(modules(&binary(317)).unwrap().len(), 61);
        assert_eq!(modules(&binary(321)).unwrap().len(), 61);
        assert_eq!(modules(&binary(322)).unwrap().len(), 65);
        assert_eq!(modules(b"a").unwrap().len(), 21);
        assert!(modules(&binary(2953)).is_ok());
        assert!(modules(&binary(2954)).is_err());
    }

    #[test]
    fn binary_survives_at_any_scale_and_quiet_zone() {
        let bytes = binary(317);
        assert!(bytes.contains(&0) && bytes.contains(&0xff) && std::str::from_utf8(&bytes).is_err());
        for (scale, quiet) in [(1, 4), (2, 1), (3, 4), (4, 0), (5, 2), (8, 4), (10, 10), (24, 4)] {
            let picture = draw(&bytes, scale, quiet).unwrap();
            assert_eq!(read_codes(&picture), vec![bytes.clone()], "scale {scale} quiet {quiet}");
        }
        // Small and large codes too.
        for len in [1, 20, 100, 1000, 2953] {
            let bytes = binary(len);
            assert_eq!(read_codes(&draw(&bytes, 4, 4).unwrap()), vec![bytes], "{len} bytes");
        }
    }

    #[test]
    fn a_code_inside_a_larger_gray_picture() {
        // As a camera sees it: off center, on a background that is not white, dark gray ink.
        let bytes = binary(317);
        let code = draw(&bytes, 5, 4).unwrap();
        let (width, height) = (960, 720);
        let mut pixels = vec![150u8; width * height];
        let (left, top) = (410, 160);
        for y in 0..code.height {
            for x in 0..code.width {
                pixels[(top + y) * width + left + x] = if code.pixels[y * code.width + x] == 0 { 40 } else { 225 };
            }
        }
        assert_eq!(read_codes(&Gray::new(width, height, pixels).unwrap()), vec![bytes]);
        // A picture with no code in it.
        assert!(read_codes(&Gray::new(64, 48, vec![128; 64 * 48]).unwrap()).is_empty());
        assert!(Gray::new(10, 10, vec![0; 99]).is_err());
        assert!(Gray::new(0, 10, vec![]).is_err());
    }

    #[test]
    fn zxing_returns_the_exact_bytes() {
        // The first reader alone, on every byte value: no text decoding may touch them.
        for (len, scale) in [(317, 3), (717, 4), (917, 5), (2953, 3)] {
            let bytes = binary(len);
            assert_eq!(read_zxing(&draw(&bytes, scale, 4).unwrap()), vec![bytes], "{len} bytes");
        }
        // A code of only digits has no byte segment; it still counts as a code.
        let digits = QrCode::with_version(b"0123456789", Version::Normal(1), EcLevel::L).unwrap();
        let width = digits.width();
        let side = (width + 8) * 4;
        let mut pixels = vec![255u8; side * side];
        for (i, color) in digits.to_colors().iter().enumerate() {
            if *color == Color::Dark {
                let (x0, y0) = ((i % width + 4) * 4, (i / width + 4) * 4);
                for y in y0..y0 + 4 {
                    pixels[y * side + x0..y * side + x0 + 4].fill(0);
                }
            }
        }
        assert_eq!(read_codes(&Gray::new(side, side, pixels).unwrap()), vec![b"0123456789".to_vec()]);
    }

    #[test]
    fn the_quick_look_reads_nearly_every_clean_code() {
        // Without the slower reader: 900 byte codes at several sizes, almost all read.
        let mut missed = 0;
        for scale in [3, 4, 5, 6] {
            for seed in 0..40u64 {
                let bytes: Vec<u8> = (0..917).map(|i| ((i as u64 * 2_654_435_761 + seed * 97) >> 7) as u8).collect();
                if read_camera(&draw(&bytes, scale, 4).unwrap(), false) != vec![bytes] {
                    missed += 1;
                }
            }
        }
        assert!(missed <= 3, "{missed} of 160 missed");
    }

    #[test]
    fn clean_codes_always_read_on_the_camera_path() {
        // Perfectly sharp codes of random bytes, which trip up some finders: every one reads.
        for scale in [2, 3, 4, 6] {
            for seed in 0..60u64 {
                let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                let bytes: Vec<u8> = (0..317)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect();
                assert_eq!(read_camera(&draw(&bytes, scale, 4).unwrap(), true), vec![bytes], "scale {scale}, seed {seed}");
            }
        }
    }

    #[test]
    fn large_turned_codes_as_a_webcam_sees_them() {
        // Frames of 700 and 900 bytes (versions 18 and 21), turned 4 degrees, soft and noisy,
        // about 520 pixels wide in a 720 pixel picture: what the page posts.
        use crate::flash::dev::{BenchOptions, webcam_picture};
        for chunk in [700, 900] {
            let options = BenchOptions { chunk, code_px: 520, picture: 720, frames: 1, tilt: 4.0, blur: 1, noise: 6, sample: None, color: false, crosstalk: 0.0 };
            for seed in 0..3u64 {
                let bytes: Vec<u8> = (0..chunk + 17).map(|i| (i as u64 * 131 + seed * 7) as u8).collect();
                let picture = webcam_picture(&draw(&bytes, 1, 4).unwrap(), &options, seed + 1);
                assert_eq!(read_codes(&picture), vec![bytes], "chunk {chunk}, seed {seed}");
            }
        }
    }

    #[test]
    fn png_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = binary(317);
        let path = dir.path().join("code.png");
        write_png(&path, &draw(&bytes, 4, 4).unwrap()).unwrap();
        let back = read_png(&path).unwrap();
        assert_eq!((back.width, back.height), ((61 + 8) * 4, (61 + 8) * 4));
        assert_eq!(read_codes(&back), vec![bytes.clone()]);

        // The same picture saved as RGBA with a transparent border, as an app might.
        let gray = draw(&bytes, 3, 4).unwrap();
        let rgba: Vec<u8> = gray.pixels.iter().flat_map(|p| if *p == 0 { [10, 10, 10, 255] } else { [255, 255, 255, 0] }).collect();
        let path = dir.path().join("rgba.png");
        let mut encoder = png::Encoder::new(std::fs::File::create(&path).unwrap(), gray.width as u32, gray.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgba).unwrap();
        writer.finish().unwrap();
        assert_eq!(read_codes(&read_png(&path).unwrap()), vec![bytes]);

        std::fs::write(dir.path().join("not.png"), b"hello").unwrap();
        assert!(read_png(&dir.path().join("not.png")).is_err());
        assert!(read_png(&dir.path().join("missing.png")).is_err());
    }
}
