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

use anyhow::{Context, Result, anyhow, bail};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

use crate::daemon_info::DaemonInfo;
use crate::db::{ACCENTS, Cursor, Db, Device, Filter, FromPhone, Order, REMOVED_KEPT_SECONDS, Row};
use crate::flash::color::{ColorRead, Rgb};
use crate::flash::qr::Gray;
use crate::flash::worker::FlashWorker;
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
/// The largest body of any other request: a list of item ids, a setting.
const MAX_JSON_BODY: usize = 256 * 1024;
const PAGE_LIMIT: usize = 60;
/// The most items one request may ask for.
const MAX_PAGE_LIMIT: usize = 500;
/// The most items one export may hold.
const MAX_EXPORT_ITEMS: usize = 50_000;
/// The most ids one request may name.
const MAX_IDS: usize = 2000;
/// How long an answer about the latest release is reused before GitHub is asked again.
const UPDATE_ANSWER_KEPT_SECONDS: f64 = 12.0 * 3600.0;
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
    /// The flash being received through the page, if any, on a thread of its own.
    pub flash: FlashWorker,
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
    // The Flash routes take a camera picture or the bytes of one frame. The page's own writes
    // take a little JSON.
    if head.method == Method::POST && head.path.starts_with("/api/") {
        let most = if head.path.starts_with("/api/flash/") { MAX_BODY } else { MAX_JSON_BODY };
        match Limited::new(request.into_body(), most).collect().await {
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
            if has_session(head, state) { page(nonce, state) } else { locked(nonce) }
        }
        "/api/status" if get => guarded(head, state, |db| status(db, state)),
        "/api/items" if get => guarded(head, state, |db| items(db, &head.query)),
        "/api/item" if get => guarded(head, state, |db| item(db, &head.query)),
        "/api/stats" if get => guarded(head, state, |db| stats(db, &head.query)),
        "/api/export" if get => guarded(head, state, |db| export(db, &head.query)),
        "/api/settings" if get => guarded(head, state, |db| settings(db, state)),
        "/api/removed" if get => guarded(head, state, removed),
        // These two touch no item: which AI clients have zone, and whether a newer zone is out.
        "/api/agents" if get => unguarded(head, state, agents),
        "/api/update" if get => unguarded(head, state, || Ok(update_answer(head.query.contains_key("again")))),
        // Every write is a POST, and like every /api/ route it needs the session cookie, the
        // right Host, and no other site as its Origin (checked above).
        "/api/pair" if post => guarded(head, state, |db| pair(db, state)),
        "/api/done" if post => guarded(head, state, |db| set_done(db, &body_json(head)?)),
        "/api/remove" if post => guarded(head, state, |db| remove(db, &body_json(head)?)),
        "/api/restore" if post => guarded(head, state, |db| restore(db, &body_json(head)?)),
        "/api/settings" if post => guarded(head, state, |db| change_settings(db, state, &body_json(head)?)),
        "/api/phone" if post => guarded(head, state, |db| rename_phone(db, &body_json(head)?)),
        "/api/unpair" if post => guarded(head, state, |db| unpair(db, &body_json(head)?)),
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
        "/api/flash/frame" if post => flash_reply(head, state, |flash| flash.frame(head.body.to_vec())),
        "/api/flash/reset" if post => flash_reply(head, state, FlashWorker::reset),
        // The font holds nothing private, so the locked page can use it too.
        "/font/zone-serif.woff2" if get => font(),
        "/" | "/flash" | "/font/zone-serif.woff2" | "/api/status" | "/api/items" | "/api/item" | "/api/pair" | "/api/flash/status" | "/api/flash/picture"
        | "/api/flash/frame" | "/api/flash/reset" | "/api/stats" | "/api/export" | "/api/settings" | "/api/removed" | "/api/agents" | "/api/update"
        | "/api/done" | "/api/remove" | "/api/restore" | "/api/phone" | "/api/unpair" => text(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed."),
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

/// A route that needs the session but not the library.
fn unguarded(head: &RequestHead, state: &WebState, work: impl FnOnce() -> Result<Value>) -> Reply {
    if !has_session(head, state) {
        return json_reply(StatusCode::UNAUTHORIZED, &json!({ "error": "locked", "fix": "Run `zone open` to unlock this page." }));
    }
    match work() {
        Ok(value) => json_reply(StatusCode::OK, &value),
        Err(error) => json_reply(StatusCode::BAD_REQUEST, &json!({ "error": format!("{error:#}") })),
    }
}

/// The JSON object a write was sent with.
fn body_json(head: &RequestHead) -> Result<Value> {
    let value: Value = serde_json::from_slice(&head.body).map_err(|_| anyhow!("the request body is not JSON"))?;
    if !value.is_object() {
        bail!("the request body is not a JSON object");
    }
    Ok(value)
}

/// The item ids a write names.
fn body_ids(body: &Value) -> Result<Vec<String>> {
    let ids: Vec<String> = body.get("ids").and_then(Value::as_array).context("ids is required: a list of item ids")?.iter().filter_map(Value::as_str).map(str::to_string).collect();
    if ids.is_empty() {
        bail!("ids is empty");
    }
    if ids.len() > MAX_IDS {
        bail!("at most {MAX_IDS} items at a time");
    }
    Ok(ids)
}

/// Runs one Flash request and answers with where the flash stands. The flash's own thread
/// does the work (see flash/worker.rs), so no request waits long.
fn flash_reply(head: &RequestHead, state: &WebState, work: impl FnOnce(&FlashWorker) -> Value) -> Reply {
    if !has_session(head, state) {
        return json_reply(StatusCode::UNAUTHORIZED, &json!({ "error": "locked", "fix": "Run `zone open` to unlock this page." }));
    }
    json_reply(StatusCode::OK, &work(&state.flash))
}

/// Where the flash stands, and whether this computer has anything to open one with: the page
/// says so before anyone holds a phone up. While a flash is being stored the library is busy,
/// and the setup is what it was a moment ago.
fn flash_status(head: &RequestHead, state: &WebState) -> Reply {
    flash_reply(head, state, |flash| {
        let mut status = flash.status();
        let fresh = match state.db.try_lock() {
            Ok(db) => Some(setup(&db)),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(setup(&poisoned.into_inner())),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(setup) = flash.setup(fresh) {
            status["setup"] = setup;
        }
        status
    })
}

fn setup(db: &Db) -> Value {
    let phones = db.devices().map(|d| d.len()).unwrap_or(0);
    let codes = db.open_offers(paths::now_unix()).unwrap_or(0);
    json!({ "phones": phones, "pairing_codes": codes })
}

/// A camera picture: `w` times `h` gray bytes, or with `format=rgb` three bytes (red, green,
/// blue) per pixel. Every QR code in it is taken as a frame: a color picture can carry three
/// (see flash/color.rs).
///
/// The codes are read here, so the pictures the page sends at once are read side by side, one
/// per processor; the flash's thread then takes their frames one at a time.
fn flash_picture(head: &RequestHead, state: &WebState) -> Reply {
    if !has_session(head, state) {
        return json_reply(StatusCode::UNAUTHORIZED, &json!({ "error": "locked", "fix": "Run `zone open` to unlock this page." }));
    }
    let side = |key: &str| -> Result<usize> { head.query.get(key).and_then(|v| v.parse().ok()).with_context(|| format!("{key} is required")) };
    let thorough = state.flash.codes_in_view();
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
    json_reply(StatusCode::OK, &state.flash.picture(codes, color))
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

/// The page, drawn from its first moment in the appearance and accent chosen in Settings.
fn page(nonce: &str, state: &WebState) -> Reply {
    let look = match state.db.try_lock() {
        Ok(db) => Look::of(&db),
        Err(_) => Look::default(),
    };
    html(StatusCode::OK, PAGE.replace("__NONCE__", nonce).replace("__THEME__", look.theme).replace("__ACCENT__", &look.accent).replace("__VERSION__", env!("CARGO_PKG_VERSION")))
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

/// The phone a request asks about: `device` is the id of a paired phone (as `/api/status`
/// lists it), or `none` for items no phone is recorded for.
fn phone_param(db: &Db, query: &HashMap<String, String>) -> Result<Option<FromPhone>> {
    match query.get("device").map(|v| v.trim()).filter(|v| !v.is_empty() && *v != "all") {
        None => Ok(None),
        Some("none") => Ok(Some(FromPhone::Unknown)),
        Some(id) => {
            let device = db.device(id)?.context("no such paired phone")?;
            // A phone that never said which install it is has no items that can be told apart.
            Ok(Some(FromPhone::Id(device.phone_id.unwrap_or_else(|| "(unknown)".to_string()))))
        }
    }
}

/// What a request selects. The recording behind a voice note is left out when the note is
/// there too, unless the request says `fold=0` or asks for those recordings by their source.
fn filter_from(db: &Db, query: &HashMap<String, String>, clock: &Clock) -> Result<Filter> {
    let time = |key: &str, bound: Bound| -> Result<Option<f64>> {
        match query.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            Some(value) => Ok(Some(clock.parse(value, bound).map_err(|e| anyhow!("{key}: {e}"))?)),
            None => Ok(None),
        }
    };
    let done = query.get("done").map(|v| v.trim()).unwrap_or("");
    if !matches!(done, "" | "all" | "open" | "done") {
        bail!("done must be open, done or all");
    }
    let sources = list_param(query, "sources");
    Ok(Filter {
        start: time("start", Bound::Start)?,
        end: time("end", Bound::End)?,
        kinds: list_param(query, "kinds").iter().map(|k| normalize_kind(k)).collect(),
        fold_note_recordings: query.get("fold").map(String::as_str) != Some("0") && !sources.iter().any(|s| s == "note"),
        sources,
        unprocessed_only: done == "open",
        processed_only: done == "done",
        phone: phone_param(db, query)?,
        scope: Default::default(),
    })
}

/// What the page needs to know to draw items: the clock, and what each phone is called.
struct Views {
    clock: Clock,
    now: f64,
    /// Install id to the name shown, for every paired phone.
    phones: HashMap<String, String>,
}

impl Views {
    fn new(db: &Db) -> Result<Views> {
        let clock = Clock::system();
        let now = unix(clock.now);
        let phones = db.devices()?.into_iter().filter_map(|d| Some((d.phone_id.clone()?, d.label()))).collect();
        Ok(Views { clock, now, phones })
    }

    fn item(&self, row: &Row, snippet: Option<&str>) -> Value {
        let clock = &self.clock;
        let total_chars = row.text.chars().count();
        let truncated = total_chars > LIST_TEXT_CHARS;
        let text: String = if truncated { row.text.chars().take(LIST_TEXT_CHARS).collect() } else { row.text.clone() };
        let mut view = json!({
            "id": row.id,
            "kind": row.kind,
            "source": row.source,
            "day": clock.day(row.created_at),
            "day_label": clock.day_label(row.created_at),
            "time": clock.time_of_day(row.created_at),
            "full_time": clock.full(row.created_at),
            "rfc3339": clock.rfc3339(row.created_at),
            "age_s": (self.now - row.created_at).max(0.0).round(),
            "duration_s": row.duration_s,
            "words": row.words,
            "title": row.title,
            "text": text,
            "truncated": truncated,
            "snippet": snippet,
            // The same flag agents set with mark_processed.
            "done": row.processed_at.is_some(),
            "phone": row.device_id.as_ref().and_then(|id| self.phones.get(id)),
        });
        if row.kind == "meeting" {
            let turns = crate::export::turns(row);
            view["speakers"] = json!(crate::export::speakers(row));
            view["turn_count"] = json!(turns.len());
            view["first_line"] = json!(turns.first().map(|turn| crate::item::preview(&turn.text, 220)));
        }
        view
    }
}

fn limit_param(query: &HashMap<String, String>) -> Result<usize> {
    match query.get("limit").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        Some(text) => Ok(text.parse::<usize>().map_err(|_| anyhow!("limit must be a number"))?.clamp(1, MAX_PAGE_LIMIT)),
        None => Ok(PAGE_LIMIT),
    }
}

fn order_param(query: &HashMap<String, String>) -> Result<Order> {
    query.get("order").map(|v| v.trim()).filter(|v| !v.is_empty()).map(Order::parse).transpose().map(Option::unwrap_or_default)
}

fn items(db: &mut Db, query: &HashMap<String, String>) -> Result<Value> {
    let views = Views::new(db)?;
    let filter = filter_from(db, query, &views.clock)?;
    let search = query.get("q").map(|q| q.trim()).filter(|q| !q.is_empty());
    let order = order_param(query)?;
    let limit = limit_param(query)?;

    // How many of them are done, whatever the request's own done filter says: the page shows
    // "12 open" beside a list of either.
    let mut either = filter.clone();
    either.unprocessed_only = false;
    either.processed_only = false;

    if let Some(q) = search {
        let result = db.search(q, &filter, Some(order), SEARCH_LIMIT)?;
        let list: Vec<Value> = result.hits.iter().map(|hit| views.item(&hit.row, Some(&hit.snippet))).collect();
        return Ok(json!({ "total": result.total, "items": list, "next_cursor": null, "matched": result.matched, "day_counts": {} }));
    }

    // The calendar's dots, only when asked for: they take a pass over everything selected.
    let day_counts = if query.contains_key("days") { crate::stats::day_counts(db, &views.clock, &filter)? } else { Map::new() };

    let cursor = match query.get("cursor").filter(|c| !c.is_empty()) {
        Some(text) => Some(Cursor::decode(text)?),
        None => None,
    };
    let total = db.count(&filter)?;
    let all = db.count(&either)?;
    let done = db.count_processed(&either)?;
    let folded = if filter.fold_note_recordings { db.count_folded(&filter)? } else { 0 };
    let mut rows = db.list(&filter, order, limit + 1, cursor)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more { rows.last().map(|row| Cursor::after(row).encode()) } else { None };
    let list: Vec<Value> = rows.iter().map(|row| views.item(row, None)).collect();
    Ok(json!({
        "total": total,
        "counts": { "all": all, "done": done, "open": all - done },
        // Recordings behind voice notes that are not listed because the note is.
        "folded": folded,
        "items": list,
        "next_cursor": next_cursor,
        "day_counts": day_counts,
    }))
}

/// One item in full. A meeting comes with its turns.
fn item(db: &mut Db, query: &HashMap<String, String>) -> Result<Value> {
    let id = query.get("id").context("id is required")?;
    let row = db.get(id)?.filter(|row| !row.deleted).context("no such item")?;
    let views = Views::new(db)?;
    let mut view = views.item(&row, None);
    view["text"] = json!(row.text);
    view["truncated"] = json!(false);
    if row.kind == "meeting" {
        let turns: Vec<Value> = crate::export::turns(&row)
            .into_iter()
            .map(|turn| json!({ "speaker": turn.speaker, "start": turn.start, "at": turn.start.map(crate::export::stamp), "text": turn.text }))
            .collect();
        view["turns"] = json!(turns);
    }
    Ok(view)
}

fn stats(db: &mut Db, query: &HashMap<String, String>) -> Result<Value> {
    let clock = Clock::system();
    let filter = filter_from(db, query, &clock)?;
    crate::stats::summary(db, &clock, &filter)
}

/// Items written out for a person: the ones named in `ids`, or everything a filter selects.
fn export(db: &mut Db, query: &HashMap<String, String>) -> Result<Value> {
    let clock = Clock::system();
    let format = crate::export::Format::parse(query.get("format").map(String::as_str).unwrap_or("md"))?;
    let order = order_param(query)?;
    let ids = list_param(query, "ids");
    let rows: Vec<Row> = if ids.is_empty() {
        let filter = filter_from(db, query, &clock)?;
        if db.count(&filter)? > MAX_EXPORT_ITEMS as i64 {
            bail!("that is more than {MAX_EXPORT_ITEMS} items. Narrow it by kind or dates, or use `zone export`.");
        }
        match query.get("q").map(|q| q.trim()).filter(|q| !q.is_empty()) {
            Some(q) => db.search(q, &filter, Some(order), MAX_EXPORT_ITEMS)?.hits.into_iter().map(|hit| hit.row).collect(),
            None => db.list(&filter, order, MAX_EXPORT_ITEMS, None)?,
        }
    } else {
        if ids.len() > MAX_IDS {
            bail!("at most {MAX_IDS} items at a time");
        }
        let mut rows = Vec::new();
        for id in &ids {
            // An item removed or deleted since the page listed it is simply left out.
            if let Some(row) = db.get(id)?.filter(|row| !row.deleted) {
                rows.push(row);
            }
        }
        rows
    };
    let title = query.get("title").map(|t| t.trim()).filter(|t| !t.is_empty());
    Ok(json!({
        "filename": crate::export::file_name(&rows, &clock, format),
        "format": format.extension(),
        "shape": crate::export::Shape::of(&rows).as_str(),
        "items": rows.len(),
        "content": crate::export::render(&rows, &clock, format, order, title),
    }))
}

// -------------------------------------------------------------------- done, and removing

/// Done on the page is the flag agents set with `mark_processed`: one column, so an agent's
/// `unprocessed_only` and the page's Open always agree.
fn set_done(db: &mut Db, body: &Value) -> Result<Value> {
    let ids = body_ids(body)?;
    let done = body.get("done").and_then(Value::as_bool).context("done is required: true or false")?;
    let (found, missing) = db.mark_processed(&ids, done, paths::now_unix())?;
    Ok(json!({ "done": done, "ids": found, "missing": missing }))
}

fn remove(db: &mut Db, body: &Value) -> Result<Value> {
    let removed = db.remove_items(&body_ids(body)?, paths::now_unix())?;
    Ok(json!({ "removed": removed, "kept_days": (REMOVED_KEPT_SECONDS / 86400.0).round() }))
}

fn restore(db: &mut Db, body: &Value) -> Result<Value> {
    let restored = db.restore_items(&body_ids(body)?)?;
    Ok(json!({ "restored": restored }))
}

/// What was removed from this computer and can still be put back.
fn removed(db: &mut Db) -> Result<Value> {
    let clock = Clock::system();
    let now = unix(clock.now);
    db.erase_old_removed(now)?;
    let list: Vec<Value> = db
        .removed_items()?
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "kind": row.kind,
                "source": row.source,
                "title": row.title,
                "preview": crate::item::preview(&row.text, 240),
                "words": row.words,
                "day_label": clock.day_label(row.created_at),
                "rfc3339": clock.rfc3339(row.created_at),
                "removed_age_s": (now - row.removed_at).max(0.0).round(),
                "days_left": ((row.removed_at + REMOVED_KEPT_SECONDS - now) / 86400.0).ceil().max(0.0),
            })
        })
        .collect();
    Ok(json!({ "items": list, "kept_days": (REMOVED_KEPT_SECONDS / 86400.0).round() }))
}

