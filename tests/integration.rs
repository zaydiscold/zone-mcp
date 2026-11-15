//! End-to-end tests that run the real `zone` program.
//!
//! Every test uses a temporary `ZONE_HOME` and a temporary home folder. Nothing here reads or
//! writes the real library, the real login items or the real AI client configs.
//!
//! The tests marked `#[ignore]` need the internet (iroh's public relays and address lookup).
//! Run them with `cargo test -- --ignored`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const ZONE: &str = env!("CARGO_BIN_EXE_zone");

/// Where the program under test is, the way zone itself writes its own path: resolved, and on
/// Windows without the `\\?\` prefix a resolved path starts with.
fn zone_exe() -> PathBuf {
    let resolved = std::fs::canonicalize(ZONE).unwrap();
    let text = resolved.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// A temporary world: a data directory and a home folder that belong to one test.
struct Sandbox {
    _dir: tempfile::TempDir,
    zone_home: PathBuf,
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let zone_home = dir.path().join("zone-home");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { _dir: dir, zone_home, home }
    }

    /// `zone` with every path that could reach the real machine pointed into the sandbox.
    fn zone(&self) -> Command {
        let mut command = Command::new(ZONE);
        command
            .env("ZONE_HOME", &self.zone_home)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("AppData").join("Roaming"))
            .env("LOCALAPPDATA", self.home.join("AppData").join("Local"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env("ZONE_NAME", "Test Computer")
            .env("PATH", self.search_path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("HERMES_HOME")
            .env_remove("ZONE_PORT")
            .env_remove("ZONE_RELAY_URL");
        command
    }

    /// A PATH with the sandbox's own `bin` folder and the system's basic tools only, so no
    /// AI client installed on the real computer is found or run.
    fn search_path(&self) -> std::ffi::OsString {
        let bin = self.home.join("bin");
        #[cfg(windows)]
        let system = {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            vec![PathBuf::from(format!(r"{root}\System32")), PathBuf::from(root)]
        };
        #[cfg(not(windows))]
        let system = vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin"), PathBuf::from("/usr/sbin"), PathBuf::from("/sbin")];
        std::env::join_paths(std::iter::once(bin).chain(system)).unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.zone().args(args).output().unwrap()
    }

    /// Runs a command that must succeed and returns its standard output.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "zone {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let text = self.ok(args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("zone {args:?} did not print JSON ({e}):\n{text}"))
    }

    fn daemon_info(&self) -> Option<Value> {
        let bytes = std::fs::read(self.zone_home.join("daemon.json")).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// A running `zone serve`, stopped when the test ends.
struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(sandbox: &Sandbox) -> Daemon {
        Daemon::start_with_env(sandbox, &[])
    }

    fn start_with_env(sandbox: &Sandbox, env: &[(&str, &str)]) -> Daemon {
        let child = sandbox
            .zone()
            .envs(env.iter().copied())
            .args(["serve", "--port", "0", "--quiet"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Daemon { child };
        let deadline = Instant::now() + Duration::from_secs(30);
        while sandbox.daemon_info().is_none() {
            assert!(Instant::now() < deadline, "zone serve did not start within 30 seconds");
            std::thread::sleep(Duration::from_millis(100));
        }
        daemon
    }

    /// Waits until the daemon reports a relay connection. Needs the internet.
    fn wait_for_relay(sandbox: &Sandbox) -> String {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(info) = sandbox.daemon_info()
                && info["relay_connected"] == true {
                    return info["relay_url"].as_str().unwrap().to_string();
                }
            assert!(Instant::now() < deadline, "the daemon did not reach an iroh relay within 45 seconds (is this computer online?)");
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The address the daemon's iroh endpoint listens on, as reachable from this computer.
fn loopback_addr(sandbox: &Sandbox) -> String {
    let info = sandbox.daemon_info().unwrap();
    let port = info["bound_sockets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|s| s.starts_with("0.0.0.0:") || s.starts_with("127.0.0.1:"))
        .and_then(|s| s.rsplit(':').next())
        .unwrap()
        .to_string();
    format!("127.0.0.1:{port}")
}

fn pair_url(sandbox: &Sandbox) -> String {
    sandbox.json(&["pair", "--json"])["url"].as_str().unwrap().to_string()
}

fn phone(sandbox: &Sandbox, args: &[&str]) -> Value {
    let mut all = vec!["dev", "fake-phone"];
    all.extend_from_slice(args);
    sandbox.json(&all)
}

fn total_items(sandbox: &Sandbox) -> i64 {
    sandbox.json(&["status", "--json"])["items"]["total"].as_i64().unwrap()
}

/// The whole Link conversation against a real daemon, over real iroh, on this computer.
/// The phone is given the daemon's loopback address, so this needs no internet.
#[test]
fn link_pairing_and_sync_over_iroh() {
    let sandbox = Sandbox::new();

    // Without a daemon, pairing explains what to do instead of handing out a dead code.
    let no_daemon = sandbox.run(&["pair"]);
    assert!(!no_daemon.status.success());
    let message = String::from_utf8_lossy(&no_daemon.stderr).to_string();
    assert!(message.contains("zone is not running") && message.contains("zone serve"), "{message}");

    let _daemon = Daemon::start(&sandbox);
    let addr = loopback_addr(&sandbox);
    let identity = sandbox.home.join("phone.json");
    let identity = identity.to_str().unwrap();

    let url = pair_url(&sandbox);
    assert!(url.starts_with("zone://pair?v=1&n="));
    assert!(url.contains("&name=Test%20Computer"));
    let endpoint_id = sandbox.daemon_info().unwrap()["endpoint_id"].as_str().unwrap().to_string();
    assert_eq!(endpoint_id.len(), 64);
    assert!(url.contains(&format!("n={endpoint_id}&")));

    // Pair and push 600 items in four frames of 150.
    let report = phone(&sandbox, &["--url", &url, "--identity", identity, "--count", "600", "--batch", "150", "--addr", &addr]);
    assert_eq!(report["result"], "welcome", "{report}");
    assert_eq!(report["presented_secret"], true);
    assert_eq!(report["computer"]["name"], "Test Computer");
    assert_eq!(report["computer"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["items_before"], 0);
    assert_eq!(report["resend_all"], false);
    let acks = report["acks"].as_array().unwrap();
    assert_eq!(acks.len(), 4);
    for (index, ack) in acks.iter().enumerate() {
        assert_eq!(ack["seq"], index as u64 + 1);
        assert_eq!(ack["stored"], 150);
    }
    assert_eq!(report["stored"], 600);
    assert_eq!(report["path"], "direct", "a loopback address was given, so the path is direct");
    let database_id = report["computer"]["id"].as_str().unwrap().to_string();

    // Row counts by kind match what was sent.
    let status = sandbox.json(&["status", "--json"]);
    assert_eq!(status["items"]["total"], 600);
    assert_eq!(status["items"]["by_kind"], json!({ "dictation": 540, "note": 48, "meeting": 12 }));
    assert_eq!(status["phones"][0]["name"], "Fake iPhone");
    assert_eq!(status["phones"][0]["items_received"], 600);
    assert_eq!(status["daemon"]["running"], true);

    // Reconnect with the saved identity and no secret: accepted.
    let report = phone(&sandbox, &["--identity", identity, "--count", "100", "--start-index", "600", "--addr", &addr]);
    assert_eq!(report["result"], "welcome", "{report}");
    assert_eq!(report["presented_secret"], false);
    assert_eq!(report["items_before"], 600);
    assert_eq!(report["computer"]["id"].as_str().unwrap(), database_id, "the database id is stable");
    assert_eq!(report["stored"], 100);
    assert_eq!(total_items(&sandbox), 700);

    // Sending the same items again is harmless.
    let report = phone(&sandbox, &["--identity", identity, "--count", "700", "--batch", "500", "--addr", &addr]);
    assert_eq!((report["frames"].as_u64(), report["stored"].as_u64()), (Some(2), Some(700)));
    assert_eq!(total_items(&sandbox), 700);

    // An identity that never paired is refused.
    let report = phone(&sandbox, &["--url", &url, "--no-secret", "--count", "5", "--addr", &addr]);
    assert_eq!((report["result"].as_str(), report["reason"].as_str()), (Some("refused"), Some("not_paired")), "{report}");

    // The secret was used up by the first phone: a second phone cannot pair with it.
    let report = phone(&sandbox, &["--url", &url, "--count", "5", "--addr", &addr]);
    assert_eq!((report["result"].as_str(), report["reason"].as_str()), (Some("refused"), Some("bad_secret")), "{report}");

    // A secret that was never issued.
    let forged = format!("zone://pair?v=1&n={endpoint_id}&s={}&name=x", "ab".repeat(32));
    let report = phone(&sandbox, &["--url", &forged, "--count", "5", "--addr", &addr]);
    assert_eq!((report["result"].as_str(), report["reason"].as_str()), (Some("refused"), Some("bad_secret")), "{report}");

    // A phone speaking a later protocol version.
    let report = phone(&sandbox, &["--identity", identity, "--protocol-version", "2", "--addr", &addr]);
    assert_eq!((report["result"].as_str(), report["reason"].as_str()), (Some("refused"), Some("version")), "{report}");
    assert_eq!(total_items(&sandbox), 700, "refused sessions stored nothing");

    // Items from a JSONL file, one of which has no kind: it is counted as rejected, the
    // rest are stored, and the session carries on.
    let jsonl = sandbox.home.join("items.jsonl");
    let lines = [
        json!({ "id": "0A000000-0000-4000-8000-000000000001", "kind": "note", "created_at": 1794000000.0, "text": "from a file" }).to_string(),
        json!({ "id": "0A000000-0000-4000-8000-000000000002", "created_at": 1794000001.0, "text": "no kind" }).to_string(),
        json!({ "id": "0a000000-0000-4000-8000-000000000003", "kind": "dictation", "source": "watch", "created_at": 1794000002.0, "updated_at": 1794000003.0, "text": "lowercase id, unknown source", "surprise": true }).to_string(),
    ];
    std::fs::write(&jsonl, lines.join("\n")).unwrap();
    let report = phone(&sandbox, &["--identity", identity, "--file", jsonl.to_str().unwrap(), "--addr", &addr]);
    assert_eq!((report["acks"][0]["stored"].as_u64(), report["acks"][0]["rejected"].as_u64()), (Some(2), Some(1)), "{report}");
    assert_eq!(total_items(&sandbox), 702);
    // Ids are stored uppercase, and a source this version has never heard of passes through.
    let shown = sandbox.json(&["show", "0A000000-0000-4000-8000-000000000003", "--json"]);
    assert_eq!((shown[0]["source"].as_str(), shown[0]["carrier"].as_str()), (Some("watch"), Some("link")));
    let report = phone(&sandbox, &["--identity", identity, "--delete", "0A000000-0000-4000-8000-000000000001", "--delete", "0A000000-0000-4000-8000-000000000003", "--addr", &addr]);
    assert_eq!(report["stored"], 2);
    assert_eq!(total_items(&sandbox), 700);

    // A deletion removes the item from every listing.
    let victim = "FA4E0000-0000-4000-8000-000000000003";
    let before = sandbox.json(&["show", victim, "--json"]);
    assert_eq!(before[0]["id"], victim);
    let report = phone(&sandbox, &["--identity", identity, "--delete", victim, "--addr", &addr]);
    assert_eq!(report["stored"], 1);
    assert_eq!(total_items(&sandbox), 699);
    let listed = sandbox.json(&["list", "--all", "--json"]);
    assert_eq!(listed["total"], 699);
    assert!(listed["items"].as_array().unwrap().iter().all(|item| item["id"] != victim));

    // The phone sending that item live again brings it back, although its updated_at is older
    // than the deletion: the phone is the source of truth.
    let report = phone(&sandbox, &["--identity", identity, "--count", "1", "--start-index", "3", "--addr", &addr]);
    assert_eq!(report["stored"], 1);
    assert_eq!(total_items(&sandbox), 700);
    assert_eq!(sandbox.json(&["show", victim, "--json"])[0]["text"], before[0]["text"]);
    let report = phone(&sandbox, &["--identity", identity, "--delete", victim, "--addr", &addr]);
    assert_eq!(report["stored"], 1);
    assert_eq!(total_items(&sandbox), 699);

    // Asking for a full resend shows up in the next welcome, once.
    sandbox.ok(&["resync"]);
    let report = phone(&sandbox, &["--identity", identity, "--count", "1", "--addr", &addr]);
    assert_eq!(report["resend_all"], true);
    let report = phone(&sandbox, &["--identity", identity, "--count", "1", "--addr", &addr]);
    assert_eq!(report["resend_all"], false);

    // The daemon's log never contains item text.
    let log = std::fs::read_to_string(sandbox.zone_home.join("zone.log")).unwrap();
    assert!(log.contains("sent 600 items in 4 frames"));
    for word in ["amber", "basalt", "juniper", "Synthetic meeting"] {
        assert!(!log.contains(word), "the log contains item text: {word}");
    }

    // Unpairing ends it: the same identity is now refused.
    let unpaired = sandbox.ok(&["unpair", "Fake iPhone"]);
    assert!(unpaired.contains("Unpaired Fake iPhone"));
    let report = phone(&sandbox, &["--identity", identity, "--count", "1", "--addr", &addr]);
    assert_eq!((report["result"].as_str(), report["reason"].as_str()), (Some("refused"), Some("not_paired")), "{report}");
    assert_eq!(total_items(&sandbox), 699, "its items stay");
}

/// The off-network path: the phone has no IP transport at all, so every packet goes through
/// an iroh relay. Needs the internet.
#[test]
#[ignore = "needs the internet: uses iroh's public relays"]
fn link_sync_through_the_relay_only() {
    let sandbox = Sandbox::new();
    let _daemon = Daemon::start(&sandbox);
    let relay = Daemon::wait_for_relay(&sandbox);
    let identity = sandbox.home.join("phone.json");
    let identity = identity.to_str().unwrap();

    // The link carries the relay address, so this does not depend on address lookup.
    let url = sandbox.json(&["pair", "--relay", "--json"])["url"].as_str().unwrap().to_string();
    assert!(url.contains("&r=https%3A%2F%2F"), "{url}");

    let report = phone(&sandbox, &["--url", &url, "--identity", identity, "--count", "300", "--batch", "100", "--relay-only"]);
    assert_eq!(report["result"], "welcome", "{report}");
    assert_eq!(report["relay_only"], true);
    assert_eq!(report["path"], "relay", "with no IP transport the only path is the relay ({relay})");
    assert_eq!(report["acks"].as_array().unwrap().len(), 3);
    assert_eq!(report["stored"], 300);
    assert_eq!(total_items(&sandbox), 300);

    // Reconnect, still relay only, no secret.
    let report = phone(&sandbox, &["--identity", identity, "--count", "50", "--start-index", "300", "--relay-only"]);
    assert_eq!((report["result"].as_str(), report["path"].as_str()), (Some("welcome"), Some("relay")), "{report}");
    assert_eq!(total_items(&sandbox), 350);

    let status = sandbox.json(&["status", "--json"]);
    assert_eq!(status["phones"][0]["last_path"], "relay");
}

/// What a real phone does: it knows only the endpoint id from the pairing link and finds the
/// computer through iroh's address lookup. Needs the internet.
#[test]
#[ignore = "needs the internet: uses iroh's address lookup and relays"]
fn link_sync_found_by_endpoint_id_alone() {
    let sandbox = Sandbox::new();
    let _daemon = Daemon::start(&sandbox);
    Daemon::wait_for_relay(&sandbox);
    let url = pair_url(&sandbox);
    assert!(!url.contains("&r="), "the plain link has no relay hint");
    let identity = sandbox.home.join("phone.json");
    let identity = identity.to_str().unwrap();

    // The daemon publishes its address a moment after it reaches the relay.
    let deadline = Instant::now() + Duration::from_secs(60);
    let report = loop {
        let report = phone(&sandbox, &["--url", &url, "--identity", identity, "--count", "250", "--batch", "100", "--timeout", "15"]);
        if report["result"] != "unreachable" || Instant::now() > deadline {
            break report;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    assert_eq!(report["result"], "welcome", "{report}");
    assert_eq!(report["stored"], 250);
    let path = report["path"].as_str().unwrap();
    assert!(path.contains("relay") || path.contains("direct"), "{path}");
    eprintln!("found by endpoint id alone, path: {path}");
    assert_eq!(total_items(&sandbox), 250);
}

// ------------------------------------------------------------------------- web page

fn http(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, response)
}

#[test]
fn web_page_is_local_and_locked() {
    let sandbox = Sandbox::new();
    let _daemon = Daemon::start(&sandbox);
    let port = sandbox.daemon_info().unwrap()["http_port"].as_u64().unwrap() as u16;
    let host = format!("127.0.0.1:{port}");
    let get = |path: &str, host: &str, extra: &str| http(port, &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n"));

    // Locked without the token, and the page itself is not sent.
    let (status, body) = get("/", &host, "");
    assert_eq!(status, 401);
    assert!(!body.contains("Search what you said"));
    assert_eq!(get("/api/items", &host, "").0, 401);

    // A request that arrives under another host name is refused (DNS rebinding).
    assert_eq!(get("/", &format!("evil.example:{port}"), "").0, 403);
    assert_eq!(get("/api/status", "evil.example", "").0, 403);

    // `zone open --print` gives the link with the key. It becomes a cookie and a redirect.
    let link = sandbox.ok(&["open", "--print"]);
    let token = link.trim().split("?t=").nth(1).unwrap().to_string();
    assert_eq!(token.len(), 64);
    let (status, response) = get(&format!("/?t={token}"), &host, "");
    assert_eq!(status, 303);
    let lower = response.to_ascii_lowercase();
    assert!(lower.contains("location: /\r\n"));
    let cookie_line = response.lines().find(|l| l.to_ascii_lowercase().starts_with("set-cookie:")).unwrap();
    assert!(cookie_line.contains("HttpOnly") && cookie_line.contains("SameSite=Strict"));
    let cookie = cookie_line.split_once(':').unwrap().1.trim().split(';').next().unwrap().to_string();

    // `zone open --flash --print` leads to the Flash page the same way.
    let flash_link = sandbox.ok(&["open", "--flash", "--print"]);
    assert_eq!(flash_link.trim(), format!("http://{host}/flash?t={token}"));
    let (status, response) = get(&format!("/flash?t={token}"), &host, "");
    assert_eq!(status, 303);
    assert!(response.to_ascii_lowercase().contains("location: /flash\r\n") && response.to_ascii_lowercase().contains("set-cookie:"));
    assert_eq!(get("/flash?t=wrong", &host, "").0, 401);
    assert_eq!(get("/flash", &host, "").0, 401);
    assert_eq!(get("/flash", &host, &format!("Cookie: {cookie}\r\n")).0, 200);

    // With the cookie: the page, with a strict policy and no CORS headers.
    let with_cookie = format!("Cookie: {cookie}\r\n");
    let (status, response) = get("/", &host, &with_cookie);
    assert_eq!(status, 200);
    assert!(response.contains("Search what you said"));
    let lower = response.to_ascii_lowercase();
    assert!(lower.contains("content-security-policy: default-src 'none'"));
    assert!(!lower.contains("access-control-allow"));
    let (status, response) = get("/api/status", &host, &with_cookie);
    assert_eq!(status, 200);
    assert!(response.contains("\"endpoint_id\""));
    assert_eq!(get("/api/status", "localhost:1", &with_cookie).0, 403);

    // A page on another site cannot create a pairing code, even with the cookie.
    let post = |extra: &str| http(port, &format!("POST /api/pair HTTP/1.1\r\nHost: {host}\r\n{with_cookie}{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"));
    assert_eq!(post("Origin: https://evil.example\r\n").0, 403);
    let (status, response) = post(&format!("Origin: http://{host}\r\n"));
    assert_eq!(status, 200);
    assert!(response.contains("zone://pair?v=1&n=") && response.contains("<svg"));

    // A second daemon for the same data folder refuses to start.
    let second = sandbox.run(&["serve", "--port", "0", "--quiet"]);
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));
}

// ------------------------------------------------------------------------- flash

/// Posts bytes to the daemon's page and returns the status and the JSON it answered with.
fn http_post(port: u16, path: &str, cookie: &str, body: &[u8]) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nCookie: {cookie}\r\n\
         Content-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    // The daemon may answer (413) and close before a body that is too large is all sent, so a
    // failed write is not an error here: the answer is what counts.
    let _ = stream.write_all(body);
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let json = response.split_once("\r\n\r\n").and_then(|(_, body)| serde_json::from_str(body).ok()).unwrap_or(Value::Null);
    (status, json)
}

/// Unlocks the page the way `zone open` does and returns the session cookie.
fn page_cookie(sandbox: &Sandbox, port: u16) -> String {
    let link = sandbox.ok(&["open", "--print"]);
    let token = link.trim().split("?t=").nth(1).unwrap().to_string();
    let (status, response) = http(port, &format!("GET /?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"));
    assert_eq!(status, 303);
    let line = response.lines().find(|l| l.to_ascii_lowercase().starts_with("set-cookie:")).unwrap();
    line.split_once(':').unwrap().1.trim().split(';').next().unwrap().to_string()
}

/// A grayscale PNG as written by `zone dev flash-encode`: width, height, one byte per pixel.
fn read_gray_png(path: &Path) -> (usize, usize, Vec<u8>) {
    let decoder = png::Decoder::new(BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0u8; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((frame.color_type, frame.bit_depth), (png::ColorType::Grayscale, png::BitDepth::Eight));
    pixels.truncate(frame.buffer_size());
    (frame.width as usize, frame.height as usize, pixels)
}

/// A flash from pictures to the library: `flash-encode` draws the frames a phone would show,
/// `flash-decode` reads them back, and the daemon takes them as camera pictures over HTTP.
#[test]
fn flash_pictures_through_the_command_line_and_the_daemon() {
    let sandbox = Sandbox::new();
    let _daemon = Daemon::start(&sandbox);
    let port = sandbox.daemon_info().unwrap()["http_port"].as_u64().unwrap() as u16;
    let frames = sandbox.home.join("frames");
    let frames_text = frames.to_str().unwrap();

    // The phone scanned a pairing code (and nothing else: it has never been online).
    let url = pair_url(&sandbox);
    let secret = url.split("&s=").nth(1).unwrap().split('&').next().unwrap().to_string();
    assert_eq!(secret.len(), 64);
    let made = sandbox.json(&[
        "dev", "flash-encode", "--out", frames_text, "--secret", &secret, "--count", "130", "--scale", "3", "--phone-id", "PHONE-FLASH", "--name", "Flash phone",
    ]);
    let k = made["k"].as_u64().unwrap();
    assert!(k > 10 && made["frames"].as_u64().unwrap() > k, "{made}");

    // flash-decode reads the folder back and reports counts, never text.
    let output = sandbox.ok(&["dev", "flash-decode", frames_text, "--secret", &secret]);
    let report: Value = serde_json::from_str(&output).unwrap();
    assert_eq!((report["complete"].as_bool(), report["items"].as_u64(), report["k"].as_u64()), (Some(true), Some(130), Some(k)));
    assert_eq!(report["by_kind"], json!({ "dictation": 117, "meeting": 2, "note": 11 }));
    assert!(!output.contains("amber") && !output.contains("harbor"), "flash-decode printed item text");
    assert_eq!(total_items(&sandbox), 0, "nothing is stored without --import");
    // The wrong secret opens nothing.
    let wrong = sandbox.run(&["dev", "flash-decode", frames_text, "--secret", &"cd".repeat(32)]);
    assert!(!wrong.status.success());

    // The daemon takes the same pictures as the page's camera would post them. Every fourth
    // picture is missed and they arrive from the last to the first.
    let cookie = page_cookie(&sandbox, port);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&frames).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files.reverse();
    let (code, before) = http(port, &format!("GET /api/flash/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"));
    assert_eq!(code, 200);
    let before: Value = serde_json::from_str(before.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!((before["state"].as_str(), before["setup"]["pairing_codes"].as_u64()), (Some("waiting"), Some(1)), "{before}");
    let ones = |hex: &str| -> u64 { (0..hex.len()).step_by(2).map(|i| u64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap().count_ones())).sum() };
    let mut last = Value::Null;
    let mut most_solved = 0;
    for (n, file) in files.iter().enumerate() {
        if n % 4 == 3 {
            continue;
        }
        let (width, height, pixels) = read_gray_png(file);
        let (status, answer) = http_post(port, &format!("/api/flash/picture?w={width}&h={height}"), &cookie, &pixels);
        assert_eq!(status, 200, "{answer}");
        last = answer;
        assert_eq!((last["last"]["codes"].as_u64(), last["last"]["frames"].as_u64()), (Some(1), Some(1)), "{last}");
        if last["state"] == "done" {
            break;
        }
        assert_eq!(last["state"], "receiving", "{last}");
        // The page's mosaic: one bit per chunk, the held ones exactly the rows, the solved
        // ones among them.
        let held = last["blocks"]["held"].as_str().unwrap();
        let solved = last["blocks"]["solved"].as_str().unwrap();
        assert_eq!(held.len() as u64, k.div_ceil(8) * 2);
        assert_eq!(Some(ones(held)), last["rows"].as_u64());
        assert_eq!(Some(ones(solved)), last["solved"].as_u64());
        most_solved = most_solved.max(ones(solved));
    }
    assert_eq!(last["state"], "done", "{last}");
    assert_eq!(last["k"].as_u64(), Some(k));
    assert_eq!(ones(last["blocks"]["solved"].as_str().unwrap()), k, "every tile is lit at the end");
    assert!(most_solved < k, "the last frames resolve the rest at once");
    assert_eq!(last["result"], json!({ "ok": true, "phone": "Flash phone", "items": 130, "new": 130, "changed": 0, "unchanged": 0, "deletions": 0, "rejected": 0, "paired": true }));
    assert_eq!(total_items(&sandbox), 130);

    // The flash paired the phone and used the code up.
    let devices = sandbox.json(&["devices", "--json"]);
    let list = devices.as_array().or_else(|| devices["devices"].as_array()).unwrap().clone();
    assert_eq!(list.len(), 1, "{devices}");
    assert_eq!(list[0]["name"], "Flash phone");
    assert_eq!(list[0]["items_received"], 130);
    let refused = phone(&sandbox, &["--url", &url, "--addr", &loopback_addr(&sandbox), "--count", "1"]);
    assert_eq!(refused["reason"], "bad_secret", "{refused}");

    // Without the page's key the daemon takes no pictures.
    assert_eq!(http_post(port, "/api/flash/picture?w=4&h=4", "zone_session_1=nope", &[0u8; 16]).0, 401);
    // More than a picture's worth of bytes is refused before it is read in full.
    assert_eq!(http_post(port, "/api/flash/picture?w=4&h=4", &cookie, &vec![0u8; 5 * 1024 * 1024]).0, 413);

    // The log names the phone and the counts, and none of the words.
    let log = std::fs::read_to_string(sandbox.zone_home.join("zone.log")).unwrap_or_default();
    assert!(log.contains("flash: Flash phone sent 130 items"), "{log}");
    assert!(!log.contains("amber") && !log.contains("harbor"));

    // Later flashes open with the phone's kept key: here straight from the folder, with
    // --import and no secret. Everything is already there.
    let again = sandbox.json(&["dev", "flash-decode", frames_text, "--import"]);
    assert_eq!(again["imported"], json!({ "new": 0, "changed": 0, "unchanged": 130, "paired": false }));
    assert_eq!(total_items(&sandbox), 130);

    // The feed: the code, the pairing it led to, both flashes, and the phone that was refused.
    let events = feed(&sandbox, &[]);
    assert_eq!(feed_kinds(&events), ["daemon_started", "pair_offered", "paired", "flash_received", "link_refused", "flash_received"], "{events:#?}");
    let paired = events.iter().find(|e| e["event"] == "paired").unwrap();
    assert_eq!((paired["device"].as_str(), paired["via"].as_str(), paired["device_id"].as_str()), (Some("Flash phone"), Some("flash"), Some("flash:PHONE-FLASH")), "{paired}");
    assert_eq!(paired["offer"], events.iter().find(|e| e["event"] == "pair_offered").unwrap()["offer"]);
    let flashes: Vec<&Value> = events.iter().filter(|e| e["event"] == "flash_received").collect();
    assert_eq!(
        (flashes[0]["items"].as_u64(), flashes[0]["new"].as_u64(), flashes[0]["unchanged"].as_u64(), flashes[0]["paired"].as_bool(), flashes[0]["color"].as_bool()),
        (Some(130), Some(130), Some(0), Some(true), Some(false)),
        "{}",
        flashes[0]
    );
    assert_eq!(flashes[0]["bytes"].as_u64(), made["sealed_bytes"].as_u64());
    assert_eq!((flashes[1]["new"].as_u64(), flashes[1]["unchanged"].as_u64(), flashes[1]["paired"].as_bool()), (Some(0), Some(130), Some(false)), "{}", flashes[1]);
    assert!(sandbox.ok(&["events"]).contains("Flash phone sent a Flash: 130 new, and is now paired"));
}

fn read_rgb_png(path: &Path) -> (usize, usize, Vec<u8>) {
    let decoder = png::Decoder::new(BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0u8; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((frame.color_type, frame.bit_depth), (png::ColorType::Rgb, png::BitDepth::Eight));
    pixels.truncate(frame.buffer_size());
    (frame.width as usize, frame.height as usize, pixels)
}

/// Color Flash: three frames to a picture, drawn by `flash-encode --color`, read back by
/// `flash-decode`, and taken by the daemon as color camera pictures.
#[test]
fn color_flash_pictures_through_the_command_line_and_the_daemon() {
    let sandbox = Sandbox::new();
    let _daemon = Daemon::start(&sandbox);
    let port = sandbox.daemon_info().unwrap()["http_port"].as_u64().unwrap() as u16;
    let url = pair_url(&sandbox);
    let secret = url.split("&s=").nth(1).unwrap().split('&').next().unwrap().to_string();
    let pictures = sandbox.home.join("color");
    let dir = pictures.to_str().unwrap();
    let made = sandbox.json(&["dev", "flash-encode", "--color", "--out", dir, "--secret", &secret, "--count", "300", "--chunk", "700", "--scale", "3", "--name", "Color phone"]);
    let k = made["k"].as_u64().unwrap();
    let count = made["pictures"].as_u64().unwrap();
    assert_eq!(made["color"], true);
    assert_eq!(count, made["frames"].as_u64().unwrap() / 3, "{made}");

    let report = sandbox.json(&["dev", "flash-decode", dir, "--secret", &secret]);
    assert_eq!((report["complete"].as_bool(), report["items"].as_u64(), report["color_pictures"].as_u64()), (Some(true), Some(300), Some(count)), "{report}");
    let read = sandbox.ok(&["dev", "flash-read", dir]);
    let first: Value = serde_json::from_str(read.lines().next().unwrap()).unwrap();
    assert_eq!((first["color"].as_bool(), first["codes"].as_array().unwrap().len()), (Some(true), 3), "{first}");

    // The daemon, as the page posts them: three bytes a pixel, format=rgb.
    let cookie = page_cookie(&sandbox, port);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&pictures).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let mut last = Value::Null;
    let mut sent = 0u64;
    for file in &files {
        let (width, height, pixels) = read_rgb_png(file);
        let (status, answer) = http_post(port, &format!("/api/flash/picture?w={width}&h={height}&format=rgb"), &cookie, &pixels);
        assert_eq!(status, 200, "{answer}");
        sent += 1;
        last = answer;
        if last["state"] == "done" {
            break;
        }
        assert_eq!((last["last"]["frames"].as_u64(), last["last"]["color"].as_bool()), (Some(3), Some(true)), "{last}");
    }
    assert_eq!(last["state"], "done", "{last}");
    assert_eq!((last["result"]["new"].as_u64(), last["result"]["paired"].as_bool()), (Some(300), Some(true)));
    assert!(sent * 3 < k + 10, "a third as many pictures: {sent} for k = {k}");
    assert_eq!(last["pictures"]["color"].as_u64(), Some(sent));
    assert_eq!(total_items(&sandbox), 300);
    let events = feed(&sandbox, &[]);
    let flash = events.iter().find(|e| e["event"] == "flash_received").unwrap();
    assert_eq!((flash["color"].as_bool(), flash["new"].as_u64(), flash["device"].as_str()), (Some(true), Some(300), Some("Color phone")), "{flash}");

    // The same flash drawn in a designed palette (each channel with its own dark and light
    // level) reads the same, by the command line and by the daemon, without either being told
    // the palette. Everything in it is already here.
    let palette = sandbox.home.join("palette");
    let dir = palette.to_str().unwrap();
    let levels = "60,255,0,200,90,255";
    let made = sandbox.json(&["dev", "flash-encode", "--color", "--levels", levels, "--out", dir, "--secret", &secret, "--count", "300", "--chunk", "700", "--scale", "3", "--name", "Color phone"]);
    assert_eq!(made["levels"], levels);
    let report = sandbox.json(&["dev", "flash-decode", dir, "--secret", &secret]);
    assert_eq!((report["complete"].as_bool(), report["items"].as_u64(), report["color_pictures"].as_u64()), (Some(true), Some(300), made["pictures"].as_u64()), "{report}");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&palette).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let (width, height, pixels) = read_rgb_png(&files[0]);
    assert_eq!(&pixels[..3], &[255, 200, 255], "the quiet zone is the all-light color");
    let mut last = Value::Null;
    for file in &files {
        let (width, height, pixels) = read_rgb_png(file);
        last = http_post(port, &format!("/api/flash/picture?w={width}&h={height}&format=rgb"), &cookie, &pixels).1;
        if last["state"] == "done" {
            break;
        }
        assert_eq!((last["last"]["frames"].as_u64(), last["last"]["color"].as_bool()), (Some(3), Some(true)), "{last}");
    }
    assert_eq!((last["state"].as_str(), last["result"]["unchanged"].as_u64(), last["result"]["paired"].as_bool()), (Some("done"), Some(300), Some(false)), "{last}");
    assert!(width == height && width > 100);
    // Levels that are not a palette are refused.
    assert!(!sandbox.run(&["dev", "flash-encode", "--color", "--levels", "0,255,200,100,0,255", "--out", dir, "--secret", &secret]).status.success());
    assert!(!sandbox.run(&["dev", "flash-encode", "--levels", levels, "--out", dir, "--secret", &secret]).status.success(), "--levels needs --color");
}

// ------------------------------------------------------------------------- backup import and queries

/// A synthetic backup in the phone's format. Dates are seconds since 2001-01-01 UTC.
fn write_backup(root: &Path) -> PathBuf {
    // 2026-11-11 20:00:00 UTC is 816120000 seconds after 2001-01-01.
    let noon = 816_120_000.0_f64;
    let mut dictations = Vec::new();
    for i in 0..40 {
        dictations.push(json!({
            "id": format!("D0000000-0000-4000-8000-{i:012X}"),
            "text": format!("synthetic dictation number {i} about the lighthouse"),
            "createdAt": noon + (i as f64) * 600.0 - 3000.0,
            "updatedAt": noon + (i as f64) * 600.0 - 2997.0,
            "audioSeconds": 3.5,
            "engine": "fluidMiniCPU",
            "source": if i % 5 == 0 { "app" } else { "keyboard" },
            "status": "inserted",
        }));
    }
    let body = json!({
        "formatVersion": 1,
        "createdAt": noon + 90000.0,
        "notes": [
            { "id": "A0000000-0000-4000-8000-000000000001", "text": "synthetic note about tide tables", "createdAt": noon + 5400.0 },
            { "id": "A0000000-0000-4000-8000-000000000002", "text": "an older synthetic note", "createdAt": noon - 400000.0, "editedAt": noon - 300000.0 }
        ],
        "dictations": dictations,
        "meetings": [
            { "id": "E0000000-0000-4000-8000-000000000001", "title": "Synthetic standup", "createdAt": noon + 7200.0, "updatedAt": noon + 9000.0,
              "status": "done", "engine": "fluidMiniCPU", "progress": 1, "transcribedThrough": 60.0,
              "parts": [ { "fileName": "part-1.pcm", "bytes": 1920000 } ],
              "speakers": [ { "id": "S1", "isMe": true }, { "id": "S2", "name": "Grace", "isMe": false } ],
              "segments": [
                  { "start": 0, "end": 5.0, "speaker": "S1", "text": "the lighthouse survey is done" },
                  { "start": 5.0, "end": 9.0, "speaker": "S2", "text": "then we ship on Friday" }
              ] }
        ]
    });
    let package = root.join("Zone Backups").join("Zone library.zonebackup");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("library.json"), serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    // An older copy nested inside the package, which must be ignored.
    let nested = package.join("Zone library.zonebackup");
    std::fs::create_dir_all(&nested).unwrap();
    let old = json!({ "formatVersion": 1, "createdAt": 1.0, "dictations": [], "meetings": [],
        "notes": [ { "id": "FFFFFFFF-0000-4000-8000-000000000001", "text": "stale nested note", "createdAt": 700000000.0 } ] });
    std::fs::write(nested.join("library.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    package
}

#[test]
fn import_then_query_from_the_command_line() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home);

    let out = sandbox.ok(&["import", package.to_str().unwrap()]);
    assert!(out.contains("2 notes, 40 dictations, 1 meetings"), "{out}");
    assert!(out.contains("43 new, 0 changed, 0 already in the library"), "{out}");
    // Importing the parent folder again finds the same single backup and changes nothing.
    let out = sandbox.ok(&["import", package.parent().unwrap().to_str().unwrap()]);
    assert!(out.contains("0 new, 0 changed, 43 already in the library"), "{out}");

    let all = sandbox.json(&["list", "--all", "--json"]);
    assert_eq!(all["total"], 43);
    assert!(all["items"].as_array().unwrap().iter().all(|i| i["id"] != "FFFFFFFF-0000-4000-8000-000000000001"));

    // Wednesday 2026-11-11 from 12:00 to 17:00 in Los Angeles is 20:00 to 01:00 UTC.
    // Dictations fall every ten minutes from 11:10 to 17:40 local: 12:00 through 16:50 is 30.
    let window = ["list", "--json", "--all", "--tz", "America/Los_Angeles", "--since", "2026-11-11 12:00", "--until", "2026-11-11 17:00"];
    let listed = sandbox.json(&window);
    let count = |kind: &str| listed["items"].as_array().unwrap().iter().filter(|i| i["kind"] == kind).count();
    assert_eq!((count("dictation"), count("note"), count("meeting")), (30, 1, 1), "{}", listed["total"]);
    // The same instants written with an explicit offset, read from another zone.
    let offset = ["list", "--json", "--all", "--tz", "Asia/Tokyo", "--since", "2026-11-11T20:00:00Z", "--until", "2026-11-12T01:00:00+00:00"];
    assert_eq!(sandbox.json(&offset)["total"], 32);
    // Times are shown in the zone asked for, with the weekday.
    let text = sandbox.ok(&["list", "--kind", "meeting", "--tz", "America/Los_Angeles"]);
    assert!(text.contains("Wed 2026-11-11 14:00  meeting  1m 00s"), "{text}");
    assert!(text.contains("Synthetic standup"));

    let found = sandbox.json(&["search", "lighthouse", "survey", "--json"]);
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["kind"], "meeting");
    assert!(found["items"][0]["text"].as_str().unwrap().starts_with("Me: the lighthouse survey is done\nGrace: then we ship on Friday"));
    assert_eq!(sandbox.json(&["search", "lighthouse", "--source", "app", "--json", "--limit", "100"])["total"], 8);

    let stats = sandbox.json(&["stats", "--by", "kind", "--json"]);
    assert_eq!(stats["total"]["items"], 43);
    assert_eq!(stats["groups"][0], json!({ "kind": "dictation", "items": 40, "words": 280, "recorded_s": 140.0 }));

    let exported = sandbox.ok(&["export", "--format", "jsonl", "--kind", "note"]);
    let lines: Vec<Value> = exported.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0]["created_at"].as_f64().unwrap() < lines[1]["created_at"].as_f64().unwrap(), "oldest first");
    let markdown = sandbox.ok(&["export", "--format", "md", "--kind", "meeting", "--tz", "America/Los_Angeles"]);
    assert!(markdown.contains("## Wednesday, November 11, 2026") && markdown.contains("### 14:00 meeting: Synthetic standup"));

    // A watched folder is remembered, and cleared.
    let parent = package.parent().unwrap();
    let out = sandbox.ok(&["backup-folder", parent.to_str().unwrap()]);
    assert!(out.contains("Watching") && out.contains("library.json"), "{out}");
    assert!(out.contains("0 new, 0 changed, 43 already in the library"), "{out}");
    assert!(sandbox.json(&["status", "--json"])["backup_folder"].as_str().unwrap().ends_with("Zone Backups"));
    sandbox.ok(&["backup-folder", "--clear"]);
    assert_eq!(sandbox.json(&["status", "--json"])["backup_folder"], Value::Null);
    let missing = sandbox.run(&["backup-folder", sandbox.home.join("nowhere").to_str().unwrap()]);
    assert!(!missing.status.success());
}

/// Adds one note to the synthetic backup, as the phone would when it writes a newer backup.
fn add_note_to_backup(package: &Path, id: &str) {
    let manifest = package.join("library.json");
    let mut body: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    body["notes"].as_array_mut().unwrap().push(json!({ "id": id, "text": "a note added later about the ferry", "createdAt": 816_130_000.0 }));
    std::fs::write(&manifest, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
}

fn wait_for_total(sandbox: &Sandbox, wanted: i64, seconds: u64, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while total_items(sandbox) != wanted {
        assert!(Instant::now() < deadline, "{what} (the library holds {} items, expected {wanted})", total_items(sandbox));
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `zone backup-folder` imports what is there itself, right away, and the daemon then
/// re-imports whenever the file changes.
#[test]
fn backup_folder_is_imported_at_once_and_then_watched() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home);
    let folder = package.parent().unwrap().to_str().unwrap().to_string();

    // No daemon is running: the command itself reads the folder, in the foreground.
    let out = sandbox.ok(&["backup-folder", &folder]);
    assert!(out.contains("2 notes, 40 dictations, 1 meetings") && out.contains("43 new, 0 changed"), "{out}");
    assert!(out.contains("The library now holds 43 items.") && out.contains("Watching "), "{out}");
    assert!(out.contains("Zone is not running"), "{out}");
    assert_eq!(total_items(&sandbox), 43, "imported before any daemon runs");
    let status = sandbox.json(&["status", "--json"]);
    assert!(status["last_sync"]["backup_folder"]["time"].is_string());
    assert_eq!(status["backup_folder_state"]["state"], "not_watching");

    // The daemon takes over. What was just imported is not imported again.
    let first_import = status["last_sync"]["backup_folder"]["time"].clone();
    let _daemon = Daemon::start_with_env(&sandbox, &[("ZONE_BACKUP_POLL_SECONDS", "1")]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = sandbox.json(&["status", "--json"]);
        if status["backup_folder_state"]["state"] == "watching" {
            assert_eq!(status["last_sync"]["backup_folder"]["time"], first_import, "the daemon imported the same file again");
            break;
        }
        assert!(Instant::now() < deadline, "the daemon never read the folder: {}", status["backup_folder_state"]);
        std::thread::sleep(Duration::from_millis(200));
    }
    let log = std::fs::read_to_string(sandbox.zone_home.join("zone.log")).unwrap();
    assert!(log.contains("backup folder: watching") && log.contains("backup folder: reading it now") && log.contains("backup folder: it can be read"), "{log}");
    assert!(!log.contains("new, "), "nothing was imported by the daemon yet:\n{log}");

    // The phone writes a newer backup: the daemon picks it up.
    add_note_to_backup(&package, "A0000000-0000-4000-8000-000000000003");
    wait_for_total(&sandbox, 44, 20, "the daemon did not import the changed backup");

    // With the daemon running, the command still imports by itself and says who watches.
    let other = sandbox.home.join("second");
    let package = write_backup(&other);
    add_note_to_backup(&package, "A0000000-0000-4000-8000-000000000004");
    let out = sandbox.ok(&["backup-folder", package.parent().unwrap().to_str().unwrap()]);
    assert!(out.contains("1 new, 0 changed, 43 already in the library") && out.contains("The running zone reads it again"), "{out}");
    assert_eq!(total_items(&sandbox), 45);

    // The feed has each read that brought something: the two by the command, the one by the daemon.
    let imports: Vec<(String, u64)> = feed(&sandbox, &[])
        .iter()
        .filter(|e| e["event"] == "backup_import")
        .map(|e| (e["trigger"].as_str().unwrap().to_string(), e["new"].as_u64().unwrap()))
        .collect();
    assert_eq!(imports, [("backup-folder".to_string(), 43), ("watch".to_string(), 1), ("backup-folder".to_string(), 1)]);
}

/// The bug from the real install: under launchd, macOS held the daemon's first read of an
/// iCloud Drive folder behind a permission prompt, forever, and zone said nothing. A debug
/// build can hold a read the same way. While it is held: the log has said what is going on,
/// `zone status` and `zone doctor` say it, and syncing and the web page do not wait.
#[cfg(debug_assertions)]
#[test]
fn a_held_backup_read_is_reported_and_blocks_nothing() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home);
    let folder = package.parent().unwrap().to_str().unwrap().to_string();
    let hold = sandbox.home.join("hold-the-read");
    let hold_text = hold.to_str().unwrap().to_string();
    std::fs::write(&hold, "").unwrap();

    // In the foreground a held read is explained, then completes when it is let go.
    let mut child = sandbox
        .zone()
        .env("ZONE_TEST_HOLD_BACKUP_READ", &hold_text)
        .args(["backup-folder", &folder])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for the explanation itself, however long a busy computer takes to get there.
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut err = String::new();
    while !err.contains("Still reading") {
        assert!(stderr.read_line(&mut err).unwrap() > 0, "the read was never explained: {err}");
    }
    assert_eq!(total_items(&sandbox), 0, "nothing is imported while the read is held");
    std::fs::remove_file(&hold).unwrap();
    stderr.read_to_string(&mut err).unwrap();
    let output = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(output.status.success(), "{out}\n{err}");
    assert!(out.contains("43 new") && out.contains("Watching "), "{out}");
    assert_eq!(total_items(&sandbox), 43);

    // Now the daemon's turn: its read is held.
    std::fs::write(&hold, "").unwrap();
    let _daemon = Daemon::start_with_env(&sandbox, &[("ZONE_TEST_HOLD_BACKUP_READ", &hold_text), ("ZONE_BACKUP_POLL_SECONDS", "1")]);
    let waiting = if cfg!(target_os = "macos") { "waiting for macOS permission to read the backup folder" } else { "waiting for the backup folder to answer" };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = sandbox.json(&["status", "--json"]);
        let state = &status["backup_folder_state"];
        if state["state"] == "waiting" {
            assert_eq!(state["message"], waiting);
            assert!(state["waiting_s"].as_f64().unwrap() >= 5.0);
            break;
        }
        assert!(Instant::now() < deadline, "status never said the read is waiting: {state}");
        std::thread::sleep(Duration::from_millis(250));
    }
    let text = sandbox.ok(&["status"]);
    assert!(text.contains(waiting), "{text}");
    let doctor = sandbox.run(&["doctor"]);
    let doctor = String::from_utf8_lossy(&doctor.stdout).to_string();
    let line = doctor.lines().find(|l| l.contains("Backup folder")).unwrap_or("");
    assert!(line.starts_with("[note]") && line.contains(waiting), "{doctor}");
    // Said before the read began, so it is in the log although the read never came back.
    let log = std::fs::read_to_string(sandbox.zone_home.join("zone.log")).unwrap();
    assert!(log.contains("backup folder: watching") && log.contains("backup folder: reading it now"), "{log}");
    if cfg!(target_os = "macos") {
        assert!(log.contains("macOS may ask for permission"), "{log}");
    }
    assert!(!log.contains("it can be read"), "{log}");

    // Nothing else waits for it: a phone pairs and syncs, the web page answers, queries run.
    let addr = loopback_addr(&sandbox);
    let url = pair_url(&sandbox);
    let started = Instant::now();
    let report = phone(&sandbox, &["--url", &url, "--count", "120", "--addr", &addr]);
    assert_eq!((report["result"].as_str(), report["stored"].as_u64()), (Some("welcome"), Some(120)), "{report}");
    assert_eq!(total_items(&sandbox), 163);
    let port = sandbox.daemon_info().unwrap()["http_port"].as_u64().unwrap() as u16;
    let (code, _) = http(port, &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"));
    assert_eq!(code, 401, "the web page answers (locked) while the read is held");
    assert!(started.elapsed() < Duration::from_secs(20), "syncing waited for the held read");
    // Still exactly one read in flight after all this time, and the daemon says since when.
    let info = sandbox.daemon_info().unwrap();
    assert!(info["backup"]["read_started_at"].is_f64(), "{info}");

    // The person answers the prompt. The read comes back, and watching works from then on.
    add_note_to_backup(&package, "A0000000-0000-4000-8000-000000000003");
    std::fs::remove_file(&hold).unwrap();
    wait_for_total(&sandbox, 164, 20, "the daemon did not import after the read was let go");
    let status = sandbox.json(&["status", "--json"]);
    assert_eq!(status["backup_folder_state"]["state"], "watching", "{status}");
    assert!(sandbox.daemon_info().unwrap()["backup"]["read_started_at"].is_null());
    let log = std::fs::read_to_string(sandbox.zone_home.join("zone.log")).unwrap();
    assert!(log.contains("backup folder: it can be read"), "{log}");
}

// ------------------------------------------------------------------------- the feed

/// What `zone events --json` prints: one JSON object per line.
fn feed(sandbox: &Sandbox, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["events", "--json"];
    all.extend_from_slice(args);
    let text = sandbox.ok(&all);
    text.lines().map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("not a JSON line ({e}): {line}"))).collect()
}

