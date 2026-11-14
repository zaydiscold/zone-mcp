//! Carrier 3: reading a Zone backup folder (PROTOCOL.md).
//!
//! A backup is a folder named `<something>.zonebackup` holding `library.json`. Only that one
//! file is read. Audio beside it is ignored, and so is anything nested deeper, including an
//! older backup package saved inside this one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::db::{Applied, Db, Delivery};
use crate::item::{Carrier, Item};

/// Seconds between 1970-01-01 and 2001-01-01 UTC. Dates in `library.json` count from 2001.
pub const APPLE_EPOCH: f64 = 978_307_200.0;

const MANIFEST: &str = "library.json";
const PACKAGE_SUFFIX: &str = ".zonebackup";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024 * 1024;

/// What one `library.json` contained and what importing it changed.
#[derive(Clone, Debug)]
pub struct ImportReport {
    pub manifest: PathBuf,
    pub notes: usize,
    pub dictations: usize,
    pub meetings: usize,
    /// Records that could not be read (no id, no date). Counted, never fatal.
    pub skipped: usize,
    /// When the phone wrote the backup, Unix seconds.
    pub written_at: Option<f64>,
    pub applied: Applied,
}

/// Finds the `library.json` files a path refers to: the file itself, a `.zonebackup` folder,
/// or a folder that holds one or more `.zonebackup` folders directly inside it.
pub fn locate(path: &Path) -> Result<Vec<PathBuf>> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot find {}", path.display()))?;
    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let direct = path.join(MANIFEST);
    if direct.is_file() {
        return Ok(vec![direct]);
    }
    if not_downloaded(path) {
        bail!(
            "{} is in iCloud Drive but has not been downloaded to this computer yet. \
             Open the folder in Finder so iCloud downloads it, then try again.",
            direct.display()
        );
    }
    let mut found = Vec::new();
    let mut waiting = false;
    for entry in std::fs::read_dir(path).with_context(|| format!("cannot read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(PACKAGE_SUFFIX) || !entry.path().is_dir() {
            continue;
        }
        let manifest = entry.path().join(MANIFEST);
        if manifest.is_file() {
            found.push(manifest);
        } else if not_downloaded(&entry.path()) {
            waiting = true;
        }
    }
    found.sort();
    if found.is_empty() {
        if waiting {
            bail!(
                "the backup in {} is in iCloud Drive but has not been downloaded to this computer yet. \
                 Open the folder in Finder so iCloud downloads it, then try again.",
                path.display()
            );
        }
        bail!(
            "no Zone backup in {}. Point this at a folder ending in {PACKAGE_SUFFIX}, at its {MANIFEST}, \
             or at the folder that contains it.",
            path.display()
        );
    }
    Ok(found)
}

/// iCloud Drive leaves a `.library.json.icloud` placeholder for a file it has not fetched.
fn not_downloaded(package: &Path) -> bool {
    package.join(format!(".{MANIFEST}.icloud")).exists()
}

/// A cheap fingerprint of a file, to notice that it changed.
pub fn signature(manifest: &Path) -> Option<String> {
    let meta = std::fs::metadata(manifest).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}.{:09}:{}", modified.as_secs(), modified.subsec_nanos(), meta.len()))
}

/// Imports every backup a path refers to.
pub fn import_path(db: &mut Db, path: &Path, now: f64) -> Result<Vec<ImportReport>> {
    let mut reports = Vec::new();
    for manifest in locate(path)? {
        reports.push(import_manifest(db, &manifest, now)?);
    }
    Ok(reports)
}

/// Imports one `library.json` in a single transaction.
pub fn import_manifest(db: &mut Db, manifest: &Path, now: f64) -> Result<ImportReport> {
    let parsed = read_manifest(manifest)?;
    apply_parsed(db, manifest, &parsed, now)
}

/// Merges a backup that has already been read. Touches the library only, never the folder.
pub fn apply_parsed(db: &mut Db, manifest: &Path, parsed: &Parsed, now: f64) -> Result<ImportReport> {
    let delivery = Delivery { carrier: Carrier::Backup, device_id: None, received_at: now };
    let applied = db.apply_items(&parsed.items, &delivery)?;
    Ok(ImportReport {
        manifest: manifest.to_path_buf(),
        notes: parsed.notes,
        dictations: parsed.dictations,
        meetings: parsed.meetings,
        skipped: parsed.skipped,
        written_at: parsed.written_at,
        applied,
    })
}

