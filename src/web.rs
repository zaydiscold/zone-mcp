//! The local web page: a window onto the library, served by the daemon on 127.0.0.1 only.
//!
//! Three things keep other programs and other web pages out:
//!
//! - the listener is bound to 127.0.0.1, so nothing off this computer can reach it;
//! - every request must carry the right `Host` (`127.0.0.1:<port>` or `localhost:<port>`),
//!   which defeats DNS rebinding, and a cross-site `Origin` is refused;
//! - the page and its API need a random token. `zone open` hands it over once in the URL
//!   (`/?t=...`), the server swaps it for an HttpOnly SameSite=Strict cookie and redirects, so
//!   the token does not stay in the address bar or the history.
//!
//! No CORS headers are ever sent, so no other origin can read a response.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

use crate::daemon_info::DaemonInfo;
use crate::db::{Cursor, Db, Filter, Order, Row};
use crate::flash::color::{ColorRead, Rgb};
use crate::flash::qr::Gray;
use crate::flash::receive::{self, Outcome, PictureRead, Receiver, Step};
use crate::info;
use crate::pairing;
use crate::paths;
use crate::timeparse::{Bound, Clock, unix};
use crate::tools::normalize_kind;
use crate::warn;

/// The port the page is served on unless it is taken.
pub const DEFAULT_PORT: u16 = 47813;

const PAGE: &str = include_str!("../assets/index.html");
const FLASH_PAGE: &str = include_str!("../assets/flash.html");
/// What a browser without the key sees.
const LOCKED_PAGE: &str = include_str!("../assets/locked.html");
/// The pages' serif: Charis SemiBold (SIL Global, OFL 1.1), subset and renamed Zone Serif. It is
/// served from here so the pages need nothing from the network. See assets/fonts/README.md.
const SERIF: &[u8] = include_bytes!("../assets/fonts/zone-serif.woff2");
/// The largest request body read: one camera picture, a gray byte per pixel or three for a
/// color one (1080 by 1080 in color is about 3.5 MB).
const MAX_BODY: usize = 4 * 1024 * 1024;
const PAGE_LIMIT: usize = 60;
const SEARCH_LIMIT: usize = 200;
/// Longer texts are cut in listings. The page fetches the rest when asked.
const LIST_TEXT_CHARS: usize = 4000;

pub struct WebState {
    pub db: Arc<Mutex<Db>>,
    pub token: String,
    pub port: u16,
    pub endpoint_id: String,
    pub computer_name: String,
    /// What the daemon currently knows about its own connectivity.
    pub info: Arc<Mutex<DaemonInfo>>,
    /// The flash being received through the page, if any.
    pub flash: Mutex<Receiver>,
}

/// Loads the web token, creating it the first time.
pub fn load_or_create_token(dir: &std::path::Path) -> Result<String> {
    let path = paths::web_token(dir);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let token = text.trim().to_string();
        if token.len() == 64 && paths::unhex(&token).is_some() {
            if paths::is_shared_with_others(&path) {
                paths::write_private(&path, format!("{token}\n").as_bytes())?;
            }
            return Ok(token);
        }
    }
    let token = paths::random_hex32();
    paths::write_private(&path, format!("{token}\n").as_bytes())?;
    Ok(token)
}

/// Binds 127.0.0.1 on the preferred port, or on any free port when that one is taken.
pub async fn bind(preferred: u16) -> Result<(TcpListener, u16)> {
    let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, preferred))).await {
        Ok(listener) => listener,
        Err(_) => TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .context("cannot open a local port for the web page")?,
    };
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

pub async fn serve(listener: TcpListener, state: Arc<WebState>) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!("web: accept failed: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle(request, state).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

type Reply = Response<Full<Bytes>>;

async fn handle(request: Request<Incoming>, state: Arc<WebState>) -> Reply {
    let nonce = paths::hex(&paths::random_bytes::<16>());
    let mut head = RequestHead::from(&request);
    // Only the Flash routes take a body: a camera picture or the bytes of one frame.
    if head.method == Method::POST && head.path.starts_with("/api/flash/") {
        match Limited::new(request.into_body(), MAX_BODY).collect().await {
            Ok(body) => head.body = body.to_bytes(),
            Err(_) => return secure(text(StatusCode::PAYLOAD_TOO_LARGE, "That is too much to send in one request."), &nonce),
        }
    }
    let reply = match tokio::task::spawn_blocking({
        let nonce = nonce.clone();
        move || route(&head, &state, &nonce)
    })
    .await
    {
        Ok(reply) => reply,
        Err(_) => text(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong."),
    };
    secure(reply, &nonce)
}

/// The parts of a request the routes need. The body is read for the Flash routes only.
pub struct RequestHead {
    pub method: Method,
    pub path: String,
    pub query: HashMap<String, String>,
    pub host: Option<String>,
    pub origin: Option<String>,
    pub fetch_site: Option<String>,
    pub cookie: Option<String>,
    pub body: Bytes,
}

impl RequestHead {
    fn from(request: &Request<Incoming>) -> Self {
        let header = |name: header::HeaderName| request.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        RequestHead {
            method: request.method().clone(),
            path: request.uri().path().to_string(),
            query: parse_query(request.uri().query().unwrap_or("")),
            host: header(header::HOST),
            origin: header(header::ORIGIN),
            fetch_site: request.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok()).map(str::to_string),
            cookie: header(header::COOKIE),
            body: Bytes::new(),
        }
    }
}

pub fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        // Browsers write a space in a query as +.
        let decode = |s: &str| pairing::percent_decode(&s.replace('+', " ")).unwrap_or_default();
        out.insert(decode(key), decode(value));
    }
    out
}

fn cookie_name(port: u16) -> String {
    format!("zone_session_{port}")
}