/// The feed without the relay coming and going, which depends on whether this computer is
/// online while the test runs.
fn feed_kinds(events: &[Value]) -> Vec<String> {
    events.iter().filter_map(|e| e["event"].as_str()).filter(|kind| !kind.starts_with("relay_")).map(str::to_string).collect()
}

/// Everything that happens to a library shows up in `zone events`, in order, as names and
/// counts: pairing, sessions, refusals, imports, the daemon starting and stopping. A pairing
/// code can be asked what became of it, and a follower gets new events as they happen.
#[test]
fn the_feed_tells_what_happened_and_never_the_words() {
    let sandbox = Sandbox::new();
    // Nothing yet, and no daemon needed to say so.
    assert!(sandbox.ok(&["events"]).contains("Nothing has happened yet."));
    assert!(feed(&sandbox, &[]).is_empty());

    let daemon = Daemon::start(&sandbox);
    let addr = loopback_addr(&sandbox);
    let identity = sandbox.home.join("phone.json");
    let identity = identity.to_str().unwrap();

    // A code nobody uses: it says so when asked.
    let unused = sandbox.json(&["pair", "--json"]);
    let unused_id = unused["id"].as_str().unwrap().to_string();
    assert_eq!(unused_id.len(), 16);
    assert_eq!((unused["state"].as_str(), unused["claimed"].as_bool()), (Some("waiting"), Some(false)), "{unused}");
    assert!(unused["qr_svg"].as_str().unwrap().starts_with("<svg") && unused["expires_local"].is_string());
    assert!(!unused["url"].as_str().unwrap().contains(&unused_id), "the id is not part of the secret");
    let check = sandbox.run(&["pair", "--check", &unused_id, "--json"]);
    assert_eq!(check.status.code(), Some(2), "not claimed");
    let check: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!((check["state"].as_str(), check["claimed"].as_bool(), &check["device"]), (Some("waiting"), Some(false), &Value::Null));
    let unknown = sandbox.run(&["pair", "--check", "0123456789abcdef", "--json"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(serde_json::from_slice::<Value>(&unknown.stdout).unwrap()["state"], "unknown");
    assert!(!sandbox.run(&["pair", "--check", "not an id"]).status.success());

    // `zone pair --json --wait` prints the code, then, once a phone has used it, who did.
    let mut waiting = sandbox.zone().args(["pair", "--json", "--wait"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut lines = BufReader::new(waiting.stdout.take().unwrap()).lines();
    let offer: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    let url = offer["url"].as_str().unwrap().to_string();
    let offer_id = offer["id"].as_str().unwrap().to_string();
    let report = phone(&sandbox, &["--url", &url, "--identity", identity, "--count", "40", "--addr", &addr]);
    assert_eq!(report["result"], "welcome", "{report}");
    let claimed: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!((claimed["id"].as_str(), claimed["state"].as_str(), claimed["claimed"].as_bool()), (Some(offer_id.as_str()), Some("claimed"), Some(true)), "{claimed}");
    assert_eq!(claimed["device"]["name"], "Fake iPhone");
    assert!(waiting.wait().unwrap().success());
    let check = sandbox.ok(&["pair", "--check", &offer_id]);
    assert!(check.contains("Fake iPhone paired with this code."), "{check}");

    // A second session: everything again, two new items and one deletion.
    let report = phone(&sandbox, &["--identity", identity, "--count", "42", "--delete", "FA4E0000-0000-4000-8000-000000000003", "--addr", &addr]);
    assert_eq!(report["stored"], 43, "{report}");
    // A phone that never paired.
    let stranger = phone(&sandbox, &["--url", &url, "--no-secret", "--count", "5", "--addr", &addr]);
    assert_eq!(stranger["reason"], "not_paired", "{stranger}");
    // A backup read by hand, a full resend asked for, and the phone unpaired.
    let package = write_backup(&sandbox.home);
    sandbox.ok(&["import", package.to_str().unwrap()]);
    sandbox.ok(&["resync", "Fake iPhone"]);
    sandbox.ok(&["unpair", "Fake iPhone"]);

    let events = feed(&sandbox, &[]);
    let kinds = feed_kinds(&events);
    assert_eq!(
        kinds,
        ["daemon_started", "pair_offered", "pair_offered", "paired", "link_session", "link_session", "link_refused", "backup_import", "resync_requested", "unpaired"],
        "{events:#?}"
    );
    let of = |kind: &str, nth: usize| -> Value { events.iter().filter(|e| e["event"] == kind).nth(nth).cloned().unwrap_or_else(|| panic!("no {kind} number {nth}")) };

    // Every line: a number that only goes up, the moment twice, and the kind, in that order.
    let mut last_seq = 0;
    for event in &events {
        let keys: Vec<&String> = event.as_object().unwrap().keys().collect();
        assert_eq!(&keys[..4], ["seq", "at", "time", "event"], "{event}");
        let seq = event["seq"].as_i64().unwrap();
        assert!(seq > last_seq, "{event}");
        last_seq = seq;
        let time = event["time"].as_str().unwrap();
        assert!(time.len() == 25 && time.as_bytes()[10] == b'T', "{time}");
        assert!((event["at"].as_f64().unwrap() - 1.79e9).abs() < 1e9);
    }

    let started = of("daemon_started", 0);
    assert_eq!(started["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(started["port"], sandbox.daemon_info().unwrap()["http_port"]);
    assert_eq!((of("pair_offered", 0)["offer"].as_str(), of("pair_offered", 1)["offer"].as_str()), (Some(unused_id.as_str()), Some(offer_id.as_str())));
    assert_eq!(of("pair_offered", 0)["via"], "cli");
    let paired = of("paired", 0);
    assert_eq!((paired["device"].as_str(), paired["via"].as_str(), paired["offer"].as_str()), (Some("Fake iPhone"), Some("link"), Some(offer_id.as_str())));
    let device_id = paired["device_id"].as_str().unwrap().to_string();
    assert_eq!(device_id.len(), 64);
    let first = of("link_session", 0);
    assert_eq!(
        (first["items"].as_u64(), first["new"].as_u64(), first["changed"].as_u64(), first["deleted"].as_u64(), first["unchanged"].as_u64()),
        (Some(40), Some(40), Some(0), Some(0), Some(0)),
        "{first}"
    );
    assert_eq!((first["device"].as_str(), first["device_id"].as_str(), first["paired"].as_bool()), (Some("Fake iPhone"), Some(device_id.as_str()), Some(true)));
    assert_eq!((first["path"].as_str(), first["end"].as_str(), first["frames"].as_u64(), first["rejected"].as_u64()), (Some("direct"), Some("bye"), Some(1), Some(0)));
    assert!(first["duration_s"].as_f64().unwrap() >= 0.0);
    let second = of("link_session", 1);
    assert_eq!(
        (second["items"].as_u64(), second["new"].as_u64(), second["changed"].as_u64(), second["deleted"].as_u64(), second["unchanged"].as_u64(), second["paired"].as_bool()),
        (Some(43), Some(2), Some(0), Some(1), Some(40), Some(false)),
        "{second}"
    );
    let refused = of("link_refused", 0);
    assert_eq!(refused["reason"], "not_paired");
    assert_ne!(refused["device_id"], device_id);
    let import = of("backup_import", 0);
    assert_eq!((import["trigger"].as_str(), import["items"].as_u64(), import["new"].as_u64()), (Some("import"), Some(43), Some(43)), "{import}");
    assert_eq!((import["notes"].as_u64(), import["dictations"].as_u64(), import["meetings"].as_u64()), (Some(2), Some(40), Some(1)));
    assert_eq!(of("resync_requested", 0)["device"], "Fake iPhone");
    assert_eq!((of("unpaired", 0)["device"].as_str(), of("unpaired", 0)["device_id"].as_str()), (Some("Fake iPhone"), Some(device_id.as_str())));

    // Names and counts, never the words: not as JSON and not as text.
    let as_json = sandbox.ok(&["events", "--json"]);
    let as_text = sandbox.ok(&["events"]);
    for word in ["amber", "basalt", "juniper", "Synthetic", "lighthouse", "synthetic dictation"] {
        assert!(!as_json.contains(word) && !as_text.contains(word), "the feed contains item text: {word}");
    }
    assert!(as_text.contains("Fake iPhone synced over Link: 40 new (direct, ") && as_text.contains("Fake iPhone synced over Link: 2 new, 1 deleted"), "{as_text}");
    assert!(as_text.contains("A phone was refused") || as_text.contains("Fake iPhone was refused"), "{as_text}");
    assert!(as_text.contains("Backup folder read: 43 new") && as_text.contains("Fake iPhone was unpaired"), "{as_text}");

    // Asking for part of it.
    let middle = events[4]["seq"].as_i64().unwrap();
    let after = feed(&sandbox, &["--after", &middle.to_string()]);
    assert_eq!(feed_kinds(&after), feed_kinds(&events[5..]));
    assert!(after.iter().all(|e| e["seq"].as_i64().unwrap() > middle));
    let newest = feed(&sandbox, &["--limit", "2"]);
    assert_eq!(feed_kinds(&newest).last().map(String::as_str), Some("unpaired"));
    assert_eq!(newest.len(), 2);
    assert_eq!(feed_kinds(&feed(&sandbox, &["--since", "1h"])), kinds);
    assert!(feed(&sandbox, &["--since", "2099-01-01"]).is_empty());
    assert!(feed(&sandbox, &["--limit", "0"]).is_empty());

    // A follower gets nothing old with --limit 0, and each new event as it happens.
    let mut follower =
        sandbox.zone().args(["events", "--json", "--follow", "--limit", "0"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut followed = BufReader::new(follower.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(600));
    sandbox.ok(&["import", package.to_str().unwrap()]);
    // (The relay may come or go meanwhile, when this computer is online: that is news too.)
    let event = loop {
        let mut line = String::new();
        assert!(followed.read_line(&mut line).unwrap() > 0, "the follower stopped");
        let event: Value = serde_json::from_str(&line).unwrap();
        if !event["event"].as_str().unwrap().starts_with("relay_") {
            break event;
        }
    };
    assert_eq!((event["event"].as_str(), event["new"].as_u64(), event["unchanged"].as_u64()), (Some("backup_import"), Some(0), Some(43)), "{event}");
    assert!(event["seq"].as_i64().unwrap() > last_seq);
    // When its reader goes away, the follower notices and ends by itself.
    drop(followed);
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        while follower.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the follower kept running after its reader went away");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = follower.kill();
    let _ = follower.wait();

    // Stopped the ordinary way, the daemon says goodbye in the feed.
    #[cfg(unix)]
    {
        let mut daemon = daemon;
        let pid = daemon.child.id().to_string();
        assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
        let _ = daemon.child.wait();
        let last = feed(&sandbox, &["--limit", "1"]);
        assert_eq!(last[0]["event"], "daemon_stopped", "{last:?}");
        assert!(last[0]["seq"].as_i64().unwrap() > event["seq"].as_i64().unwrap());
        assert!(last[0]["uptime_s"].as_f64().unwrap() >= 0.0);
        assert!(sandbox.ok(&["events", "--limit", "1"]).contains("Zone stopped after"));
    }
    #[cfg(not(unix))]
    drop(daemon);
}

// ------------------------------------------------------------------------- MCP over stdio

/// A `zone mcp` child process and the JSON-RPC conversation with it.
struct Mcp {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    /// What the server told the agent when it connected.
    instructions: String,
}

impl Mcp {
    fn start(sandbox: &Sandbox, extra: &[&str]) -> Mcp {
        let mut child = sandbox.zone().arg("mcp").args(extra).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp { child, stdin, stdout, next_id: 1, instructions: String::new() };
        let init = mcp.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "integration-test", "version": "1" } }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "zone");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        let instructions = init["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("typed by voice") && instructions.contains("voice note") && instructions.contains("meeting"));
        mcp.instructions = instructions.to_string();
        mcp.notify("notifications/initialized", json!({}));
        mcp
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "zone mcp closed its output");
            // Only protocol messages may appear on standard output.
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON-RPC on stdout ({e}): {line}"));
            assert_eq!(message["jsonrpc"], "2.0");
            if message["id"] == id {
                return message;
            }
        }
    }

    /// Calls a tool and returns its JSON answer.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let reply = self.request("tools/call", json!({ "name": name, "arguments": arguments }));
        assert!(reply["error"].is_null(), "{name} failed at the protocol level: {reply}");
        assert_ne!(reply["result"]["isError"], true, "{name} reported an error: {reply}");
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    fn tool_error(&mut self, name: &str, arguments: Value) -> String {
        let reply = self.request("tools/call", json!({ "name": name, "arguments": arguments }));
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        reply["result"]["content"][0]["text"].as_str().unwrap().to_string()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_round_trip_over_stdio() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home);
    sandbox.ok(&["import", package.to_str().unwrap()]);

    // No daemon is running: the server reads the library directly.
    let mut mcp = Mcp::start(&sandbox, &[]);

    let tools = mcp.request("tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["list_items", "search_items", "get_items", "library_stats", "sync_status", "mark_processed"]);
    for tool in &tools {
        let read_only = tool["name"] != "mark_processed";
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{}", tool["name"]);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["description"].as_str().unwrap().len() > 80);
    }
    let list_description = tools[0]["description"].as_str().unwrap();
    assert!(list_description.contains("typed by voice") && list_description.contains("voice notes") && list_description.contains("meeting transcripts"));

    // sync_status: the clock an agent needs to resolve "Wednesday", and the counts.
    let status = mcp.tool("sync_status", json!({}));
    assert!(status["now"]["weekday"].is_string() && status["now"]["timezone"].is_string() && status["now"]["utc_offset"].is_string());
    assert_eq!(status["items"]["total"], 43);
    assert_eq!(status["items"]["by_kind"], json!({ "dictation": 40, "note": 2, "meeting": 1 }));
    assert_eq!(status["daemon_running"], false);
    assert!(status["last_sync"]["backup_folder"]["time"].is_string());

    // list_items: Wednesday 12:00 to 17:00 in Los Angeles, with paging.
    let window = json!({ "start": "2026-11-11 12:00", "end": "2026-11-11 17:00", "timezone": "America/Los_Angeles", "order": "oldest", "limit": 20 });
    let first = mcp.tool("list_items", window.clone());
    assert_eq!(first["total"], 32);
    assert_eq!(first["total_by_kind"], json!({ "dictation": 30, "note": 1, "meeting": 1 }));
    assert_eq!(first["returned"], 20);
    assert_eq!(first["range"]["start"], "Wed 2026-11-11 12:00:00 -08:00");
    assert_eq!(first["items"][0]["time"], "Wed 2026-11-11 12:00:00 -08:00");
    assert!(first["items"][0]["preview"].as_str().unwrap().starts_with("synthetic dictation number"));
    let mut next = window.clone();
    next["cursor"] = first["next_cursor"].clone();
    let second = mcp.tool("list_items", next);
    assert_eq!((second["returned"].as_i64(), second["next_cursor"].clone()), (Some(12), Value::Null));
    let last_time = second["items"][11]["time"].as_str().unwrap();
    assert_eq!(last_time, "Wed 2026-11-11 16:50:00 -08:00", "the item at 17:00 is outside the window");

    // search_items with snippets, and a cut that is reported.
    let found = mcp.tool("search_items", json!({ "query": "lighthouse", "limit": 5 }));
    assert_eq!((found["total"].as_i64(), found["returned"].as_i64(), found["truncated"].as_bool()), (Some(41), Some(5), Some(true)));
    assert!(found["note"].as_str().unwrap().contains("5 best of 41"));
    assert!(found["items"][0]["snippet"].as_str().unwrap().contains("[lighthouse]"));
    let meeting = mcp.tool("search_items", json!({ "query": "\"ship on Friday\"", "kinds": ["meeting"] }));
    assert_eq!(meeting["total"], 1);
    let meeting_id = meeting["items"][0]["id"].as_str().unwrap().to_string();

    // get_items: full text, bounded.
    let got = mcp.tool("get_items", json!({ "ids": [meeting_id, "NOT-AN-ID"], "max_chars": 20 }));
    assert_eq!(got["items"][0]["title"], "Synthetic standup");
    assert_eq!(got["items"][0]["text"], "Me: the lighthouse s");
    assert_eq!((got["items"][0]["truncated"].as_bool(), got["items"][0]["next_offset"].as_i64()), (Some(true), Some(20)));
    assert_eq!(got["not_found"], json!(["NOT-AN-ID"]));
    let whole = mcp.tool("get_items", json!({ "ids": [meeting_id] }));
    assert_eq!(whole["items"][0]["text"], "Me: the lighthouse survey is done\nGrace: then we ship on Friday");
    assert_eq!(whole["items"][0]["truncated"], false);

    // library_stats by local day and by kind.
    let by_day = mcp.tool("library_stats", json!({ "start": "2026-11-11", "end": "2026-11-11", "timezone": "America/Los_Angeles" }));
    assert_eq!(by_day["groups"].as_array().unwrap().len(), 1);
    assert_eq!(by_day["groups"][0]["day"], "2026-11-11");
    assert_eq!(by_day["groups"][0]["weekday"], "Wed");
    assert_eq!(by_day["groups"][0]["items"], 42);
    let by_kind = mcp.tool("library_stats", json!({ "group_by": "kind" }));
    assert_eq!(by_kind["total"]["items"], 43);

    // mark_processed changes a flag and nothing else.
    let before = mcp.tool("get_items", json!({ "ids": [meeting_id] }));
    let marked = mcp.tool("mark_processed", json!({ "ids": [meeting_id] }));
    assert_eq!((marked["updated"].as_i64(), marked["unprocessed_remaining"].as_i64()), (Some(1), Some(42)));
    let after = mcp.tool("get_items", json!({ "ids": [meeting_id] }));
    assert_eq!(after["items"][0]["processed"], true);
    assert_eq!(after["items"][0]["text"], before["items"][0]["text"]);
    let unprocessed = mcp.tool("list_items", json!({ "unprocessed_only": true, "limit": 1 }));
    assert_eq!(unprocessed["total"], 42);
    mcp.tool("mark_processed", json!({ "ids": [meeting_id], "processed": false }));

    // Mistakes come back as tool errors an agent can read, not protocol failures.
    assert!(mcp.tool_error("list_items", json!({ "start": "next blue moon" })).contains("cannot read the time"));
    assert!(mcp.tool_error("list_items", json!({ "timezone": "Mars/Olympus" })).contains("unknown time zone"));
    let unknown = mcp.request("tools/call", json!({ "name": "delete_everything", "arguments": {} }));
    assert!(unknown["error"].is_object(), "{unknown}");

    // --read-only hides the one tool that writes, and refuses it if called anyway.
    drop(mcp);
    let mut read_only = Mcp::start(&sandbox, &["--read-only"]);
    let tools = read_only.request("tools/list", json!({}));
    let names: Vec<String> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["list_items", "search_items", "get_items", "library_stats", "sync_status"]);
    let refused = read_only.request("tools/call", json!({ "name": "mark_processed", "arguments": { "ids": [meeting_id] } }));
    assert!(refused["error"].is_object(), "{refused}");
    assert_eq!(read_only.tool("list_items", json!({ "unprocessed_only": true, "limit": 1 }))["total"], 43);
}

/// A server started with a narrower view, over the real protocol. The synthetic backup holds
/// 32 dictations typed by voice in other apps (source keyboard), 8 dictated in Zone (app), 2
/// notes and 1 meeting. What a view hides cannot be listed, searched, read, counted or
/// flagged, and the agent is told what it is not seeing.
#[test]
fn mcp_with_a_limited_view_over_stdio() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home);
    sandbox.ok(&["import", package.to_str().unwrap()]);
    let keyboard_id = "D0000000-0000-4000-8000-000000000001";
    let app_id = "D0000000-0000-4000-8000-000000000000";
    let note_id = "A0000000-0000-4000-8000-000000000001";

    // Without the private messages.
    let mut mcp = Mcp::start(&sandbox, &["--no-keyboard"]);
    assert!(mcp.instructions.contains("limited view") && mcp.instructions.contains("`keyboard` (what they typed by voice into other apps)"), "{}", mcp.instructions);
    let tools = mcp.request("tools/list", json!({}));
    let list_tool = &tools["result"]["tools"][0];
    assert!(list_tool["description"].as_str().unwrap().contains("limited view (hides keyboard)"), "{list_tool}");
    let status = mcp.tool("sync_status", json!({}));
    assert_eq!(status["items"]["total"], 11);
    assert_eq!(status["items"]["by_kind"], json!({ "dictation": 8, "note": 2, "meeting": 1 }));
    assert_eq!(status["items"]["dictations_by_source"], json!({ "app": 8 }));
    assert_eq!(status["scope"]["excluded_sources"], json!(["keyboard"]));
    let listed = mcp.tool("list_items", json!({ "limit": 200 }));
    assert_eq!((listed["total"].as_i64(), listed["returned"].as_i64()), (Some(11), Some(11)));
    assert!(listed["items"].as_array().unwrap().iter().all(|item| item["source"] != "keyboard"));
    assert!(mcp.tool_error("list_items", json!({ "sources": ["keyboard"] })).contains("limited view"));
    assert_eq!(mcp.tool("search_items", json!({ "query": "lighthouse", "limit": 100 }))["total"], 9, "8 dictated in Zone and the meeting");
    let got = mcp.tool("get_items", json!({ "ids": [keyboard_id, app_id] }));
    assert_eq!((got["items"].as_array().unwrap().len(), got["items"][0]["id"].as_str(), got["not_found"].clone()), (1, Some(app_id), json!([keyboard_id])));
    let stats = mcp.tool("library_stats", json!({ "group_by": "source" }));
    assert_eq!(stats["total"]["items"], 11);
    assert!(stats["groups"].as_array().unwrap().iter().all(|g| g["source"] != "keyboard"), "{stats}");
    let marked = mcp.tool("mark_processed", json!({ "ids": [keyboard_id, app_id] }));
    assert_eq!((marked["updated"].as_i64(), marked["not_found"].clone(), marked["unprocessed_remaining"].as_i64()), (Some(1), json!([keyboard_id]), Some(10)));
    drop(mcp);
    assert_eq!(sandbox.json(&["show", keyboard_id, "--json"])[0]["processed_at"], Value::Null, "the hidden item was not flagged");

    // Notes and meetings only.
    let mut mcp = Mcp::start(&sandbox, &["--kinds", "meeting,note"]);
    assert!(mcp.instructions.contains("only voice notes and meeting transcripts (dictations are hidden)"), "{}", mcp.instructions);
    let tools = mcp.request("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"][0]["inputSchema"]["properties"]["kinds"]["items"]["enum"], json!(["note", "meeting"]));
    let listed = mcp.tool("list_items", json!({}));
    assert_eq!((listed["total"].as_i64(), listed["total_by_kind"].clone()), (Some(3), json!({ "note": 2, "meeting": 1 })));
    assert_eq!(mcp.tool("search_items", json!({ "query": "lighthouse" }))["total"], 1, "only the meeting");
    assert!(mcp.tool_error("list_items", json!({ "kinds": ["dictation"] })).contains("does not show dictation items"));
    let got = mcp.tool("get_items", json!({ "ids": [app_id, note_id] }));
    assert_eq!((got["items"][0]["id"].as_str(), got["not_found"].clone()), (Some(note_id), json!([app_id])));
    assert_eq!(mcp.tool("sync_status", json!({}))["items"]["total"], 3);
    drop(mcp);

    // The short spelling, and both limits at once with read-only.
    let mut mcp = Mcp::start(&sandbox, &["--notes-only"]);
    assert_eq!(mcp.tool("list_items", json!({}))["total"], 2);
    drop(mcp);
    let mut mcp = Mcp::start(&sandbox, &["--read-only", "--kinds", "dictation", "--exclude-sources", "keyboard"]);
    assert_eq!(mcp.tool("list_items", json!({}))["total"], 8);
    assert_eq!(mcp.request("tools/list", json!({}))["result"]["tools"].as_array().unwrap().len(), 5);
    drop(mcp);

    // A mistyped limit is refused, never read as no limit.
    let typo = sandbox.zone().args(["mcp", "--kinds", "notse"]).stdin(Stdio::null()).output().unwrap();
    assert!(!typo.status.success() && typo.stdout.is_empty());
    assert!(String::from_utf8_lossy(&typo.stderr).contains("notse"));
}

// ------------------------------------------------------------------------- install and client registration

#[test]
fn install_writes_the_login_item_without_loading_it() {
    let sandbox = Sandbox::new();
    let out = sandbox.ok(&["install", "--no-start"]);
    assert!(out.contains("Not started"), "{out}");

    #[cfg(target_os = "macos")]
    let path = sandbox.home.join("Library/LaunchAgents/wtf.zayd.zone.link.plist");
    #[cfg(all(unix, not(target_os = "macos")))]
    let path = sandbox.home.join(".config/systemd/user/zone.service");
    #[cfg(windows)]
    let path = sandbox.zone_home.join("zone-serve.vbs");

    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{} was not written", path.display()));
    let exe = zone_exe();
    assert!(text.contains(&*exe.to_string_lossy()), "the login item starts this program");
    assert!(text.contains("serve"));
    // ZONE_HOME was overridden, so the login item carries it.
    assert!(text.contains("ZONE_HOME"));
    assert!(text.contains(&*sandbox.zone_home.to_string_lossy()));

    #[cfg(target_os = "macos")]
    {
        assert!(text.contains("<key>RunAtLoad</key>"));
        let lint = Command::new("plutil").arg("-lint").arg(&path).output().unwrap();
        assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
    }

    // Nothing was started.
    assert_eq!(sandbox.json(&["status", "--json"])["daemon"]["running"], false);

    // Removing it again, without touching anything that is running.
    let out = sandbox.ok(&["uninstall", "--no-stop"]);
    assert!(out.contains("no longer starts at login"), "{out}");
    assert!(!path.exists());
}

#[test]
fn mcp_install_edits_only_the_sandbox_configs() {
    let sandbox = Sandbox::new();
    let exe = zone_exe();
    let exe = exe.to_string_lossy();

    // Cursor: a JSON file with other servers already in it.
    let cursor = sandbox.home.join(".cursor").join("mcp.json");
    std::fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    std::fs::write(&cursor, r#"{ "mcpServers": { "other": { "command": "other-server" } } }"#).unwrap();
    sandbox.ok(&["mcp", "install", "cursor"]);
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["other"]["command"], "other-server");
    assert_eq!(config["mcpServers"]["zone"]["command"], *exe);
    assert_eq!(config["mcpServers"]["zone"]["args"], json!(["mcp"]));
    assert_eq!(config["mcpServers"]["zone"]["env"]["ZONE_HOME"], *sandbox.zone_home.to_string_lossy());

    // Claude Desktop: no config yet.
    sandbox.ok(&["mcp", "install", "claude-desktop", "--read-only"]);
    #[cfg(target_os = "macos")]
    let desktop = sandbox.home.join("Library/Application Support/Claude/claude_desktop_config.json");
    #[cfg(all(unix, not(target_os = "macos")))]
    let desktop = sandbox.home.join(".config/Claude/claude_desktop_config.json");
    #[cfg(windows)]
    let desktop = sandbox.home.join("AppData").join("Roaming").join("Claude").join("claude_desktop_config.json");
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&desktop).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["zone"]["args"], json!(["mcp", "--read-only"]));

    // Codex: a TOML file. Two clients in one command.
    sandbox.ok(&["mcp", "install", "codex", "gemini"]);
    let codex = std::fs::read_to_string(sandbox.home.join(".codex").join("config.toml")).unwrap();
    assert!(codex.contains("[mcp_servers.zone]") && codex.contains("args = [\"mcp\"]"), "{codex}");
    assert!(sandbox.home.join(".gemini").join("settings.json").exists());

    let status = sandbox.json(&["mcp", "status", "--json"]);
    let state = |client: &str| -> String {
        status["clients"].as_array().unwrap().iter().find(|c| c["client"] == client).unwrap_or_else(|| panic!("{client} in {status}"))["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(state("cursor"), "registered");
    assert_eq!(state("claude-desktop"), "registered_read_only");
    assert_eq!(state("codex"), "registered");
    assert_eq!(state("claude-code"), "not_registered");
    let text = sandbox.ok(&["mcp", "status"]);
    assert!(text.contains("registered, read-only") && text.contains("Not found on this computer:"), "{text}");

    sandbox.ok(&["mcp", "uninstall", "cursor"]);
    sandbox.ok(&["mcp", "uninstall", "codex", "claude-desktop"]);
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(config, json!({ "mcpServers": { "other": { "command": "other-server" } } }));
    assert!(!std::fs::read_to_string(sandbox.home.join(".codex").join("config.toml")).unwrap().contains("zone"));
    assert_eq!(sandbox.json(&["mcp", "status", "--json"])["clients"].as_array().unwrap().iter().find(|c| c["client"] == "cursor").unwrap()["state"], "removed_by_you");

    let unknown = sandbox.run(&["mcp", "install", "emacs"]);
    assert!(!unknown.status.success());
    let nothing = sandbox.run(&["mcp", "install"]);
    assert!(!nothing.status.success());
    assert!(String::from_utf8_lossy(&nothing.stderr).contains("--all"));
}

/// One client can be registered with a narrower view than another. `--all` leaves such an
/// entry alone unless a view is named, and then every client gets exactly that view.
#[test]
fn mcp_install_gives_each_client_its_own_view() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.home.join(".cursor")).unwrap();
    std::fs::create_dir_all(sandbox.home.join(".gemini")).unwrap();
    let cursor = sandbox.home.join(".cursor").join("mcp.json");
    let gemini = sandbox.home.join(".gemini").join("settings.json");
    let args_of = |path: &Path| -> Value { serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap()["mcpServers"]["zone"]["args"].clone() };

    let out = sandbox.ok(&["mcp", "install", "cursor", "--notes-only"]);
    assert!(out.contains("Limited view for agents there: only voice notes."), "{out}");
    assert_eq!(args_of(&cursor), json!(["mcp", "--kinds", "note"]));
    sandbox.ok(&["mcp", "install", "gemini"]);
    assert_eq!(args_of(&gemini), json!(["mcp"]));

    let status = sandbox.json(&["mcp", "status", "--json"]);
    let client = |status: &Value, name: &str| -> Value { status["clients"].as_array().unwrap().iter().find(|c| c["client"] == name).unwrap().clone() };
    assert_eq!(client(&status, "cursor")["scope"], json!({ "limited": true, "kinds": ["note"], "exclude_sources": [] }));
    assert_eq!((client(&status, "cursor")["state"].as_str(), client(&status, "cursor")["read_only"].as_bool()), (Some("registered"), Some(false)));
    assert_eq!(client(&status, "gemini")["scope"], json!({ "limited": false, "kinds": ["dictation", "note", "meeting"], "exclude_sources": [] }));
    assert_eq!((&client(&status, "zed")["scope"], &client(&status, "zed")["read_only"]), (&Value::Null, &Value::Null), "not registered: nothing to say");
    let text = sandbox.ok(&["mcp", "status"]);
    assert!(line_for(&text, "Cursor").contains("registered, only voice notes"), "{text}");
    assert!(!line_for(&text, "Gemini CLI").contains("only"), "{text}");

    // --all on its own leaves the narrower entry as it is.
    sandbox.ok(&["mcp", "install", "--all"]);
    assert_eq!(args_of(&cursor), json!(["mcp", "--kinds", "note"]));
    // --all with a view: every client gets exactly it.
    let out = sandbox.ok(&["mcp", "install", "--all", "--no-keyboard"]);
    assert!(line_for(&out, "Cursor").contains("changed the entry") && line_for(&out, "Gemini CLI").contains("changed the entry"), "{out}");
    assert!(out.contains("Limited view for agents there: hides keyboard."), "{out}");
    assert_eq!(args_of(&cursor), json!(["mcp", "--exclude-sources", "keyboard"]));
    assert_eq!(args_of(&gemini), json!(["mcp", "--exclude-sources", "keyboard"]));
    // And back to everything.
    sandbox.ok(&["mcp", "install", "--all", "--kinds", "all"]);
    assert_eq!((args_of(&cursor), args_of(&gemini)), (json!(["mcp"]), json!(["mcp"])));

    // The flags may come before `install`, and combine with --read-only.
    sandbox.ok(&["mcp", "--read-only", "--no-keyboard", "install", "cursor"]);
    assert_eq!(args_of(&cursor), json!(["mcp", "--read-only", "--exclude-sources", "keyboard"]));
    let status = sandbox.json(&["mcp", "status", "--json"]);
    assert_eq!((client(&status, "cursor")["state"].as_str(), client(&status, "cursor")["read_only"].as_bool()), (Some("registered_read_only"), Some(true)));
    assert_eq!(client(&status, "cursor")["scope"]["exclude_sources"], json!(["keyboard"]));

    // The plain install line, run again, keeps what the client has: it never shows an agent more.
    let out = sandbox.ok(&["mcp", "install", "cursor"]);
    assert!(line_for(&out, "Cursor").contains("already registered") && line_for(&out, "Cursor").contains("(read-only, hides keyboard)"), "{out}");
    assert!(out.contains("keeps that") && out.contains("--kinds all") && out.contains("--read-write"), "{out}");
    assert_eq!(args_of(&cursor), json!(["mcp", "--read-only", "--exclude-sources", "keyboard"]));
    // Each part changes only when it is named.
    sandbox.ok(&["mcp", "install", "cursor", "--read-write"]);
    assert_eq!(args_of(&cursor), json!(["mcp", "--exclude-sources", "keyboard"]));
    sandbox.ok(&["mcp", "install", "cursor", "--read-only", "--notes-only"]);
    assert_eq!(args_of(&cursor), json!(["mcp", "--read-only", "--kinds", "note"]));
    sandbox.ok(&["mcp", "install", "cursor", "--kinds", "all"]);
    assert_eq!(args_of(&cursor), json!(["mcp", "--read-only"]));
    // --all with a view leaves read-only as it is, too.
    sandbox.ok(&["mcp", "install", "--all", "--no-keyboard"]);
    assert_eq!((args_of(&cursor), args_of(&gemini)), (json!(["mcp", "--read-only", "--exclude-sources", "keyboard"]), json!(["mcp", "--exclude-sources", "keyboard"])));
    // An entry edited by hand into something zone would not start with is not guessed at.
    std::fs::write(&gemini, r#"{ "mcpServers": { "zone": { "command": "ZONE", "args": ["mcp", "--kinds", "songs"] } } }"#.replace("ZONE", &zone_exe().to_string_lossy().replace('\\', "\\\\"))).unwrap();
    let odd = sandbox.run(&["mcp", "install", "gemini"]);
    assert!(!odd.status.success() && String::from_utf8_lossy(&odd.stdout).contains("zone will not guess"), "{}", String::from_utf8_lossy(&odd.stdout));
    sandbox.ok(&["mcp", "install", "gemini", "--kinds", "all"]);
    assert_eq!(args_of(&gemini), json!(["mcp"]));

    // A mistyped view changes nothing.
    let typo = sandbox.run(&["mcp", "install", "cursor", "--kinds", "notse"]);
    assert!(!typo.status.success() && String::from_utf8_lossy(&typo.stderr).contains("notse"));
    assert_eq!(args_of(&cursor), json!(["mcp", "--read-only", "--exclude-sources", "keyboard"]));

    // What was registered really starts a server with that view.
    let package = write_backup(&sandbox.home);
    sandbox.ok(&["import", package.to_str().unwrap()]);
    let registered: Vec<String> = args_of(&cursor).as_array().unwrap().iter().skip(1).map(|a| a.as_str().unwrap().to_string()).collect();
    let registered: Vec<&str> = registered.iter().map(String::as_str).collect();
    let mut mcp = Mcp::start(&sandbox, &registered);
    assert_eq!(mcp.tool("list_items", json!({}))["total"], 11);
}