/// Which backups have been imported already: manifest path to [`signature`].
pub type Seen = BTreeMap<String, String>;

/// One `library.json` that changed since it was last imported, read and parsed.
#[derive(Debug)]
pub struct Scanned {
    pub manifest: PathBuf,
    /// The manifest's key in [`Seen`].
    pub key: String,
    pub signature: String,
    /// A file caught halfway through a sync does not parse. It is tried again next time.
    pub parsed: Result<Parsed>,
}

/// Everything about a backup folder that needs the file system: finding the backups and
/// reading the ones whose `library.json` changed.
///
/// This touches no database, on purpose. Reading a folder can block for as long as it likes:
/// macOS holds the very first `opendir` of an iCloud Drive, Desktop or Documents folder until
/// the person answers a permission prompt, and a cloud folder can be slow. Whoever calls this
/// must not hold the library lock.
pub fn scan(folder: &Path, seen: &Seen) -> Result<Vec<Scanned>> {
    hold_for_tests();
    let mut out = Vec::new();
    for manifest in locate(folder)? {
        let key = manifest.to_string_lossy().to_string();
        let Some(signature) = signature(&manifest) else { continue };
        if seen.get(&key) == Some(&signature) {
            continue;
        }
        let parsed = read_manifest(&manifest);
        out.push(Scanned { manifest, key, signature, parsed });
    }
    Ok(out)
}

/// Debug builds only: while the file named by `ZONE_TEST_HOLD_BACKUP_READ` exists, a read of
/// the backup folder does not come back. This stands in for macOS holding `opendir` behind a
/// permission prompt, which no test can bring about for real. Release builds have no such door.
fn hold_for_tests() {
    #[cfg(debug_assertions)]
    if let Some(gate) = std::env::var_os("ZONE_TEST_HOLD_BACKUP_READ") {
        while Path::new(&gate).exists() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// Runs file system work on its own thread and waits for it for at most `patience`.
/// None means it has not answered yet. The thread is left to finish, or not, by itself: a
/// read stuck in the kernel cannot be cancelled.
pub fn with_patience<T: Send + 'static>(patience: std::time::Duration, work: impl FnOnce() -> T + Send + 'static) -> Patient<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    match receiver.recv_timeout(patience) {
        Ok(value) => Patient::Done(value),
        Err(_) => Patient::Waiting(receiver),
    }
}

/// The answer of [`with_patience`].
pub enum Patient<T> {
    Done(T),
    /// Still running. The receiver yields the result if it ever comes.
    Waiting(std::sync::mpsc::Receiver<T>),
}

/// True when the operating system refused to let this program read something.
pub fn is_permission_denied(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.downcast_ref::<std::io::Error>().map(|e| e.kind() == std::io::ErrorKind::PermissionDenied).unwrap_or(false))
}

/// How to fix a refusal.
pub fn denied_fix() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS did not let zone read the folder.\n\
         Allow it in System Settings > Privacy & Security > Files and Folders (or Full Disk Access), then wait half a minute.\n\
         Or run `zone backup-folder <path>` in a terminal: that imports the folder right away, with the terminal's access."
    } else {
        "Make the folder readable by your user, or choose another with `zone backup-folder <path>`."
    }
}

/// What to tell a person when a read of the backup folder does not come back.
pub fn waiting_reason() -> &'static str {
    if cfg!(target_os = "macos") {
        "waiting for macOS permission to read the backup folder"
    } else {
        "waiting for the backup folder to answer"
    }
}

/// How to get a stuck read moving.
pub fn waiting_fix() -> &'static str {
    if cfg!(target_os = "macos") {
        "Look for a macOS dialog asking whether zone may access the folder, and allow it.\n\
         If there is no dialog: System Settings > Privacy & Security > Files and Folders, and allow zone there.\n\
         Running `zone backup-folder <path>` in a terminal imports the folder right away, with the terminal's access."
    } else {
        "The folder has not answered. If it is on a network or cloud drive, check that the drive is connected."
    }
}

#[derive(Debug)]
pub struct Parsed {
    pub items: Vec<Item>,
    pub notes: usize,
    pub dictations: usize,
    pub meetings: usize,
    pub skipped: usize,
    pub written_at: Option<f64>,
}

