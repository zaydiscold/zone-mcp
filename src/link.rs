//! Carrier 1: Link. The computer's side of the iroh conversation in PROTOCOL.md.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, presets};
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::codec::{self, FrameError, IDLE_TIMEOUT};
use crate::db::{Admission, Db, Delivery, PhoneInfo};
use crate::item::{Carrier, Item};
use crate::paths;
use crate::{info, warn};

/// The protocol name both endpoints must offer.
pub const ALPN: &[u8] = b"zone-sync/1";
pub const PROTOCOL_VERSION: i64 = 1;

/// Sessions served at once. A phone normally opens one. More than this is refused as `busy`.
const MAX_SESSIONS: usize = 4;

/// Use a relay other than iroh's defaults: `ZONE_RELAY_URL=https://relay.example.com`.
pub const ENV_RELAY_URL: &str = "ZONE_RELAY_URL";

/// Shared by every session.
pub struct LinkState {
    pub db: Arc<Mutex<Db>>,
    pub computer_name: String,
    sessions: Semaphore,
}

impl LinkState {
    pub fn new(db: Arc<Mutex<Db>>, computer_name: String) -> Arc<Self> {
        Arc::new(Self { db, computer_name, sessions: Semaphore::new(MAX_SESSIONS) })
    }
}

/// Loads this computer's iroh secret key, creating it the first time. The file holds the 32
/// key bytes as hex and is readable only by its owner.
pub fn load_or_create_key(dir: &Path) -> Result<SecretKey> {
    let path = paths::secret_key(dir);
    if let Some(key) = read_key(dir)? {
        if paths::is_shared_with_others(&path) {
            // Tighten a file someone loosened, rather than refusing to start.
            let text = std::fs::read(&path)?;
            paths::write_private(&path, &text)?;
        }
        return Ok(key);
    }
    let key = SecretKey::from_bytes(&paths::random_bytes::<32>());
    paths::write_private(&path, format!("{}\n", paths::hex(&key.to_bytes())).as_bytes())?;
    Ok(key)
}

/// The stored key, if there is one. Never creates it.
pub fn read_key(dir: &Path) -> Result<Option<SecretKey>> {
    let path = paths::secret_key(dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let bytes = paths::unhex(text.trim()).filter(|b| b.len() == 32).with_context(|| {
        format!("{} is damaged. Delete it to get a new identity (every phone must then pair again).", path.display())
    })?;
    let bytes: [u8; 32] = bytes.try_into().expect("length checked");
    Ok(Some(SecretKey::from_bytes(&bytes)))
}

/// This computer's endpoint id in iroh's standard text form (64 lowercase hex characters).
pub fn endpoint_id_text(key: &SecretKey) -> String {
    key.public().to_string()
}

/// A custom relay from the environment, when one is set.
pub fn custom_relay() -> Result<Option<RelayUrl>> {
    match std::env::var(ENV_RELAY_URL) {
        Ok(url) if !url.trim().is_empty() => {
            let relay = RelayUrl::from_str(url.trim()).map_err(|e| anyhow!("{ENV_RELAY_URL} is not a valid URL: {e}"))?;
            Ok(Some(relay))
        }
        _ => Ok(None),
    }
}

/// Binds the accepting endpoint with iroh's standard setup: the public relays and DNS
/// address lookup, so a phone can find this computer from its endpoint id alone.
pub async fn bind(key: SecretKey) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0).secret_key(key).alpns(vec![ALPN.to_vec()]);
    if let Some(relay) = custom_relay()? {
        builder = builder.relay_mode(RelayMode::custom([relay]));
    }
    builder.bind().await.map_err(|e| anyhow!("cannot start the iroh endpoint: {e}"))
}

/// Accepts sessions until the endpoint closes.
pub async fn accept_loop(endpoint: Endpoint, state: Arc<LinkState>) {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(incoming, state).await {
                warn!("link: session ended with an error: {error:#}");
            }
        });
    }
}

