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
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

use crate::daemon_info::DaemonInfo;
use crate::db::{Cursor, Db, Filter, Order, Row};
use crate::pairing;
use crate::paths;
use crate::timeparse::{Bound, Clock, unix};
use crate::tools::normalize_kind;
use crate::warn;

/// The port the page is served on unless it is taken.
pub const DEFAULT_PORT: u16 = 47813;

const PAGE: &str = include_str!("../assets/index.html");
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
    let reply = match tokio::task::spawn_blocking({
        let nonce = nonce.clone();
        let head = RequestHead::from(&request);
        move || route(&head, &state, &nonce)
    })
    .await
    {
        Ok(reply) => reply,
        Err(_) => text(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong."),
    };
    secure(reply, &nonce)
}

/// The parts of a request the routes need. Bodies are never read: no route takes one.
pub struct RequestHead {
    pub method: Method,
    pub path: String,
    pub query: HashMap<String, String>,
    pub host: Option<String>,
    pub origin: Option<String>,
    pub fetch_site: Option<String>,
    pub cookie: Option<String>,
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
                    return redirect_with_session(state);
                }
                return locked();
            }
            if has_session(head, state) { page(nonce) } else { locked() }
        }
        "/api/status" if get => guarded(head, state, |db| status(db, state)),
        "/api/items" if get => guarded(head, state, |db| items(db, &head.query)),
        "/api/item" if get => guarded(head, state, |db| item(db, &head.query)),
        "/api/pair" if post => guarded(head, state, |db| pair(db, state)),
        // Carrier 2 (Flash) plugs in here: a page that reads QR frames with the camera and an
        // endpoint that takes the reassembled payload and calls `Db::apply_items` with
        // `Carrier::Flash`. Neither exists yet.
        "/flash" | "/api/flash" => text(StatusCode::NOT_IMPLEMENTED, "Flash is not built yet."),
        "/" | "/api/status" | "/api/items" | "/api/item" | "/api/pair" => text(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed."),
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

fn locked() -> Reply {
    html(
        StatusCode::UNAUTHORIZED,
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"dark light\"><title>Zone</title>\
         <body><h1>This page is locked</h1><p>Run <code>zone open</code> in a terminal. It opens this page with the key \
         that unlocks it in this browser.</p></body>"
            .to_string(),
    )
}

fn redirect_with_session(state: &WebState) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::new()));
    *reply.status_mut() = StatusCode::SEE_OTHER;
    reply.headers_mut().insert(header::LOCATION, HeaderValue::from_static("/"));
    let cookie = format!("{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", cookie_name(state.port), state.token);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        reply.headers_mut().insert(header::SET_COOKIE, value);
    }
    reply
}

/// Headers on every reply. Nothing may be loaded from anywhere but this page itself.
fn secure(mut reply: Reply, nonce: &str) -> Reply {
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; img-src data:; \
         connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
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
        "revision": format!("{total}:{}:{}", link_sync.unwrap_or(0.0), backup_sync.unwrap_or(0.0)),
    }))
}

fn pair(db: &mut Db, state: &WebState) -> Result<Value> {
    let offer = pairing::create_offer(db, &state.endpoint_id, &state.computer_name, None)?;
    let url = offer.link.to_url();
    Ok(json!({
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
    use http_body_util::BodyExt;

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
        // Without anything: locked, and the page is not sent.
        let reply = route(&head(Method::GET, "/"), &state, "n");
        assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
        assert!(!body(reply).contains("Search what you said"));
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
        assert!(page.contains("Search what you said"));
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
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    }

    #[test]
    fn the_page_loads_nothing_from_elsewhere() {
        // Every URL in the page is either the SVG namespace or a relative API path.
        let mut rest = PAGE;
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
            assert!(!PAGE.contains(forbidden), "{forbidden}");
        }
        assert!(!PAGE.contains('\u{2014}'), "no em dashes in the page");
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

        // The Flash seam answers honestly.
        assert_eq!(route(&with_cookie(head(Method::GET, "/flash"), &state), &state, "n").status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(route(&with_cookie(head(Method::GET, "/nope"), &state), &state, "n").status(), StatusCode::NOT_FOUND);
    }
}