/// Compares without stopping at the first difference.
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn host_allowed(host: Option<&str>, port: u16) -> bool {
    match host {
        Some(host) => {
            let host = host.to_ascii_lowercase();
            host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
        }
        None => false,
    }
}

fn origin_allowed(head: &RequestHead, port: u16) -> bool {
    if let Some(origin) = &head.origin {
        let origin = origin.to_ascii_lowercase();
        if origin != format!("http://127.0.0.1:{port}") && origin != format!("http://localhost:{port}") {
            return false;
        }
    }
    // Browsers say where a request came from. A page on another site never gets through.
    !matches!(head.fetch_site.as_deref(), Some("cross-site") | Some("same-site"))
}

fn has_session(head: &RequestHead, state: &WebState) -> bool {
    let name = cookie_name(state.port);
    head.cookie
        .as_deref()
        .unwrap_or("")
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .any(|(key, value)| key == name && same_secret(value, &state.token))
}

pub fn route(head: &RequestHead, state: &WebState, nonce: &str) -> Reply {
    if !host_allowed(head.host.as_deref(), state.port) {
        return text(StatusCode::FORBIDDEN, "This page is only served to http://127.0.0.1 on this computer.");
    }
    let api = head.path.starts_with("/api/");
    if api && !origin_allowed(head, state.port) {
        return text(StatusCode::FORBIDDEN, "Requests from other sites are not accepted.");
    }
    let get = head.method == Method::GET || head.method == Method::HEAD;
    let post = head.method == Method::POST;

    match head.path.as_str() {
        "/" if get => {
            if let Some(offered) = head.query.get("t") {
                if same_secret(offered, &state.token) {
                    return redirect_with_session(state, "/");
                }
                return locked(nonce);
            }
            if has_session(head, state) { page(nonce) } else { locked(nonce) }
        }
        "/api/status" if get => guarded(head, state, |db| status(db, state)),
        "/api/items" if get => guarded(head, state, |db| items(db, &head.query)),
        "/api/item" if get => guarded(head, state, |db| item(db, &head.query)),
        "/api/pair" if post => guarded(head, state, |db| pair(db, state)),
        // Carrier 2 (Flash): the page reads the phone's screen with the camera and posts what
        // it sees. See flash/receive.rs.
        "/flash" if get => {
            // `zone open --flash` comes straight here with the key, as `zone open` does to /.
            if let Some(offered) = head.query.get("t") {
                if same_secret(offered, &state.token) {
                    return redirect_with_session(state, "/flash");
                }
                return locked(nonce);
            }
            if has_session(head, state) {
                html(StatusCode::OK, FLASH_PAGE.replace("__NONCE__", nonce))
            } else {
                locked(nonce)
            }
        }
        "/api/flash/status" if get => flash_status(head, state),
        "/api/flash/picture" if post => flash_picture(head, state),
        "/api/flash/frame" if post => flash_guarded(head, state, |receiver| {
            flash_take(state, receiver, &head.body);
            Ok(())
        }),
        "/api/flash/reset" if post => flash_guarded(head, state, |receiver| {
            receiver.reset();
            Ok(())
        }),
        // The font holds nothing private, so the locked page can use it too.
        "/font/zone-serif.woff2" if get => font(),
        "/" | "/flash" | "/font/zone-serif.woff2" | "/api/status" | "/api/items" | "/api/item" | "/api/pair" | "/api/flash/status" | "/api/flash/picture"
        | "/api/flash/frame" | "/api/flash/reset" => text(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed."),
        _ => text(StatusCode::NOT_FOUND, "Not found."),
    }
}

fn guarded(head: &RequestHead, state: &WebState, work: impl FnOnce(&mut Db) -> Result<Value>) -> Reply {
    if !has_session(head, state) {
        return json_reply(StatusCode::UNAUTHORIZED, &json!({ "error": "locked", "fix": "Run `zone open` to unlock this page." }));
    }
    let Ok(mut db) = state.db.lock() else {
        return json_reply(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": "the library is unavailable" }));
    };
    match work(&mut db) {
        Ok(value) => json_reply(StatusCode::OK, &value),
        Err(error) => json_reply(StatusCode::BAD_REQUEST, &json!({ "error": format!("{error:#}") })),
    }
}

/// Runs one Flash request and answers with where the flash stands.
fn flash_guarded(head: &RequestHead, state: &WebState, work: impl FnOnce(&mut Receiver) -> Result<()>) -> Reply {
    if !has_session(head, state) {
        return json_reply(StatusCode::UNAUTHORIZED, &json!({ "error": "locked", "fix": "Run `zone open` to unlock this page." }));
    }
    let Ok(mut receiver) = state.flash.lock() else {
        return json_reply(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": "flash is unavailable" }));
    };
    match work(&mut receiver) {
        Ok(()) => json_reply(StatusCode::OK, &receiver.status()),
        Err(error) => json_reply(StatusCode::BAD_REQUEST, &json!({ "error": format!("{error:#}") })),
    }
}

/// Where the flash stands, and whether this computer has anything to open one with: the page
/// says so before anyone holds a phone up.
fn flash_status(head: &RequestHead, state: &WebState) -> Reply {
    let reply = flash_guarded(head, state, |_| Ok(()));
    if reply.status() != StatusCode::OK {
        return reply;
    }
    let mut status = state.flash.lock().map(|r| r.status()).unwrap_or(Value::Null);
    if let Ok(db) = state.db.lock() {
        let phones = db.devices().map(|d| d.len()).unwrap_or(0);
        let codes = db.open_offers(paths::now_unix()).unwrap_or(0);
        status["setup"] = json!({ "phones": phones, "pairing_codes": codes });
    }
    json_reply(StatusCode::OK, &status)
}

/// A camera picture: `w` times `h` gray bytes, or with `format=rgb` three bytes (red, green,
/// blue) per pixel. Every QR code in it is taken as a frame: a color picture can carry three
/// (see flash/color.rs).
///
/// The codes are read before the flash is locked, so the pictures the page sends at once are
/// read side by side, one per processor; only taking their frames is one at a time.
fn flash_picture(head: &RequestHead, state: &WebState) -> Reply {
    if !has_session(head, state) {
        return json_reply(StatusCode::UNAUTHORIZED, &json!({ "error": "locked", "fix": "Run `zone open` to unlock this page." }));
    }
    let side = |key: &str| -> Result<usize> { head.query.get(key).and_then(|v| v.parse().ok()).with_context(|| format!("{key} is required")) };
    let thorough = state.flash.lock().map(|r| r.codes_in_view()).unwrap_or(true);
    let rgb = match head.query.get("format").map(String::as_str) {
        None | Some("gray") => false,
        Some("rgb") => true,
        Some(other) => return json_reply(StatusCode::BAD_REQUEST, &json!({ "error": format!("format must be gray or rgb, not {other}") })),
    };
    let read = side("w").and_then(|w| Ok((w, side("h")?))).and_then(|(w, h)| {
        if rgb {
            Ok(crate::flash::color::read_picture(&Rgb::new(w, h, head.body.to_vec())?, thorough))
        } else {
            Ok(ColorRead { codes: crate::flash::qr::read_camera(&Gray::new(w, h, head.body.to_vec())?, thorough), color: false })
        }
    });
    let ColorRead { codes, color } = match read {
        Ok(read) => read,
        Err(error) => return json_reply(StatusCode::BAD_REQUEST, &json!({ "error": format!("{error:#}") })),
    };
    flash_guarded(head, state, |receiver| {
        receiver.pictures += 1;
        if !codes.is_empty() {
            receiver.pictures_with_code += 1;
            receiver.last_code_at = Some(std::time::Instant::now());
        }
        if color {
            receiver.color_pictures += 1;
        }
        let mut read = PictureRead { codes: codes.len(), color, ..PictureRead::default() };
        for code in &codes {
            let (frame, useful) = flash_take(state, receiver, code);
            read.frames += usize::from(frame);
            read.useful += usize::from(useful);
        }
        receiver.last_picture = Some(read);
        Ok(())
    })
}

/// Takes the bytes of one frame. The frame that completes a flash stores it. Returns whether
/// it was a flash frame, and whether it brought a new row.
fn flash_take(state: &WebState, receiver: &mut Receiver, raw: &[u8]) -> (bool, bool) {
    let (shape, payload) = match receiver.add(raw) {
        Step::Ignored => return (false, false),
        Step::AlreadyDone => return (true, false),
        Step::Progress { useful } => return (true, useful),
        Step::GaveUp { reason, bytes } => {
            warn!("flash: every code of a flash of {bytes} bytes agrees and no key opens it: {reason}");
            if let Ok(db) = state.db.lock() {
                receive::record_events(&db, &Outcome::Failed(reason), bytes, receiver.color_pictures > 0);
            }
            return (true, false);
        }
        Step::Complete { shape, payload } => (shape, payload),
    };
    let outcome = match state.db.lock() {
        Ok(mut db) => {
            let outcome = receive::import(&mut db, &payload, paths::now_unix());
            receive::record_events(&db, &outcome, payload.len(), receiver.color_pictures > 0);
            outcome
        }
        Err(_) => Outcome::NotStored("The library is unavailable just now. Trying again.".to_string()),
    };
    match &outcome {
        Outcome::Stored(stored) => info!(
            "flash: {} sent {} items, {} new or changed{}{}",
            stored.phone_name.as_deref().unwrap_or("a phone"),
            stored.items,
            stored.inserted + stored.replaced,
            if stored.paired { ", and is now paired" } else { "" },
            if stored.check.all_checked() { String::new() } else { format!(". The check found a problem: {}", stored.check.problems.join(" ")) }
        ),
        Outcome::Failed(reason) => warn!("flash: a complete flash of {} bytes was not stored: {reason}", payload.len()),
        Outcome::NotOpened(_) => info!("flash: a complete flash of {} bytes did not open; reading more codes to check it", payload.len()),
        Outcome::NotStored(reason) => warn!("flash: a flash of {} bytes opened but was not stored yet: {reason}", payload.len()),
    }
    receiver.finish(shape, &payload, outcome);
    (true, true)
}

// -------------------------------------------------------------------- replies

fn text(status: StatusCode, body: &str) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::from(body.to_string())));
    *reply.status_mut() = status;
    reply.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    reply
}

