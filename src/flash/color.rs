//! Color Flash: one picture carries up to three frames, one in each of red, green and blue.
//!
//! The phone draws three ordinary frames of the same size on top of each other, frame A in the
//! red channel, B in green, C in blue: a light module is 255 in its channel and a dark one 0.
//! Where all three are light the picture is white, where all are dark it is black, and in
//! between it shows the six other corners of the color cube. The finder patterns and the
//! quiet zone line up, so they are black and white as in any QR code (PROTOCOL.md, "Color
//! Flash").
//!
//! A camera does not see the three channels cleanly. A phone screen's red light also lands a
//! little in the camera's green, and so on (crosstalk), and the levels shift with exposure.
//! What the camera sees is close to `black + M * s`, where `s` holds the three channel bits of
//! a module and `M` is a 3 by 3 mixing matrix. [`Unmix::estimate`] finds `black` and `M` from
//! the picture itself: the pixels of a color code gather around the eight colors, so it sorts
//! a sample of pixels into eight groups (k-means), labels each group with the corner of the
//! cube it is nearest to, and fits `black` and `M` to the groups by least squares. Turning
//! `M` around gives each channel back on its own, which is then read like any black and white
//! code. When the picture does not have enough distinct colors to fit, the raw channels are
//! read instead.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use super::qr::{self, Gray};

/// A color picture: three bytes (red, green, blue) per pixel, rows from the top.
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Rgb {
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Result<Rgb> {
        if width == 0 || height == 0 || width > qr::MAX_SIDE || height > qr::MAX_SIDE {
            bail!("a picture must be between 1 and {} pixels on each side", qr::MAX_SIDE);
        }
        if pixels.len() != width * height * 3 {
            bail!("a {width} by {height} color picture is {} bytes, not {}", width * height * 3, pixels.len());
        }
        Ok(Rgb { width, height, pixels })
    }

    /// The picture in gray, weighted the way the page weighs it.
    pub fn luminance(&self) -> Gray {
        let pixels = self.pixels.chunks_exact(3).map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8).collect();
        Gray { width: self.width, height: self.height, pixels }
    }

    /// One channel as it is, without unmixing.
    fn raw_channel(&self, channel: usize) -> Gray {
        let pixels = self.pixels.chunks_exact(3).map(|p| p[channel]).collect();
        Gray { width: self.width, height: self.height, pixels }
    }

    /// About `target` evenly spread pixels.
    fn sample(&self, target: usize) -> Vec<[f32; 3]> {
        let count = self.width * self.height;
        let step = (count / target.max(1)).max(1);
        // An odd step walks diagonally through the rows instead of down the same columns.
        let step = step | 1;
        (0..count).step_by(step).map(|i| [self.pixels[3 * i] as f32, self.pixels[3 * i + 1] as f32, self.pixels[3 * i + 2] as f32]).collect()
    }
}

/// How much of a picture is strongly colored: the share of sampled pixels whose brightest and
/// darkest channels differ by more than a quarter of the range. A black and white code under
/// warm light stays well below a color code, which is about three quarters colored pixels.
pub fn colorfulness(rgb: &Rgb) -> f64 {
    let sample = rgb.sample(4096);
    if sample.is_empty() {
        return 0.0;
    }
    let colored = sample
        .iter()
        .filter(|p| {
            let (max, min) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            max - min > 64.0
        })
        .count();
    colored as f64 / sample.len() as f64
}

/// Above this share of colored pixels a picture is read as a color code.
pub const COLORFUL: f64 = 0.12;

/// What a camera did to the three channels, and how to undo it.
#[derive(Clone, Debug, PartialEq)]
pub struct Unmix {
    /// What a module with every channel dark looks like.
    pub black: [f32; 3],
    /// Column `j` is what turning on channel `j` adds.
    pub mix: [[f32; 3]; 3],
    inverse: [[f32; 3]; 3],
}

fn distance(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn invert(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-6 {
        return None;
    }
    let mut out = [[0f32; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            // The cofactor of (c, r), for the transpose.
            let (r1, r2) = ((c + 1) % 3, (c + 2) % 3);
            let (c1, c2) = ((r + 1) % 3, (r + 2) % 3);
            *cell = (m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]) / det;
        }
    }
    Some(out)
}