// -------------------------------------------------------------------- how the page looks

/// What can be chosen for the page's appearance. `phone` follows the iPhone, once one says
/// how Zone looks there.
const APPEARANCE_CHOICES: [&str; 4] = ["system", "light", "dark", "phone"];
const SETTING_APPEARANCE: &str = "page_appearance";
const SETTING_ACCENT: &str = "page_accent";

/// The page's look: what was chosen, and what that comes to right now.
struct Look {
    /// `system`, `light`, `dark` or `phone`.
    appearance: String,
    /// One of [`ACCENTS`], or `phone`.
    accent_choice: String,
    /// What the page is drawn in: `light`, `dark`, or `auto` to follow the computer.
    theme: &'static str,
    /// The accent the page is drawn with, one of [`ACCENTS`].
    accent: String,
    /// The phone whose look was last reported: its name, appearance and accent.
    phone: Option<(String, Option<String>, Option<String>)>,
}

impl Default for Look {
    fn default() -> Look {
        Look { appearance: "system".to_string(), accent_choice: "phone".to_string(), theme: "auto", accent: "violet".to_string(), phone: None }
    }
}

impl Look {
    fn of(db: &Db) -> Look {
        let devices = db.devices().unwrap_or_default();
        Look::from(db.meta_get(SETTING_APPEARANCE).ok().flatten(), db.meta_get(SETTING_ACCENT).ok().flatten(), &devices)
    }

