//! Pairing links: `zone://pair?v=1&n=<endpoint id>&s=<secret>&name=<computer name>&r=<relay url>`
//! and the QR codes that carry them.

use anyhow::{Context, Result, bail};
use qrcode::{Color, EcLevel, QrCode};

use crate::db::Db;
use crate::paths;

/// A pairing offer lasts ten minutes from the moment it is shown.
pub const OFFER_TTL_SECONDS: f64 = 600.0;

const SCHEME_PREFIX: &str = "zone://pair?";

/// The parts of a pairing link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairLink {
    /// The computer's iroh endpoint id, 64 lowercase hex characters.
    pub endpoint_id: String,
    /// 32 random bytes as 64 lowercase hex characters.
    pub secret: String,
    pub name: String,
    pub relay: Option<String>,
}

impl PairLink {
    pub fn to_url(&self) -> String {
        let mut url = format!(
            "{SCHEME_PREFIX}v=1&n={}&s={}&name={}",
            self.endpoint_id,
            self.secret,
            percent_encode(&self.name)
        );
        if let Some(relay) = &self.relay {
            url.push_str("&r=");
            url.push_str(&percent_encode(relay));
        }
        url
    }

    pub fn parse(url: &str) -> Result<PairLink> {
        let url = url.trim();
        let Some(query) = url.strip_prefix(SCHEME_PREFIX) else {
            bail!("a pairing link starts with {SCHEME_PREFIX}");
        };
        let (mut version, mut endpoint_id, mut secret, mut name, mut relay) = (None, None, None, None, None);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value).with_context(|| format!("the {key} part of the pairing link is not valid"))?;
            match key {
                "v" => version = Some(value),
                "n" => endpoint_id = Some(value),
                "s" => secret = Some(value),
                "name" => name = Some(value),
                "r" => relay = Some(value).filter(|r| !r.is_empty()),
                // Parts added by a later version are ignored.
                _ => {}
            }
        }
        match version.as_deref() {
            Some("1") => {}
            Some(other) => bail!("this pairing link is version {other}. This zone understands version 1. Update zone."),
            None => bail!("the pairing link has no version"),
        }
        let endpoint_id = endpoint_id.context("the pairing link has no endpoint id (n)")?;
        let secret = secret.context("the pairing link has no secret (s)")?.to_ascii_lowercase();
        if secret.len() != 64 || paths::unhex(&secret).is_none() {
            bail!("the pairing secret must be 64 hexadecimal characters");
        }
        Ok(PairLink { endpoint_id, secret, name: name.unwrap_or_default(), relay })
    }
}

/// A fresh offer, recorded in the library so the running daemon will honor it once.
pub struct Offer {
    pub link: PairLink,
    /// Unix seconds.
    pub expires_at: f64,
    /// The offer's public name: 16 hex characters that say nothing about the secret. `zone
    /// pair --check <id>` and the feed use it to say what became of the offer.
    pub id: String,
}

/// Makes an offer and notes it in the feed. `via` is where it was asked for: `cli` or `page`.
pub fn create_offer(db: &Db, endpoint_id: &str, name: &str, relay: Option<String>, via: &str) -> Result<Offer> {
    let secret = paths::random_hex32();
    let expires_at = db.create_offer(&secret, paths::now_unix(), OFFER_TTL_SECONDS)?;
    let id = crate::db::offer_id(&secret).context("a pairing secret is 64 hexadecimal characters")?;
    crate::events::record(db, "pair_offered", serde_json::json!({ "offer": id, "expires_at": expires_at.round(), "via": via }));
    Ok(Offer { link: PairLink { endpoint_id: endpoint_id.to_string(), secret, name: name.to_string(), relay }, expires_at, id })
}

/// Percent-encodes everything except unreserved characters (RFC 3986).
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn percent_decode(text: &str) -> Result<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let pair = bytes.get(i + 1..i + 3).context("a % is not followed by two hexadecimal digits")?;
                let decoded = paths::unhex(std::str::from_utf8(pair).unwrap_or("")).context("a % is not followed by two hexadecimal digits")?;
                out.push(decoded[0]);
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(out).context("not valid UTF-8")
}

/// The modules of a QR code: `true` is dark. Rows of equal length, without a quiet zone.
pub fn qr_matrix(data: &str) -> Result<Vec<Vec<bool>>> {
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).context("the text is too long for a QR code")?;
    let width = code.width();
    let colors = code.to_colors();
    Ok(colors.chunks(width).map(|row| row.iter().map(|c| *c == Color::Dark).collect()).collect())
}

/// Modules of light border a QR code needs around it to scan reliably.
const QUIET: usize = 4;