/// The places a sandbox has clients in, the way they look on a real computer: a Claude Code
/// profile besides the default, Zed's settings with comments, a commented Hermes config.
fn sandbox_with_clients(sandbox: &Sandbox) -> (PathBuf, PathBuf, String, PathBuf, String) {
    let home = &sandbox.home;
    std::fs::write(home.join(".claude.json"), "{\n  \"numStartups\": 2\n}\n").unwrap();
    std::fs::create_dir_all(home.join(".claude_work")).unwrap();
    std::fs::write(home.join(".claude_work").join(".claude.json"), "{}").unwrap();
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    let zed = home.join(".config").join("zed").join("settings.json");
    std::fs::create_dir_all(zed.parent().unwrap()).unwrap();
    let zed_text = "// Zed settings\n{\n  \"theme\": \"One Dark\", // dark\n  \"context_servers\": {},\n}\n".to_string();
    std::fs::write(&zed, &zed_text).unwrap();
    let hermes = home.join(".hermes").join("config.yaml");
    std::fs::create_dir_all(hermes.parent().unwrap()).unwrap();
    let hermes_text = "model:\n  default: some-model # the usual\nmcp_servers:\n  files:\n    command: npx\n    args:\n      - files\n\n# voice\nvoice:\n  enabled: false\n".to_string();
    std::fs::write(&hermes, &hermes_text).unwrap();
    (home.join(".claude.json"), zed, zed_text, hermes, hermes_text)
}