/// Solves the 4 by 4 system `a x = b` by elimination with pivoting.
fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> Option<[f64; 4]> {
    for col in 0..4 {
        let pivot = (col..4).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[pivot][col].abs() < 1e-9 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let pivot_row = a[col];
        let pivot_b = b[col];
        for (row, (cells, rhs)) in a.iter_mut().zip(b.iter_mut()).enumerate() {
            if row != col {
                let factor = cells[col] / pivot_row[col];
                for (cell, p) in cells.iter_mut().zip(pivot_row).skip(col) {
                    *cell -= factor * p;
                }
                *rhs -= factor * pivot_b;
            }
        }
    }
    Some([b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2], b[3] / a[3][3]])
}

impl Unmix {
    /// Finds the black level and the mixing matrix from the picture's own pixels, or None when
    /// it does not show enough of the eight colors to tell.
    pub fn estimate(rgb: &Rgb) -> Option<Unmix> {
        let sample = rgb.sample(6000);
        if sample.len() < 64 {
            return None;
        }
        // Start the eight groups at the corners of the box the colors span.
        let mut low = [0f32; 3];
        let mut high = [0f32; 3];
        for c in 0..3 {
            let mut values: Vec<f32> = sample.iter().map(|p| p[c]).collect();
            values.sort_by(f32::total_cmp);
            low[c] = values[values.len() / 50];
            high[c] = values[values.len() - 1 - values.len() / 50];
            if high[c] - low[c] < 24.0 {
                return None;
            }
        }
        let corner = |bits: usize| -> [f32; 3] { std::array::from_fn(|c| if bits >> c & 1 == 1 { high[c] } else { low[c] }) };
        let mut centers: Vec<[f32; 3]> = (0..8).map(corner).collect();
        let mut counts = [0usize; 8];
        let mut labels = vec![0usize; sample.len()];
        for _ in 0..12 {
            for (label, pixel) in labels.iter_mut().zip(&sample) {
                *label = (0..8).min_by(|&a, &b| distance(pixel, &centers[a]).total_cmp(&distance(pixel, &centers[b]))).unwrap_or(0);
            }
            let mut sums = [[0f64; 3]; 8];
            counts = [0; 8];
            for (label, pixel) in labels.iter().zip(&sample) {
                counts[*label] += 1;
                for c in 0..3 {
                    sums[*label][c] += pixel[c] as f64;
                }
            }
            for k in 0..8 {
                if counts[k] > 0 {
                    centers[k] = std::array::from_fn(|c| (sums[k][c] / counts[k] as f64) as f32);
                }
            }
        }
        // Which corner each group stands for: each channel on when it is past the middle of
        // that channel's range. Small groups (strays, edges between modules) are left out.
        let mut rows: Vec<([f64; 4], [f64; 3], f64)> = Vec::new();
        let mut seen_corners = [false; 8];
        for k in 0..8 {
            if counts[k] < sample.len() / 100 {
                continue;
            }
            let bits: [f64; 3] = std::array::from_fn(|c| if centers[k][c] > (low[c] + high[c]) / 2.0 { 1.0 } else { 0.0 });
            let id = bits[0] as usize | (bits[1] as usize) << 1 | (bits[2] as usize) << 2;
            seen_corners[id] = true;
            rows.push(([1.0, bits[0], bits[1], bits[2]], std::array::from_fn(|c| centers[k][c] as f64), counts[k] as f64));
        }
        // Black, white and at least two single or double colors, so the fit is determined.
        if !seen_corners[0] || !seen_corners[7] || seen_corners.iter().filter(|s| **s).count() < 5 {
            return None;
        }
        // Least squares, weighted by group size: for each camera channel c,
        // value = black[c] + mix[c][0] * s0 + mix[c][1] * s1 + mix[c][2] * s2.
        let mut normal = [[0f64; 4]; 4];
        let mut rhs = [[0f64; 4]; 3];
        for (x, y, w) in &rows {
            for i in 0..4 {
                for j in 0..4 {
                    normal[i][j] += w * x[i] * x[j];
                }
                for c in 0..3 {
                    rhs[c][i] += w * x[i] * y[c];
                }
            }
        }
        let mut black = [0f32; 3];
        let mut mix = [[0f32; 3]; 3];
        for c in 0..3 {
            let solution = solve4(normal, rhs[c])?;
            black[c] = solution[0] as f32;
            for j in 0..3 {
                mix[c][j] = solution[j + 1] as f32;
            }
        }
        // Each channel must mostly answer to its own light.
        for (c, row) in mix.iter().enumerate() {
            if row[c] < 16.0 || row.iter().enumerate().any(|(j, v)| j != c && v.abs() > row[c]) {
                return None;
            }
        }
        let inverse = invert(&mix)?;
        Some(Unmix { black, mix, inverse })
    }

