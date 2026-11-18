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
    /// Send each of these texts as a clip, after the items.
    pub clips: Vec<String>,
    /// The source the clips claim: clipboard, share, shortcut or typed.
    pub clip_source: Option<String>,
    /// How many seconds ago the clips say they were sent (negative: in the future).
    pub clip_age: f64,
    /// Use only the relay: no direct addresses are ever tried.
    pub relay_only: bool,
    /// Direct addresses of the computer to try, skipping address lookup.
    pub addrs: Vec<SocketAddr>,
    pub name: Option<String>,
    /// How Zone looks on this phone, to say in hello: `dark` or `light`.
    pub appearance: Option<String>,
    /// The accent chosen on this phone, to say in hello.
    pub accent: Option<String>,
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

/// A made-up library that reads like a real one, for looking at the page: notes that are
/// things to do, meetings with titles and several speakers, short and long dictations from
/// each source, voice notes with the recording behind them, and a few clips. Item 0 was made
/// a few minutes before `now` and the rest go back about six weeks.
pub fn demo_items(start_index: usize, count: usize, now: f64) -> Vec<Value> {
    const NOTES: &[&str] = &[
        "Order the cedar boards for the back fence, twelve of the six foot ones, and ask whether they deliver on Saturday.",
        "Call the dentist about moving the cleaning to the first week of December.",
        "Draft the outline for the harbor talk. Start with the story about the lantern, then the three numbers, then the ask.",
        "Renew the car registration before the end of the month.",
        "Send Grace the photos from the orchard and ask which two she wants printed.",
        "Idea: a weekly page that lists what I said I would do and what actually happened. One screen, no charts.",
        "Book the ferry for the Friday crossing. Check whether the early one still takes bikes.",
        "Look into why the kitchen light flickers when the dryer runs. Might be the same circuit.\nAsk the electrician when he comes for the porch.",
        "Reply to the landlord about the gutter. Keep it short and attach the two pictures.",
        "Read the chapter on tides before Thursday and write down three questions.",
        "Pick up the saffron and the good rice for Sunday.",
        "Remember to thank Ada for covering the standup. She did the whole budget section without notes.",
    ];
    const KEYBOARD: &[&str] = &[
        "Running ten minutes late, start without me.",
        "Yes that works, see you at the harbor at noon.",
        "Can you send me the address again?",
        "Sounds good. I will bring the lantern and the extra batteries.",
        "I looked at the quote and I think the second option is the better one, mostly because the delivery date is a week earlier and we do not need the larger size.",
        "cedar fence boards six foot near me",
        "Thanks for the notes from this morning. Two things I would change: move the budget part to the end, and cut the slide about the river entirely.",
        "On my way.",
        "Could we move it to Thursday? Wednesday filled up.",
        "Write a short, friendly reply saying I can do the first week of December but not the second.",
        "That is the one. Order two.",
        "Happy birthday! Hope the day is a good one.",
        "I think the bug is in how the list is sorted after a refresh, not in the fetch itself. Try sorting by the created time and then the id.",
        "ferry timetable friday early crossing bikes",
        "Let me check with Grace and get back to you tonight.",
    ];
    const IN_APP: &[&str] = &[
        "Thinking out loud about the talk. The opening is too slow. People do not need the history, they need the one picture of the harbor at night and then the question.",
        "Grocery list: rice, saffron, two lemons, olive oil, the small tomatoes, bread.",
        "The thing I keep coming back to is that the plan only works if the first week goes well, so the first week has to be boring on purpose.",
    ];
    const CLIPS: &[(&str, &str)] = &[
        ("clipboard", "https://example.invalid/ferry/timetable?day=friday"),
        ("share", "Cedar fence boards, 6 ft, pack of 12. In stock at the yard on Orchard Road, open until 5."),
        ("typed", "Gate code 4471, then the second door on the left."),
        ("shortcut", "https://example.invalid/docs/harbor-talk-outline"),
    ];
    const MEETINGS: &[(&str, &[&str], &[&str])] = &[
        (
            "Harbor talk run-through",
            &["Ada", "Grace", "Me"],
            &[
                "Okay, let us run it from the top. You have twelve minutes and the room is about forty people.",
                "I will start with the lantern story. It takes about ninety seconds if I do not wander.",
                "Keep it. That story is the reason anyone remembers the rest.",
                "Then the three numbers: crossings per week, how many were late, and what a late one costs.",
                "Can you put the cost on its own slide? It got lost last time.",
                "Yes. One number, large, nothing else on it.",
                "And the ask at the end needs a date. People agree to things with dates.",
                "The fifteenth, then. I will say it twice.",
                "Good. Let us go again and I will time it.",
            ],
        ),
        (
            "Fence and porch with the contractor",
            &["Me", "Sam"],
            &[
                "So the fence is about sixty feet along the back and it turns at the cedar.",
                "Sixty feet, six foot boards. I would put posts every eight feet and set them in concrete.",
                "How long does that take once the boards are here?",
                "Two days for the fence. The porch light is half a day, it depends what I find in the wall.",
                "The kitchen light flickers when the dryer runs. Could that be related?",
                "Could be. I will look at the panel while I am there. Might just be a loose neutral.",
                "Great. Saturday delivery, and you start Monday?",
                "Monday works.",
            ],
        ),
        (
            "Weekly planning",
            &["Grace", "Me", "Ada", "Noor"],
            &[
                "Three things this week: the outline, the budget, and deciding about the ferry trip.",
                "The outline is mine. I will have a draft by Wednesday.",
                "I have the budget. It is mostly done, I am waiting on one quote.",
                "I can chase the quote today if you send me the name.",
                "Sending it now.",
                "And the ferry: are we going Friday or the week after?",
                "Friday, if the early crossing still takes bikes.",
                "I will check and book it tonight.",
                "Then that is the week. Short one.",
            ],
        ),
    ];
    let rev = |i: usize| format!("{:016x}", (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDE40);
    let mut items = Vec::new();
    for i in start_index..start_index + count {
        // Recent items are minutes apart and older ones hours apart, with whole quiet days.
        let age = 240.0 + (i as f64).powf(1.62) * 95.0 + ((i * 37) % 11) as f64 * 53.0;
        let created = now - age;
        let id = |salt: usize| format!("DE400000-0000-4000-8000-{:012X}", i * 4 + salt);
        match i % 40 {
            17 => {
                let (title, names, lines) = MEETINGS[(i / 40) % MEETINGS.len()];
                let mut at = 0.0;
                let mut segments = Vec::new();
                let mut text = Vec::new();
                // The lines three times over, so a meeting is long enough to scroll.
                for (n, line) in lines.iter().cycle().take(lines.len() * 3).enumerate() {
                    let speaker = names[(n * 7 + n / 3) % names.len()];
                    let length = 2.0 + line.len() as f64 / 14.0;
                    segments.push(json!({ "speaker": speaker, "start": at, "end": at + length, "text": line }));
                    text.push(format!("{speaker}: {line}"));
                    at += length + 0.6;
                }
                items.push(json!({
                    "id": id(0), "kind": "meeting", "source": null, "created_at": created, "updated_at": created + at,
                    "text": text.join("\n"), "title": title, "duration_s": at.round(), "engine": "fluidMini", "status": "done",
                    "extra": { "segments": segments }, "rev": rev(i),
                }));
            }
            5 | 13 | 22 | 31 | 37 => {
                let text = NOTES[(i / 5 + i / 40) % NOTES.len()];
                let seconds = 3.0 + text.len() as f64 / 15.0;
                // A voice note arrives as two items: the recording, and the note saved from it.
                items.push(json!({
                    "id": id(1), "kind": "dictation", "source": "note", "created_at": created - seconds - 1.0, "updated_at": created,
                    "text": text, "title": null, "duration_s": seconds, "engine": "fluidMini", "status": "note", "extra": {}, "rev": rev(i * 3 + 1),
                }));
                items.push(json!({
                    "id": id(0), "kind": "note", "source": null, "created_at": created, "updated_at": created,
                    "text": text, "title": null, "duration_s": null, "engine": null, "status": null, "extra": {}, "rev": rev(i),
                }));
            }
            9 | 29 => {
                let (source, text) = CLIPS[(i / 20) % CLIPS.len()];
                items.push(json!({
                    "id": id(0), "kind": "clip", "source": source, "created_at": created, "updated_at": created,
                    "text": text, "title": null, "duration_s": null, "engine": null, "status": null, "extra": {}, "rev": rev(i),
                }));
            }
            n => {
                let (source, text) = if n % 9 == 2 { ("app", IN_APP[(i / 9) % IN_APP.len()]) } else { ("keyboard", KEYBOARD[(i * 7 + i / 15) % KEYBOARD.len()]) };
                items.push(json!({
                    "id": id(0), "kind": "dictation", "source": source, "created_at": created, "updated_at": created + 2.0,
                    "text": text, "title": null, "duration_s": (1.5 + text.len() as f64 / 16.0).round(), "engine": "fluidMini",
                    "status": if source == "app" { "finished" } else { "inserted" }, "extra": {}, "rev": rev(i),
                }));
            }
        }
    }
    items
}

/// A clip as the phone sends one: text the person chose to send, with a new id.
pub fn clip_item(text: &str, source: &str, created_at: f64) -> Value {
    json!({
        "id": paths::new_uuid().to_ascii_uppercase(), "kind": "clip", "source": source, "created_at": created_at, "updated_at": created_at,
        "text": text, "title": null, "duration_s": null, "engine": null, "status": null, "extra": {},
        "rev": paths::hex(&paths::random_bytes::<8>()),
    })
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
    if let Some(appearance) = &options.appearance {
        hello["phone"]["appearance"] = json!(appearance);
    }
    if let Some(accent) = &options.accent {
        hello["phone"]["accent"] = json!(accent);
    }
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
    let mut clip_ids = Vec::new();
    for text in &options.clips {
        let item = clip_item(text, options.clip_source.as_deref().unwrap_or("clipboard"), now - options.clip_age);
        clip_ids.push(item["id"].clone());
        items.push(item);
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
    if !clip_ids.is_empty() {
        report["clips"] = json!(clip_ids);
    }
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
        // A clip is an item too, with an id of its own each time.
        let clip = Item::from_json(&clip_item("copied text", "share", 12.5)).unwrap();
        assert_eq!((clip.kind.as_str(), clip.source.as_deref(), clip.created_at, clip.text.as_str()), ("clip", Some("share"), 12.5, "copied text"));
        assert_ne!(clip.id, Item::from_json(&clip_item("copied text", "share", 12.5)).unwrap().id);
        // A later run continues the ids instead of repeating them.
        let more = synthetic_items(300, 5);
        assert!(more.iter().all(|v| !ids.contains(v["id"].as_str().unwrap())));
        assert_eq!(synthetic_items(7, 1)[0], synthetic_items(0, 8)[7]);
    }
}