fn line_for<'a>(output: &'a str, label: &str) -> &'a str {
    output.lines().find(|l| l.trim_start().starts_with(label)).unwrap_or_else(|| panic!("no line for {label} in:\n{output}"))
}

#[test]
fn mcp_install_all_registers_every_client_found_and_respects_removals() {
    let sandbox = Sandbox::new();
    let (claude, zed, zed_text, hermes, hermes_text) = sandbox_with_clients(&sandbox);
    let exe = zone_exe();
    let exe = exe.to_string_lossy().to_string();

    let out = sandbox.ok(&["mcp", "install", "--all"]);
    for label in ["Claude Code (default)", "Claude Code (~", "Gemini CLI", "Zed", "Hermes"] {
        assert!(line_for(&out, label).contains("  registered"), "{label}:\n{out}");
    }
    assert!(line_for(&out, "Goose").contains("skipped: not installed"), "{out}");
    assert!(line_for(&out, "Cursor").contains("skipped: not installed"), "{out}");

    // With no `claude` command the profiles' files are edited the way `claude mcp add` would.
    for file in [claude.clone(), sandbox.home.join(".claude_work").join(".claude.json")] {
        let config: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["zone"]["type"], "stdio");
        assert_eq!(config["mcpServers"]["zone"]["command"], exe.as_str());
    }
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
    assert_eq!(config["numStartups"], 2);
    let zed_now = std::fs::read_to_string(&zed).unwrap();
    assert!(zed_now.starts_with("// Zed settings\n{\n  \"theme\": \"One Dark\", // dark\n  \"context_servers\": {\n    \"zone\": {\n"), "{zed_now}");
    let hermes_now = std::fs::read_to_string(&hermes).unwrap();
    assert!(hermes_now.contains("      - files\n  zone:\n    command: "), "{hermes_now}");
    assert!(hermes_now.ends_with("\n# voice\nvoice:\n  enabled: false\n"), "{hermes_now}");

    // Again: nothing changes. `detected` means the same as --all.
    let out = sandbox.ok(&["mcp", "install", "detected"]);
    assert!(line_for(&out, "Zed").contains("already registered"), "{out}");
    assert_eq!(std::fs::read_to_string(&zed).unwrap(), zed_now);

    // Taken out of Zed on purpose: --all leaves Zed alone from then on.
    let out = sandbox.ok(&["mcp", "uninstall", "zed"]);
    assert!(line_for(&out, "Zed").contains("removed"), "{out}");
    assert_eq!(std::fs::read_to_string(&zed).unwrap(), zed_text);
    let out = sandbox.ok(&["mcp", "install", "--all"]);
    assert!(line_for(&out, "Zed").contains("skipped: you removed zone here"), "{out}");
    assert_eq!(std::fs::read_to_string(&zed).unwrap(), zed_text);
    // Naming it puts it back.
    sandbox.ok(&["mcp", "install", "zed"]);
    assert!(std::fs::read_to_string(&zed).unwrap().contains("\"zone\""));

    // --all takes zone out of every one, leaving each file as it was.
    let out = sandbox.ok(&["mcp", "uninstall", "--all"]);
    assert!(line_for(&out, "Hermes").contains("removed"), "{out}");
    assert_eq!(std::fs::read_to_string(&zed).unwrap(), zed_text);
    assert_eq!(std::fs::read_to_string(&hermes).unwrap(), hermes_text);
    let status = sandbox.json(&["mcp", "status", "--json"]);
    assert!(status["clients"].as_array().unwrap().iter().all(|c| !c["state"].as_str().unwrap().starts_with("registered")), "{status}");
    assert_eq!(status["automatic"], false);
    // The originals were kept beside the files.
    assert_eq!(std::fs::read_to_string(format!("{}.before-zone", zed.display())).unwrap(), zed_text);
}