    fn from(appearance: Option<String>, accent: Option<String>, devices: &[Device]) -> Look {
        // The phone seen most recently among those that said how they look.
        let phone = devices
            .iter()
            .filter(|d| d.appearance.is_some() || d.accent.is_some())
            .max_by(|a, b| a.last_seen.unwrap_or(0.0).total_cmp(&b.last_seen.unwrap_or(0.0)))
            .map(|d| (d.label(), d.appearance.clone(), d.accent.clone()));
        let appearance = appearance.filter(|a| APPEARANCE_CHOICES.contains(&a.as_str())).unwrap_or_else(|| "system".to_string());
        // Until an accent is chosen here, the iPhone's is used when it is known.
        let accent_choice = accent.filter(|a| a == "phone" || ACCENTS.contains(&a.as_str())).unwrap_or_else(|| "phone".to_string());
        let phone_appearance = phone.as_ref().and_then(|p| p.1.as_deref());
        let theme = match (appearance.as_str(), phone_appearance) {
            ("light", _) | ("phone", Some("light")) => "light",
            ("dark", _) | ("phone", Some("dark")) => "dark",
            _ => "auto",
        };
        let accent = match accent_choice.as_str() {
            "phone" => phone.as_ref().and_then(|p| p.2.clone()).unwrap_or_else(|| "violet".to_string()),
            chosen => chosen.to_string(),
        };
        Look { appearance, accent_choice, theme, accent, phone }
    }