fn json_reply(status: StatusCode, value: &Value) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::from(value.to_string())));
    *reply.status_mut() = status;
    reply.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    reply
}

fn html(status: StatusCode, body: String) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::from(body)));
    *reply.status_mut() = status;
    reply.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    reply
}

fn page(nonce: &str) -> Reply {
    html(StatusCode::OK, PAGE.replace("__NONCE__", nonce))
}

fn locked(nonce: &str) -> Reply {
    html(StatusCode::UNAUTHORIZED, LOCKED_PAGE.replace("__NONCE__", nonce))
}

fn font() -> Reply {
    let mut reply = Response::new(Full::new(Bytes::from_static(SERIF)));
    reply.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("font/woff2"));
    reply
}

/// Swaps the key in the address for a cookie and sends the browser on to `to`, one of this
/// server's own pages.
fn redirect_with_session(state: &WebState, to: &'static str) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::new()));
    *reply.status_mut() = StatusCode::SEE_OTHER;
    reply.headers_mut().insert(header::LOCATION, HeaderValue::from_static(to));
    let cookie = format!("{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", cookie_name(state.port), state.token);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        reply.headers_mut().insert(header::SET_COOKIE, value);
    }
    reply
}

/// Headers on every reply. Nothing may be loaded from anywhere but this page itself.
fn secure(mut reply: Reply, nonce: &str) -> Reply {
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; img-src data:; font-src 'self'; \
         media-src 'self' blob: mediastream:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    let headers = reply.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    headers.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    reply
}