/// Claude Code is registered through its own command when there is one, once per profile. A
/// stand-in `claude` records what it was asked (and writes the entry, as the real one does),
/// so the real one is never run.
#[cfg(unix)]
#[test]
fn mcp_install_claude_code_calls_claude_mcp_add_for_each_profile() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let bin = sandbox.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(sandbox.home.join(".claude_work")).unwrap();
    std::fs::write(sandbox.home.join(".claude_work").join(".claude.json"), "{}").unwrap();
    let record = sandbox.home.join("claude-calls.txt");
    let script = format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"${{CLAUDE_CONFIG_DIR:-unset}}\" \"$*\" >> '{}'\n\
         file=\"${{CLAUDE_CONFIG_DIR:-$HOME}}/.claude.json\"\n\
         if [ \"$2\" = add ]; then while [ \"$1\" != -- ]; do shift; done; shift; printf '{{\"mcpServers\":{{\"zone\":{{\"type\":\"stdio\",\"command\":\"%s\",\"args\":[\"%s\"]}}}}}}' \"$1\" \"$2\" > \"$file\"; fi\n",
        record.display()
    );
    let fake = bin.join("claude");
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = sandbox.zone().args(["mcp", "install", "claude-code"]).output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let calls = std::fs::read_to_string(&record).unwrap();
    let exe = zone_exe();
    let add = format!("mcp add --scope user zone --env ZONE_HOME={} -- {} mcp", sandbox.zone_home.display(), exe.display());
    let expected = format!("unset|{add}\n{}|{add}\n", sandbox.home.join(".claude_work").display());
    assert_eq!(calls, expected);
    let status = sandbox.ok(&["mcp", "status"]);
    assert!(line_for(&status, "Claude Code (default)").contains("registered"), "{status}");
    assert!(line_for(&status, "Claude Code (~").contains("registered"), "{status}");
}

