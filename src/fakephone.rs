//! A stand-in for the iPhone, for tests: it speaks the phone's side of Link over real iroh.
//!
//! It pairs with a pairing link or reconnects with a saved identity, sends synthetic items
//! (or items from a JSONL file) in frames, and reports every ack as JSON.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codec::{self, IDLE_TIMEOUT};
use crate::link::{self, ALPN, PROTOCOL_VERSION};
use crate::pairing::PairLink;
use crate::paths;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// A `zone://pair?...` link. Its secret is sent in hello unless `no_secret` is set.
    pub url: Option<String>,
    /// Where the phone's identity is kept between runs. Created when missing.
    pub identity: Option<PathBuf>,
    /// The computer's endpoint id, when there is neither a link nor a saved identity.
    pub computer: Option<String>,
    /// Connect without presenting the pairing secret, even when the link has one.
    pub no_secret: bool,
    /// How many synthetic items to send.
    pub count: usize,
    /// Index of the first synthetic item, so separate runs can send separate items.
    pub start_index: usize,
    /// Items per frame, at most 500.
    pub batch: usize,
    /// Send the items in this JSONL file instead of synthetic ones.
    pub file: Option<PathBuf>,
    /// Send deletions for these item ids after the items.
    pub delete: Vec<String>,
    /// Use only the relay: no direct addresses are ever tried.
    pub relay_only: bool,
    /// Direct addresses of the computer to try, skipping address lookup.
    pub addrs: Vec<SocketAddr>,
    pub name: Option<String>,
    /// Protocol version to claim in hello.
    pub version: Option<i64>,
    /// How long to wait for the connection.
    pub timeout: Duration,
}

/// The phone's identity as saved between runs.
#[derive(Serialize, Deserialize)]
struct Identity {
    secret_key: String,
    phone_id: String,
    name: String,
    /// The computer this phone last connected to.
    #[serde(default)]
    computer: Option<String>,
    #[serde(default)]
    relay: Option<String>,
}

impl Identity {
    fn new(name: &str) -> Identity {
        Identity {
            secret_key: paths::hex(&paths::random_bytes::<32>()),
            phone_id: paths::new_uuid(),
            name: name.to_string(),
            computer: None,
            relay: None,
        }
    }

