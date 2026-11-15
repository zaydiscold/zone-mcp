//! The fountain code of Flash (PROTOCOL.md, "Carrier 2: Flash").
//!
//! The sealed payload is cut into `k` chunks of `c` bytes. Frame `i` carries the XOR of a set
//! of chunks: just chunk `i` while `i < k`, and after that a set drawn from splitmix64. The
//! receiver keeps every frame as a row over GF(2) and is done when it holds `k` independent
//! rows, whichever frames those were and in whatever order they came.
//!
//! `scripts/flash_reference.py` is the reference. The tests here reproduce its vector.

use std::collections::HashSet;

/// The three bytes every frame starts with.
pub const MAGIC: &[u8; 3] = b"ZF1";
/// `"ZF1" || session(u32) || total_len(u32) || k(u16) || i(u32)`, all big endian.
pub const HEADER_LEN: usize = 17;
/// The chunk size the phone uses unless told otherwise: a version 11 QR code.
pub const DEFAULT_CHUNK: usize = 300;
/// The most bytes a QR code can hold in byte mode (version 40, level L).
pub const MAX_FRAME_LEN: usize = 2953;
/// The largest sealed payload accepted. With 300 byte chunks, `k` reaches its limit of 65535
/// at under 20 MB, which is already hours of flashing.
pub const MAX_PAYLOAD_LEN: usize = 64 * 1024 * 1024;
/// The smallest sealed payload there can be: a nonce and a tag around nothing.
pub const MIN_PAYLOAD_LEN: usize = 12 + 16;

/// What every frame of one flash has in common.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shape {
    /// Random, the same for the whole flash.
    pub session: u32,
    /// Bytes of the sealed payload.
    pub total_len: u32,
    /// Number of chunks.
    pub k: u16,
    /// Bytes per chunk.
    pub chunk: u16,
}

/// One frame, as read from a QR code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub shape: Shape,
    pub index: u32,
    pub body: &'a [u8],
}

/// Why some bytes are not a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Does not start with `ZF1`: some other QR code.
    NotFlash,
    /// Shorter than a header and one byte of body.
    TooShort,
    /// Longer than any QR code can be.
    TooLong,
    /// The header contradicts itself (see the message).
    Malformed(&'static str),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::NotFlash => write!(f, "not a Zone flash code"),
            FrameError::TooShort => write!(f, "a flash frame shorter than its header"),
            FrameError::TooLong => write!(f, "a flash frame longer than a QR code can hold"),
            FrameError::Malformed(why) => write!(f, "a malformed flash frame: {why}"),
        }
    }
}

/// Reads a frame's header and checks that it makes sense.
pub fn parse(raw: &[u8]) -> Result<Frame<'_>, FrameError> {
    if raw.len() < MAGIC.len() || &raw[..MAGIC.len()] != MAGIC {
        return Err(FrameError::NotFlash);
    }
    if raw.len() <= HEADER_LEN {
        return Err(FrameError::TooShort);
    }
    if raw.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLong);
    }
    let be32 = |at: usize| u32::from_be_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
    let session = be32(3);
    let total_len = be32(7);
    let k = u16::from_be_bytes([raw[11], raw[12]]);
    let index = be32(13);
    let body = &raw[HEADER_LEN..];
    let chunk = body.len();
    if k == 0 {
        return Err(FrameError::Malformed("k is zero"));
    }
    if (total_len as usize) < MIN_PAYLOAD_LEN {
        return Err(FrameError::Malformed("the payload is too short to be sealed"));
    }
    if total_len as usize > MAX_PAYLOAD_LEN {
        return Err(FrameError::Malformed("the payload is larger than this version accepts"));
    }
    // k chunks of this size must hold the payload exactly, with only the last one padded.
    if (total_len as usize).div_ceil(chunk) != k as usize {
        return Err(FrameError::Malformed("k does not match the payload length and the chunk size"));
    }
    Ok(Frame { shape: Shape { session, total_len, k, chunk: chunk as u16 }, index, body })
}