/// A QR code as a self-contained SVG: black on white, crisp at any size.
pub fn qr_svg(data: &str) -> Result<String> {
    let matrix = qr_matrix(data)?;
    let size = matrix.len() + QUIET * 2;
    let mut path = String::new();
    for (y, row) in matrix.iter().enumerate() {
        let mut x = 0;
        while x < row.len() {
            if row[x] {
                let start = x;
                while x < row.len() && row[x] {
                    x += 1;
                }
                path.push_str(&format!("M{} {}h{}v1h-{}z", start + QUIET, y + QUIET, x - start, x - start));
            } else {
                x += 1;
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" shape-rendering=\"crispEdges\" \
         role=\"img\" aria-label=\"Pairing QR code\"><rect width=\"{size}\" height=\"{size}\" fill=\"#fff\"/>\
         <path d=\"{path}\" fill=\"#000\"/></svg>"
    ))
}

/// A QR code for a terminal, two modules per character row. Colors are set explicitly (black
/// on white) so it scans the same on dark and light terminal themes.
pub fn qr_terminal(data: &str) -> Result<String> {
    let matrix = qr_matrix(data)?;
    let size = matrix.len() + QUIET * 2;
    let dark = |x: usize, y: usize| -> bool {
        x >= QUIET && y >= QUIET && x < size - QUIET && y < size - QUIET && matrix[y - QUIET][x - QUIET]
    };
    const ON: &str = "\x1b[38;5;16m\x1b[48;5;231m";
    const OFF: &str = "\x1b[0m";
    let mut out = String::new();
    let mut y = 0;
    while y < size {
        out.push_str(ON);
        for x in 0..size {
            let top = dark(x, y);
            let bottom = y + 1 < size && dark(x, y + 1);
            out.push(match (top, bottom) {
                (true, true) => '\u{2588}',
                (true, false) => '\u{2580}',
                (false, true) => '\u{2584}',
                (false, false) => ' ',
            });
        }
        out.push_str(OFF);
        out.push('\n');
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> PairLink {
        PairLink {
            endpoint_id: "7f".repeat(32),
            secret: "0a".repeat(32),
            name: "Zed's MacBook Pro (caf\u{e9})".to_string(),
            relay: None,
        }
    }

    #[test]
    fn url_round_trip() {
        let link = link();
        let url = link.to_url();
        assert_eq!(
            url,
            format!("zone://pair?v=1&n={}&s={}&name=Zed%27s%20MacBook%20Pro%20%28caf%C3%A9%29", "7f".repeat(32), "0a".repeat(32))
        );
        assert_eq!(PairLink::parse(&url).unwrap(), link);

        let with_relay = PairLink { relay: Some("https://relay.example.com:8443/".to_string()), ..link.clone() };
        let url = with_relay.to_url();
        assert!(url.ends_with("&r=https%3A%2F%2Frelay.example.com%3A8443%2F"));
        assert_eq!(PairLink::parse(&url).unwrap(), with_relay);
    }

    #[test]
    fn parse_is_strict_about_what_matters() {
        let good = link().to_url();
        // Unknown parts and a different order are fine.
        let reordered = format!("zone://pair?future=1&name=x&s={}&n={}&v=1", "0A".repeat(32), "7f".repeat(32));
        let parsed = PairLink::parse(&reordered).unwrap();
        assert_eq!((parsed.name.as_str(), parsed.secret), ("x", "0a".repeat(32)));

        assert!(PairLink::parse("https://example.com/?v=1").is_err());
        assert!(PairLink::parse(&good.replace("v=1", "v=2")).unwrap_err().to_string().contains("version 2"));
        assert!(PairLink::parse(&good.replace("&s=", "&x=")).is_err());
        assert!(PairLink::parse(&format!("zone://pair?v=1&n={}&s=abc", "7f".repeat(32))).is_err());
        assert!(PairLink::parse(&format!("zone://pair?v=1&s={}", "0a".repeat(32))).is_err());
        assert!(PairLink::parse(&format!("{good}&name=%ZZ")).is_err());
    }

    #[test]
    fn offers_are_recorded_by_hash() {
        let db = Db::open_in_memory().unwrap();
        let offer = create_offer(&db, &"7f".repeat(32), "desk", None, "cli").unwrap();
        assert_eq!(offer.link.secret.len(), 64);
        let now = paths::now_unix();
        assert!((offer.expires_at - now - OFFER_TTL_SECONDS).abs() < 5.0);
        assert_eq!(db.open_offers(now).unwrap(), 1);
        let another = create_offer(&db, &"7f".repeat(32), "desk", None, "cli").unwrap();
        assert_ne!(another.link.secret, offer.link.secret);
    }

    #[test]
    fn qr_codes_render() {
        let url = link().to_url();
        let matrix = qr_matrix(&url).unwrap();
        assert!(matrix.len() >= 21 && matrix.iter().all(|row| row.len() == matrix.len()));
        // The three finder patterns: a dark 7 by 7 ring in three corners.
        let n = matrix.len();
        for (x, y) in [(0, 0), (n - 7, 0), (0, n - 7)] {
            assert!((0..7).all(|i| matrix[y][x + i] && matrix[y + 6][x + i] && matrix[y + i][x] && matrix[y + i][x + 6]));
        }
        let svg = qr_svg(&url).unwrap();
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert!(svg.contains(&format!("viewBox=\"0 0 {0} {0}\"", n + 8)));
        assert!(!svg.contains("http://") || svg.matches("http://").count() == 1, "only the SVG namespace");
        let terminal = qr_terminal(&url).unwrap();
        assert_eq!(terminal.lines().count(), (n + 8).div_ceil(2));

        // Reading the terminal picture back gives the same modules, inside a quiet zone.
        let size = n + 8;
        let mut read = vec![vec![false; size]; size + 1];
        for (row, line) in terminal.lines().enumerate() {
            let cells: Vec<char> = line.replace("\x1b[38;5;16m\x1b[48;5;231m", "").replace("\x1b[0m", "").chars().collect();
            assert_eq!(cells.len(), size);
            for (x, cell) in cells.iter().enumerate() {
                let (top, bottom) = match cell {
                    '\u{2588}' => (true, true),
                    '\u{2580}' => (true, false),
                    '\u{2584}' => (false, true),
                    ' ' => (false, false),
                    other => panic!("unexpected character {other:?}"),
                };
                read[row * 2][x] = top;
                read[row * 2 + 1][x] = bottom;
            }
        }
        for y in 0..size {
            for x in 0..size {
                let inside = (4..size - 4).contains(&x) && (4..size - 4).contains(&y);
                let expected = inside && matrix[y - 4][x - 4];
                assert_eq!(read[y][x], expected, "module at {x},{y}");
            }
        }
    }
}