    /// The three channels on their own, as gray pictures: 255 where the channel is light.
    pub fn channels(&self, rgb: &Rgb) -> [Gray; 3] {
        let n = rgb.width * rgb.height;
        let mut out = [vec![0u8; n], vec![0u8; n], vec![0u8; n]];
        for (i, p) in rgb.pixels.chunks_exact(3).enumerate() {
            let d = [p[0] as f32 - self.black[0], p[1] as f32 - self.black[1], p[2] as f32 - self.black[2]];
            for (c, channel) in out.iter_mut().enumerate() {
                let s = self.inverse[c][0] * d[0] + self.inverse[c][1] * d[1] + self.inverse[c][2] * d[2];
                channel[i] = (s * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
        out.map(|pixels| Gray { width: rgb.width, height: rgb.height, pixels })
    }
}

/// What reading one color picture found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorRead {
    pub codes: Vec<Vec<u8>>,
    /// True when the picture was read as a color code (its channels one by one).
    pub color: bool,
}

fn add_unique(into: &mut Vec<Vec<u8>>, found: Vec<Vec<u8>>) {
    for code in found {
        if !into.contains(&code) {
            into.push(code);
        }
    }
}

/// Every code in a color picture, black and white or color. `thorough` as for
/// [`qr::read_camera`].
///
/// A colorful picture is unmixed and its three channels read one by one (the raw channels
/// too, when unmixing is not possible or finds nothing); otherwise, or when the channels hold
/// nothing, the picture is read in gray.
pub fn read_picture(rgb: &Rgb, thorough: bool) -> ColorRead {
    read_picture_with(rgb, thorough, true)
}

/// [`read_picture`], optionally without unmixing (for measuring what unmixing is worth).
pub fn read_picture_with(rgb: &Rgb, thorough: bool, unmix: bool) -> ColorRead {
    let mut read = ColorRead::default();
    if colorfulness(rgb) >= COLORFUL {
        let unmixed = if unmix { Unmix::estimate(rgb).map(|u| u.channels(rgb)) } else { None };
        if let Some(channels) = &unmixed {
            for channel in channels {
                add_unique(&mut read.codes, qr::read_camera(channel, thorough));
            }
        }
        if read.codes.len() < 3 && (unmixed.is_none() || thorough) {
            for c in 0..3 {
                add_unique(&mut read.codes, qr::read_camera(&rgb.raw_channel(c), thorough));
            }
        }
        if !read.codes.is_empty() {
            read.color = true;
            return read;
        }
    }
    read.codes = qr::read_camera(&rgb.luminance(), thorough);
    read
}

// -------------------------------------------------------------------- drawing and files

/// Up to three frames of the same length as one color picture: frame A in red, B in green, C
/// in blue. `scale` pixels per module, `quiet` modules of white around it. With fewer than
/// three frames the missing channels repeat the last one given, so every channel still reads.
pub fn draw(frames: &[&[u8]], scale: usize, quiet: usize) -> Result<Rgb> {
    if frames.is_empty() || frames.len() > 3 {
        bail!("a color picture holds one to three frames, not {}", frames.len());
    }
    let grids: Vec<Vec<Vec<bool>>> = frames.iter().map(|f| qr::modules(f)).collect::<Result<_>>()?;
    let size = grids[0].len();
    if grids.iter().any(|g| g.len() != size) {
        bail!("the frames of one color picture must make QR codes of the same size");
    }
    let scale = scale.max(1);
    let side = (size + 2 * quiet) * scale;
    let mut pixels = vec![255u8; side * side * 3];
    for row in 0..size {
        for column in 0..size {
            let (x0, y0) = ((column + quiet) * scale, (row + quiet) * scale);
            for c in 0..3 {
                let grid = &grids[c.min(grids.len() - 1)];
                if !grid[row][column] {
                    continue;
                }
                for y in y0..y0 + scale {
                    for x in x0..x0 + scale {
                        pixels[(y * side + x) * 3 + c] = 0;
                    }
                }
            }
        }
    }
    Rgb::new(side, side, pixels)
}

pub fn write_png(path: &Path, rgb: &Rgb) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), rgb.width as u32, rgb.height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgb.pixels)?;
    writer.finish()?;
    Ok(())
}