    fn json(&self) -> Value {
        json!({
            "appearance": self.appearance,
            "accent": self.accent_choice,
            "theme": self.theme,
            "accent_shown": self.accent,
            "accents": ACCENTS,
            "phone": self.phone.as_ref().map(|(name, appearance, accent)| json!({ "name": name, "appearance": appearance, "accent": accent })),
        })
    }
}

fn clipboard_json(db: &Db) -> Value {
    let tool = crate::clip::tool_name();
    json!({
        "enabled": crate::clip::enabled(db),
        "available": tool.is_some(),
        "tool": tool,
        "fix": if tool.is_none() { Some(crate::clip::how_to_get_a_tool()) } else { None },
        "max_age_s": crate::clip::FRESH_SECONDS,
    })
}

/// Everything the Settings view shows that lives in the library or the data folder.
fn settings(db: &mut Db, state: &WebState) -> Result<Value> {
    let now = paths::now_unix();
    let dir = paths::data_dir()?;
    let backup_folder = db.meta_get("backup_folder")?;
    let backup_state = match state.info.lock() {
        Ok(info) => crate::tools::backup_folder_state(Some(&info), backup_folder.as_deref(), now),
        Err(_) => Value::Null,
    };
    let exe = paths::current_exe().ok();
    Ok(json!({
        "look": Look::of(db).json(),
        "clipboard": clipboard_json(db),
        "backup": {
            "folder": backup_folder,
            "state": backup_state,
            "last_read_age_s": db.meta_f64("last_sync_backup")?.map(|t| (now - t).max(0.0).round()),
        },
        "data": {
            "folder": dir.to_string_lossy(),
            "library": paths::database(&dir).to_string_lossy(),
            "log": paths::log_file(&dir).to_string_lossy(),
        },
        "computer": { "name": state.computer_name, "version": env!("CARGO_PKG_VERSION"), "zone": exe.map(|p| p.to_string_lossy().to_string()) },
        "removed": db.removed_count()?,
    }))
}

/// Changes what the body names: `appearance`, `accent`, `clipboard`. Answers with the settings.
fn change_settings(db: &mut Db, state: &WebState, body: &Value) -> Result<Value> {
    let known = ["appearance", "accent", "clipboard"];
    if let Some(other) = body.as_object().and_then(|o| o.keys().find(|k| !known.contains(&k.as_str()))) {
        bail!("{other} is not a setting this page can change");
    }
    if let Some(value) = body.get("appearance") {
        let choice = value.as_str().filter(|v| APPEARANCE_CHOICES.contains(v)).context("appearance must be system, light, dark or phone")?;
        db.meta_set(SETTING_APPEARANCE, choice)?;
    }
    if let Some(value) = body.get("accent") {
        let choice = value.as_str().filter(|v| *v == "phone" || ACCENTS.contains(v)).context("accent must be violet, ice, ember, mint, rose or phone")?;
        db.meta_set(SETTING_ACCENT, choice)?;
    }
    if let Some(value) = body.get("clipboard") {
        crate::clip::set_enabled(db, value.as_bool().context("clipboard must be true or false")?)?;
    }
    settings(db, state)
}

/// Which AI clients have zone registered, and what the agents there may do and see. Read
/// from the clients' own settings, as `zone mcp status` does. Nothing is changed.
fn agents() -> Result<Value> {
    let status = crate::clients::status_json()?;
    let shown: Vec<Value> = status["clients"]
        .as_array()
        .map(|list| list.iter().filter(|c| c["installed"] == true || c["state"] != "not_registered").cloned().collect())
        .unwrap_or_default();
    Ok(json!({ "zone": status["zone"], "automatic": status["automatic"], "clients": shown }))
}

/// Whether a newer zone is out: the same question `zone update --check` asks, asked at most
/// twice a day however often the page looks (`again` asks now).
fn update_answer(again: bool) -> Value {
    static LAST: Mutex<Option<(f64, Value)>> = Mutex::new(None);
    let now = paths::now_unix();
    if !again
        && let Ok(last) = LAST.lock()
        && let Some((at, answer)) = last.as_ref()
        && now - at < UPDATE_ANSWER_KEPT_SECONDS
    {
        return answer.clone();
    }
    let origin = paths::current_exe().map(|exe| crate::update::origin_of(&exe)).unwrap_or(crate::update::Origin::Script);
    let answer = update_view(&crate::update::check(), origin);
    if let Ok(mut last) = LAST.lock() {
        *last = Some((now, answer.clone()));
    }
    answer
}

/// The check as the page shows it: one line of words beside the version, and how to update.
fn update_view(check: &crate::update::Check, origin: crate::update::Origin) -> Value {
    use crate::update::Check;
    let mut view = crate::update::check_json(check, origin);
    view["words"] = json!(match check {
        Check::Latest { .. } => "Up to date".to_string(),
        Check::Newer { latest, .. } => format!("{latest} is available"),
        Check::Unknown(why) => format!("Could not check: {why}"),
        Check::Off => "Checking for updates is turned off".to_string(),
    });
    view
}

// -------------------------------------------------------------------- phones

fn phone_json(device: &Device, items: i64, now: f64) -> Value {
    let age = |t: Option<f64>| t.map(|t| (now - t).max(0.0).round());
    json!({
        // The phone's public key: it names the phone to this page and is no secret.
        "id": device.endpoint_id,
        // What it is called here, then what the phone calls itself, then what was typed here.
        "label": device.label(),
        "name": device.name,
        "nickname": device.nickname,
        "app": device.app,
        "paired_age_s": age(Some(device.paired_at)),
        "last_sync_age_s": age(device.last_sync),
        "last_seen_age_s": age(device.last_seen),
        "items_received": device.items_received,
        // Items in the library now that this phone delivered.
        "items": items,
        "last_path": device.last_path,
        "appearance": device.appearance,
        "accent": device.accent,
    })
}

/// Gives a phone a name on this computer. An empty name goes back to the phone's own.
fn rename_phone(db: &mut Db, body: &Value) -> Result<Value> {
    let id = body.get("id").and_then(Value::as_str).context("id is required: the phone's id")?;
    let nickname = body.get("nickname").and_then(Value::as_str);
    if !db.set_device_nickname(id, nickname)? {
        bail!("no such paired phone");
    }
    let device = db.device(id)?.context("no such paired phone")?;
    Ok(json!({ "id": device.endpoint_id, "label": device.label(), "name": device.name, "nickname": device.nickname }))
}

/// Forgets a paired phone, as `zone unpair` does. Its items stay.
fn unpair(db: &mut Db, body: &Value) -> Result<Value> {
    let id = body.get("id").and_then(Value::as_str).context("id is required: the phone's id")?;
    let device = db.device(id)?.context("no such paired phone")?;
    db.remove_device(&device.endpoint_id)?;
    crate::events::record(db, "unpaired", json!({ "device": device.name, "device_id": device.endpoint_id }));
    Ok(json!({ "unpaired": device.endpoint_id, "label": device.label() }))
}