#[test]
fn install_registers_with_clients_and_doctor_names_the_missing_ones() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.home.join(".cursor")).unwrap();
    let out = sandbox.ok(&["install", "--no-start"]);
    assert!(out.contains("Registering zone with the AI clients"), "{out}");
    assert!(line_for(&out, "Cursor").contains("registered"), "{out}");
    let state: Value = serde_json::from_slice(&std::fs::read(sandbox.zone_home.join("mcp-clients.json")).unwrap()).unwrap();
    assert_eq!(state["auto"], true);

    // A client installed later: doctor names it and gives the command.
    std::fs::create_dir_all(sandbox.home.join(".gemini")).unwrap();
    let doctor = String::from_utf8(sandbox.run(&["doctor"]).stdout).unwrap();
    let line = line_for(&doctor, "[note] AI clients");
    assert!(line.contains("installed but without zone: Gemini CLI"), "{doctor}");
    assert!(doctor.contains("zone mcp install --all"), "{doctor}");
    // No daemon runs in the sandbox, so doctor exits 1; its report is still JSON.
    let report: Value = serde_json::from_slice(&sandbox.run(&["doctor", "--json"]).stdout).unwrap();
    let check = report["checks"].as_array().unwrap().iter().find(|c| c["check"] == "AI clients").unwrap().clone();
    assert_eq!(check["result"], "warn");

    // --no-mcp: nothing is registered and the daemon will not either.
    let other = Sandbox::new();
    std::fs::create_dir_all(other.home.join(".cursor")).unwrap();
    let out = other.ok(&["install", "--no-start", "--no-mcp"]);
    assert!(out.contains("--no-mcp"), "{out}");
    assert!(!other.home.join(".cursor").join("mcp.json").exists());
    sandbox.ok(&["uninstall", "--no-stop"]);
    let state: Value = serde_json::from_slice(&std::fs::read(sandbox.zone_home.join("mcp-clients.json")).unwrap()).unwrap();
    assert_eq!(state["auto"], false);
}