async fn serve_connection(incoming: Incoming, state: Arc<LinkState>) -> Result<()> {
    let connection = incoming.await.map_err(|e| anyhow!("handshake failed: {e}"))?;
    let endpoint_id = connection.remote_id().to_string();
    let short = short_id(&endpoint_id);

    let (mut send, mut recv) = tokio::time::timeout(IDLE_TIMEOUT, connection.accept_bi())
        .await
        .map_err(|_| anyhow!("{short} opened no stream"))?
        .map_err(|e| anyhow!("{short} opened no stream: {e}"))?;

    let outcome = session(&connection, &mut send, &mut recv, &state, &endpoint_id).await;
    let _ = send.finish();
    // Give the last frame time to reach the phone before the connection goes away. The
    // phone closes first in the normal case.
    let _ = tokio::time::timeout(Duration::from_secs(5), connection.closed()).await;
    match &outcome {
        Ok(_) => connection.close(0u32.into(), b"done"),
        Err(_) => connection.close(1u32.into(), b"protocol error"),
    }
    outcome
}

/// What the path a connection is using right now looks like from here.
pub fn path_kind(connection: &Connection) -> &'static str {
    let paths = connection.paths();
    let selected = paths.iter().find(|p| p.is_selected());
    match selected {
        Some(p) if p.is_relay() => "relay",
        Some(p) if p.is_ip() => "direct",
        Some(_) => "other",
        None => {
            if paths.iter().any(|p| p.is_ip()) {
                "direct"
            } else if paths.iter().any(|p| p.is_relay()) {
                "relay"
            } else {
                "unknown"
            }
        }
    }
}

async fn refuse(send: &mut SendStream, reason: &str) -> Result<()> {
    codec::write_frame(send, &json!({ "t": "refused", "reason": reason })).await
}

