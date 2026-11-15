//! Color Flash: one picture carries up to three frames, one in each of red, green and blue.
//!
//! The phone draws three ordinary frames of the same size on top of each other, frame A in the
//! red channel, B in green, C in blue. In each channel a module is at one of two levels, a
//! dark one and a light one ([`Levels`]): 0 and 255 for the plain palette of black, white and
//! the six pure colors, or levels the phone chose to give the picture a designed look (red 60
//! and 255, green 0 and 200, blue 90 and 255, say). Either way a picture shows eight colors:
//! all-dark, all-light, and the six in between. The finder patterns and the quiet zone line
//! up, so they are all-dark and all-light (PROTOCOL.md, "Color Flash").
//!
//! A camera does not see the three channels cleanly. A phone screen's red light also lands a
//! little in the camera's green, and so on (crosstalk), and the levels shift with exposure.
//! What the camera sees is close to `black + M * s`, where `s` holds the three channel bits of
//! a module, `black` is the all-dark color and `M` is a 3 by 3 matrix that holds both the
//! palette's contrast and the camera's mixing. [`Unmix::estimate`] finds `black` and `M` from
//! the picture itself, so it needs to be told nothing about the palette:
//!
//! 1. It finds where the code is, as the part of the picture that is busy (a code has strong
//!    contrast between neighbouring pixels; a wall, a hand and the phone's body do not), and
//!    looks only at the middle of that. The room around the phone would otherwise be taken
//!    for one of the code's colors, which matters as soon as the palette's dark is not black.
//! 2. The pixels there gather around the eight colors, so it sorts a sample of them into
//!    eight groups (k-means), labels each group by which side of each channel's own range it
//!    is on, and fits `black` and `M` to the groups by least squares.
//!
//! Turning `M` around gives each channel back on its own, stretched to the full range
//! whatever the palette's contrast was, and it is then read like any black and white code.
//! When the picture does not have enough distinct colors to fit, the raw channels are read
//! instead.

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

    fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let i = 3 * (y * self.width + x);
        [self.pixels[i] as f32, self.pixels[i + 1] as f32, self.pixels[i + 2] as f32]
    }

    /// About `target` evenly spread pixels of a region.
    fn sample_in(&self, region: &Region, target: usize) -> Vec<[f32; 3]> {
        let (w, h) = (region.x1 - region.x0, region.y1 - region.y0);
        let step = (((w * h) as f64 / target.max(1) as f64).sqrt().floor() as usize).max(1);
        let mut out = Vec::with_capacity(target + target / 2);
        for (row, y) in (region.y0..region.y1).step_by(step).enumerate() {
            // Every other row starts half a step in, so the sample is not a square grid that
            // could fall in step with the modules.
            let start = region.x0 + if row % 2 == 1 { step / 2 } else { 0 };
            for x in (start..region.x1).step_by(step) {
                out.push(self.pixel(x, y));
            }
        }
        out
    }

    /// The pixels the palette and the mixing are worked out from: the middle of the code when
    /// one can be found, else the whole picture.
    #[cfg(test)]
    fn code_sample(&self, target: usize) -> Vec<[f32; 3]> {
        match code_middle(self) {
            Some(region) => self.sample_in(&region, target),
            None => self.sample(target),
        }
    }
}

/// A box of pixels: `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

/// The side of the blocks a picture is cut into to find the busy part of it.
const BLOCK: usize = 16;