#[test]
fn serve_registers_with_new_clients_but_never_where_zone_was_taken_out() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.home.join(".cursor")).unwrap();
    std::fs::create_dir_all(sandbox.home.join(".gemini")).unwrap();
    sandbox.ok(&["install", "--no-start"]);
    // Cursor's entry removed by hand, Gemini's with the command, and Qwen installed since.
    let cursor = sandbox.home.join(".cursor").join("mcp.json");
    std::fs::write(&cursor, "{\"mcpServers\": {}}").unwrap();
    sandbox.ok(&["mcp", "uninstall", "gemini"]);
    std::fs::create_dir_all(sandbox.home.join(".qwen")).unwrap();

    let _daemon = Daemon::start(&sandbox);
    let qwen = sandbox.home.join(".qwen").join("settings.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !qwen.exists() {
        assert!(Instant::now() < deadline, "zone serve did not register with the new client");
        std::thread::sleep(Duration::from_millis(100));
    }
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&qwen).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["zone"]["args"], json!(["mcp"]));
    // Give it time to finish, then check it left the others alone.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(std::fs::read_to_string(&cursor).unwrap(), "{\"mcpServers\": {}}");
    let gemini: Value = serde_json::from_str(&std::fs::read_to_string(sandbox.home.join(".gemini").join("settings.json")).unwrap()).unwrap();
    assert!(gemini["mcpServers"].get("zone").is_none());
    let log = std::fs::read_to_string(sandbox.zone_home.join("zone.log")).unwrap();
    assert!(log.contains("mcp: registered with Qwen Code"), "{log}");
    assert!(log.contains("taken out of Cursor by hand"), "{log}");
}