// -------------------------------------------------------------------- API

fn list_param(query: &HashMap<String, String>, key: &str) -> Vec<String> {
    query.get(key).map(|v| v.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

fn filter_from(query: &HashMap<String, String>, clock: &Clock) -> Result<Filter> {
    let time = |key: &str, bound: Bound| -> Result<Option<f64>> {
        match query.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            Some(value) => Ok(Some(clock.parse(value, bound).map_err(|e| anyhow!("{key}: {e}"))?)),
            None => Ok(None),
        }
    };
    Ok(Filter {
        start: time("start", Bound::Start)?,
        end: time("end", Bound::End)?,
        kinds: list_param(query, "kinds").iter().map(|k| normalize_kind(k)).collect(),
        sources: list_param(query, "sources"),
        unprocessed_only: false,
        scope: Default::default(),
    })
}

fn item_view(row: &Row, clock: &Clock, snippet: Option<&str>) -> Value {
    let total_chars = row.text.chars().count();
    let truncated = total_chars > LIST_TEXT_CHARS;
    let text: String = if truncated { row.text.chars().take(LIST_TEXT_CHARS).collect() } else { row.text.clone() };
    json!({
        "id": row.id,
        "kind": row.kind,
        "source": row.source,
        "day": clock.day(row.created_at),
        "day_label": clock.day_label(row.created_at),
        "time": clock.time_of_day(row.created_at),
        "full_time": clock.full(row.created_at),
        "rfc3339": clock.rfc3339(row.created_at),
        "duration_s": row.duration_s,
        "words": row.words,
        "title": row.title,
        "text": text,
        "truncated": truncated,
        "snippet": snippet,
    })
}

fn items(db: &mut Db, query: &HashMap<String, String>) -> Result<Value> {
    let clock = Clock::system();
    let filter = filter_from(query, &clock)?;
    let search = query.get("q").map(|q| q.trim()).filter(|q| !q.is_empty());

    let mut day_counts = Map::new();
    if search.is_none() {
        let mut counts: Vec<(String, i64)> = Vec::new();
        for row in db.stat_rows(&filter)? {
            let day = clock.day(row.created_at);
            match counts.last_mut() {
                Some((last, n)) if *last == day => *n += 1,
                _ => counts.push((day, 1)),
            }
        }
        for (day, count) in counts {
            day_counts.insert(day, json!(count));
        }
    }

    if let Some(q) = search {
        let result = db.search(q, &filter, Some(Order::Newest), SEARCH_LIMIT)?;
        let list: Vec<Value> = result.hits.iter().map(|hit| item_view(&hit.row, &clock, Some(&hit.snippet))).collect();
        return Ok(json!({ "total": result.total, "items": list, "next_cursor": null, "matched": result.matched, "day_counts": {} }));
    }

    let cursor = match query.get("cursor").filter(|c| !c.is_empty()) {
        Some(text) => Some(Cursor::decode(text)?),
        None => None,
    };
    let total = db.count(&filter)?;
    let mut rows = db.list(&filter, Order::Newest, PAGE_LIMIT + 1, cursor)?;
    let more = rows.len() > PAGE_LIMIT;
    rows.truncate(PAGE_LIMIT);
    let next_cursor = if more { rows.last().map(|row| Cursor::after(row).encode()) } else { None };
    let list: Vec<Value> = rows.iter().map(|row| item_view(row, &clock, None)).collect();
    Ok(json!({ "total": total, "items": list, "next_cursor": next_cursor, "day_counts": day_counts }))
}

fn item(db: &mut Db, query: &HashMap<String, String>) -> Result<Value> {
    let id = query.get("id").context("id is required")?;
    let row = db.get(id)?.filter(|row| !row.deleted).context("no such item")?;
    Ok(json!({ "id": row.id, "title": row.title, "text": row.text }))
}

fn status(db: &mut Db, state: &WebState) -> Result<Value> {
    let clock = Clock::system();
    let now = unix(clock.now);
    let age = |t: Option<f64>| t.map(|t| (now - t).max(0.0).round());
    let phones: Vec<Value> = db
        .devices()?
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "app": d.app,
                "last_sync_age_s": age(d.last_sync),
                "items_received": d.items_received,
                "last_path": d.last_path,
            })
        })
        .collect();
    let mut by_kind = Map::new();
    for (kind, count) in db.counts_by_kind()? {
        by_kind.insert(kind, json!(count));
    }
    let mut by_source = Map::new();
    for (kind, source, count) in db.counts_by_kind_and_source()? {
        if kind == "dictation"
            && let Some(source) = source {
                by_source.insert(source, json!(count));
            }
    }
    let link_sync = db.meta_f64("last_sync_link")?;
    let backup_sync = db.meta_f64("last_sync_backup")?;
    let total = db.total_items()?;
    let backup_folder = db.meta_get("backup_folder")?;
    let (relay_connected, relay_url, backup_folder_state) = match state.info.lock() {
        Ok(info) => (info.relay_connected, info.relay_url.clone(), crate::tools::backup_folder_state(Some(&info), backup_folder.as_deref(), now)),
        Err(_) => (false, None, Value::Null),
    };
    Ok(json!({
        "computer": { "name": state.computer_name, "endpoint_id": state.endpoint_id, "version": env!("CARGO_PKG_VERSION") },
        "relay": { "connected": relay_connected, "url": relay_url },
        "phones": phones,
        "last_sync": { "link_age_s": age(link_sync), "backup_age_s": age(backup_sync) },
        "backup_folder": backup_folder,
        "backup_folder_state": backup_folder_state,
        "items": { "total": total, "by_kind": by_kind, "dictations_by_source": by_source },
        // Changes whenever anything arrives, so the page knows to refresh.
        "revision": format!(
            "{total}:{}:{}:{}",
            link_sync.unwrap_or(0.0),
            backup_sync.unwrap_or(0.0),
            db.meta_f64("last_sync_flash")?.unwrap_or(0.0)
        ),
    }))
}