    fn load(path: &Path) -> Result<Option<Identity>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("{} is not a saved identity", path.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    fn key(&self) -> Result<SecretKey> {
        let bytes: [u8; 32] = paths::unhex(&self.secret_key)
            .and_then(|b| b.try_into().ok())
            .context("the saved identity has a damaged key")?;
        Ok(SecretKey::from_bytes(&bytes))
    }
}

/// Synthetic items that look like a real library in shape, with made-up words.
pub fn synthetic_items(start_index: usize, count: usize) -> Vec<Value> {
    const WORDS: &[&str] = &[
        "amber", "basalt", "cedar", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern",
        "meadow", "nimbus", "orchard", "pebble", "quartz", "river", "saffron", "tundra", "umber", "violet", "willow", "zephyr",
    ];
    // Wednesday 2026-11-11 20:00:00 UTC, walking backwards seven minutes per item.
    let base = 1_794_427_200.0_f64;
    (start_index..start_index + count)
        .map(|i| {
            let created = base - (i as f64) * 420.0;
            let word = |n: usize| WORDS[(i * 7 + n * 13) % WORDS.len()];
            let length = 4 + (i % 23);
            let text = (0..length).map(word).collect::<Vec<_>>().join(" ");
            let id = format!("FA4E0000-0000-4000-8000-{i:012X}");
            if i % 50 == 49 {
                let lines = [format!("Ada: {text}"), format!("Grace: {} {}", word(1), word(2))];
                json!({
                    "id": id, "kind": "meeting", "source": null, "created_at": created, "updated_at": created + 900.0,
                    "text": lines.join("\n"), "title": format!("Synthetic meeting {i}"), "duration_s": 840.0,
                    "engine": "fluidMini", "status": "done",
                    "extra": { "segments": [
                        { "speaker": "Ada", "start": 0.0, "end": 6.5, "text": text },
                        { "speaker": "Grace", "start": 6.5, "end": 9.0, "text": format!("{} {}", word(1), word(2)) }
                    ] },
                    "rev": format!("{:016x}", (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                })
            } else if i % 10 == 9 {
                json!({
                    "id": id, "kind": "note", "source": null, "created_at": created, "updated_at": created,
                    "text": text, "title": null, "duration_s": null, "engine": null, "status": null, "extra": {},
                    "rev": format!("{:016x}", (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                })
            } else {
                let source = if i % 7 == 3 { "app" } else { "keyboard" };
                json!({
                    "id": id, "kind": "dictation", "source": source, "created_at": created, "updated_at": created + 2.5,
                    "text": text, "title": null, "duration_s": 1.5 + (i % 11) as f64, "engine": "fluidMini",
                    "status": "inserted", "extra": {},
                    "rev": format!("{:016x}", (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                })
            }
        })
        .collect()
}

pub fn load_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut items = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).with_context(|| format!("line {} of {} is not JSON", number + 1, path.display()))?;
        items.push(value);
    }
    Ok(items)
}

/// Runs one session and returns its report. A refusal is a normal result, not an error.
pub async fn run(options: Options) -> Result<Value> {
    let started = Instant::now();
    let link = options.url.as_deref().map(PairLink::parse).transpose()?;

    let mut identity = match &options.identity {
        Some(path) => Identity::load(path)?,
        None => None,
    }
    .unwrap_or_else(|| Identity::new(options.name.as_deref().unwrap_or("Fake iPhone")));
    if let Some(name) = &options.name {
        identity.name = name.clone();
    }

    let computer = link
        .as_ref()
        .map(|l| l.endpoint_id.clone())
        .or_else(|| options.computer.clone())
        .or_else(|| identity.computer.clone())
        .context("which computer? Give --url with a pairing link, --computer with its endpoint id, or an --identity that has connected before")?;
    let computer_id = EndpointId::from_str(&computer).map_err(|e| anyhow!("\"{computer}\" is not an endpoint id: {e}"))?;
    let relay = link.as_ref().and_then(|l| l.relay.clone()).or_else(|| identity.relay.clone());

    let mut builder = Endpoint::builder(presets::N0).secret_key(identity.key()?);
    if let Some(custom) = link::custom_relay()? {
        builder = builder.relay_mode(RelayMode::custom([custom]));
    }
    if options.relay_only {
        // Without IP transports there is nothing to hole punch with: every packet goes
        // through the relay.
        builder = builder.clear_ip_transports();
    }
    let endpoint = builder.bind().await.map_err(|e| anyhow!("cannot start the iroh endpoint: {e}"))?;

    let mut addr = EndpointAddr::new(computer_id);
    if let Some(relay) = &relay {
        addr = addr.with_relay_url(RelayUrl::from_str(relay).map_err(|e| anyhow!("the relay URL in the link is not valid: {e}"))?);
    }
    if !options.relay_only {
        for socket in &options.addrs {
            addr = addr.with_ip_addr(*socket);
        }
    }

    let mut report = json!({
        "phone": { "endpoint_id": endpoint.id().to_string(), "id": identity.phone_id, "name": identity.name },
        "computer_endpoint_id": computer,
        "relay_only": options.relay_only,
    });

    let connection = match tokio::time::timeout(options.timeout, endpoint.connect(addr, ALPN)).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(e)) => {
            report["result"] = json!("unreachable");
            report["error"] = json!(format!("{e:#}"));
            endpoint.close().await;
            return Ok(report);
        }
        Err(_) => {
            report["result"] = json!("unreachable");
            report["error"] = json!(format!("no connection within {} seconds", options.timeout.as_secs()));
            endpoint.close().await;
            return Ok(report);
        }
    };
    report["connect_ms"] = json!(started.elapsed().as_millis() as u64);
    let first_path = link::path_kind(&connection);

    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| anyhow!("cannot open a stream: {e}"))?;

    // hello
    let mut hello = json!({
        "t": "hello",
        "v": options.version.unwrap_or(PROTOCOL_VERSION),
        "phone": { "id": identity.phone_id, "name": identity.name, "app": format!("fake {}", env!("CARGO_PKG_VERSION")) },
    });
    let presents_secret = link.is_some() && !options.no_secret;
    if presents_secret {
        hello["pair"] = json!(link.as_ref().unwrap().secret);
    }
    codec::write_frame(&mut send, &hello).await?;

    // welcome or refused
    let answer = codec::read_frame(&mut recv, IDLE_TIMEOUT).await.map_err(|e| anyhow!("no answer to hello: {e}"))?;
    match answer.get("t").and_then(Value::as_str) {
        Some("welcome") => {}
        Some("refused") => {
            report["result"] = json!("refused");
            report["reason"] = answer.get("reason").cloned().unwrap_or(Value::Null);
            report["path"] = json!(first_path);
            connection.close(0u32.into(), b"refused");
            endpoint.close().await;
            return Ok(report);
        }
        other => bail!("unexpected answer to hello: {other:?}"),
    }
    report["result"] = json!("welcome");
    report["presented_secret"] = json!(presents_secret);
    report["computer"] = answer.get("computer").cloned().unwrap_or(Value::Null);
    report["items_before"] = answer.get("items").cloned().unwrap_or(Value::Null);
    report["resend_all"] = answer.get("resend_all").cloned().unwrap_or(Value::Null);

    // Remember the computer, as the phone does after pairing.
    identity.computer = Some(computer.clone());
    identity.relay = relay;
    if let Some(path) = &options.identity {
        paths::write_private(path, &serde_json::to_vec_pretty(&identity)?)?;
    }

    // items
    let mut items = match &options.file {
        Some(path) => load_jsonl(path)?,
        None => synthetic_items(options.start_index, options.count),
    };
    let now = paths::now_unix();
    for id in &options.delete {
        items.push(json!({ "id": id, "kind": "dictation", "deleted": true, "updated_at": now }));
    }
    let batch = if options.batch == 0 { codec::MAX_ITEMS_PER_FRAME } else { options.batch };
    let frames = codec::batches(&items, batch);
    let mut acks = Vec::new();
    let mut stored_total = 0u64;
    for (index, frame) in frames.iter().enumerate() {
        let seq = index as u64 + 1;
        codec::write_frame(&mut send, &json!({ "t": "items", "seq": seq, "items": frame })).await?;
        let ack = codec::read_frame(&mut recv, IDLE_TIMEOUT).await.map_err(|e| anyhow!("no ack for frame {seq}: {e}"))?;
        if ack.get("t").and_then(Value::as_str) != Some("ack") || ack.get("seq").and_then(Value::as_u64) != Some(seq) {
            bail!("expected the ack for frame {seq}, got {ack}");
        }
        stored_total += ack.get("stored").and_then(Value::as_u64).unwrap_or(0);
        acks.push(json!({ "seq": seq, "sent": frame.len(), "stored": ack.get("stored"), "rejected": ack.get("rejected") }));
    }

    // bye
    codec::write_frame(&mut send, &json!({ "t": "bye" })).await?;
    let _ = send.finish();
    // The computer finishes its stream in answer.
    let _ = tokio::time::timeout(Duration::from_secs(10), recv.read_to_end(1024)).await;
    let last_path = link::path_kind(&connection);
    connection.close(0u32.into(), b"bye");
    endpoint.close().await;

    report["sent"] = json!(items.len());
    report["frames"] = json!(frames.len());
    report["stored"] = json!(stored_total);
    report["acks"] = json!(acks);
    report["path"] = json!(if first_path == last_path { last_path.to_string() } else { format!("{first_path} then {last_path}") });
    report["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    #[test]
    fn synthetic_items_are_valid_and_varied() {
        let items = synthetic_items(0, 300);
        assert_eq!(items.len(), 300);
        let mut ids = std::collections::HashSet::new();
        let (mut dictations, mut notes, mut meetings) = (0, 0, 0);
        for value in &items {
            let item = Item::from_json(value).unwrap();
            assert!(ids.insert(item.id.clone()));
            match item.kind.as_str() {
                "dictation" => dictations += 1,
                "note" => notes += 1,
                "meeting" => meetings += 1,
                other => panic!("unexpected kind {other}"),
            }
        }
        assert_eq!((dictations, notes, meetings), (270, 24, 6));
        // A later run continues the ids instead of repeating them.
        let more = synthetic_items(300, 5);
        assert!(more.iter().all(|v| !ids.contains(v["id"].as_str().unwrap())));
        assert_eq!(synthetic_items(7, 1)[0], synthetic_items(0, 8)[7]);
    }
}