/// `zone mcp status | head -4` used to panic with "failed printing to stdout: Broken pipe".
/// Every command given a standard output nobody reads must finish quietly.
#[test]
fn a_closed_pipe_is_not_a_crash() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home.join("Zone Backups"));
    sandbox.ok(&["import", package.to_str().unwrap()]);
    let commands: &[&[&str]] = &[
        &["mcp", "status"],
        &["status"],
        &["status", "--json"],
        &["doctor"],
        &["devices"],
        &["list"],
        &["list", "--json"],
        &["search", "synthetic"],
        &["stats"],
        &["export"],
        &["backup-folder"],
        &["events"],
        &["events", "--json"],
        &["import", package.to_str().unwrap()],
        &["dev", "synthetic", "--count", "2000"],
        &["--help"],
    ];
    for args in commands {
        // A pipe whose reading end is already closed: the first write fails.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = sandbox.zone().args(*args).stdin(Stdio::null()).stdout(writer).stderr(Stdio::piped()).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("panicked") && !stderr.contains("Broken pipe"), "zone {args:?} with a closed pipe:\n{stderr}");
        // `doctor` exits 1 because no daemon runs in this sandbox. Nothing may exit by panic (101) or signal.
        let code = output.status.code();
        assert!(matches!(code, Some(0) | Some(1)), "zone {args:?} with a closed pipe exited with {code:?}:\n{stderr}");
        if args[0] != "doctor" {
            assert_eq!(code, Some(0), "zone {args:?} with a closed pipe:\n{stderr}");
        }
    }
}