fn pair(db: &mut Db, state: &WebState) -> Result<Value> {
    let offer = pairing::create_offer(db, &state.endpoint_id, &state.computer_name, None, "page")?;
    let url = offer.link.to_url();
    Ok(json!({
        "id": offer.id,
        "url": url,
        "svg": pairing::qr_svg(&url)?,
        "expires_in_s": (offer.expires_at - paths::now_unix()).round(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Delivery;
    use crate::item::{Carrier, Item};

    const PORT: u16 = 47813;

    fn state() -> WebState {
        let mut db = Db::open_in_memory().unwrap();
        let items: Vec<Item> = crate::fakephone::synthetic_items(0, 130).iter().map(|v| Item::from_json(v).unwrap()).collect();
        db.apply_items(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: 5.0 }).unwrap();
        WebState {
            db: Arc::new(Mutex::new(db)),
            token: "5e".repeat(32),
            port: PORT,
            endpoint_id: "7f".repeat(32),
            computer_name: "desk".to_string(),
            info: Arc::new(Mutex::new(DaemonInfo::default())),
            flash: Mutex::new(Receiver::new()),
        }
    }

    fn head(method: Method, target: &str) -> RequestHead {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        RequestHead {
            method,
            path: path.to_string(),
            query: parse_query(query),
            host: Some(format!("127.0.0.1:{PORT}")),
            origin: None,
            fetch_site: None,
            cookie: None,
            body: Bytes::new(),
        }
    }

    fn with_cookie(mut head: RequestHead, state: &WebState) -> RequestHead {
        head.cookie = Some(format!("other=1; {}={}", cookie_name(state.port), state.token));
        head
    }

    fn body(reply: Reply) -> String {
        let bytes = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(reply.into_body().collect()).unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn json_body(reply: Reply) -> Value {
        serde_json::from_str(&body(reply)).unwrap()
    }

    #[test]
    fn wrong_host_is_refused() {
        let state = state();
        for host in [Some("evil.example:47813"), Some("127.0.0.1"), Some("127.0.0.1:1"), Some("attacker.127.0.0.1.nip.io:47813"), None] {
            let mut h = with_cookie(head(Method::GET, "/api/status"), &state);
            h.host = host.map(str::to_string);
            assert_eq!(route(&h, &state, "n").status(), StatusCode::FORBIDDEN, "{host:?}");
        }
        for host in ["127.0.0.1:47813", "localhost:47813", "LOCALHOST:47813"] {
            let mut h = with_cookie(head(Method::GET, "/api/status"), &state);
            h.host = Some(host.to_string());
            assert_eq!(route(&h, &state, "n").status(), StatusCode::OK, "{host}");
        }
    }

    #[test]
    fn token_becomes_a_cookie_and_leaves_the_url() {
        let state = state();
        // Without anything: locked, and the page is not sent. The locked page says how to get in.
        let reply = route(&head(Method::GET, "/"), &state, "n");
        assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
        let locked = body(reply);
        assert!(!locked.contains("Search everything you said"));
        assert!(locked.contains("zone open") && locked.contains("nonce=\"n\"") && !locked.contains("__NONCE__"));
        assert_eq!(route(&head(Method::GET, "/api/status"), &state, "n").status(), StatusCode::UNAUTHORIZED);
        assert_eq!(route(&head(Method::GET, "/api/items"), &state, "n").status(), StatusCode::UNAUTHORIZED);
        assert_eq!(route(&head(Method::POST, "/api/pair"), &state, "n").status(), StatusCode::UNAUTHORIZED);

        // A wrong token unlocks nothing.
        let reply = route(&head(Method::GET, &format!("/?t={}", "00".repeat(32))), &state, "n");
        assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
        assert!(reply.headers().get(header::SET_COOKIE).is_none());

        // The right token: a redirect to the bare URL with the cookie set.
        let reply = route(&head(Method::GET, &format!("/?t={}", state.token)), &state, "n");
        assert_eq!(reply.status(), StatusCode::SEE_OTHER);
        assert_eq!(reply.headers().get(header::LOCATION).unwrap(), "/");
        let cookie = reply.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict") && cookie.contains("Path=/"));

        // With the cookie the page is served, with the nonce filled in.
        let reply = route(&with_cookie(head(Method::GET, "/"), &state), &state, "abc123");
        assert_eq!(reply.status(), StatusCode::OK);
        let page = body(reply);
        assert!(page.contains("Search everything you said"));
        assert!(page.contains("nonce=\"abc123\"") && !page.contains("__NONCE__"));

        // A cookie with the wrong value is not a session.
        let mut h = head(Method::GET, "/api/status");
        h.cookie = Some(format!("{}={}", cookie_name(PORT), "11".repeat(32)));
        assert_eq!(route(&h, &state, "n").status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn other_sites_cannot_call_the_api() {
        let state = state();
        let mut h = with_cookie(head(Method::POST, "/api/pair"), &state);
        h.origin = Some("https://evil.example".to_string());
        assert_eq!(route(&h, &state, "n").status(), StatusCode::FORBIDDEN);
        let mut h = with_cookie(head(Method::GET, "/api/items"), &state);
        h.fetch_site = Some("cross-site".to_string());
        assert_eq!(route(&h, &state, "n").status(), StatusCode::FORBIDDEN);
        let mut h = with_cookie(head(Method::POST, "/api/pair"), &state);
        h.origin = Some(format!("http://127.0.0.1:{PORT}"));
        h.fetch_site = Some("same-origin".to_string());
        assert_eq!(route(&h, &state, "n").status(), StatusCode::OK);
        // Pairing is not available through a plain link.
        assert_eq!(route(&with_cookie(head(Method::GET, "/api/pair"), &state), &state, "n").status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn replies_carry_no_cors_and_a_strict_policy() {
        let state = state();
        let reply = secure(route(&with_cookie(head(Method::GET, "/"), &state), &state, "abc"), "abc");
        let headers = reply.headers();
        assert!(headers.keys().all(|name| !name.as_str().starts_with("access-control-")));
        let csp = headers.get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap();
        assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'nonce-abc'") && csp.contains("frame-ancestors 'none'"));
        // Fonts only from zone itself: the serif it carries.
        assert!(csp.contains("font-src 'self'"));
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    }

    #[test]
    fn the_page_loads_nothing_from_elsewhere() {
        // Every URL in the pages is either the SVG namespace or a relative API path.
        for page in [PAGE, FLASH_PAGE, LOCKED_PAGE] {
            let mut rest = page;
            while let Some(at) = rest.find("http") {
                let tail = &rest[at..];
                assert!(
                    tail.starts_with("http://www.w3.org/2000/svg") || tail.starts_with("http-equiv") || tail.starts_with("http://127.0.0.1"),
                    "unexpected URL in the page: {}",
                    &tail[..tail.len().min(60)]
                );
                rest = &tail[4..];
            }
            for forbidden in ["<link rel=\"stylesheet\"", "<script src", "@import", "url(http", "fonts.googleapis", "cdn."] {
                assert!(!page.contains(forbidden), "{forbidden}");
            }
            assert!(!page.contains('\u{2014}'), "no em dashes in the page");
        }
        assert!(PAGE.contains("href=\"/flash\""), "the main page links to Flash");
    }

    #[test]
    fn the_serif_comes_from_zone_itself() {
        // The pages' serif is served from the binary, to anyone on this computer's address: it
        // holds nothing private, and the locked page uses it too.
        let state = state();
        for page in [PAGE, LOCKED_PAGE] {
            assert!(page.contains("url(\"/font/zone-serif.woff2\")"), "every page draws its serif from zone");
        }
        let reply = route(&head(Method::GET, "/font/zone-serif.woff2"), &state, "n");
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(reply.headers().get(header::CONTENT_TYPE).unwrap(), "font/woff2");
        let bytes = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(reply.into_body().collect()).unwrap().to_bytes();
        assert!(bytes.starts_with(b"wOF2") && bytes.len() < 40_000, "a small woff2");
        let mut foreign = head(Method::GET, "/font/zone-serif.woff2");
        foreign.host = Some("evil.example".to_string());
        assert_eq!(route(&foreign, &state, "n").status(), StatusCode::FORBIDDEN);
        assert_eq!(route(&head(Method::POST, "/font/zone-serif.woff2"), &state, "n").status(), StatusCode::METHOD_NOT_ALLOWED);
        // It is a subset of Charis under the SIL Open Font License, which travels with it.
        assert!(include_str!("../assets/fonts/OFL.txt").contains("SIL OPEN FONT LICENSE Version 1.1"));
    }

    #[test]
    fn the_flash_page_draws_from_the_decoder_and_guides() {
        // The mosaic is drawn from the decoder's own bit sets, and the page reads what each
        // picture held, so nothing it shows is made up.
        for needle in ["status.blocks.held", "status.blocks.solved", "last.frames", "status.setup", "requestVideoFrameCallback"] {
            assert!(FLASH_PAGE.contains(needle), "{needle}");
        }
        // The tally comes from what the codes carry, and the check from the library: the page
        // shows both as the daemon reports them (status.codes, status.needed, status.trouble,
        // result.check).
        for needle in ["status.codes", "status.needed", "codes.missed_ranges", "codes.set_aside", "lastStatus.trouble", "r.check", "check.declared"] {
            assert!(FLASH_PAGE.contains(needle), "{needle}");
        }
        // The states it explains, the guide, and the basics for keyboards and screen readers.
        for needle in ["NotAllowedError", "Send by Flash", "zone pair", "not from Zone", "aria-live", "role=\"progressbar\"", "prefers-reduced-motion", ":focus-visible"] {
            assert!(FLASH_PAGE.contains(needle), "{needle}");
        }
        // Nothing but its own nonce runs or styles it.
        assert!(!FLASH_PAGE.contains(" style=\""), "no inline style attributes (the policy would block them)");
        assert!(!FLASH_PAGE.contains("onclick="), "no inline handlers");
    }

    #[test]
    fn flash_through_the_page_routes() {
        use crate::flash::fountain::Encoder;
        use crate::flash::{qr, seal};

        let state = state();
        let secret = "ab".repeat(32);
        state.db.lock().unwrap().create_offer(&secret, paths::now_unix(), 600.0).unwrap();
        let cookie = |h: RequestHead| with_cookie(h, &state);
        let post = |target: &str, body: Vec<u8>| {
            let mut h = cookie(head(Method::POST, target));
            h.body = Bytes::from(body);
            route(&h, &state, "n")
        };

        // The page and its routes are locked like everything else.
        assert_eq!(route(&head(Method::GET, "/flash"), &state, "n").status(), StatusCode::UNAUTHORIZED);
        assert_eq!(route(&head(Method::GET, "/api/flash/status"), &state, "n").status(), StatusCode::UNAUTHORIZED);
        assert_eq!(route(&head(Method::POST, "/api/flash/frame"), &state, "n").status(), StatusCode::UNAUTHORIZED);
        let mut foreign = cookie(head(Method::POST, "/api/flash/frame"));
        foreign.origin = Some("https://evil.example".to_string());
        assert_eq!(route(&foreign, &state, "n").status(), StatusCode::FORBIDDEN);
        assert_eq!(route(&cookie(head(Method::GET, "/api/flash/frame")), &state, "n").status(), StatusCode::METHOD_NOT_ALLOWED);
        let page = body(route(&cookie(head(Method::GET, "/flash")), &state, "abc"));
        assert!(page.contains("getUserMedia") && page.contains("nonce=\"abc\"") && !page.contains("__NONCE__"));
        let waiting = json_body(route(&cookie(head(Method::GET, "/api/flash/status")), &state, "n"));
        assert_eq!(waiting["state"], "waiting");
        // The page can tell before anything is shown whether a flash could be opened.
        assert_eq!(waiting["setup"], json!({ "phones": 0, "pairing_codes": 1 }));

        // A phone that only scanned the pairing code flashes 25 items: some frames as camera
        // pictures, the rest as frame bytes, with frames missing.
        let key = seal::key_bytes(&crate::flash::flash_key(&secret).unwrap()).unwrap();
        let phone = crate::db::PhoneInfo { id: Some("PHONE-W".into()), name: Some("Web phone".into()), app: None };
        let items = crate::fakephone::synthetic_items(1000, 25);
        let sealed = seal::seal(&key, &seal::flash_json(&phone, &items)).unwrap();
        let encoder = Encoder::new(0xC0FFEE, &sealed, 300).unwrap();
        let mut last = Value::Null;
        for index in 0u32..200 {
            if index % 4 == 2 {
                continue;
            }
            let frame = encoder.frame(index);
            let reply = if index % 2 == 0 {
                let picture = qr::draw(&frame, 4, 4).unwrap();
                post(&format!("/api/flash/picture?w={}&h={}", picture.width, picture.height), picture.pixels)
            } else {
                post("/api/flash/frame", frame)
            };
            assert_eq!(reply.status(), StatusCode::OK);
            last = json_body(reply);
            if index % 2 == 0 {
                // What that picture held: one code, a frame of this flash.
                assert_eq!((last["last"]["codes"].as_u64(), last["last"]["frames"].as_u64()), (Some(1), Some(1)), "{last}");
            }
            if last["state"] == "done" {
                break;
            }
            assert_eq!(last["state"], "receiving");
            // One tile per chunk, and the held ones are exactly the rows.
            let held = last["blocks"]["held"].as_str().unwrap();
            assert_eq!(held.len(), encoder.k().div_ceil(8) * 2);
            let ones: u32 = (0..held.len()).step_by(2).map(|i| u8::from_str_radix(&held[i..i + 2], 16).unwrap().count_ones()).sum();
            assert_eq!(Some(ones as u64), last["rows"].as_u64());
        }
        assert_eq!(last["state"], "done", "{last}");
        assert_eq!((last["result"]["new"].as_u64(), last["result"]["paired"].as_bool()), (Some(25), Some(true)));
        assert!(last["pictures"]["with_code"].as_u64().unwrap() > 0);
        assert!(!last.to_string().contains("amber"), "no item text in a status");
        assert_eq!(state.db.lock().unwrap().total_items().unwrap(), 155);

        // A picture of the wrong size, a picture with nothing in it, and a reset.
        assert_eq!(post("/api/flash/picture?w=10&h=10", vec![0; 50]).status(), StatusCode::BAD_REQUEST);
        assert_eq!(post("/api/flash/picture", vec![0; 50]).status(), StatusCode::BAD_REQUEST);
        let blank = json_body(post("/api/flash/picture?w=40&h=30", vec![200; 1200]));
        assert_eq!((blank["state"].as_str(), blank["last"]["codes"].as_u64()), (Some("done"), Some(0)));
        assert_eq!(blank["solved"].as_u64(), Some(encoder.k() as u64));
        // A QR code that is not a flash is counted as a code, not as a frame.
        let other = qr::draw(b"https://example.invalid/menu", 4, 4).unwrap();
        let read = json_body(post(&format!("/api/flash/picture?w={}&h={}", other.width, other.height), other.pixels));
        assert_eq!((read["last"]["codes"].as_u64(), read["last"]["frames"].as_u64()), (Some(1), Some(0)), "{read}");
        assert_eq!(json_body(route(&cookie(head(Method::GET, "/api/flash/status")), &state, "n"))["setup"]["phones"], 1);
        assert_eq!(json_body(post("/api/flash/frame", b"not a frame".to_vec()))["state"], "done");
        assert_eq!(json_body(post("/api/flash/reset", Vec::new()))["state"], "waiting");
    }

    #[test]
    fn pictures_posted_together_are_read_side_by_side() {
        use crate::flash::fountain::Encoder;
        use crate::flash::{qr, seal};

        let state = state();
        let secret = "cd".repeat(32);
        state.db.lock().unwrap().create_offer(&secret, paths::now_unix(), 600.0).unwrap();
        let key = seal::key_bytes(&crate::flash::flash_key(&secret).unwrap()).unwrap();
        let phone = crate::db::PhoneInfo { id: Some("PHONE-P".into()), name: Some("Parallel phone".into()), app: None };
        let sealed = seal::seal(&key, &seal::flash_json(&phone, &crate::fakephone::synthetic_items(3000, 40))).unwrap();
        // Larger frames, as a phone may choose: 900 bytes each (version 21 codes).
        let encoder = Encoder::new(0xBEEF, &sealed, 900).unwrap();
        let pictures: Vec<qr::Gray> = (0..encoder.k() as u32 + 12).map(|i| qr::draw(&encoder.frame(i), 3, 4).unwrap()).collect();
        // Four senders at once, as the page keeps pictures in flight.
        std::thread::scope(|scope| {
            for lane in 0..4 {
                let (state, pictures) = (&state, &pictures);
                scope.spawn(move || {
                    for picture in pictures.iter().skip(lane).step_by(4) {
                        let mut h = with_cookie(head(Method::POST, &format!("/api/flash/picture?w={}&h={}", picture.width, picture.height)), state);
                        h.body = Bytes::from(picture.pixels.clone());
                        assert_eq!(route(&h, state, "n").status(), StatusCode::OK);
                    }
                });
            }
        });
        let status = json_body(route(&with_cookie(head(Method::GET, "/api/flash/status"), &state), &state, "n"));
        assert_eq!(status["state"], "done", "{status}");
        assert_eq!(status["result"]["new"], 40);
        assert_eq!(status["pictures"]["seen"].as_u64(), Some(pictures.len() as u64));
        // Before any code is in view the daemon only takes the quick look, which can miss a
        // perfectly sharp first code now and then; after that, every one is read.
        assert!(status["pictures"]["with_code"].as_u64().unwrap() + 2 >= pictures.len() as u64, "{status}");
    }

    #[test]
    fn color_pictures_through_the_page_routes() {
        use crate::flash::fountain::Encoder;
        use crate::flash::{color, seal};

        let state = state();
        let secret = "ef".repeat(32);
        state.db.lock().unwrap().create_offer(&secret, paths::now_unix(), 600.0).unwrap();
        let key = seal::key_bytes(&crate::flash::flash_key(&secret).unwrap()).unwrap();
        let phone = crate::db::PhoneInfo { id: Some("PHONE-C".into()), name: Some("Color phone".into()), app: None };
        let sealed = seal::seal(&key, &seal::flash_json(&phone, &crate::fakephone::synthetic_items(5000, 300))).unwrap();
        let encoder = Encoder::new(0xC010, &sealed, 900).unwrap();
        let post = |target: &str, body: Vec<u8>| {
            let mut h = with_cookie(head(Method::POST, target), &state);
            h.body = Bytes::from(body);
            route(&h, &state, "n")
        };
        // Three frames to a picture, so a third as many pictures.
        let mut last = Value::Null;
        let mut sent = 0;
        for first in (0..encoder.k() as u32 + 30).step_by(3) {
            let frames: Vec<Vec<u8>> = (0..3).map(|c| encoder.frame(first + c)).collect();
            let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
            let picture = color::draw(&refs, 3, 4).unwrap();
            let reply = post(&format!("/api/flash/picture?w={}&h={}&format=rgb", picture.width, picture.height), picture.pixels);
            assert_eq!(reply.status(), StatusCode::OK);
            last = json_body(reply);
            sent += 1;
            if last["state"] == "done" {
                break;
            }
            assert_eq!(last["last"], json!({ "codes": 3, "frames": 3, "useful": 3, "color": true }), "{last}");
        }
        assert_eq!(last["state"], "done", "{last}");
        assert_eq!(last["result"]["new"], 300);
        assert!(sent <= encoder.k().div_ceil(3) + 1, "{sent} pictures for {} frames", encoder.k());
        assert_eq!(last["pictures"]["color"].as_u64(), Some(sent as u64));
        // A black and white picture sent as color is still read, in gray.
        let gray = crate::flash::qr::draw(&encoder.frame(4000), 3, 4).unwrap();
        let rgb: Vec<u8> = gray.pixels.iter().flat_map(|v| [*v, *v, *v]).collect();
        let read = json_body(post(&format!("/api/flash/picture?w={}&h={}&format=rgb", gray.width, gray.height), rgb));
        assert_eq!((read["last"]["codes"].as_u64(), read["last"]["color"].as_bool()), (Some(1), Some(false)), "{read}");
        // Wrong sizes and formats are refused.
        assert_eq!(post("/api/flash/picture?w=10&h=10&format=rgb", vec![0; 100]).status(), StatusCode::BAD_REQUEST);
        assert_eq!(post("/api/flash/picture?w=10&h=10&format=cmyk", vec![0; 100]).status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn items_status_and_pairing() {
        let state = state();
        let get = |target: &str| json_body(route(&with_cookie(head(Method::GET, target), &state), &state, "n"));

        let status = get("/api/status");
        assert_eq!(status["items"]["total"], 130);
        assert_eq!(status["items"]["by_kind"]["note"], 11);
        assert_eq!(status["computer"]["endpoint_id"], "7f".repeat(32));

        let first = get("/api/items");
        assert_eq!((first["total"].as_i64(), first["items"].as_array().unwrap().len()), (Some(130), PAGE_LIMIT));
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = get(&format!("/api/items?cursor={cursor}"));
        let third = get(&format!("/api/items?cursor={}", second["next_cursor"].as_str().unwrap()));
        assert_eq!(third["items"].as_array().unwrap().len(), 10);
        assert_eq!(third["next_cursor"], Value::Null);
        let days: i64 = first["day_counts"].as_object().unwrap().values().map(|v| v.as_i64().unwrap()).sum();
        assert_eq!(days, 130);

        let notes = get("/api/items?kinds=note");
        assert_eq!(notes["total"], 11);
        assert!(notes["items"].as_array().unwrap().iter().all(|i| i["kind"] == "note"));
        let found = get("/api/items?q=harbor+amber");
        assert!(found["total"].as_i64().unwrap() > 0);
        assert!(found["items"][0]["snippet"].as_str().unwrap().contains('['));

        let id = first["items"][0]["id"].as_str().unwrap();
        assert_eq!(get(&format!("/api/item?id={id}"))["id"], id);

        let offer = json_body(route(&with_cookie(head(Method::POST, "/api/pair"), &state), &state, "n"));
        let link = pairing::PairLink::parse(offer["url"].as_str().unwrap()).unwrap();
        assert_eq!((link.endpoint_id, link.name.as_str()), ("7f".repeat(32), "desk"));
        assert!(offer["svg"].as_str().unwrap().starts_with("<svg"));
        assert!((offer["expires_in_s"].as_f64().unwrap() - 600.0).abs() < 5.0);
        assert_eq!(state.db.lock().unwrap().open_offers(paths::now_unix()).unwrap(), 1);

        assert_eq!(route(&with_cookie(head(Method::GET, "/nope"), &state), &state, "n").status(), StatusCode::NOT_FOUND);
    }
}