/// The middle of the code in a picture, when there is something that looks like one.
///
/// A code is the busiest part of a camera picture: in every small block of it some channel
/// swings between its dark and light level, while the room, a hand and the phone's body are
/// smooth. The blocks that swing at least half as much as the busiest ones do are taken to be
/// the code. Their bounding box (with a few stray blocks trimmed off each side) holds the
/// code however the phone is turned, and the middle half of that box, in each direction, is
/// inside the code at any angle: no room, no phone body, no quiet zone.
pub fn code_middle(rgb: &Rgb) -> Option<Region> {
    let (across, down) = (rgb.width / BLOCK, rgb.height / BLOCK);
    if across < 6 || down < 6 {
        return None;
    }
    // How far the busiest channel swings in each block, from four by four pixels of it.
    let mut swings = Vec::with_capacity(across * down);
    for by in 0..down {
        for bx in 0..across {
            let (mut low, mut high) = ([255f32; 3], [0f32; 3]);
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = rgb.pixel(bx * BLOCK + sx * 4 + 2, by * BLOCK + sy * 4 + 2);
                    for c in 0..3 {
                        low[c] = low[c].min(p[c]);
                        high[c] = high[c].max(p[c]);
                    }
                }
            }
            swings.push((0..3).map(|c| high[c] - low[c]).fold(0.0, f32::max));
        }
    }
    let mut sorted = swings.clone();
    sorted.sort_by(f32::total_cmp);
    let busiest = sorted[sorted.len() * 9 / 10];
    if busiest < 24.0 {
        return None;
    }
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for (index, swing) in swings.iter().enumerate() {
        if *swing >= busiest / 2.0 {
            xs.push(index % across);
            ys.push(index / across);
        }
    }
    xs.sort_unstable();
    ys.sort_unstable();
    let trimmed = |sorted: &[usize]| (sorted[sorted.len() / 20], sorted[sorted.len() - 1 - sorted.len() / 20] + 1);
    let ((bx0, bx1), (by0, by1)) = (trimmed(&xs), trimmed(&ys));
    let (x0, x1, y0, y1) = (bx0 * BLOCK, bx1 * BLOCK, by0 * BLOCK, by1 * BLOCK);
    let (quarter_w, quarter_h) = ((x1 - x0) / 4, (y1 - y0) / 4);
    let region = Region { x0: x0 + quarter_w, y0: y0 + quarter_h, x1: x1 - quarter_w, y1: y1 - quarter_h };
    // Too small a patch does not show enough of the eight colors.
    (region.x1 - region.x0 >= 40 && region.y1 - region.y0 >= 40).then_some(region)
}

/// The palette of one color picture: for each channel (red, green, blue), the value of a dark
/// module and of a light one. The phone may pick any levels it likes for a picture, as long as
/// light is above dark in every channel, so its pictures can have a designed look instead of
/// the eight pure colors. The quiet zone is the all-light color, and a finder pattern's dark
/// is the all-dark color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Levels {
    pub dark: [u8; 3],
    pub light: [u8; 3],
}

impl Levels {
    /// Dark 0 and light 255 in every channel: black, white and the six pure colors.
    pub const FULL: Levels = Levels { dark: [0; 3], light: [255; 3] };

    /// The same span in every channel, with light at 255: `span` 150 is dark 105, light 255.
    pub fn with_span(span: u8) -> Levels {
        Levels { dark: [255 - span; 3], light: [255; 3] }
    }

    /// `r0,r1,g0,g1,b0,b1`: the dark then the light level of red, of green and of blue.
    pub fn parse(text: &str) -> Result<Levels> {
        let values: Vec<u8> = text
            .split(',')
            .map(|part| part.trim().parse::<u8>().map_err(|_| anyhow!("--levels takes six numbers from 0 to 255: r0,r1,g0,g1,b0,b1 (dark then light, for red, green and blue)")))
            .collect::<Result<_>>()?;
        let [r0, r1, g0, g1, b0, b1] = values[..] else {
            bail!("--levels takes six numbers: r0,r1,g0,g1,b0,b1 (dark then light, for red, green and blue), not {}", values.len());
        };
        let levels = Levels { dark: [r0, g0, b0], light: [r1, g1, b1] };
        if levels.min_span() == 0 {
            bail!("in --levels every light level must be above its dark level");
        }
        Ok(levels)
    }

    /// Light minus dark, per channel. Zero when light is not above dark.
    pub fn spans(&self) -> [u8; 3] {
        std::array::from_fn(|c| self.light[c].saturating_sub(self.dark[c]))
    }

    /// The smallest of the three spans: what decides how well the picture reads.
    pub fn min_span(&self) -> u8 {
        self.spans().into_iter().min().unwrap_or(0)
    }

    /// `r0,r1,g0,g1,b0,b1`, as [`Levels::parse`] reads it.
    pub fn text(&self) -> String {
        (0..3).map(|c| format!("{},{}", self.dark[c], self.light[c])).collect::<Vec<_>>().join(",")
    }
}