fn status(db: &mut Db, state: &WebState) -> Result<Value> {
    let clock = Clock::system();
    let now = unix(clock.now);
    let age = |t: Option<f64>| t.map(|t| (now - t).max(0.0).round());
    // Words removed more than thirty days ago go now: the page asks this every few seconds
    // while it is open, and zone asks every hour by itself.
    db.erase_old_removed(now)?;
    let by_phone = db.counts_by_phone()?;
    let devices = db.devices()?;
    let count_of = |phone_id: &Option<String>| by_phone.iter().find(|(id, _)| id.is_some() && id == phone_id).map(|(_, n)| *n).unwrap_or(0);
    let phones: Vec<Value> = devices.iter().map(|d| phone_json(d, count_of(&d.phone_id), now)).collect();
    // Items no paired phone is recorded for: read from a backup folder, or sent by a phone
    // that has since been unpaired.
    let paired: Vec<&String> = devices.iter().filter_map(|d| d.phone_id.as_ref()).collect();
    let from_none: i64 = by_phone.iter().filter(|(id, _)| id.is_none()).map(|(_, n)| *n).sum();
    let from_unpaired: i64 = by_phone.iter().filter(|(id, _)| id.as_ref().is_some_and(|id| !paired.contains(&id))).map(|(_, n)| *n).sum();

    // The page counts a voice note once: its recording is folded into the note.
    let shown = Filter { fold_note_recordings: true, ..Filter::default() };
    let mut by_kind = Map::new();
    for (kind, count) in db.counts_by_kind_filtered(&shown)? {
        by_kind.insert(kind, json!(count));
    }
    let mut by_source = Map::new();
    for (kind, source, count) in db.counts_by_kind_and_source()? {
        if kind == "dictation"
            && let Some(source) = source {
                by_source.insert(source, json!(count));
            }
    }
    let folded = db.count_folded(&shown)?;
    if folded > 0 && let Some(count) = by_source.get("note").and_then(Value::as_i64) {
        if count > folded { by_source.insert("note".to_string(), json!(count - folded)); } else { by_source.remove("note"); }
    }
    let notes = Filter { kinds: vec!["note".to_string()], ..Filter::default() };
    let notes_all = db.count(&notes)?;
    let notes_done = db.count_processed(&notes)?;
    let link_sync = db.meta_f64("last_sync_link")?;
    let backup_sync = db.meta_f64("last_sync_backup")?;
    let total = db.count(&shown)?;
    let done = db.count_processed(&Filter::default())?;
    let removed = db.removed_count()?;
    let backup_folder = db.meta_get("backup_folder")?;
    let (relay_connected, relay_url, backup_folder_state) = match state.info.lock() {
        Ok(info) => (info.relay_connected, info.relay_url.clone(), crate::tools::backup_folder_state(Some(&info), backup_folder.as_deref(), now)),
        Err(_) => (false, None, Value::Null),
    };
    let look = Look::from(db.meta_get(SETTING_APPEARANCE)?, db.meta_get(SETTING_ACCENT)?, &devices);
    Ok(json!({
        "computer": { "name": state.computer_name, "endpoint_id": state.endpoint_id, "version": env!("CARGO_PKG_VERSION") },
        "relay": { "connected": relay_connected, "url": relay_url },
        "phones": phones,
        "items_without_a_phone": { "none_recorded": from_none, "unpaired": from_unpaired },
        "last_sync": { "link_age_s": age(link_sync), "backup_age_s": age(backup_sync) },
        "backup_folder": backup_folder,
        "backup_folder_state": backup_folder_state,
        "items": {
            "total": total,
            "by_kind": by_kind,
            "dictations_by_source": by_source,
            "folded": folded,
            "notes_open": notes_all - notes_done,
            "notes_done": notes_done,
            "removed": removed,
        },
        "look": look.json(),
        // Changes whenever anything arrives or is marked, here or by an agent, so the page
        // knows to refresh.
        "revision": format!(
            "{total}:{done}:{removed}:{}:{}:{}",
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
        let db = Arc::new(Mutex::new(db));
        WebState {
            db: db.clone(),
            token: "5e".repeat(32),
            port: PORT,
            endpoint_id: "7f".repeat(32),
            computer_name: "desk".to_string(),
            info: Arc::new(Mutex::new(DaemonInfo::default())),
            // Long enough that every answer is given after its picture was taken in.
            flash: FlashWorker::start_waiting(db, std::time::Duration::from_secs(60)),
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
        for page in [PAGE, FLASH_PAGE, LOCKED_PAGE] {
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
        for needle in ["NotAllowedError", "Send by Flash", "Pair a phone", "not from Zone", "aria-live", "role=\"progressbar\"", "prefers-reduced-motion", ":focus-visible"] {
            assert!(FLASH_PAGE.contains(needle), "{needle}");
        }
        // Pairing is done from the library's Pair a phone, not from a terminal.
        assert!(!FLASH_PAGE.contains("zone pair"));
        // A flash stored before says so, and zone being busy putting one together is not taken
        // for the phone going out of sight.
        // The flash goes by the name the phone shows under its codes and in its Done question.
        for needle in ["already_stored", "status.result.already_stored", "storingSince", "status.name", "\"Flash \" + flashName"] {
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
        let phone = crate::db::PhoneInfo { id: Some("PHONE-W".into()), name: Some("Web phone".into()), app: None, ..Default::default() };
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
        let phone = crate::db::PhoneInfo { id: Some("PHONE-P".into()), name: Some("Parallel phone".into()), app: None, ..Default::default() };
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
        let phone = crate::db::PhoneInfo { id: Some("PHONE-C".into()), name: Some("Color phone".into()), app: None, ..Default::default() };
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

        let first = get("/api/items?days=1");
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
    fn post_json(state: &WebState, target: &str, body: Value) -> Reply {
        let mut h = with_cookie(head(Method::POST, target), state);
        h.origin = Some(format!("http://127.0.0.1:{PORT}"));
        h.fetch_site = Some("same-origin".to_string());
        h.body = Bytes::from(body.to_string());
        route(&h, state, "n")
    }

    fn get_json(state: &WebState, target: &str) -> Value {
        json_body(route(&with_cookie(head(Method::GET, target), state), state, "n"))
    }

    const WRITES: [&str; 6] = ["/api/done", "/api/remove", "/api/restore", "/api/settings", "/api/phone", "/api/unpair"];

    #[test]
    fn every_write_needs_the_session_and_refuses_other_sites() {
        let state = state();
        let id = get_json(&state, "/api/items")["items"][0]["id"].as_str().unwrap().to_string();
        for path in WRITES {
            let body = json!({ "ids": [id], "done": true, "appearance": "dark", "id": "nobody" }).to_string();
            // No session: locked, whatever the body.
            let mut h = head(Method::POST, path);
            h.body = Bytes::from(body.clone());
            assert_eq!(route(&h, &state, "n").status(), StatusCode::UNAUTHORIZED, "{path}");
            // A page on another site, even with the cookie: refused before anything is read.
            for (origin, site) in [(Some("https://evil.example"), None), (None, Some("cross-site")), (None, Some("same-site")), (Some("http://localhost:1"), Some("same-origin"))] {
                let mut h = with_cookie(head(Method::POST, path), &state);
                h.origin = origin.map(str::to_string);
                h.fetch_site = site.map(str::to_string);
                h.body = Bytes::from(body.clone());
                assert_eq!(route(&h, &state, "n").status(), StatusCode::FORBIDDEN, "{path} {origin:?} {site:?}");
            }
            // The wrong Host (DNS rebinding) is refused too.
            let mut h = with_cookie(head(Method::POST, path), &state);
            h.host = Some("evil.example:47813".to_string());
            assert_eq!(route(&h, &state, "n").status(), StatusCode::FORBIDDEN, "{path}");
            // A write is never a link: only /api/settings can be read with GET, and that changes nothing.
            if path != "/api/settings" {
                assert_eq!(route(&with_cookie(head(Method::GET, path), &state), &state, "n").status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
            }
            // A body that is not a JSON object is an error, not a guess.
            for bad in ["", "[1]", "ids=1"] {
                let mut h = with_cookie(head(Method::POST, path), &state);
                h.body = Bytes::from(bad);
                assert_eq!(route(&h, &state, "n").status(), StatusCode::BAD_REQUEST, "{path} {bad:?}");
            }
        }
        // Nothing above changed anything.
        let db = state.db.lock().unwrap();
        assert_eq!((db.total_items().unwrap(), db.unprocessed_items().unwrap(), db.removed_count().unwrap()), (130, 130, 0));
        assert_eq!(db.meta_get(SETTING_APPEARANCE).unwrap(), None);
        drop(db);
        // The reads the page added are locked like the rest.
        for path in ["/api/stats", "/api/export", "/api/settings", "/api/removed", "/api/agents", "/api/update"] {
            assert_eq!(route(&head(Method::GET, path), &state, "n").status(), StatusCode::UNAUTHORIZED, "{path}");
            let mut h = with_cookie(head(Method::GET, path), &state);
            h.fetch_site = Some("cross-site".to_string());
            assert_eq!(route(&h, &state, "n").status(), StatusCode::FORBIDDEN, "{path}");
        }
    }

    #[test]
    fn done_on_the_page_is_what_agents_call_processed() {
        let state = state();
        let agent = |name: &str, args: Value| {
            let access = crate::tools::Access::default();
            crate::tools::call(&mut state.db.lock().unwrap(), name, args.as_object().unwrap(), &access).unwrap()
        };
        let notes = get_json(&state, "/api/items?kinds=note&done=open");
        assert_eq!((notes["total"].as_i64(), notes["counts"]["open"].as_i64(), notes["counts"]["done"].as_i64()), (Some(11), Some(11), Some(0)));
        let ids: Vec<String> = notes["items"].as_array().unwrap().iter().take(3).map(|i| i["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(notes["items"][0]["done"], false);

        // Ticked on the page: the agent's unprocessed_only no longer lists them.
        let reply = json_body(post_json(&state, "/api/done", json!({ "ids": ids, "done": true })));
        assert_eq!((reply["ids"].as_array().unwrap().len(), reply["missing"].as_array().unwrap().len()), (3, 0));
        let left = agent("list_items", json!({ "kinds": ["note"], "unprocessed_only": true, "limit": 50 }));
        assert_eq!(left["total"], 8);
        assert!(left["items"].as_array().unwrap().iter().all(|card| !ids.contains(&card["id"].as_str().unwrap().to_string())));
        assert_eq!(agent("get_items", json!({ "ids": [ids[0]] }))["items"][0]["processed"], true);

        // Marked by an agent: the page shows it done, and its Open list drops it.
        let fourth = notes["items"][3]["id"].as_str().unwrap().to_string();
        agent("mark_processed", json!({ "ids": [fourth] }));
        let open = get_json(&state, "/api/items?kinds=note&done=open");
        assert_eq!((open["total"].as_i64(), open["counts"]["done"].as_i64()), (Some(7), Some(4)));
        let done = get_json(&state, "/api/items?kinds=note&done=done&order=oldest");
        assert_eq!(done["total"], 4);
        assert!(done["items"].as_array().unwrap().iter().all(|i| i["done"] == true));
        assert!(done["items"].as_array().unwrap().iter().any(|i| i["id"] == fourth.as_str()));
        assert_eq!(get_json(&state, "/api/status")["items"]["notes_open"], 7);

        // Undone on the page: open again for the agent. The revision moved each time.
        let before = get_json(&state, "/api/status")["revision"].clone();
        post_json(&state, "/api/done", json!({ "ids": [fourth], "done": false }));
        assert_eq!(agent("list_items", json!({ "kinds": ["note"], "unprocessed_only": true }))["total"], 8);
        assert_ne!(get_json(&state, "/api/status")["revision"], before);
        assert_eq!(post_json(&state, "/api/done", json!({ "ids": [fourth] })).status(), StatusCode::BAD_REQUEST);
        assert_eq!(route(&with_cookie(head(Method::GET, "/api/items?done=maybe"), &state), &state, "n").status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn removing_hides_from_everyone_and_can_be_put_back() {
        let state = state();
        let first = get_json(&state, "/api/items?limit=2");
        let ids: Vec<String> = first["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect();
        let word = first["items"][0]["text"].as_str().unwrap().split(' ').next().unwrap().to_string();
        let reply = json_body(post_json(&state, "/api/remove", json!({ "ids": ids })));
        assert_eq!((reply["removed"].as_array().unwrap().len(), reply["kept_days"].as_f64()), (2, Some(30.0)));

        // Gone from the page, from an export, from one item's own address, and from agents.
        assert_eq!(get_json(&state, "/api/items")["total"], 128);
        assert_eq!(get_json(&state, "/api/status")["items"]["removed"], 2);
        assert_eq!(route(&with_cookie(head(Method::GET, &format!("/api/item?id={}", ids[0])), &state), &state, "n").status(), StatusCode::BAD_REQUEST);
        let exported = get_json(&state, &format!("/api/export?format=jsonl&ids={}", ids.join(",")));
        assert_eq!((exported["items"].as_i64(), exported["content"].as_str()), (Some(0), Some("")));
        assert_eq!(get_json(&state, "/api/export?format=csv")["items"], 128);
        let access = crate::tools::Access::default();
        let got = crate::tools::call(&mut state.db.lock().unwrap(), "get_items", json!({ "ids": [ids[0]] }).as_object().unwrap(), &access).unwrap();
        assert!(!got.to_string().contains(&word) || got["items"].as_array().is_none_or(|items| items.is_empty()), "{got}");
        assert_eq!(crate::tools::call(&mut state.db.lock().unwrap(), "list_items", json!({}).as_object().unwrap(), &access).unwrap()["total"], 128);

        // The phone sends everything again: they stay away.
        let items: Vec<Item> = crate::fakephone::synthetic_items(0, 130).iter().map(|v| Item::from_json(v).unwrap()).collect();
        state.db.lock().unwrap().apply_items(&items, &Delivery { carrier: Carrier::Link, device_id: None, received_at: 9.0 }).unwrap();
        assert_eq!(get_json(&state, "/api/items")["total"], 128);

        // Listed in Settings with what they said, and put back from there.
        let gone = get_json(&state, "/api/removed");
        assert_eq!(gone["items"].as_array().unwrap().len(), 2);
        assert!(gone["items"].as_array().unwrap().iter().all(|i| i["days_left"] == 30.0 && !i["preview"].as_str().unwrap().is_empty()));
        let back = json_body(post_json(&state, "/api/restore", json!({ "ids": [ids[0], "NOT-AN-ITEM"] })));
        assert_eq!(back["restored"], json!([ids[0]]));
        assert_eq!(get_json(&state, "/api/items")["total"], 129);
        assert_eq!(get_json(&state, &format!("/api/item?id={}", ids[0]))["text"], first["items"][0]["text"]);
        assert_eq!(get_json(&state, "/api/removed")["items"].as_array().unwrap().len(), 1);
        assert_eq!(post_json(&state, "/api/remove", json!({ "ids": [] })).status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn exports_stats_and_one_item_through_the_routes() {
        let state = state();
        // Notes alone are a checklist, with the done ones ticked.
        let notes = get_json(&state, "/api/items?kinds=note&order=oldest");
        let oldest = notes["items"][0]["id"].as_str().unwrap().to_string();
        post_json(&state, "/api/done", json!({ "ids": [oldest], "done": true }));
        let list = get_json(&state, "/api/export?format=md&kinds=note&order=oldest");
        assert_eq!((list["shape"].as_str(), list["items"].as_i64(), list["format"].as_str()), (Some("checklist"), Some(11), Some("md")));
        let content = list["content"].as_str().unwrap();
        assert!(content.starts_with("# Notes\n\n11 notes, 10 open."), "{content}");
        assert_eq!((content.matches("- [x] ").count(), content.matches("- [ ] ").count()), (1, 10));
        assert!(content.find("- [x] ").unwrap() < content.find("- [ ] ").unwrap(), "oldest first, and the oldest is the done one");
        assert!(list["filename"].as_str().unwrap().starts_with("zone-notes-") && list["filename"].as_str().unwrap().ends_with(".md"));
        assert_eq!(get_json(&state, "/api/export?format=md&kinds=note&done=open")["items"], 10);

        // One meeting is a transcript, and its own page has the turns.
        let meeting = get_json(&state, "/api/items?kinds=meeting&limit=1")["items"][0].clone();
        assert_eq!((meeting["speakers"].clone(), meeting["turn_count"].as_i64()), (json!(["Ada", "Grace"]), Some(2)));
        assert!(!meeting["first_line"].as_str().unwrap().is_empty());
        let id = meeting["id"].as_str().unwrap();
        let transcript = get_json(&state, &format!("/api/export?format=md&ids={id}"));
        assert_eq!(transcript["shape"], "meeting");
        assert!(transcript["content"].as_str().unwrap().starts_with("# Synthetic meeting ") && transcript["content"].as_str().unwrap().contains("**Ada** (0:00)\n"));
        assert!(transcript["filename"].as_str().unwrap().starts_with("zone-meeting-synthetic-meeting-"));
        let whole = get_json(&state, &format!("/api/item?id={id}"));
        assert_eq!((whole["turns"][1]["speaker"].as_str(), whole["turns"][1]["at"].as_str()), (Some("Grace"), Some("0:06")));

        // Anything else is a document by day, in every format.
        let dated = get_json(&state, "/api/export?format=txt&title=Everything");
        assert_eq!((dated["shape"].as_str(), dated["items"].as_i64()), (Some("dated"), Some(130)));
        assert!(dated["content"].as_str().unwrap().starts_with("EVERYTHING\n"));
        let csv = get_json(&state, "/api/export?format=csv&kinds=dictation");
        assert_eq!(csv["content"].as_str().unwrap().lines().count() as i64, csv["items"].as_i64().unwrap() + 1);
        let json_export = get_json(&state, "/api/export?format=json&kinds=meeting");
        assert_eq!(serde_json::from_str::<Value>(json_export["content"].as_str().unwrap()).unwrap().as_array().unwrap().len(), 2);
        let found = get_json(&state, "/api/export?format=jsonl&q=harbor+amber");
        assert!(found["items"].as_i64().unwrap() > 0);
        assert_eq!(route(&with_cookie(head(Method::GET, "/api/export?format=pdf"), &state), &state, "n").status(), StatusCode::BAD_REQUEST);

        // Statistics: everything is counted, and a filter narrows it.
        let stats = get_json(&state, "/api/stats");
        assert_eq!((stats["all"]["items"].as_i64(), stats["days"].as_array().unwrap().len(), stats["hours"].as_array().unwrap().len()), (Some(130), 30, 24));
        assert_eq!(stats["by_kind"]["note"]["items"], 11);
        assert_eq!(stats["day_counts"].as_object().unwrap().values().map(|v| v.as_i64().unwrap()).sum::<i64>(), 130);
        assert_eq!(get_json(&state, "/api/stats?kinds=meeting")["all"]["items"], 2);
        // A page of a chosen size, oldest first.
        let page = get_json(&state, "/api/items?limit=7&order=oldest");
        assert_eq!(page["items"].as_array().unwrap().len(), 7);
        assert!(page["items"][0]["rfc3339"].as_str().unwrap() < page["items"][6]["rfc3339"].as_str().unwrap());
        assert_eq!(page["day_counts"], json!({}));
    }

    #[test]
    fn settings_phones_and_the_look_of_the_page() {
        let state = state();
        // Two phones, both calling themselves iPhone. One says how Zone looks on it.
        {
            let mut db = state.db.lock().unwrap();
            for (n, endpoint) in ["aa11", "bb22"].into_iter().enumerate() {
                let secret = paths::hex(&[n as u8 + 7; 32]);
                db.create_offer(&secret, paths::now_unix(), 600.0).unwrap();
                let phone = crate::db::PhoneInfo {
                    id: Some(format!("PHONE-{n}")),
                    name: Some("iPhone".into()),
                    appearance: (n == 1).then(|| "light".to_string()),
                    accent: (n == 1).then(|| "mint".to_string()),
                    ..Default::default()
                };
                db.admit(endpoint, Some(&secret), &phone, paths::now_unix() - 50.0 + n as f64).unwrap();
            }
            let items: Vec<Item> = crate::fakephone::synthetic_items(500, 20).iter().map(|v| Item::from_json(v).unwrap()).collect();
            db.apply_items(&items[..15], &Delivery { carrier: Carrier::Link, device_id: Some("PHONE-0"), received_at: 6.0 }).unwrap();
            db.apply_items(&items[15..], &Delivery { carrier: Carrier::Link, device_id: Some("PHONE-1"), received_at: 6.0 }).unwrap();
        }
        let status = get_json(&state, "/api/status");
        let phones = status["phones"].as_array().unwrap();
        assert_eq!((phones.len(), phones[0]["id"].as_str(), phones[0]["label"].as_str(), phones[0]["items"].as_i64(), phones[1]["items"].as_i64()), (2, Some("aa11"), Some("iPhone"), Some(15), Some(5)));
        // The 130 items of the test library came with no phone on record.
        assert_eq!(status["items_without_a_phone"], json!({ "none_recorded": 130, "unpaired": 0 }));

        // Each phone's items, the ones with no phone, and all of them.
        assert_eq!(get_json(&state, "/api/items?device=aa11")["total"], 15);
        assert_eq!(get_json(&state, "/api/items?device=bb22")["total"], 5);
        assert_eq!(get_json(&state, "/api/items?device=none")["total"], 130);
        assert_eq!(get_json(&state, "/api/items?device=all")["total"], 150);
        assert_eq!(get_json(&state, "/api/stats?device=bb22")["all"]["items"], 5);
        assert_eq!(get_json(&state, "/api/export?format=jsonl&device=bb22")["items"], 5);
        assert_eq!(route(&with_cookie(head(Method::GET, "/api/items?device=cc33"), &state), &state, "n").status(), StatusCode::BAD_REQUEST);

        // A name given on this computer, shown on the phone's items; cleared, the phone's own returns.
        let renamed = json_body(post_json(&state, "/api/phone", json!({ "id": "bb22", "nickname": "Work phone" })));
        assert_eq!((renamed["label"].as_str(), renamed["name"].as_str()), (Some("Work phone"), Some("iPhone")));
        assert_eq!(get_json(&state, "/api/items?device=bb22&limit=1")["items"][0]["phone"], "Work phone");
        assert_eq!(get_json(&state, "/api/status")["phones"][1]["label"], "Work phone");
        assert_eq!(json_body(post_json(&state, "/api/phone", json!({ "id": "bb22", "nickname": "" })))["label"], "iPhone");
        assert_eq!(post_json(&state, "/api/phone", json!({ "id": "zz", "nickname": "x" })).status(), StatusCode::BAD_REQUEST);

        // The look: the computer's until chosen, with the iPhone's accent once it is known.
        let page_with = |theme: &str, accent: &str| {
            let page = body(route(&with_cookie(head(Method::GET, "/"), &state), &state, "n"));
            assert!(page.contains(&format!("<html lang=\"en\" data-theme=\"{theme}\" data-accent=\"{accent}\" ")), "wanted {theme} {accent}");
            assert!(!page.contains("__THEME__") && !page.contains("__ACCENT__"));
        };
        let look = get_json(&state, "/api/settings")["look"].clone();
        assert_eq!((look["appearance"].as_str(), look["accent"].as_str(), look["theme"].as_str(), look["accent_shown"].as_str()), (Some("system"), Some("phone"), Some("auto"), Some("mint")));
        assert_eq!(look["phone"], json!({ "name": "iPhone", "appearance": "light", "accent": "mint" }));
        page_with("auto", "mint");
        let changed = json_body(post_json(&state, "/api/settings", json!({ "appearance": "phone" })));
        assert_eq!(changed["look"]["theme"], "light");
        page_with("light", "mint");
        post_json(&state, "/api/settings", json!({ "appearance": "dark", "accent": "rose" }));
        page_with("dark", "rose");
        assert_eq!(get_json(&state, "/api/status")["look"]["accent_shown"], "rose");
        for bad in [json!({ "appearance": "sepia" }), json!({ "accent": "#ff0000" }), json!({ "clipboard": "yes" }), json!({ "backup_folder": "/tmp" })] {
            assert_eq!(post_json(&state, "/api/settings", bad.clone()).status(), StatusCode::BAD_REQUEST, "{bad}");
        }
        page_with("dark", "rose");

        // The clipboard setting is the one `zone clipboard` changes.
        assert_eq!(get_json(&state, "/api/settings")["clipboard"]["enabled"], false);
        assert_eq!(json_body(post_json(&state, "/api/settings", json!({ "clipboard": true })))["clipboard"]["enabled"], true);
        assert!(crate::clip::enabled(&state.db.lock().unwrap()));
        post_json(&state, "/api/settings", json!({ "clipboard": false }));
        assert!(!crate::clip::enabled(&state.db.lock().unwrap()));
        let settings = get_json(&state, "/api/settings");
        assert_eq!((settings["computer"]["name"].as_str(), settings["computer"]["version"].as_str(), settings["removed"].as_i64()), (Some("desk"), Some(env!("CARGO_PKG_VERSION")), Some(0)));
        assert!(settings["data"]["library"].as_str().unwrap().ends_with("zone.db"));

        // Unpairing forgets the phone and keeps its items, now with no paired phone.
        let gone = json_body(post_json(&state, "/api/unpair", json!({ "id": "bb22" })));
        assert_eq!(gone["unpaired"], "bb22");
        let status = get_json(&state, "/api/status");
        assert_eq!((status["phones"].as_array().unwrap().len(), status["items"]["total"].as_i64()), (1, Some(150)));
        assert_eq!(status["items_without_a_phone"], json!({ "none_recorded": 130, "unpaired": 5 }));
        assert_eq!(post_json(&state, "/api/unpair", json!({ "id": "bb22" })).status(), StatusCode::BAD_REQUEST);
        // With the phone that said how it looks gone, following it falls back to violet.
        post_json(&state, "/api/settings", json!({ "accent": "phone", "appearance": "phone" }));
        page_with("auto", "violet");
    }

    #[test]
    fn the_version_is_said_once_with_what_to_do() {
        use crate::update::{Check, Origin};
        let newer = update_view(&Check::Newer { latest: "9.9.9".into(), page: "p".into() }, Origin::Homebrew);
        assert_eq!((newer["words"].as_str(), newer["newer"].as_bool(), newer["update_with"].as_str()), (Some("9.9.9 is available"), Some(true), Some("brew upgrade zone")));
        assert_eq!(newer["current"], env!("CARGO_PKG_VERSION"));
        let latest = update_view(&Check::Latest { latest: "0.1.2".into() }, Origin::Script);
        assert_eq!((latest["words"].as_str(), latest["newer"].as_bool()), (Some("Up to date"), Some(false)));
        assert!(update_view(&Check::Unknown("no answer".into()), Origin::Script)["words"].as_str().unwrap().starts_with("Could not check"));
        assert_eq!(update_view(&Check::Off, Origin::Script)["checked"], false);
    }

    #[test]
    fn the_page_knows_which_zone_served_it() {
        // An open page compares this with the version in every status answer, and loads the
        // new page when zone was replaced under it.
        let state = state();
        let page = body(route(&with_cookie(head(Method::GET, "/"), &state), &state, "n"));
        assert!(page.contains(&format!("data-zone=\"{}\"", env!("CARGO_PKG_VERSION"))) && !page.contains("__VERSION__"));
        assert!(PAGE.contains("reloadIfReplaced(next.computer && next.computer.version)"));
        let status = get_json(&state, "/api/status");
        assert_eq!(status["computer"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_page_has_its_views_keys_and_no_decoration() {
        // The wave that meant nothing is gone, and the page knows every route and key.
        assert!(!PAGE.contains("<canvas") && !PAGE.contains("drawWave"));
        for needle in ["VIEWS.home", "VIEWS.notes", "VIEWS.meetings", "VIEWS.meeting", "VIEWS.history", "VIEWS.stats", "VIEWS.settings", "\"/api/done\"", "\"/api/remove\"", "\"/api/restore\"", "/api/export?", "\"/api/phone\"", "\"/api/unpair\"", "Pair another phone", "Still on your iPhone", "prefers-reduced-motion", ":focus-visible", "data-theme=\"__THEME__\""] {
            assert!(PAGE.contains(needle), "{needle}");
        }
        for accent in ACCENTS {
            assert!(accent == "violet" || PAGE.contains(&format!(":root[data-accent=\"{accent}\"]")), "{accent}");
        }
        assert!(!PAGE.contains(" style=\"") && !PAGE.contains("onclick="), "nothing the policy would block");
    }

}