pub fn read_manifest(manifest: &Path) -> Result<Parsed> {
    let meta = std::fs::metadata(manifest).with_context(|| format!("cannot find {}", manifest.display()))?;
    if meta.len() > MAX_MANIFEST_BYTES {
        bail!("{} is larger than 1 GB, which is not a Zone backup", manifest.display());
    }
    let bytes = std::fs::read(manifest).with_context(|| format!("cannot read {}", manifest.display()))?;
    parse_manifest(&bytes).with_context(|| format!("{} is not a Zone backup this version can read", manifest.display()))
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Parsed> {
    let root: Value = serde_json::from_slice(bytes).map_err(|e| anyhow!("not valid JSON ({e})"))?;
    let root = root.as_object().context("the top level is not a JSON object")?;
    match root.get("formatVersion").and_then(Value::as_i64) {
        Some(1) => {}
        Some(other) => bail!("backup format {other} needs a newer version of zone"),
        None => bail!("no formatVersion"),
    }
    let mut parsed = Parsed {
        items: Vec::new(),
        notes: 0,
        dictations: 0,
        meetings: 0,
        skipped: 0,
        written_at: number(root, "createdAt").map(|t| t + APPLE_EPOCH),
    };
    for record in array(root, "notes") {
        match note(record) {
            Some(item) => {
                parsed.items.push(item);
                parsed.notes += 1;
            }
            None => parsed.skipped += 1,
        }
    }
    for record in array(root, "dictations") {
        match dictation(record) {
            Some(item) => {
                parsed.items.push(item);
                parsed.dictations += 1;
            }
            None => parsed.skipped += 1,
        }
    }
    for record in array(root, "meetings") {
        match meeting(record) {
            Some(item) => {
                parsed.items.push(item);
                parsed.meetings += 1;
            }
            None => parsed.skipped += 1,
        }
    }
    Ok(parsed)
}

fn array<'a>(object: &'a Map<String, Value>, key: &str) -> impl Iterator<Item = &'a Map<String, Value>> {
    object.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object)
}

