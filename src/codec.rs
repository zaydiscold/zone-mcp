//! Link framing: a 4-byte big-endian length, then that many bytes of UTF-8 JSON (one object).

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest frame either side may send.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
/// The most items a sender puts in one `items` frame.
pub const MAX_ITEMS_PER_FRAME: usize = 500;
/// The most JSON a sender puts in one `items` frame.
pub const MAX_ITEMS_FRAME_BYTES: usize = 1024 * 1024;
/// Either side closes a session after this long without a frame.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A frame could not be read.
#[derive(Debug)]
pub enum FrameError {
    /// The stream ended cleanly between frames.
    Closed,
    /// The length prefix is larger than [`MAX_FRAME`], or zero.
    BadLength(u64),
    /// The stream ended in the middle of a frame.
    Truncated,
    /// The body is not a JSON object.
    NotJson(String),
    /// No frame arrived within [`IDLE_TIMEOUT`].
    Idle,
    Io(std::io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Closed => write!(f, "the stream closed"),
            FrameError::BadLength(n) => write!(f, "frame length {n} is outside 1..={MAX_FRAME}"),
            FrameError::Truncated => write!(f, "the stream ended inside a frame"),
            FrameError::NotJson(why) => write!(f, "frame is not a JSON object: {why}"),
            FrameError::Idle => write!(f, "no frame for {} seconds", IDLE_TIMEOUT.as_secs()),
            FrameError::Io(e) => write!(f, "read failed: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Length prefix plus body for one JSON object.
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    if !value.is_object() {
        bail!("a frame must be a JSON object");
    }
    let body = serde_json::to_vec(value)?;
    if body.is_empty() || body.len() > MAX_FRAME {
        bail!("frame of {} bytes is larger than the {MAX_FRAME} byte limit", body.len());
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Checks a length prefix before any body is read, so an absurd length allocates nothing.
pub fn check_length(prefix: [u8; 4]) -> Result<usize, FrameError> {
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(FrameError::BadLength(length as u64));
    }
    Ok(length)
}

pub fn decode_body(body: &[u8]) -> Result<Value, FrameError> {
    let value: Value = serde_json::from_slice(body).map_err(|e| FrameError::NotJson(e.to_string()))?;
    if !value.is_object() {
        return Err(FrameError::NotJson("not an object".to_string()));
    }
    Ok(value)
}

/// Reads one frame, waiting at most `idle` for it to arrive in full.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R, idle: Duration) -> Result<Value, FrameError> {
    match tokio::time::timeout(idle, read_frame_inner(reader)).await {
        Ok(result) => result,
        Err(_) => Err(FrameError::Idle),
    }
}

async fn read_frame_inner<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value, FrameError> {
    let mut prefix = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match reader.read(&mut prefix[filled..]).await {
            Ok(0) if filled == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let length = check_length(prefix)?;
    let mut body = vec![0u8; length];
    match reader.read_exact(&mut body).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::Truncated),
        Err(e) => return Err(FrameError::Io(e)),
    }
    decode_body(&body)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let bytes = encode(value)?;
    writer.write_all(&bytes).await.map_err(|e| anyhow!("write failed: {e}"))?;
    writer.flush().await.ok();
    Ok(())
}

/// Splits items into frames that respect both sender limits: at most 500 items and at most
/// 1 MiB of JSON per frame. A single item larger than the byte limit travels alone (the frame
/// limit of 4 MiB still applies to it).
pub fn batches(items: &[Value], max_items: usize) -> Vec<Vec<Value>> {
    let max_items = max_items.clamp(1, MAX_ITEMS_PER_FRAME);
    // Room for `{"t":"items","seq":4294967295,"items":[]}` around the items.
    let budget = MAX_ITEMS_FRAME_BYTES - 64;
    let mut out: Vec<Vec<Value>> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    let mut size = 0usize;
    for item in items {
        let item_size = serde_json::to_vec(item).map(|v| v.len()).unwrap_or(0) + 1;
        if !current.is_empty() && (current.len() >= max_items || size + item_size > budget) {
            out.push(std::mem::take(&mut current));
            size = 0;
        }
        size += item_size;
        current.push(item.clone());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn read_all(bytes: &[u8]) -> Result<Value, FrameError> {
        let mut reader = bytes;
        read_frame(&mut reader, Duration::from_secs(5)).await
    }

    #[tokio::test]
    async fn round_trip() {
        let value = json!({ "t": "hello", "v": 1, "phone": { "name": "caf\u{e9} \u{1F4F1}" } });
        let bytes = encode(&value).unwrap();
        assert_eq!(&bytes[..4], &((bytes.len() - 4) as u32).to_be_bytes());
        assert_eq!(read_all(&bytes).await.unwrap(), value);
    }

    #[tokio::test]
    async fn two_frames_back_to_back() {
        let mut bytes = encode(&json!({ "t": "a" })).unwrap();
        bytes.extend(encode(&json!({ "t": "b" })).unwrap());
        let mut reader = &bytes[..];
        assert_eq!(read_frame(&mut reader, IDLE_TIMEOUT).await.unwrap()["t"], "a");
        assert_eq!(read_frame(&mut reader, IDLE_TIMEOUT).await.unwrap()["t"], "b");
        assert!(matches!(read_frame(&mut reader, IDLE_TIMEOUT).await, Err(FrameError::Closed)));
    }

    #[tokio::test]
    async fn length_limits() {
        // Exactly at the limit is a valid length. One over is refused before reading a body.
        assert_eq!(check_length((MAX_FRAME as u32).to_be_bytes()).unwrap(), MAX_FRAME);
        assert!(matches!(check_length((MAX_FRAME as u32 + 1).to_be_bytes()), Err(FrameError::BadLength(_))));
        assert!(matches!(check_length([0, 0, 0, 0]), Err(FrameError::BadLength(0))));
        assert!(matches!(check_length([0xff, 0xff, 0xff, 0xff]), Err(FrameError::BadLength(_))));

        // An oversized prefix followed by nothing: refused at once, not treated as truncated.
        let oversized = (MAX_FRAME as u32 + 1).to_be_bytes();
        assert!(matches!(read_all(&oversized).await, Err(FrameError::BadLength(n)) if n == MAX_FRAME as u64 + 1));

        // A body of exactly the maximum size is accepted.
        let padding = "x".repeat(MAX_FRAME - r#"{"t":"p","d":""}"#.len());
        let biggest = json!({ "t": "p", "d": padding });
        let bytes = encode(&biggest).unwrap();
        assert_eq!(bytes.len(), 4 + MAX_FRAME);
        assert_eq!(read_all(&bytes).await.unwrap()["t"], "p");

        // One byte more cannot be encoded.
        let too_big = json!({ "t": "p", "d": format!("{padding}x") });
        assert!(encode(&too_big).is_err());
    }

    #[tokio::test]
    async fn truncated_and_malformed() {
        let bytes = encode(&json!({ "t": "hello" })).unwrap();
        assert!(matches!(read_all(&bytes[..2]).await, Err(FrameError::Truncated)));
        assert!(matches!(read_all(&bytes[..bytes.len() - 1]).await, Err(FrameError::Truncated)));
        assert!(matches!(read_all(&[]).await, Err(FrameError::Closed)));

        let mut not_json = 5u32.to_be_bytes().to_vec();
        not_json.extend_from_slice(b"hello");
        assert!(matches!(read_all(&not_json).await, Err(FrameError::NotJson(_))));

        let mut array = 2u32.to_be_bytes().to_vec();
        array.extend_from_slice(b"[]");
        assert!(matches!(read_all(&array).await, Err(FrameError::NotJson(_))));
        assert!(encode(&json!([1, 2])).is_err());

        let mut bad_utf8 = 4u32.to_be_bytes().to_vec();
        bad_utf8.extend_from_slice(&[b'"', 0xff, 0xfe, b'"']);
        assert!(matches!(read_all(&bad_utf8).await, Err(FrameError::NotJson(_))));
    }

    #[tokio::test]
    async fn idle_timeout() {
        let (_writer, mut reader) = tokio::io::duplex(64);
        let result = read_frame(&mut reader, Duration::from_millis(30)).await;
        assert!(matches!(result, Err(FrameError::Idle)));
    }

    #[test]
    fn batches_respect_both_limits() {
        let small: Vec<Value> = (0..1201).map(|i| json!({ "id": i, "text": "short" })).collect();
        let frames = batches(&small, 500);
        assert_eq!(frames.iter().map(Vec::len).collect::<Vec<_>>(), [500, 500, 201]);
        assert_eq!(batches(&small, 100_000)[0].len(), 500);

        // 40 items of about 100 KiB each cannot share one 1 MiB frame.
        let large: Vec<Value> = (0..40).map(|i| json!({ "id": i, "text": "y".repeat(100 * 1024) })).collect();
        let frames = batches(&large, 500);
        assert!(frames.len() >= 4);
        assert_eq!(frames.iter().map(Vec::len).sum::<usize>(), 40);
        for frame in &frames {
            let encoded = encode(&json!({ "t": "items", "seq": 1, "items": frame })).unwrap();
            assert!(encoded.len() <= MAX_ITEMS_FRAME_BYTES + 4, "frame is {} bytes", encoded.len());
        }
        // An item bigger than 1 MiB travels alone.
        let huge = vec![json!({ "id": 1, "text": "z".repeat(2 * 1024 * 1024) }), json!({ "id": 2 })];
        assert_eq!(batches(&huge, 500).iter().map(Vec::len).collect::<Vec<_>>(), [1, 1]);
        assert!(batches(&[], 500).is_empty());
    }
}