/// The splitmix64 stream of PROTOCOL.md, started at `(session << 32) | index`.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn words_for(k: usize) -> usize {
    k.div_ceil(64)
}

/// Which chunks frame `index` XORs together, as a bit mask: chunk `n` is bit `n % 64` of
/// word `n / 64`.
fn chunk_mask(session: u32, index: u32, k: usize, mask: &mut Vec<u64>) {
    let words = words_for(k);
    mask.clear();
    mask.resize(words, 0);
    if (index as usize) < k {
        mask[index as usize / 64] = 1 << (index % 64);
        return;
    }
    let mut stream = SplitMix(((session as u64) << 32) | index as u64);
    for word in mask.iter_mut() {
        *word = stream.next();
    }
    // Only bits below k name a chunk.
    if !k.is_multiple_of(64) {
        mask[words - 1] &= (1u64 << (k % 64)) - 1;
    }
    if mask.iter().all(|word| *word == 0) {
        let only = index as usize % k;
        mask[only / 64] = 1 << (only % 64);
    }
}

/// Which chunks frame `index` XORs together, as a sorted list of chunk numbers.
#[cfg(test)]
pub fn chunk_set(session: u32, index: u32, k: usize) -> Vec<usize> {
    let mut mask = Vec::new();
    chunk_mask(session, index, k, &mut mask);
    (0..k).filter(|n| mask[n / 64] >> (n % 64) & 1 == 1).collect()
}

/// `target ^= source`, eight bytes at a time. This is where decoding spends its time.
fn xor_into(target: &mut [u8], source: &[u8]) {
    let mut wide = target.chunks_exact_mut(8).zip(source.chunks_exact(8));
    for (t, s) in &mut wide {
        let mixed = u64::from_ne_bytes((&*t).try_into().expect("eight bytes")) ^ u64::from_ne_bytes(s.try_into().expect("eight bytes"));
        t.copy_from_slice(&mixed.to_ne_bytes());
    }
    let done = target.len().min(source.len()) / 8 * 8;
    for (t, s) in target[done..].iter_mut().zip(&source[done..]) {
        *t ^= s;
    }
}

/// Makes the frames of one flash. Any index can be asked for, in any order.
pub struct Encoder {
    session: u32,
    total_len: u32,
    k: usize,
    chunk: usize,
    /// The payload, zero padded to `k * chunk` bytes.
    padded: Vec<u8>,
}

impl Encoder {
    pub fn new(session: u32, payload: &[u8], chunk: usize) -> Result<Encoder, String> {
        if chunk == 0 || chunk + HEADER_LEN > MAX_FRAME_LEN {
            return Err(format!("the chunk size must be between 1 and {}", MAX_FRAME_LEN - HEADER_LEN));
        }
        if payload.len() < MIN_PAYLOAD_LEN {
            return Err("the payload is too short to be a sealed flash".to_string());
        }
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err("the payload is too large for a flash".to_string());
        }
        let k = payload.len().div_ceil(chunk);
        if k > u16::MAX as usize {
            return Err(format!("{} bytes need {k} chunks of {chunk} bytes, and a flash has at most 65535. Use larger chunks.", payload.len()));
        }
        let mut padded = payload.to_vec();
        padded.resize(k * chunk, 0);
        Ok(Encoder { session, total_len: payload.len() as u32, k, chunk, padded })
    }

    pub fn k(&self) -> usize {
        self.k
    }

    /// The bytes of frame `index`: the header, then the XOR of its chunks.
    pub fn frame(&self, index: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.chunk);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.session.to_be_bytes());
        out.extend_from_slice(&self.total_len.to_be_bytes());
        out.extend_from_slice(&(self.k as u16).to_be_bytes());
        out.extend_from_slice(&index.to_be_bytes());
        out.resize(HEADER_LEN + self.chunk, 0);
        let body = &mut out[HEADER_LEN..];
        let mut mask = Vec::new();
        chunk_mask(self.session, index, self.k, &mut mask);
        for (word_index, word) in mask.iter().enumerate() {
            let mut bits = *word;
            while bits != 0 {
                let n = word_index * 64 + bits.trailing_zeros() as usize;
                xor_into(body, &self.padded[n * self.chunk..(n + 1) * self.chunk]);
                bits &= bits - 1;
            }
        }
        out
    }
}