/// How much of a picture's code is colored: the share of pixels, in the middle of the code,
/// whose channels disagree.
///
/// Each channel is first measured against its own range there (its dark level is 0, its light
/// level 1), so the palette and the light in the room do not matter: in a black and white
/// code, even a tinted one, the three channels of a pixel then say the same thing, and in a
/// color code three quarters of the pixels have one channel light where another is dark.
#[cfg(test)]
pub fn colorfulness(rgb: &Rgb) -> f64 {
    colorfulness_of(&rgb.code_sample(4096))
}

/// The lowest and highest level of each channel in a sample, leaving out the extreme fiftieth
/// at each end. None when a channel hardly varies: nothing to read there.
fn channel_ranges(sample: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
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
    Some((low, high))
}

fn colorfulness_of(sample: &[[f32; 3]]) -> f64 {
    if sample.len() < 64 {
        return 0.0;
    }
    let Some((low, high)) = channel_ranges(sample) else {
        return 0.0;
    };
    let colored = sample
        .iter()
        .filter(|p| {
            let level: [f32; 3] = std::array::from_fn(|c| (p[c] - low[c]) / (high[c] - low[c]));
            level[0].max(level[1]).max(level[2]) - level[0].min(level[1]).min(level[2]) > 0.35
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

/// The side of the cubes the colors of a sample are counted in, to find where they are densest.
const BIN: usize = 8;
const BINS: usize = 256 / BIN;

/// The eight colors of a color code as a camera shows them: the eight places a sample of its
/// pixels is densest, each with how many pixels are there. None when eight cannot be found.
///
/// A module's middle shows one of the eight colors. Its edges, where the lens blurs it into
/// its neighbours, show everything in between, thinly. So the colors are the peaks of the
/// sample's density, and the in-between pixels, which pull averages toward gray when pixels
/// are simply sorted into eight groups, do not move a peak.
fn densest_colors(sample: &[[f32; 3]]) -> Option<([[f32; 3]; 8], [usize; 8])> {
    let bin = |p: &[f32; 3]| -> [usize; 3] { std::array::from_fn(|c| ((p[c].clamp(0.0, 255.0) as usize) / BIN).min(BINS - 1)) };
    let at = |b: [usize; 3]| (b[0] * BINS + b[1]) * BINS + b[2];
    let mut counts = vec![0u32; BINS * BINS * BINS];
    for p in sample {
        counts[at(bin(p))] += 1;
    }
    // How many pixels are in a cube and the 26 around it.
    let around = |b: [usize; 3]| -> u32 {
        let mut sum = 0;
        for x in b[0].saturating_sub(1)..=(b[0] + 1).min(BINS - 1) {
            for y in b[1].saturating_sub(1)..=(b[1] + 1).min(BINS - 1) {
                for z in b[2].saturating_sub(1)..=(b[2] + 1).min(BINS - 1) {
                    sum += counts[at([x, y, z])];
                }
            }
        }
        sum
    };
    let mut cubes: Vec<(u32, [usize; 3])> = Vec::new();
    for p in sample {
        let b = bin(p);
        // Each occupied cube once: when its first pixel comes by.
        if counts[at(b)] > 0 && !cubes.iter().rev().take(64).any(|(_, seen)| *seen == b) {
            cubes.push((around(b), b));
        }
    }
    cubes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    cubes.dedup_by(|a, b| a.1 == b.1);
    // The densest cubes, each well away from the ones taken before it.
    let apart = |a: [usize; 3], b: [usize; 3]| (0..3).map(|c| (a[c] as i32 - b[c] as i32).pow(2)).sum::<i32>() >= 9;
    let mut peaks: Vec<[usize; 3]> = Vec::new();
    for (dense, cube) in &cubes {
        if (*dense as usize) * 100 < sample.len() {
            break;
        }
        if peaks.iter().all(|peak| apart(*peak, *cube)) {
            peaks.push(*cube);
            if peaks.len() == 8 {
                break;
            }
        }
    }
    if peaks.len() < 8 {
        return None;
    }
    // Each color is the average of the pixels close to its peak.
    let middle = |b: [usize; 3]| -> [f32; 3] { std::array::from_fn(|c| (b[c] * BIN + BIN / 2) as f32) };
    let mut sums = [[0f64; 3]; 8];
    let mut near = [0usize; 8];
    let reach = (BIN as f32 * 1.5).powi(2);
    for p in sample {
        if let Some(k) = (0..8).find(|k| distance(p, &middle(peaks[*k])) <= reach) {
            near[k] += 1;
            for c in 0..3 {
                sums[k][c] += p[c] as f64;
            }
        }
    }
    if near.contains(&0) {
        return None;
    }
    Some((std::array::from_fn(|k| std::array::from_fn(|c| (sums[k][c] / near[k] as f64) as f32)), near))
}

/// Which corner of the color cube each of eight group centers is, from the shape they make.
/// Entry `k` is the corner of group `k`: bit 0 set when red is light, bit 1 green, bit 2 blue.
///
/// The eight colors a camera sees are `black + M * s` for the eight `s`, so they are the
/// corners of a slanted box: the all-dark color is its darkest corner, the all-light color its
/// brightest, three more are one step from dark along the box's three edges, and the last
/// three are sums of two of those steps. Of the twenty ways to pick the three edge corners,
/// the right one puts the other four where the box says they must be. That holds whatever
/// the palette is, also when one channel has much more contrast than another and its leak
/// into a neighbour is nearly as large as the neighbour's own signal, which is where telling
/// light from dark one channel at a time goes wrong.
fn corners_by_shape(centers: &[[f32; 3]]) -> Option<[usize; 8]> {
    let total = |k: usize| centers[k][0] + centers[k][1] + centers[k][2];
    let dark = (0..8).min_by(|a, b| total(*a).total_cmp(&total(*b)))?;
    let light = (0..8).max_by(|a, b| total(*a).total_cmp(&total(*b)))?;
    if dark == light {
        return None;
    }
    let rest: Vec<usize> = (0..8).filter(|k| *k != dark && *k != light).collect();
    let step = |k: usize| -> [f32; 3] { std::array::from_fn(|c| centers[k][c] - centers[dark][c]) };
    let sum = |a: &[f32; 3], b: &[f32; 3]| -> [f32; 3] { std::array::from_fn(|c| a[c] + b[c]) };
    let from_dark = |v: &[f32; 3], k: usize| distance(&sum(v, &centers[dark]), &centers[k]);
    // (cost, the three edge corners, the three corners two steps away: for edges 0+1, 0+2, 1+2)
    let mut best: Option<(f32, [usize; 3], [usize; 3])> = None;
    for a in 0..6 {
        for b in a + 1..6 {
            for c in b + 1..6 {
                let edges = [rest[a], rest[b], rest[c]];
                let others: Vec<usize> = rest.iter().copied().filter(|k| !edges.contains(k)).collect();
                let steps = edges.map(step);
                let pairs = [sum(&steps[0], &steps[1]), sum(&steps[0], &steps[2]), sum(&steps[1], &steps[2])];
                let all = sum(&pairs[0], &steps[2]);
                for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
                    let doubles = order.map(|i| others[i]);
                    let cost = from_dark(&all, light) + (0..3).map(|i| from_dark(&pairs[i], doubles[i])).sum::<f32>();
                    if best.as_ref().is_none_or(|(least, _, _)| cost < *least) {
                        best = Some((cost, edges, doubles));
                    }
                }
            }
        }
    }
    let (cost, edges, doubles) = best?;
    // The box must really be one: the four predicted corners within a fifth of an edge, on average.
    // And every edge must have some length: colors that are all alike make no box at all.
    let lengths = edges.map(|k| distance(&step(k), &[0.0; 3]).sqrt());
    let edge_length = lengths.iter().sum::<f32>() / 3.0;
    if lengths.iter().any(|length| *length < 8.0) || (cost / 4.0).sqrt() > edge_length / 5.0 {
        return None;
    }
    // Which edge is which light: each light is seen most by its own channel.
    let steps = edges.map(step);
    let strength = |edge: usize, channel: usize| steps[edge][channel] / distance(&steps[edge], &[0.0; 3]).sqrt().max(1.0);
    let channel_of = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]]
        .into_iter()
        .max_by(|p, q| (0..3).map(|e| strength(e, p[e])).sum::<f32>().total_cmp(&(0..3).map(|e| strength(e, q[e])).sum::<f32>()))?;
    let mut ids = [0usize; 8];
    ids[light] = 7;
    for (edge, k) in edges.iter().enumerate() {
        ids[*k] = 1 << channel_of[edge];
    }
    for (pair, k) in [(0, 1), (0, 2), (1, 2)].into_iter().zip(doubles) {
        ids[k] = 1 << channel_of[pair.0] | 1 << channel_of[pair.1];
    }
    Some(ids)
}