/// Reads a PNG file in color. Transparent pixels count as white.
pub fn read_png(path: &Path) -> Result<Rgb> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().with_context(|| format!("{} is not a PNG picture", path.display()))?;
    let size = reader.output_buffer_size().ok_or_else(|| anyhow!("{} is too large", path.display()))?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width > qr::MAX_SIDE || height > qr::MAX_SIDE {
        bail!("{} is larger than {} pixels on a side", path.display(), qr::MAX_SIDE);
    }
    let mut buffer = vec![0u8; size];
    let frame = reader.next_frame(&mut buffer).with_context(|| format!("{} is a damaged PNG picture", path.display()))?;
    let data = &buffer[..frame.buffer_size()];
    let over_white = |value: u8, alpha: u8| ((value as u32 * alpha as u32 + 255 * (255 - alpha as u32)) / 255) as u8;
    let pixels: Vec<u8> = match frame.color_type {
        png::ColorType::Grayscale => data.iter().flat_map(|v| [*v, *v, *v]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [over_white(p[0], p[1]); 3]).collect(),
        png::ColorType::Rgb => data.to_vec(),
        png::ColorType::Rgba => data.chunks_exact(4).flat_map(|p| [over_white(p[0], p[3]), over_white(p[1], p[3]), over_white(p[2], p[3])]).collect(),
        png::ColorType::Indexed => bail!("{} uses a palette that could not be expanded", path.display()),
    };
    Rgb::new(width, height, pixels).with_context(|| format!("{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// A camera's view: channels leak into each other by `crosstalk` (the pattern of the
    /// simulation the phone side used), the levels are lifted and squeezed, and noise is added.
    fn through_camera(rgb: &Rgb, crosstalk: f32, noise: i32, seed: u64) -> Rgb {
        let x = crosstalk;
        let m = [[1.0, x, x * 0.5], [x, 1.0, x], [x * 0.5, x, 1.0]];
        let mut state = seed | 1;
        let mut pixels = Vec::with_capacity(rgb.pixels.len());
        for p in rgb.pixels.chunks_exact(3) {
            let s = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
            for row in &m {
                let sum: f32 = row.iter().sum();
                let v = (row[0] * s[0] + row[1] * s[1] + row[2] * s[2]) / sum;
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let jitter = (state % (2 * noise as u64 + 1)) as i32 - noise;
                pixels.push((30.0 + v * 190.0 + jitter as f32).clamp(0.0, 255.0) as u8);
            }
        }
        Rgb { width: rgb.width, height: rgb.height, pixels }
    }

    #[test]
    fn three_frames_make_one_picture_of_eight_colors() {
        let frames = [bytes(317, 1), bytes(317, 2), bytes(317, 3)];
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let picture = draw(&refs, 4, 4).unwrap();
        let mut colors = std::collections::HashSet::new();
        for p in picture.pixels.chunks_exact(3) {
            colors.insert((p[0], p[1], p[2]));
        }
        assert_eq!(colors.len(), 8, "black, white and the six others");
        assert!(colorfulness(&picture) > 0.4);
        // The corner of the quiet zone is white and a finder pattern's middle is black.
        assert_eq!(&picture.pixels[..3], &[255, 255, 255]);
        let middle = ((4 + 3) * 4 + 2) * picture.width + (4 + 3) * 4 + 2;
        assert_eq!(&picture.pixels[middle * 3..middle * 3 + 3], &[0, 0, 0]);
        // Read clean, each channel gives back its own frame.
        let read = read_picture(&picture, false);
        assert!(read.color);
        for frame in &frames {
            assert!(read.codes.contains(frame));
        }
        assert_eq!(read.codes.len(), 3);
        // Frames of different sizes do not go in one picture.
        assert!(draw(&[&bytes(317, 1), &bytes(900, 2)], 4, 4).is_err());
        assert!(draw(&[], 4, 4).is_err());
    }

    #[test]
    fn the_mixing_is_found_and_undone() {
        let frames = [bytes(917, 4), bytes(917, 5), bytes(917, 6)];
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let clean = draw(&refs, 4, 4).unwrap();
        let seen = through_camera(&clean, 0.3, 8, 7);
        let unmix = Unmix::estimate(&seen).expect("enough colors to fit");
        // Black near the lifted level, and each channel's own light much stronger than the leak.
        for c in 0..3 {
            assert!((unmix.black[c] - 30.0).abs() < 12.0, "{unmix:?}");
            for j in 0..3 {
                if j != c {
                    assert!(unmix.mix[c][j] < unmix.mix[c][c] * 0.5, "{unmix:?}");
                    assert!(unmix.mix[c][j] > 0.0, "the leak is seen: {unmix:?}");
                }
            }
        }
        // Unmixed, every channel reads.
        let read = read_picture(&seen, false);
        assert!(read.color);
        for frame in &frames {
            assert!(read.codes.contains(frame), "{} codes read", read.codes.len());
        }
    }

    #[test]
    fn black_and_white_pictures_are_read_in_gray() {
        let frame = bytes(317, 9);
        let gray = qr::draw(&frame, 4, 4).unwrap();
        // A black and white code under warm light: tinted, but not colorful.
        let pixels: Vec<u8> = gray.pixels.iter().flat_map(|v| [(*v as u32 * 250 / 255) as u8 + 5, (*v as u32 * 225 / 255) as u8 + 10, (*v as u32 * 190 / 255) as u8 + 12]).collect();
        let tinted = Rgb::new(gray.width, gray.height, pixels).unwrap();
        assert!(colorfulness(&tinted) < COLORFUL);
        let read = read_picture(&tinted, false);
        assert_eq!((read.codes, read.color), (vec![frame], false));
        // A gray picture with nothing in it.
        let blank = Rgb::new(40, 30, vec![90; 40 * 30 * 3]).unwrap();
        assert_eq!(read_picture(&blank, true), ColorRead::default());
        assert!(Unmix::estimate(&blank).is_none());
        assert!(Rgb::new(10, 10, vec![0; 299]).is_err());
    }

    #[test]
    fn inverse_and_solve() {
        let m = [[2.0, 0.5, 0.1], [0.3, 1.5, 0.2], [0.1, 0.4, 1.8]];
        let inv = invert(&m).unwrap();
        // m times its inverse is the identity.
        for (r, row) in m.iter().enumerate() {
            let product: Vec<f32> = (0..3).map(|c| row.iter().zip(&inv).map(|(x, inv_row)| x * inv_row[c]).sum()).collect();
            for (c, v) in product.iter().enumerate() {
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-5);
            }
        }
        assert!(invert(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 1.0, 1.0]]).is_none());
        let x = solve4([[4.0, 1.0, 0.0, 0.0], [1.0, 3.0, 1.0, 0.0], [0.0, 1.0, 2.0, 1.0], [0.0, 0.0, 1.0, 5.0]], [6.0, 10.0, 12.0, 23.0]).unwrap();
        for (got, want) in x.iter().zip([1.0, 2.0, 3.0, 4.0]) {
            assert!((got - want).abs() < 1e-9);
        }
    }

    #[test]
    fn png_files_in_color() {
        let dir = tempfile::tempdir().unwrap();
        let frames = [bytes(317, 1), bytes(317, 2), bytes(317, 3)];
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let picture = draw(&refs, 3, 4).unwrap();
        let path = dir.path().join("color.png");
        write_png(&path, &picture).unwrap();
        let back = read_png(&path).unwrap();
        assert_eq!(back.pixels, picture.pixels);
        // A gray PNG reads as a color picture with equal channels.
        let gray_path = dir.path().join("gray.png");
        qr::write_png(&gray_path, &qr::draw(&frames[0], 3, 4).unwrap()).unwrap();
        let gray = read_png(&gray_path).unwrap();
        assert!(gray.pixels.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
        assert_eq!(read_picture(&gray, true).codes, vec![frames[0].clone()]);
    }
}