/// What a frame did for the decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// It was independent of everything held so far: one row closer.
    Useful,
    /// A frame index seen before.
    Duplicate,
    /// A new frame that says nothing the held rows do not already say.
    Redundant,
}

struct Row {
    /// The chunks this row mentions. The highest one is the row's pivot. Words above the
    /// pivot's are not stored.
    mask: Vec<u64>,
    body: Vec<u8>,
}

/// Rebuilds the payload from frames by elimination over GF(2), one frame at a time.
pub struct Decoder {
    shape: Shape,
    k: usize,
    chunk: usize,
    /// The row whose highest chunk is `n`, at position `n`.
    rows: Vec<Option<Row>>,
    rank: usize,
    seen: HashSet<u32>,
    scratch: Vec<u64>,
}

impl Decoder {
    pub fn new(shape: Shape) -> Decoder {
        let k = shape.k as usize;
        Decoder {
            shape,
            k,
            chunk: shape.chunk as usize,
            rows: (0..k).map(|_| None).collect(),
            rank: 0,
            seen: HashSet::new(),
            scratch: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Independent rows held. The payload can be rebuilt when this reaches `k`.
    pub fn rank(&self) -> usize {
        self.rank
    }

    pub fn k(&self) -> usize {
        self.k
    }

    /// Different frame indexes seen.
    pub fn frames(&self) -> usize {
        self.seen.len()
    }

    pub fn is_complete(&self) -> bool {
        self.rank == self.k
    }

    /// Takes one frame of this flash. `body` must be `chunk` bytes (which `parse` ensures for
    /// a frame of this shape).
    pub fn add(&mut self, index: u32, body: &[u8]) -> Added {
        if body.len() != self.chunk || !self.seen.insert(index) {
            return Added::Duplicate;
        }
        if self.is_complete() {
            return Added::Redundant;
        }
        let mut mask = std::mem::take(&mut self.scratch);
        chunk_mask(self.shape.session, index, self.k, &mut mask);
        let mut body = body.to_vec();
        // Words at and above `top` are zero. It only ever moves down.
        let mut top = mask.len();
        let added = loop {
            while top > 0 && mask[top - 1] == 0 {
                top -= 1;
            }
            if top == 0 {
                break Added::Redundant;
            }
            let pivot = (top - 1) * 64 + 63 - mask[top - 1].leading_zeros() as usize;
            match &self.rows[pivot] {
                Some(row) => {
                    for (m, r) in mask.iter_mut().zip(&row.mask) {
                        *m ^= r;
                    }
                    xor_into(&mut body, &row.body);
                }
                None => {
                    self.rows[pivot] = Some(Row { mask: mask[..top].to_vec(), body });
                    self.rank += 1;
                    break Added::Useful;
                }
            }
        };
        self.scratch = mask;
        added
    }

    /// The payload, once `k` independent rows are held.
    pub fn solve(&self) -> Option<Vec<u8>> {
        if !self.is_complete() {
            return None;
        }
        let chunk = self.chunk;
        let mut out = vec![0u8; self.k * chunk];
        // Lowest pivot first: every other chunk a row mentions is lower, so already solved.
        for pivot in 0..self.k {
            let row = self.rows[pivot].as_ref()?;
            let (solved, rest) = out.split_at_mut(pivot * chunk);
            let target = &mut rest[..chunk];
            target.copy_from_slice(&row.body);
            for (word_index, word) in row.mask.iter().enumerate() {
                let mut bits = *word;
                while bits != 0 {
                    let n = word_index * 64 + bits.trailing_zeros() as usize;
                    if n != pivot {
                        xor_into(target, &solved[n * chunk..(n + 1) * chunk]);
                    }
                    bits &= bits - 1;
                }
            }
        }
        out.truncate(self.shape.total_len as usize);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::hex;
    use sha2::{Digest, Sha256};

    fn reference_payload() -> Vec<u8> {
        (0..1280u32).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect()
    }

    fn decode(frames: &[Vec<u8>]) -> Option<Vec<u8>> {
        let mut decoder: Option<Decoder> = None;
        for raw in frames {
            let frame = parse(raw).ok()?;
            let decoder = decoder.get_or_insert_with(|| Decoder::new(frame.shape));
            if frame.shape != decoder.shape() {
                continue;
            }
            decoder.add(frame.index, frame.body);
            if decoder.is_complete() {
                break;
            }
        }
        decoder?.solve()
    }

    /// The vector printed by `scripts/flash_reference.py` and quoted in PROTOCOL.md.
    #[test]
    fn reference_vector() {
        let session = 0x0102_0304;
        let payload = reference_payload();
        let encoder = Encoder::new(session, &payload, 300).unwrap();
        assert_eq!(encoder.k(), 5);
        let sets: Vec<Vec<usize>> = (5..13).map(|i| chunk_set(session, i, 5)).collect();
        assert_eq!(sets, vec![vec![1], vec![0, 3], vec![1], vec![0, 3, 4], vec![1, 2, 4], vec![1], vec![1, 2, 3], vec![2, 4]]);
        // The plain frames are one chunk each.
        for i in 0..5 {
            assert_eq!(chunk_set(session, i, 5), vec![i as usize]);
        }
        let frames: Vec<Vec<u8>> = (0..40).map(|i| encoder.frame(i)).collect();
        assert_eq!(hex(&frames[0][..32]), "5a46310102030400000500000500000000030a11181f262d343b424950575e65");
        assert_eq!(hex(&frames[7][..32]), "5a46310102030400000500000500000007373e454c535a61686f767d848b9299");
        assert!(frames.iter().all(|f| f.len() == 317));
        let mut all = Sha256::new();
        for frame in &frames {
            all.update(frame);
        }
        assert_eq!(hex(all.finalize().as_slice()), "16e659e1a7b1a24735a6f04aeca9514d3ab1a59f285993c2b1e6e97a00c39b12");

        // The same three decodes the reference asserts: plain frames, coded frames only, and
        // a reversed slice with frames missing at both ends.
        assert_eq!(decode(&frames[..5]).unwrap(), payload);
        assert_eq!(decode(&frames[5..]).unwrap(), payload, "must decode from coded frames alone");
        let reversed: Vec<Vec<u8>> = frames[3..30].iter().rev().cloned().collect();
        assert_eq!(decode(&reversed).unwrap(), payload);
        // Not enough yet.
        assert_eq!(decode(&frames[..4]), None);
    }

    /// The reference's larger case: 400 000 bytes, k = 1334, from coded frames only.
    #[test]
    fn large_payload_from_coded_frames_only() {
        let payload: Vec<u8> = (0..400_000u32).map(|i| ((i.wrapping_mul(31).wrapping_add(5)) & 0xFF) as u8).collect();
        let encoder = Encoder::new(0xA1B2_C3D4, &payload, 300).unwrap();
        let k = encoder.k();
        assert_eq!(k, 1334);
        let mut decoder: Option<Decoder> = None;
        let mut used = 0;
        for i in k as u32..(2 * k + 20) as u32 {
            let raw = encoder.frame(i);
            let frame = parse(&raw).unwrap();
            let decoder = decoder.get_or_insert_with(|| Decoder::new(frame.shape));
            decoder.add(frame.index, frame.body);
            used += 1;
            if decoder.is_complete() {
                break;
            }
        }
        // About k + 10, as PROTOCOL.md says.
        assert!(used <= k + 20, "needed {used} frames for k = {k}");
        assert_eq!(decoder.unwrap().solve().unwrap(), payload);
    }

    #[test]
    fn lost_reordered_and_repeated_frames() {
        let mut state = 0x5EED_u64;
        let mut random = move || {
            let mut stream = SplitMix(state);
            state = stream.next();
            state
        };
        for (len, chunk) in [(28usize, 300usize), (299, 300), (300, 300), (301, 300), (5000, 300), (40_000, 300), (9_000, 64), (30_000, 1000)] {
            let payload: Vec<u8> = (0..len).map(|_| random() as u8).collect();
            let session = random() as u32;
            let encoder = Encoder::new(session, &payload, chunk).unwrap();
            let k = encoder.k();
            // A phone showing 3k + 60 frames, of which the camera misses 40 percent, and what
            // it does catch arrives shuffled and sometimes twice.
            let mut caught: Vec<u32> = (0..(3 * k + 60) as u32).filter(|_| random() % 100 >= 40).collect();
            for i in (1..caught.len()).rev() {
                caught.swap(i, random() as usize % (i + 1));
            }
            let repeats: Vec<u32> = caught.iter().step_by(7).copied().collect();
            caught.extend(repeats);

            let mut decoder = Decoder::new(parse(&encoder.frame(0)).unwrap().shape);
            let (mut useful, mut duplicate) = (0, 0);
            for index in caught {
                let raw = encoder.frame(index);
                let frame = parse(&raw).unwrap();
                match decoder.add(frame.index, frame.body) {
                    Added::Useful => useful += 1,
                    Added::Duplicate => duplicate += 1,
                    Added::Redundant => {}
                }
            }
            assert!(decoder.is_complete(), "len {len} chunk {chunk}: rank {} of {k}", decoder.rank());
            assert_eq!(useful, k);
            assert!(duplicate > 0 || k < 4);
            assert_eq!(decoder.solve().unwrap(), payload, "len {len} chunk {chunk}");
        }
    }

    #[test]
    fn a_truncated_flash_is_not_complete() {
        let payload: Vec<u8> = (0..6000u32).map(|i| (i % 251) as u8).collect();
        let encoder = Encoder::new(9, &payload, 300).unwrap();
        let mut decoder = Decoder::new(parse(&encoder.frame(0)).unwrap().shape);
        // The phone was put down after 12 of the 20 frames the payload needs.
        for i in 0..12 {
            assert_eq!(decoder.add(i, &encoder.frame(i)[HEADER_LEN..]), Added::Useful);
        }
        assert_eq!((decoder.rank(), decoder.k(), decoder.frames()), (12, 20, 12));
        assert!(!decoder.is_complete());
        assert_eq!(decoder.solve(), None);
        // Showing the same frames again does not help.
        for i in 0..12 {
            assert_eq!(decoder.add(i, &encoder.frame(i)[HEADER_LEN..]), Added::Duplicate);
        }
        assert_eq!(decoder.rank(), 12);
    }

    #[test]
    fn a_few_thousand_chunks_decode_quickly() {
        // 900 kB sealed: k = 3000, a very large library. Every third plain frame is lost and
        // made up from coded frames.
        let payload: Vec<u8> = (0..900_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        let encoder = Encoder::new(77, &payload, 300).unwrap();
        let k = encoder.k();
        assert_eq!(k, 3000);
        let started = std::time::Instant::now();
        let mut decoder = Decoder::new(parse(&encoder.frame(0)).unwrap().shape);
        let mut index = 0u32;
        while !decoder.is_complete() {
            if !(index < k as u32 && index.is_multiple_of(3)) {
                let raw = encoder.frame(index);
                decoder.add(index, &raw[HEADER_LEN..]);
            }
            index += 1;
            assert!(index < 2 * k as u32, "did not converge");
        }
        assert_eq!(decoder.solve().unwrap(), payload);
        let elapsed = started.elapsed();
        // Encoding the frames is most of this. An unoptimized build takes a few seconds.
        assert!(elapsed < std::time::Duration::from_secs(60), "took {elapsed:?}");
    }

    #[test]
    fn an_empty_set_falls_back_to_one_chunk() {
        // With k = 1 half of all coded frames draw the empty set, and the rule sends them to
        // chunk i mod k: so every frame of a one chunk flash is that chunk.
        let payload = vec![0xA5u8; 100];
        let encoder = Encoder::new(3, &payload, 300).unwrap();
        assert_eq!(encoder.k(), 1);
        for i in [0u32, 1, 2, 50, 1000] {
            assert_eq!(chunk_set(3, i, 1), vec![0]);
            let frame = encoder.frame(i);
            assert_eq!(&frame[HEADER_LEN..HEADER_LEN + 100], &payload[..]);
        }
        // No frame's set is ever empty, whatever k is.
        for k in [1usize, 2, 3, 63, 64, 65, 128, 129] {
            for i in 0..400u32 {
                let set = chunk_set(0xDEAD_BEEF, i, k);
                assert!(!set.is_empty() && set.iter().all(|n| *n < k));
            }
        }
    }

    #[test]
    fn frames_that_are_not_frames() {
        let payload = reference_payload();
        let encoder = Encoder::new(1, &payload, 300).unwrap();
        let good = encoder.frame(3);
        let parsed = parse(&good).unwrap();
        assert_eq!(parsed.shape, Shape { session: 1, total_len: 1280, k: 5, chunk: 300 });
        assert_eq!((parsed.index, parsed.body.len()), (3, 300));

        // Some other QR code.
        assert_eq!(parse(b"zone://pair?v=1&n=abc"), Err(FrameError::NotFlash));
        assert_eq!(parse(b""), Err(FrameError::NotFlash));
        assert_eq!(parse(b"ZF2 something else entirely"), Err(FrameError::NotFlash));
        // Cut off inside or right after the header.
        assert_eq!(parse(&good[..10]), Err(FrameError::TooShort));
        assert_eq!(parse(&good[..HEADER_LEN]), Err(FrameError::TooShort));
        // Longer than a QR code can be.
        let mut oversize = good.clone();
        oversize.resize(MAX_FRAME_LEN + 1, 0);
        assert_eq!(parse(&oversize), Err(FrameError::TooLong));
        // A body of another length no longer agrees with k and the payload length.
        assert!(matches!(parse(&good[..good.len() - 100]), Err(FrameError::Malformed(_))));
        // k of zero, a payload too short to be sealed, a payload too large.
        let with = |at: usize, bytes: &[u8]| {
            let mut frame = good.clone();
            frame[at..at + bytes.len()].copy_from_slice(bytes);
            frame
        };
        assert!(matches!(parse(&with(11, &[0, 0])), Err(FrameError::Malformed(_))));
        assert!(matches!(parse(&with(7, &[0, 0, 0, 5])), Err(FrameError::Malformed(_))));
        assert!(matches!(parse(&with(7, &[0xFF, 0xFF, 0xFF, 0xFF])), Err(FrameError::Malformed(_))));
        assert!(matches!(parse(&with(11, &[0, 6])), Err(FrameError::Malformed(_))));

        // The encoder refuses what it could not frame.
        assert!(Encoder::new(1, &payload, 0).is_err());
        assert!(Encoder::new(1, &payload, MAX_FRAME_LEN).is_err());
        assert!(Encoder::new(1, &[0u8; 10], 300).is_err());
        assert!(Encoder::new(1, &vec![0u8; 70_000], 1).is_err());
    }
}
