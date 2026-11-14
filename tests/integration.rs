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
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("ZONE_PORT")
            .env_remove("ZONE_RELAY_URL");
        command
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
        let child = sandbox
            .zone()
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
    assert!(sandbox.json(&["status", "--json"])["backup_folder"].as_str().unwrap().ends_with("Zone Backups"));
    sandbox.ok(&["backup-folder", "--clear"]);
    assert_eq!(sandbox.json(&["status", "--json"])["backup_folder"], Value::Null);
    let missing = sandbox.run(&["backup-folder", sandbox.home.join("nowhere").to_str().unwrap()]);
    assert!(!missing.status.success());
}

/// The daemon notices a backup folder and re-imports it when the file changes.
#[test]
fn daemon_imports_the_watched_backup_folder() {
    let sandbox = Sandbox::new();
    let package = write_backup(&sandbox.home);
    sandbox.ok(&["backup-folder", package.parent().unwrap().to_str().unwrap()]);
    assert_eq!(total_items(&sandbox), 0, "nothing is read until the daemon runs");
    let _daemon = Daemon::start(&sandbox);
    let deadline = Instant::now() + Duration::from_secs(20);
    while total_items(&sandbox) != 43 {
        assert!(Instant::now() < deadline, "the daemon did not import the backup folder");
        std::thread::sleep(Duration::from_millis(200));
    }
    let status = sandbox.json(&["status", "--json"]);
    assert!(status["last_sync"]["backup_folder"]["time"].is_string());
}

// ------------------------------------------------------------------------- MCP over stdio

/// A `zone mcp` child process and the JSON-RPC conversation with it.
struct Mcp {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(sandbox: &Sandbox, extra: &[&str]) -> Mcp {
        let mut child = sandbox.zone().arg("mcp").args(extra).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp { child, stdin, stdout, next_id: 1 };
        let init = mcp.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "integration-test", "version": "1" } }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "zone");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        let instructions = init["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("typed by voice") && instructions.contains("voice note") && instructions.contains("meeting"));
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
    let exe = std::fs::canonicalize(ZONE).unwrap();
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
    let exe = std::fs::canonicalize(ZONE).unwrap();
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

    // Codex: a TOML file.
    sandbox.ok(&["mcp", "install", "codex"]);
    let codex = std::fs::read_to_string(sandbox.home.join(".codex").join("config.toml")).unwrap();
    assert!(codex.contains("[mcp_servers.zone]") && codex.contains("args = [\"mcp\"]"), "{codex}");

    let status = sandbox.ok(&["mcp", "status"]);
    assert!(status.contains("Cursor          registered"), "{status}");
    assert!(status.contains("Claude Desktop  registered (read-only)"), "{status}");
    assert!(status.contains("Codex           registered"), "{status}");
    assert!(status.contains("Claude Code     not registered"), "{status}");

    sandbox.ok(&["mcp", "uninstall", "cursor"]);
    sandbox.ok(&["mcp", "uninstall", "codex"]);
    sandbox.ok(&["mcp", "uninstall", "claude-desktop"]);
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(config, json!({ "mcpServers": { "other": { "command": "other-server" } } }));
    assert!(!std::fs::read_to_string(sandbox.home.join(".codex").join("config.toml")).unwrap().contains("zone"));

    let unknown = sandbox.run(&["mcp", "install", "emacs"]);
    assert!(!unknown.status.success());
}

/// Claude Code is registered through its own command. A stand-in `claude` records what it
/// was asked, so the real one is never run.
#[cfg(unix)]
#[test]
fn mcp_install_claude_code_calls_claude_mcp_add() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let bin = sandbox.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let record = sandbox.home.join("claude-calls.txt");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n", record.display());
    let fake = bin.join("claude");
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Only the stand-in is on PATH.
    let output = sandbox.zone().env("PATH", &bin).args(["mcp", "install", "claude-code"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let calls = std::fs::read_to_string(&record).unwrap();
    let exe = std::fs::canonicalize(ZONE).unwrap();
    let expected = format!("mcp add --scope user zone --env ZONE_HOME={} -- {} mcp", sandbox.zone_home.display(), exe.display());
    assert!(calls.lines().any(|line| line == expected), "expected:\n{expected}\ngot:\n{calls}");

    // With no `claude` at all, it explains the command to run by hand.
    let empty = sandbox.home.join("empty-bin");
    std::fs::create_dir_all(&empty).unwrap();
    let output = sandbox.zone().env("PATH", &empty).args(["mcp", "install", "claude-code"]).output().unwrap();
    assert!(!output.status.success());
    let explained = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(explained.contains("claude mcp add --scope user zone"), "{explained}");
}