async fn session(
    connection: &Connection,
    send: &mut SendStream,
    recv: &mut RecvStream,
    state: &Arc<LinkState>,
    endpoint_id: &str,
) -> Result<()> {
    let short = short_id(endpoint_id);

    // 1. hello
    let hello = match codec::read_frame(recv, IDLE_TIMEOUT).await {
        Ok(frame) => frame,
        Err(e) => bail!("{short} sent no hello: {e}"),
    };
    if hello.get("t").and_then(Value::as_str) != Some("hello") {
        bail!("{short} did not start with hello");
    }
    if hello.get("v").and_then(Value::as_i64) != Some(PROTOCOL_VERSION) {
        info!("link: refused {short}: protocol version {} is not {PROTOCOL_VERSION}", hello.get("v").unwrap_or(&Value::Null));
        return refuse(send, "version").await;
    }
    let Ok(_permit) = state.sessions.try_acquire() else {
        info!("link: refused {short}: busy");
        return refuse(send, "busy").await;
    };
    let phone_value = hello.get("phone").cloned().unwrap_or(Value::Null);
    let text = |key: &str| phone_value.get(key).and_then(Value::as_str).map(|s| s.chars().take(200).collect::<String>());
    let phone = PhoneInfo { id: text("id"), name: text("name"), app: text("app") };
    let pair_secret = hello.get("pair").and_then(Value::as_str).map(str::to_string);

    // 2. welcome or refused
    let admission = {
        let db = state.db.clone();
        let endpoint_id = endpoint_id.to_string();
        let phone = phone.clone();
        blocking(move || db.lock().map_err(poisoned)?.admit(&endpoint_id, pair_secret.as_deref(), &phone, paths::now_unix())).await?
    };
    let phone_label = phone.name.clone().unwrap_or_else(|| "a phone".to_string());
    match admission {
        Admission::NotPaired => {
            info!("link: refused {short}: not paired");
            return refuse(send, "not_paired").await;
        }
        Admission::BadSecret => {
            info!("link: refused {short}: pairing secret is wrong, expired or already used");
            return refuse(send, "bad_secret").await;
        }
        Admission::Paired => info!("link: paired {phone_label} ({short})"),
        Admission::Known => {}
    }

    let (database_id, items, resend_all) = {
        let db = state.db.clone();
        let endpoint_id = endpoint_id.to_string();
        blocking(move || {
            let db = db.lock().map_err(poisoned)?;
            let resend = db.device(&endpoint_id)?.map(|d| d.resend_all).unwrap_or(false);
            Ok((db.database_id()?, db.total_items()?, resend))
        })
        .await?
    };
    let first_path = path_kind(connection);
    codec::write_frame(
        send,
        &json!({
            "t": "welcome",
            "v": PROTOCOL_VERSION,
            "computer": {
                "id": database_id,
                "name": state.computer_name,
                "os": std::env::consts::OS,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "items": items,
            "resend_all": resend_all,
        }),
    )
    .await?;

    // 3. items and acks, until bye
    let mut received = 0usize;
    let mut changed = 0usize;
    let mut frames = 0usize;
    loop {
        let frame = match codec::read_frame(recv, IDLE_TIMEOUT).await {
            Ok(frame) => frame,
            // The phone went away without bye. Everything acked is already on disk.
            Err(FrameError::Closed) => break,
            Err(FrameError::Idle) => {
                info!("link: {short} was silent for {} seconds, closing", IDLE_TIMEOUT.as_secs());
                break;
            }
            Err(e) => bail!("{short}: {e}"),
        };
        match frame.get("t").and_then(Value::as_str) {
            Some("items") => {
                let seq = frame.get("seq").cloned().unwrap_or(Value::Null);
                let list = frame.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut parsed = Vec::with_capacity(list.len());
                let mut rejected = 0usize;
                for value in &list {
                    match Item::from_json(value) {
                        Ok(item) => parsed.push(item),
                        Err(_) => rejected += 1,
                    }
                }
                let applied = {
                    let db = state.db.clone();
                    let endpoint_id = endpoint_id.to_string();
                    let device_id = phone.id.clone();
                    let path = path_kind(connection);
                    blocking(move || {
                        let now = paths::now_unix();
                        let mut db = db.lock().map_err(poisoned)?;
                        let delivery = Delivery { carrier: Carrier::Link, device_id: device_id.as_deref(), received_at: now };
                        // Committed before the ack is written.
                        let applied = db.apply_items(&parsed, &delivery)?;
                        // Counts what was new to this library, so sending an item twice counts once.
                        db.record_delivery(&endpoint_id, applied.changed(), Some(path), now)?;
                        Ok(applied)
                    })
                    .await?
                };
                received += applied.stored();
                changed += applied.changed();
                frames += 1;
                let mut ack = json!({ "t": "ack", "seq": seq, "stored": applied.stored() });
                if rejected > 0 {
                    // Items without an id, kind or time cannot be stored. Saying so lets the
                    // phone log it. They still count as handled, so they are not sent forever.
                    ack["rejected"] = json!(rejected);
                    warn!("link: {short} sent {rejected} items that could not be read (frame {seq})");
                }
                codec::write_frame(send, &ack).await?;
            }
            Some("bye") => break,
            // A message from a newer phone that this version does not know.
            _ => {}
        }
    }

    let last_path = path_kind(connection);
    {
        let db = state.db.clone();
        let endpoint_id = endpoint_id.to_string();
        blocking(move || db.lock().map_err(poisoned)?.record_path(&endpoint_id, last_path, paths::now_unix())).await?;
    }
    let path = if first_path == last_path { last_path.to_string() } else { format!("{first_path} then {last_path}") };
    info!("link: {phone_label} ({short}) sent {received} items in {frames} frames, {changed} new or changed, path: {path}");
    Ok(())
}

pub fn short_id(endpoint_id: &str) -> String {
    endpoint_id.chars().take(10).collect()
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow!("the library is unavailable after an earlier failure")
}

/// Runs database work off the async threads.
async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|e| anyhow!("database task failed: {e}"))?
}