fn number(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

fn id(object: &Map<String, Value>) -> Option<String> {
    let id = object.get("id")?.as_str()?.trim();
    if id.is_empty() || id.len() > 128 { None } else { Some(id.to_ascii_uppercase()) }
}

/// `{"id","text","createdAt","editedAt"?}`
fn note(record: &Map<String, Value>) -> Option<Item> {
    let created_at = number(record, "createdAt")? + APPLE_EPOCH;
    let updated_at = number(record, "editedAt").map(|t| t + APPLE_EPOCH).unwrap_or(created_at);
    Some(Item {
        id: id(record)?,
        kind: "note".to_string(),
        source: None,
        created_at,
        updated_at,
        text: string(record, "text").unwrap_or_default(),
        title: None,
        duration_s: None,
        engine: None,
        status: None,
        extra: "{}".to_string(),
        rev: None,
        deleted: false,
    })
}

/// `{"id","text","createdAt","updatedAt","audioSeconds","engine","source","status",
///   "destination"?, "recoveredFrom"?, "interruptions"?}`
fn dictation(record: &Map<String, Value>) -> Option<Item> {
    let created_at = number(record, "createdAt")? + APPLE_EPOCH;
    let updated_at = number(record, "updatedAt").map(|t| t + APPLE_EPOCH).unwrap_or(created_at);
    let mut extra = Map::new();
    if let Some(recovered) = number(record, "recoveredFrom") {
        extra.insert("recovered_from".to_string(), json!(recovered + APPLE_EPOCH));
    }
    if let Some(destination) = string(record, "destination") {
        extra.insert("destination".to_string(), json!(destination));
    }
    if let Some(interruptions) = record.get("interruptions").and_then(Value::as_i64) {
        extra.insert("interruptions".to_string(), json!(interruptions));
    }
    Some(Item {
        id: id(record)?,
        kind: "dictation".to_string(),
        source: string(record, "source"),
        created_at,
        updated_at,
        text: string(record, "text").unwrap_or_default(),
        title: None,
        duration_s: number(record, "audioSeconds"),
        engine: string(record, "engine").filter(|e| !e.is_empty()),
        status: string(record, "status"),
        extra: Value::Object(extra).to_string(),
        rev: None,
        deleted: false,
    })
}

/// `{"id","title","createdAt","updatedAt","status","engine","segments","speakers","parts",...}`
fn meeting(record: &Map<String, Value>) -> Option<Item> {
    let created_at = number(record, "createdAt")? + APPLE_EPOCH;
    let updated_at = number(record, "updatedAt").map(|t| t + APPLE_EPOCH).unwrap_or(created_at);
    let speakers: Vec<&Map<String, Value>> = array(record, "speakers").collect();

    let mut lines = Vec::new();
    let mut segments = Vec::new();
    for segment in array(record, "segments") {
        let text = string(segment, "text").unwrap_or_default();
        let speaker = segment.get("speaker").and_then(Value::as_str).map(|raw| speaker_name(&speakers, raw));
        lines.push(match &speaker {
            Some(name) => format!("{name}: {text}"),
            None => text.clone(),
        });
        segments.push(json!({
            "speaker": speaker,
            "start": number(segment, "start"),
            "end": number(segment, "end"),
            "text": text,
        }));
    }

    // Audio is 16 kHz mono 16-bit, so two bytes per sample.
    let mut bytes: i64 = 0;
    let mut has_parts = false;
    for part in array(record, "parts") {
        if let Some(size) = part.get("bytes").and_then(Value::as_i64) {
            bytes += size.max(0) / 2 * 2;
            has_parts = true;
        }
    }
    let duration_s = if has_parts { Some((bytes / 2) as f64 / 16_000.0) } else { None };

    let mut extra = Map::new();
    extra.insert("segments".to_string(), Value::Array(segments));
    if let Some(diarizer) = string(record, "diarizer") {
        extra.insert("diarizer".to_string(), json!(diarizer));
    }
    Some(Item {
        id: id(record)?,
        kind: "meeting".to_string(),
        source: None,
        created_at,
        updated_at,
        text: lines.join("\n"),
        title: string(record, "title"),
        duration_s,
        engine: string(record, "engine"),
        status: string(record, "status"),
        extra: Value::Object(extra).to_string(),
        rev: None,
        deleted: false,
    })
}

/// The name the phone shows for a speaker id: the person's name, "Me", or "Speaker N".
fn speaker_name(speakers: &[&Map<String, Value>], raw: &str) -> String {
    let Some(index) = speakers.iter().position(|s| s.get("id").and_then(Value::as_str) == Some(raw)) else {
        return raw.to_string();
    };
    let speaker = speakers[index];
    if let Some(name) = speaker.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    if speaker.get("isMe").and_then(Value::as_bool).unwrap_or(false) {
        return "Me".to_string();
    }
    format!("Speaker {}", index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Filter, Order};

    /// A synthetic backup with the same shape as a real one. Dates are seconds since 2001.
    fn fixture() -> Value {
        json!({
            "formatVersion": 1,
            "createdAt": 816317953.440582,
            "historyArchive": "eyJyZWNvcmRzIjpbXX0=",
            "historyRecoveryFiles": { "inflight-AAAA.json": "e30=" },
            "notes": [
                { "id": "11111111-0000-4000-8000-000000000001", "text": "Buy oat milk", "createdAt": 816200000.5 },
                { "id": "11111111-0000-4000-8000-000000000002", "text": "Edited note", "createdAt": 816200100.25, "editedAt": 816200900.75 }
            ],
            "dictations": [
                { "id": "22222222-0000-4000-8000-000000000001", "text": "On my way, see you at six", "createdAt": 816210000,
                  "updatedAt": 816210004, "audioSeconds": 3.75, "engine": "fluidMiniCPU", "source": "keyboard", "status": "inserted" },
                { "id": "22222222-0000-4000-8000-000000000002", "text": "A thought for later", "createdAt": 816210100,
                  "updatedAt": 816210100, "audioSeconds": 9, "engine": "moonshineMedium", "source": "note", "status": "note",
                  "interruptions": 2, "recoveredFrom": 816209000, "destination": "Notes" },
                { "text": "no id, skipped", "createdAt": 816210200, "updatedAt": 816210200, "audioSeconds": 1, "engine": "x", "source": "app", "status": "finished" }
            ],
            "meetings": [
                { "id": "33333333-0000-4000-8000-000000000001", "title": "Weekly planning", "createdAt": 816220000, "updatedAt": 816223600,
                  "status": "done", "engine": "fluidMiniCPU", "diarizer": "pyannote", "progress": 1, "transcribedThrough": 120.0,
                  "parts": [ { "fileName": "part-1.pcm", "bytes": 1920000 }, { "fileName": "part-2.pcm", "bytes": 1920001 } ],
                  "speakers": [ { "id": "S1", "isMe": true }, { "id": "S2", "isMe": false }, { "id": "S3", "name": " Grace ", "isMe": false } ],
                  "segments": [
                      { "start": 0, "end": 4.5, "speaker": "S1", "text": "Shall we start" },
                      { "start": 4.5, "end": 9.0, "speaker": "S2", "text": "Yes, the launch is Friday" },
                      { "start": 9.0, "end": 12.0, "speaker": "S3", "text": "I will send the notes" },
                      { "start": 12.0, "end": 13.0, "speaker": "S9", "text": "Unknown voice" },
                      { "start": 13.0, "end": 14.0, "text": "Nobody in particular" }
                  ] },
                { "id": "33333333-0000-4000-8000-000000000002", "title": "Empty", "createdAt": 816230000, "updatedAt": 816230000,
                  "status": "done", "engine": "fluidMiniCPU", "progress": 1, "transcribedThrough": 0.5,
                  "parts": [ { "fileName": "part-1.pcm", "bytes": 16000 } ], "speakers": [], "segments": [] }
            ]
        })
    }

    fn write_package(root: &Path, name: &str, body: &Value) -> PathBuf {
        let package = root.join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(MANIFEST), serde_json::to_vec_pretty(body).unwrap()).unwrap();
        package
    }

    #[test]
    fn parses_every_kind() {
        let parsed = parse_manifest(&serde_json::to_vec(&fixture()).unwrap()).unwrap();
        assert_eq!((parsed.notes, parsed.dictations, parsed.meetings, parsed.skipped), (2, 2, 2, 1));
        assert_eq!(parsed.written_at, Some(816317953.440582 + APPLE_EPOCH));
        let by_id = |suffix: &str| parsed.items.iter().find(|i| i.id.ends_with(suffix)).unwrap();

        let note = by_id("11111111-0000-4000-8000-000000000001");
        assert_eq!((note.kind.as_str(), note.source.as_deref()), ("note", None));
        assert_eq!(note.created_at, 816200000.5 + APPLE_EPOCH);
        assert_eq!(note.updated_at, note.created_at);
        let edited = by_id("11111111-0000-4000-8000-000000000002");
        assert_eq!(edited.updated_at, 816200900.75 + APPLE_EPOCH);

        let typed = by_id("22222222-0000-4000-8000-000000000001");
        assert_eq!((typed.kind.as_str(), typed.source.as_deref()), ("dictation", Some("keyboard")));
        assert_eq!(typed.created_at, 816210000.0 + APPLE_EPOCH);
        assert_eq!(typed.updated_at, 816210004.0 + APPLE_EPOCH);
        assert_eq!(typed.duration_s, Some(3.75));
        assert_eq!(typed.engine.as_deref(), Some("fluidMiniCPU"));
        assert_eq!(typed.status.as_deref(), Some("inserted"));
        assert_eq!(typed.extra, "{}");

        let spoken = by_id("22222222-0000-4000-8000-000000000002");
        assert_eq!(spoken.source.as_deref(), Some("note"));
        assert_eq!(spoken.duration_s, Some(9.0));
        let extra: Value = serde_json::from_str(&spoken.extra).unwrap();
        assert_eq!(extra["recovered_from"], json!(816209000.0 + APPLE_EPOCH));
        assert_eq!(extra["destination"], "Notes");
        assert_eq!(extra["interruptions"], 2);

        let meeting = by_id("33333333-0000-4000-8000-000000000001");
        assert_eq!(meeting.kind, "meeting");
        assert_eq!(meeting.title.as_deref(), Some("Weekly planning"));
        assert_eq!(
            meeting.text,
            "Me: Shall we start\nSpeaker 2: Yes, the launch is Friday\nGrace: I will send the notes\n\
             S9: Unknown voice\nNobody in particular"
        );
        // 1920000 + 1920000 bytes (the odd byte is dropped) at 32000 bytes per second.
        assert_eq!(meeting.duration_s, Some(120.0));
        let extra: Value = serde_json::from_str(&meeting.extra).unwrap();
        assert_eq!(extra["segments"].as_array().unwrap().len(), 5);
        assert_eq!(extra["segments"][2], json!({ "speaker": "Grace", "start": 9.0, "end": 12.0, "text": "I will send the notes" }));
        assert_eq!(extra["segments"][4]["speaker"], Value::Null);
        assert_eq!(extra["diarizer"], "pyannote");

        let empty = by_id("33333333-0000-4000-8000-000000000002");
        assert_eq!((empty.text.as_str(), empty.duration_s), ("", Some(0.5)));
    }

    #[test]
    fn imports_and_reimports_without_churn() {
        let dir = tempfile::tempdir().unwrap();
        let package = write_package(dir.path(), "Zone library.zonebackup", &fixture());
        let mut db = Db::open_in_memory().unwrap();

        let reports = import_path(&mut db, &package, 100.0).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].applied.inserted, 6);
        assert_eq!(db.total_items().unwrap(), 6);
        assert_eq!(
            db.counts_by_kind().unwrap(),
            vec![("dictation".to_string(), 2), ("meeting".to_string(), 2), ("note".to_string(), 2)]
        );
        assert_eq!(db.search("launch", &Filter::default(), None, 5).unwrap().total, 1);
        assert_eq!(db.meta_f64("last_sync_backup").unwrap(), Some(100.0));

        // The same file again changes nothing.
        let again = import_path(&mut db, &package.join(MANIFEST), 200.0).unwrap();
        assert_eq!((again[0].applied.changed(), again[0].applied.unchanged), (0, 6));
        let row = db.list(&Filter::default(), Order::Newest, 1, None).unwrap().remove(0);
        assert_eq!(row.received_at, 100.0);
        assert_eq!(row.carrier, "backup");
    }

    #[test]
    fn the_nested_older_copy_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("Zone Backups");
        let package = write_package(&parent, "Zone library.zonebackup", &fixture());
        // An older backup saved inside the package, with an item the outer one lacks.
        let mut old = fixture();
        old["notes"] = json!([{ "id": "99999999-0000-4000-8000-000000000009", "text": "stale", "createdAt": 700000000.0 }]);
        write_package(&package, "Zone library.zonebackup", &old);
        std::fs::write(package.join("Notes and transcripts.txt"), "ignored").unwrap();

        // Pointed at the parent folder, the package, or the file: one manifest, the outer one.
        for path in [parent.clone(), package.clone(), package.join(MANIFEST)] {
            let found = locate(&path).unwrap();
            assert_eq!(found, vec![package.join(MANIFEST)], "{}", path.display());
        }
        let mut db = Db::open_in_memory().unwrap();
        import_path(&mut db, &parent, 1.0).unwrap();
        assert_eq!(db.total_items().unwrap(), 6);
        assert!(db.get("99999999-0000-4000-8000-000000000009").unwrap().is_none());
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        assert!(locate(&dir.path().join("missing")).is_err());
        assert!(locate(dir.path()).unwrap_err().to_string().contains("no Zone backup"));

        let mut newer = fixture();
        newer["formatVersion"] = json!(2);
        let err = parse_manifest(&serde_json::to_vec(&newer).unwrap()).unwrap_err();
        assert!(err.to_string().contains("newer version"));
        assert!(parse_manifest(b"not json").is_err());
        assert!(parse_manifest(b"[]").is_err());
        assert!(parse_manifest(b"{}").is_err());

        // A package iCloud has not downloaded says so.
        let package = dir.path().join("Cloud.zonebackup");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(".library.json.icloud"), "").unwrap();
        assert!(locate(&package).unwrap_err().to_string().contains("not been downloaded"));
        assert!(locate(dir.path()).unwrap_err().to_string().contains("not been downloaded"));
    }

    #[test]
    fn a_changed_file_has_a_new_signature() {
        let dir = tempfile::tempdir().unwrap();
        let package = write_package(dir.path(), "A.zonebackup", &fixture());
        let manifest = package.join(MANIFEST);
        let before = signature(&manifest).unwrap();
        assert_eq!(signature(&manifest).unwrap(), before);
        let mut bigger = fixture();
        bigger["notes"].as_array_mut().unwrap().push(json!({ "id": "N3", "text": "new", "createdAt": 816300000.0 }));
        std::fs::write(&manifest, serde_json::to_vec_pretty(&bigger).unwrap()).unwrap();
        assert_ne!(signature(&manifest).unwrap(), before);
        assert!(signature(&dir.path().join("nope")).is_none());
    }
}