impl Unmix {
    /// Finds the black level and the mixing matrix from the picture's own pixels, or None when
    /// it does not show enough of the eight colors to tell. The middle of the code is tried
    /// first, then the whole picture.
    #[cfg(test)]
    pub fn estimate(rgb: &Rgb) -> Option<Unmix> {
        let middle = code_middle(rgb).and_then(|region| Unmix::fit(&rgb.sample_in(&region, 6000)));
        middle.or_else(|| Unmix::fit(&rgb.sample(6000)))
    }

    /// Fits the black level and the mixing matrix to a sample of a color code's pixels.
    fn fit(sample: &[[f32; 3]]) -> Option<Unmix> {
        if sample.len() < 64 {
            return None;
        }
        let (low, high) = channel_ranges(sample)?;
        // Three ways to find the eight colors and say which is which. Which one is best
        // depends on the picture, so each is tried and the one that explains the pixels best
        // is kept (see `misfit`).
        let labelled = |centers: &[[f32; 3]], counts: &[usize], ids: &[usize; 8]| -> Vec<([f64; 4], [f64; 3], f64)> {
            (0..8)
                .map(|k| {
                    let bits: [f64; 3] = std::array::from_fn(|c| (ids[k] >> c & 1) as f64);
                    ([1.0, bits[0], bits[1], bits[2]], std::array::from_fn(|c| centers[k][c] as f64), counts[k] as f64)
                })
                .collect()
        };
        let mut candidates: Vec<Unmix> = Vec::new();
        // 1. The eight places the pixels are densest, labelled by the shape they make. Best
        //    for a sharp picture in any palette: the pixels on the edges between modules,
        //    which show every color in between, do not move a peak.
        if let Some((centers, counts)) = densest_colors(sample)
            && let Some(ids) = corners_by_shape(&centers)
        {
            candidates.extend(Unmix::solve(&labelled(&centers, &counts, &ids)));
        }
        // 2 and 3. The pixels sorted into eight groups that start at the corners of the box
        //    the colors span. Best for a soft picture, where hardly a pixel shows a pure
        //    color, and the only way for a picture with fewer than eight colors (a frame
        //    repeated in two channels).
        let corner = |bits: usize| -> [f32; 3] { std::array::from_fn(|c| if bits >> c & 1 == 1 { high[c] } else { low[c] }) };
        let mut centers: Vec<[f32; 3]> = (0..8).map(corner).collect();
        let mut counts = [0usize; 8];
        let mut labels = vec![0usize; sample.len()];
        for _ in 0..12 {
            for (label, pixel) in labels.iter_mut().zip(sample) {
                *label = (0..8).min_by(|&a, &b| distance(pixel, &centers[a]).total_cmp(&distance(pixel, &centers[b]))).unwrap_or(0);
            }
            let mut sums = [[0f64; 3]; 8];
            counts = [0; 8];
            for (label, pixel) in labels.iter().zip(sample) {
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
        // 2. The groups labelled by the shape they make, when all eight are there.
        if (0..8).all(|k| counts[k] >= sample.len() / 100)
            && let Some(ids) = corners_by_shape(&centers)
        {
            candidates.extend(Unmix::solve(&labelled(&centers, &counts, &ids)));
        }
        // 3. Each group labelled one channel at a time: light when it is past the middle of
        //    that channel's range. Small groups (strays, edges between modules) are left out.
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
        if seen_corners[0] && seen_corners[7] && seen_corners.iter().filter(|s| **s).count() >= 5 {
            candidates.extend(Unmix::solve(&rows));
        }
        candidates.into_iter().map(|unmix| (unmix.misfit(sample), unmix)).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, unmix)| unmix)
    }

    /// How badly this fit explains a sample: unmixed, every channel of every pixel should be
    /// dark (0) or light (1), so this is how far they are from that, on average (squared).
    /// The pixels on the edges between modules are in between whatever the fit, and count the
    /// same against each.
    fn misfit(&self, sample: &[[f32; 3]]) -> f32 {
        let mut total = 0f32;
        for p in sample {
            let d = [p[0] - self.black[0], p[1] - self.black[1], p[2] - self.black[2]];
            for row in &self.inverse {
                let s = row[0] * d[0] + row[1] * d[1] + row[2] * d[2];
                total += s.abs().min((1.0 - s).abs()).powi(2);
            }
        }
        total / sample.len().max(1) as f32
    }

    /// Fits `black` and the mixing matrix to labelled colors: for each, which channels are
    /// light (after a leading 1), what the camera saw, and how many pixels it stands for.
    fn solve(rows: &[([f64; 4], [f64; 3], f64)]) -> Option<Unmix> {
        // Least squares, weighted by group size: for each camera channel c,
        // value = black[c] + mix[c][0] * s0 + mix[c][1] * s1 + mix[c][2] * s2.
        let mut normal = [[0f64; 4]; 4];
        let mut rhs = [[0f64; 4]; 3];
        for (x, y, w) in rows {
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
        // Each light must be seen, and seen most by its own channel. (A channel may well see
        // more of a neighbour's light than of its own, when the neighbour has much the larger
        // contrast in the palette: that still unmixes.)
        for (c, own) in mix.iter().enumerate() {
            if own[c] < 12.0 || mix.iter().enumerate().any(|(r, other)| r != c && other[c].abs() > own[c]) {
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
    let middle = code_middle(rgb);
    let sample = match &middle {
        Some(region) => rgb.sample_in(region, 6000),
        None => rgb.sample(6000),
    };
    if colorfulness_of(&sample) >= COLORFUL {
        let fitted = if unmix { Unmix::fit(&sample).or_else(|| middle.and_then(|_| Unmix::fit(&rgb.sample(6000)))) } else { None };
        let unmixed = fitted.map(|u| u.channels(rgb));
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
#[cfg(test)]
pub fn draw(frames: &[&[u8]], scale: usize, quiet: usize) -> Result<Rgb> {
    draw_with(frames, scale, quiet, &Levels::FULL)
}

/// [`draw`] in a palette: each channel's dark and light modules at the given levels, and the
/// quiet zone in the all-light color.
pub fn draw_with(frames: &[&[u8]], scale: usize, quiet: usize, levels: &Levels) -> Result<Rgb> {
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
    let mut pixels: Vec<u8> = levels.light.iter().copied().cycle().take(side * side * 3).collect();
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
                        pixels[(y * side + x) * 3 + c] = levels.dark[c];
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

    /// A phone on a desk: the picture in the middle of a larger, dim, slightly uneven scene.
    fn on_a_desk(rgb: &Rgb, margin: usize) -> Rgb {
        let (width, height) = (rgb.width + 2 * margin, rgb.height + 2 * margin);
        let mut pixels = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                if (margin..margin + rgb.width).contains(&x) && (margin..margin + rgb.height).contains(&y) {
                    let i = ((y - margin) * rgb.width + x - margin) * 3;
                    pixels.extend_from_slice(&rgb.pixels[i..i + 3]);
                } else {
                    let v = 40 + (y * 30 / height) as u8;
                    pixels.extend_from_slice(&[v, v, v]);
                }
            }
        }
        Rgb { width, height, pixels }
    }

    #[test]
    fn levels_are_read_and_drawn() {
        let levels = Levels::parse("60,255, 0,200,90,255").unwrap();
        assert_eq!((levels.dark, levels.light), ([60, 0, 90], [255, 200, 255]));
        assert_eq!((levels.spans(), levels.min_span(), levels.text()), ([195, 200, 165], 165, "60,255,0,200,90,255".to_string()));
        assert_eq!(Levels::parse("0,255,0,255,0,255").unwrap(), Levels::FULL);
        assert_eq!(Levels::with_span(150), Levels { dark: [105; 3], light: [255; 3] });
        for wrong in ["", "1,2,3", "0,255,0,255,0,256", "0,255,200,100,0,255", "0,255,90,90,0,255", "a,b,c,d,e,f"] {
            assert!(Levels::parse(wrong).is_err(), "{wrong}");
        }

        let frames = [bytes(317, 1), bytes(317, 2), bytes(317, 3)];
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let picture = draw_with(&refs, 4, 4, &levels).unwrap();
        let mut colors = std::collections::HashSet::new();
        for p in picture.pixels.chunks_exact(3) {
            colors.insert((p[0], p[1], p[2]));
        }
        assert_eq!(colors.len(), 8);
        assert!(colors.iter().all(|c| [60, 255].contains(&c.0) && [0, 200].contains(&c.1) && [90, 255].contains(&c.2)));
        // The quiet zone is the all-light color and a finder pattern's middle the all-dark one.
        assert_eq!(&picture.pixels[..3], &[255, 200, 255]);
        let middle = ((4 + 3) * 4 + 2) * picture.width + (4 + 3) * 4 + 2;
        assert_eq!(&picture.pixels[middle * 3..middle * 3 + 3], &[60, 0, 90]);
        // Read clean, without being told the palette.
        let read = read_picture(&picture, false);
        assert!(read.color && read.codes.len() == 3 && frames.iter().all(|f| read.codes.contains(f)));
    }

    #[test]
    fn a_palette_is_worked_out_from_the_picture() {
        let frames = [bytes(917, 4), bytes(917, 5), bytes(917, 6)];
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        // The palette of the brief, one with little contrast whose dark is far from black, and
        // one where green has much less contrast than its neighbours and sees more of their
        // light than of its own.
        for (text, crosstalk) in [("60,255,0,200,90,255", 0.3), ("135,255,135,255,135,255", 0.3), ("0,255,60,180,0,255", 0.3), ("0,120,0,255,0,120", 0.15)] {
            let levels = Levels::parse(text).unwrap();
            let clean = draw_with(&refs, 4, 4, &levels).unwrap();
            let seen = on_a_desk(&through_camera(&clean, crosstalk, 6, 11), 90);
            // The middle of the code is found, inside the code and clear of the desk.
            let region = code_middle(&seen).expect("a busy part");
            assert!(region.x0 >= 90 + 60 && region.y0 >= 90 + 60 && region.x1 <= seen.width - 150 && region.y1 <= seen.height - 150, "{text}: {region:?}");
            assert!(colorfulness(&seen) > 0.4, "{text}: {}", colorfulness(&seen));
            let unmix = Unmix::estimate(&seen).unwrap_or_else(|| panic!("{text}: no fit"));
            // The all-dark color the camera saw: the palette's dark levels, mixed, lifted and squeezed.
            let mix = [[1.0, crosstalk, crosstalk * 0.5], [crosstalk, 1.0, crosstalk], [crosstalk * 0.5, crosstalk, 1.0]];
            for (c, row) in mix.iter().enumerate() {
                let sum: f32 = row.iter().sum();
                let dark: f32 = (0..3).map(|j| row[j] * levels.dark[j] as f32 / 255.0).sum::<f32>() / sum;
                assert!((unmix.black[c] - (30.0 + dark * 190.0)).abs() < 12.0, "{text}: channel {c} of {unmix:?}");
            }
            let read = read_picture(&seen, false);
            assert!(read.color, "{text}");
            for frame in &frames {
                assert!(read.codes.contains(frame), "{text}: {} codes read", read.codes.len());
            }
        }
    }

    #[test]
    fn eight_colors_are_told_apart_by_their_shape() {
        // Green with a third of the contrast of red and blue, and a camera that mixes them.
        let black = [40.0f32, 70.0, 35.0];
        let mix = [[120.0f32, 22.0, 14.0], [36.0, 45.0, 38.0], [12.0, 20.0, 125.0]];
        let color = |id: usize| -> [f32; 3] { std::array::from_fn(|c| black[c] + (0..3).map(|j| if id >> j & 1 == 1 { mix[c][j] } else { 0.0 }).sum::<f32>()) };
        // In any order, with a little error in each.
        let order = [5usize, 0, 3, 6, 1, 7, 2, 4];
        let centers: Vec<[f32; 3]> = order.iter().enumerate().map(|(n, id)| color(*id).map(|v| v + (n as f32 - 3.5) * 0.6)).collect();
        assert_eq!(corners_by_shape(&centers), Some(order));
        // Eight colors that are not the corners of a box are not labelled.
        let scattered = [[10.0, 10.0, 10.0], [200.0, 30.0, 60.0], [40.0, 180.0, 90.0], [90.0, 60.0, 220.0], [150.0, 150.0, 20.0], [30.0, 90.0, 140.0], [220.0, 200.0, 100.0], [240.0, 240.0, 240.0]];
        assert_eq!(corners_by_shape(&scattered), None);
        assert_eq!(corners_by_shape(&[[90.0; 3]; 8]), None);
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
