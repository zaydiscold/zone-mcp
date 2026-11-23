//! What the tray shows, worked out apart from Windows so it can be tested anywhere: what zone
//! says about itself (its JSON, read as it is printed), the icon's state, the one line under
//! the pointer, and the menu from top to bottom.
//!
//! The words are the ones Zone for Mac uses (mac/ZoneKit/Sources/ZoneKit/Menu.swift and
//! Formats.swift), so both read the same.

use serde::Deserialize;

// -------------------------------------------------------------------- zone's JSON

/// `zone status --json`. Every field may be missing: an older zone says less.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Status {
    pub now: Now,
    pub computer: Computer,
    pub daemon_running: bool,
    pub phones: Vec<Phone>,
    pub last_sync: LastSync,
    pub items: Items,
    pub data_directory: String,
    pub agents: Option<Agents>,
    pub daemon: Daemon,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Now {
    /// The day, as `2026-11-19`, in the computer's time zone.
    pub date: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Computer {
    pub zone_version: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Phone {
    pub name: String,
    pub last_sync_age_s: Option<f64>,
    pub items_received: u64,
    pub last_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct LastSync {
    pub link: Option<Synced>,
    pub flash: Option<Synced>,
    pub backup_folder: Option<Synced>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Synced {
    pub age_s: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Items {
    pub total: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Agents {
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Daemon {
    pub running: bool,
    pub version: Option<String>,
    pub relay_connected: Option<bool>,
}

/// One item of `zone list --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub source: Option<String>,
    pub created_at: f64,
    pub created_local: String,
    pub text: String,
    pub title: Option<String>,
    pub duration_s: Option<f64>,
}

/// `zone list --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ItemList {
    pub items: Vec<Item>,
}

/// `zone clip --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ClipAnswer {
    pub clip: Option<Clip>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Clip {
    pub id: String,
    pub created_local: String,
    pub first_line: String,
    pub text: String,
}

/// `zone clipboard status --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ClipboardSetting {
    pub enabled: bool,
    pub available: bool,
}

/// `zone mcp status --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Clients {
    pub clients: Vec<Client>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Client {
    pub client: String,
    pub label: String,
    pub installed: bool,
    pub state: String,
}

/// `zone update --check --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdateAnswer {
    pub current: String,
    pub latest: Option<String>,
    pub newer: bool,
    pub checked: bool,
    pub note: Option<String>,
    pub update_with: String,
}

/// `zone doctor --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Doctor {
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DoctorCheck {
    pub check: String,
    pub result: String,
    pub detail: String,
    pub fix: Option<String>,
}

/// One line of `zone events --json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Event {
    pub seq: i64,
    pub event: String,
    pub new: Option<u64>,
    pub changed: Option<u64>,
    pub items: Option<u64>,
}

impl Event {
    /// True when this event put something new in the library: the icon then shows
    /// "receiving" for a few seconds, as the Mac's does.
    pub fn brought_items(&self) -> bool {
        match self.event.as_str() {
            "clip" => true,
            "link_session" | "flash_received" | "backup_import" => match (self.new, self.changed) {
                (None, None) => self.items.unwrap_or(0) > 0,
                (new, changed) => new.unwrap_or(0) + changed.unwrap_or(0) > 0,
            },
            _ => false,
        }
    }
}

// -------------------------------------------------------------------- what the tray knows

/// Whether zone starts at login, as the login entry stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Login {
    On,
    #[default]
    Off,
    /// There is an entry, but not one this tray can switch: the words say why.
    Other(String),
}

/// What is known about updates. Nothing is asked until the person asks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Update {
    #[default]
    NotAsked,
    Latest,
    Newer {
        latest: String,
        /// How this copy of zone is updated: `zone update` when the install script put it there.
        update_with: String,
    },
    Unknown(String),
}

impl Update {
    pub fn from_answer(answer: &UpdateAnswer) -> Update {
        match (&answer.latest, answer.checked, answer.newer) {
            (Some(latest), true, true) => Update::Newer {
                latest: latest.clone(),
                update_with: answer.update_with.clone(),
            },
            (_, true, _) => Update::Latest,
            _ => Update::Unknown(answer.note.clone().unwrap_or_else(|| "no answer".to_string())),
        }
    }
}

/// Everything the tray knows at one moment.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// When zone was asked, in Unix seconds by this computer's clock. Ages zone gave are
    /// counted on from here, so the line stays right between two askings.
    pub fetched_at: f64,
    /// None when zone could not be asked. `trouble` then says why.
    pub status: Option<Status>,
    pub trouble: Option<String>,
    pub latest: Vec<Item>,
    pub meeting: Option<Item>,
    pub clip: Option<Clip>,
    pub clipboard: Option<ClipboardSetting>,
    pub apps: Option<Vec<Client>>,
    pub login: Login,
    pub update: Update,
    /// Something is arriving until this moment (Unix seconds).
    pub receiving_until: f64,
    pub tray_version: String,
    /// The version of the zone program beside the tray.
    pub zone_version: Option<String>,
    /// What the tray is in the middle of, when that takes a while: "Updating zone".
    pub working: Option<String>,
}

impl Snapshot {
    pub fn running(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.daemon_running)
    }

    fn age(&self, age_at_fetch: f64, now: f64) -> f64 {
        age_at_fetch + (now - self.fetched_at).max(0.0)
    }
}

// -------------------------------------------------------------------- the icon and its line

/// What the icon is saying. The same four faces as the Mac's menu bar icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// Running: the Z with its dot.
    Running,
    /// Something is arriving right now: the dot with a ring.
    Receiving,
    /// zone is not running: a faded Z with a hollow dot.
    Stopped,
    /// Something needs the person: an exclamation mark.
    Attention,
}

pub fn state(snapshot: &Snapshot, now: f64) -> State {
    match &snapshot.status {
        None => State::Attention,
        Some(status) if !status.daemon_running => State::Stopped,
        Some(_) if now < snapshot.receiving_until => State::Receiving,
        Some(_) => State::Running,
    }
}

/// The newest sync, however it came: its age now, and the words for it with and without the
/// phone's name.
fn newest_sync(snapshot: &Snapshot, status: &Status, now: f64) -> Option<(String, String)> {
    let phone = status
        .phones
        .iter()
        .filter(|phone| phone.last_sync_age_s.is_some())
        .min_by(|a, b| a.last_sync_age_s.partial_cmp(&b.last_sync_age_s).unwrap_or(std::cmp::Ordering::Equal));
    let mut newest: Option<(f64, String, String)> = None;
    let mut offer = |age: Option<f64>, named: &dyn Fn(&str) -> String, plain: &dyn Fn(&str) -> String| {
        let Some(age) = age else { return };
        if newest.as_ref().is_none_or(|(best, _, _)| age < best - 1.0) {
            let ago = ago(snapshot.age(age, now));
            newest = Some((age, named(&ago), plain(&ago)));
        }
    };
    if let Some(phone) = phone {
        let name = if phone.name.is_empty() { "iPhone" } else { &phone.name };
        offer(phone.last_sync_age_s, &|ago| format!("{name}, synced {ago}"), &|ago| {
            format!("synced {ago}")
        });
    }
    offer(
        status.last_sync.flash.as_ref().and_then(|s| s.age_s),
        &|ago| format!("Synced by Flash {ago}"),
        &|ago| format!("synced by Flash {ago}"),
    );
    offer(
        status.last_sync.backup_folder.as_ref().and_then(|s| s.age_s),
        &|ago| format!("Backup folder read {ago}"),
        &|ago| format!("backup folder read {ago}"),
    );
    // zone can know of a sync without naming the phone (an older zone).
    offer(
        status.last_sync.link.as_ref().and_then(|s| s.age_s),
        &|ago| format!("Synced {ago}"),
        &|ago| format!("synced {ago}"),
    );
    newest.map(|(_, named, plain)| (named, plain))
}

/// The first line of the menu: which phone synced last and when, or why not.
pub fn status_line(snapshot: &Snapshot, now: f64) -> String {
    let Some(status) = &snapshot.status else {
        return snapshot
            .trouble
            .clone()
            .unwrap_or_else(|| "zone could not be asked how it is".to_string());
    };
    if let Some(working) = &snapshot.working {
        return working.clone();
    }
    if !status.daemon_running {
        return "Zone is not running".to_string();
    }
    if now < snapshot.receiving_until {
        return "Receiving".to_string();
    }
    if let Some((named, _)) = newest_sync(snapshot, status, now) {
        return named;
    }
    match status.phones.as_slice() {
        [] => "Ready to pair".to_string(),
        [only] => format!("{}, waiting for its first sync", only.name),
        _ => "Waiting for the first sync".to_string(),
    }
}

/// The one line shown when the pointer rests on the icon. It never holds anything a person
/// said, and Windows shows at most 127 characters of it.
pub fn tooltip(snapshot: &Snapshot, now: f64) -> String {
    let line = match &snapshot.status {
        None => format!(
            "Zone needs attention: {}",
            snapshot.trouble.as_deref().unwrap_or("zone could not be asked how it is")
        ),
        Some(_) if snapshot.working.is_some() => format!("Zone: {}", lower_first(snapshot.working.as_deref().unwrap_or(""))),
        Some(status) if !status.daemon_running => "Zone: not running".to_string(),
        Some(_) if now < snapshot.receiving_until => "Zone: receiving".to_string(),
        Some(status) => match newest_sync(snapshot, status, now) {
            Some((_, plain)) => format!("Zone: {plain}"),
            None if status.phones.is_empty() => "Zone: ready to pair".to_string(),
            None => "Zone: waiting for the first sync".to_string(),
        },
    };
    shorten(&line, 127)
}

// -------------------------------------------------------------------- the menu

/// A place on zone's page (docs/page-routes.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Pair,
    Settings,
    Meeting(String),
}

impl Route {
    /// The part of the address after `#`. None for the page as it opens by itself.
    pub fn fragment(&self) -> Option<String> {
        match self {
            Route::Home => None,
            Route::Pair => Some("/pair".to_string()),
            Route::Settings => Some("/settings".to_string()),
            Route::Meeting(id) => Some(format!("/meeting/{}", percent(id))),
        }
    }

    /// The page's own link (what `zone open --print` prints), pointed at this place.
    pub fn url(&self, page: &str) -> String {
        let page = page.trim().split('#').next().unwrap_or("");
        match self.fragment() {
            Some(fragment) => format!("{page}#{fragment}"),
            None => page.to_string(),
        }
    }
}

fn percent(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// What choosing a line of the menu does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Put this text on the clipboard.
    Copy(String),
    OpenPage(Route),
    OpenFlash,
    StartZone,
    StopZone,
    RestartZone,
    SetAgents(bool),
    SetClipboard(bool),
    SetLogin(bool),
    CopyMcpSettings,
    OpenLog,
    OpenDataFolder,
    RunDoctor,
    CheckForUpdates,
    UpdateNow,
    OpenGitHub,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Line {
        label: String,
        /// Shown at the right edge: a time.
        aside: Option<String>,
        /// None: a line to read, not to choose.
        action: Option<Action>,
        checked: bool,
        /// Drawn gray: a heading, or something that cannot be chosen right now.
        dimmed: bool,
    },
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
    Separator,
}

/// A fact to read.
fn read(label: impl Into<String>) -> Entry {
    Entry::Line {
        label: label.into(),
        aside: None,
        action: None,
        checked: false,
        dimmed: false,
    }
}

/// A heading, or something that cannot be chosen right now.
fn dim(label: impl Into<String>) -> Entry {
    Entry::Line {
        label: label.into(),
        aside: None,
        action: None,
        checked: false,
        dimmed: true,
    }
}

fn choose(label: impl Into<String>, action: Action) -> Entry {
    Entry::Line {
        label: label.into(),
        aside: None,
        action: Some(action),
        checked: false,
        dimmed: false,
    }
}

fn tick(label: impl Into<String>, on: bool, action: Action) -> Entry {
    Entry::Line {
        label: label.into(),
        aside: None,
        action: Some(action),
        checked: on,
        dimmed: false,
    }
}

/// How many of the latest items the menu lists.
pub const LATEST_ROWS: usize = 6;
/// How many `zone list` is asked for, so that enough are left once the recordings behind
/// voice notes, the clip and the meeting are taken out.
pub const LATEST_ASKED: usize = 24;
/// How wide a line of someone's words may be in the menu.
const ROW_CHARS: usize = 58;

/// True when `item` is the recording behind one of `notes`: a dictation whose source is
/// `note`, with a note saved while it was recorded or in the two minutes after. The same
/// rule as the page's and the Mac's.
fn is_recording_behind_a_note(item: &Item, notes: &[&Item]) -> bool {
    if item.kind != "dictation" || item.source.as_deref() != Some("note") {
        return false;
    }
    let (from, to) = (item.created_at - 5.0, item.created_at + item.duration_s.unwrap_or(0.0) + 120.0);
    notes.iter().any(|note| note.created_at >= from && note.created_at <= to)
}

/// The rows under "Latest": the newest items, without the recording behind a voice note
/// (the note is there with the same words) and without what has a line of its own (the
/// newest clip, the last meeting).
pub fn latest_rows(snapshot: &Snapshot) -> Vec<&Item> {
    let notes: Vec<&Item> = snapshot.latest.iter().filter(|item| item.kind == "note").collect();
    let above = [
        snapshot.clip.as_ref().map(|clip| clip.id.as_str()),
        snapshot.meeting.as_ref().map(|meeting| meeting.id.as_str()),
    ];
    snapshot
        .latest
        .iter()
        .filter(|item| !above.contains(&Some(item.id.as_str())) && !is_recording_behind_a_note(item, &notes))
        .take(LATEST_ROWS)
        .collect()
}

/// One line for an item: a meeting by its title, anything else by its words.
fn row_text(item: &Item) -> String {
    if item.kind == "meeting" {
        return meeting_title(item, "Meeting");
    }
    let flat = one_line(&item.text);
    match item.title.as_deref().map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) if flat.is_empty() => title.to_string(),
        Some(title) => format!("{title}: {flat}"),
        None => flat,
    }
}

fn meeting_title(item: &Item, untitled: &str) -> String {
    match item.title.as_deref().map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => title.to_string(),
        None => untitled.to_string(),
    }
}

fn kind_word(item: &Item) -> &'static str {
    match item.kind.as_str() {
        "note" => "Note",
        "meeting" => "Meeting",
        "clip" => "Clip",
        _ => "Dictation",
    }
}

/// How a phone's last sync travelled, for someone who has never heard of a relay.
fn route_words(path: Option<&str>) -> Option<&'static str> {
    match path? {
        "direct" => Some("straight over this network"),
        "relay" => Some("through Zone's relay"),
        "relay then direct" => Some("through the relay, then straight"),
        "direct then relay" => Some("straight, then through the relay"),
        _ => None,
    }
}

/// One line per paired phone.
fn phone_lines(snapshot: &Snapshot, status: &Status, now: f64) -> Vec<String> {
    let single = status.phones.len() == 1;
    status
        .phones
        .iter()
        .map(|phone| {
            let items = format!(
                "{} {}",
                number(phone.items_received),
                if phone.items_received == 1 { "item" } else { "items" }
            );
            let mut parts = Vec::new();
            if single {
                parts.push(format!("{items} from {}", phone.name));
            } else {
                parts.push(match phone.last_sync_age_s {
                    Some(age) => format!("{}: synced {}", phone.name, ago(snapshot.age(age, now))),
                    None => format!("{}: waiting for its first sync", phone.name),
                });
                parts.push(items);
            }
            if let Some(route) = route_words(phone.last_path.as_deref()) {
                parts.push(route.to_string());
            }
            parts.join(", ")
        })
        .collect()
}

/// A connected app's standing in a few words, as Zone for Mac says it.
fn standing_words(state: &str) -> &'static str {
    match state {
        "registered" => "Connected",
        "registered_read_only" => "Connected, read only",
        "other_copy" => "Starts another copy of zone",
        "removed_by_you" => "Removed by you",
        "unreadable" => "Its settings could not be read",
        _ => "Not connected",
    }
}

/// The apps under "Connected apps": each app once however many profiles it has, the ones
/// that have zone first. An app that is on this computer and not connected is listed too.
pub fn app_lines(clients: &[Client]) -> Vec<String> {
    let mut seen: Vec<(String, String, Vec<&str>)> = Vec::new();
    for client in clients {
        let has_zone = matches!(client.state.as_str(), "registered" | "registered_read_only" | "other_copy");
        if !has_zone && !client.installed {
            continue;
        }
        // "Claude Code (default)" is the app "Claude Code".
        let name = client.label.split(" (").next().unwrap_or(&client.label).trim().to_string();
        let name = if name.is_empty() { client.client.clone() } else { name };
        match seen.iter_mut().find(|(key, _, _)| *key == client.client) {
            Some((_, _, states)) => states.push(&client.state),
            None => seen.push((client.client.clone(), name, vec![&client.state])),
        }
    }
    let rank = |states: &[&str]| {
        if states.iter().any(|s| matches!(*s, "registered" | "registered_read_only" | "other_copy")) {
            0
        } else {
            1
        }
    };
    seen.sort_by_key(|(_, _, states)| rank(states));
    seen.iter()
        .map(|(_, name, states)| {
            // The best standing among its profiles speaks for the app.
            let order = ["registered", "registered_read_only", "other_copy", "unreadable", "removed_by_you"];
            let best = order.iter().find(|wanted| states.contains(wanted)).copied().unwrap_or("not_registered");
            let connected = states.iter().filter(|s| matches!(**s, "registered" | "registered_read_only")).count();
            let words = if connected > 1 {
                format!("Connected, {connected} profiles")
            } else {
                standing_words(best).to_string()
            };
            format!("{name}: {words}")
        })
        .collect()
}

/// The versions line: the tray's, the zone beside it when that is another, and the zone
/// that is running when that is another again.
pub fn versions_line(snapshot: &Snapshot) -> String {
    let plain = |text: &str| text.trim().trim_start_matches(['v', 'V']).to_string();
    let tray = plain(&snapshot.tray_version);
    let mut line = format!("Zone {tray}");
    let beside = snapshot.zone_version.as_deref().map(plain).filter(|version| !version.is_empty());
    if let Some(zone) = &beside
        && *zone != tray
    {
        line.push_str(&format!(", with zone {zone}"));
    }
    let running = snapshot
        .status
        .as_ref()
        .filter(|status| status.daemon_running)
        .and_then(|status| status.daemon.version.as_deref())
        .map(plain)
        .filter(|version| !version.is_empty());
    if let Some(running) = running
        && running != beside.unwrap_or(tray)
    {
        line.push_str(&format!(". Running: zone {running}"));
    }
    line
}

/// The menu, top to bottom.
pub fn menu(snapshot: &Snapshot, now: f64) -> Vec<Entry> {
    let mut entries = vec![read(status_line(snapshot, now))];
    let running = snapshot.running();
    let today = snapshot.status.as_ref().map(|status| status.now.date.as_str()).unwrap_or("");

    if let Some(status) = &snapshot.status {
        if !running {
            entries.push(choose("Start zone", Action::StartZone));
        } else {
            for line in phone_lines(snapshot, status, now) {
                entries.push(read(line));
            }
            if status.daemon.relay_connected == Some(false) {
                entries.push(read("Reachable on this network only, while zone reconnects to the relay"));
            }
        }

        let rows = latest_rows(snapshot);
        if !rows.is_empty() || snapshot.meeting.is_some() || snapshot.clip.is_some() {
            entries.push(Entry::Separator);
        }
        if let Some(clip) = &snapshot.clip {
            let line = if clip.first_line.trim().is_empty() {
                one_line(&clip.text)
            } else {
                one_line(&clip.first_line)
            };
            entries.push(Entry::Line {
                label: format!("Copy clip: {}", shorten(&line, ROW_CHARS - 11)),
                aside: Some(when(&clip.created_local, today)),
                action: Some(Action::Copy(clip.text.clone())),
                checked: false,
                dimmed: false,
            });
        }
        if !rows.is_empty() {
            entries.push(dim("Latest"));
            for item in rows {
                entries.push(Entry::Line {
                    label: shorten(&format!("{}: {}", kind_word(item), row_text(item)), ROW_CHARS),
                    aside: Some(when(&item.created_local, today)),
                    action: Some(Action::Copy(item.text.clone())),
                    checked: false,
                    dimmed: false,
                });
            }
        }
        if let Some(meeting) = &snapshot.meeting {
            let mut facts = vec![when(&meeting.created_local, today)];
            if let Some(seconds) = meeting.duration_s.filter(|seconds| *seconds > 0.0) {
                facts.push(length(seconds));
            }
            entries.push(Entry::Line {
                label: shorten(&format!("Last meeting: {}", meeting_title(meeting, "Untitled")), ROW_CHARS),
                aside: Some(facts.join(", ")),
                // The page is zone's: it can only be opened while zone runs.
                action: running.then(|| Action::OpenPage(Route::Meeting(meeting.id.clone()))),
                checked: false,
                dimmed: !running,
            });
        }
    }

    entries.push(Entry::Separator);
    let page = |label: &str, action: Action| if running { choose(label, action) } else { dim(label) };
    entries.push(page("Open Zone", Action::OpenPage(Route::Home)));
    entries.push(page("Flash", Action::OpenFlash));
    entries.push(page("Pair an iPhone", Action::OpenPage(Route::Pair)));

    let mut agents = Vec::new();
    if let Some(switch) = snapshot.status.as_ref().and_then(|status| status.agents.as_ref()) {
        agents.push(tick("Let AI agents read my library", switch.enabled, Action::SetAgents(!switch.enabled)));
    }
    agents.push(page("Agent settings", Action::OpenPage(Route::Settings)));
    entries.push(Entry::Submenu {
        label: "Agents".to_string(),
        entries: agents,
    });

    entries.push(Entry::Separator);
    match &snapshot.clipboard {
        Some(setting) if setting.available || setting.enabled => entries.push(tick(
            "Copy what I send from my iPhone",
            setting.enabled,
            Action::SetClipboard(!setting.enabled),
        )),
        Some(_) => entries.push(dim("Copy what I send from my iPhone (no clipboard tool)")),
        None => {}
    }
    match &snapshot.login {
        Login::On => entries.push(tick("Start at login", true, Action::SetLogin(false))),
        Login::Off => entries.push(tick("Start at login", false, Action::SetLogin(true))),
        Login::Other(why) => entries.push(dim(format!("Start at login ({why})"))),
    }

    let mut developer = vec![choose("Copy MCP settings", Action::CopyMcpSettings)];
    let apps = snapshot.apps.as_deref().map(app_lines).unwrap_or_default();
    developer.push(Entry::Submenu {
        label: "Connected apps".to_string(),
        entries: if apps.is_empty() {
            vec![
                read("No AI apps found on this computer"),
                page("Agent settings", Action::OpenPage(Route::Settings)),
            ]
        } else {
            apps.into_iter()
                .map(read)
                .chain([Entry::Separator, page("Agent settings", Action::OpenPage(Route::Settings))])
                .collect()
        },
    });
    developer.push(Entry::Separator);
    developer.push(choose("Open the log", Action::OpenLog));
    developer.push(choose("Open the data folder", Action::OpenDataFolder));
    developer.push(choose("Run zone doctor", Action::RunDoctor));
    developer.push(Entry::Separator);
    if running {
        developer.push(choose("Restart zone", Action::RestartZone));
        developer.push(choose("Stop zone", Action::StopZone));
    } else {
        developer.push(choose("Start zone", Action::StartZone));
    }
    developer.push(Entry::Separator);
    developer.push(read(versions_line(snapshot)));
    match &snapshot.update {
        Update::NotAsked => developer.push(choose("Check for updates", Action::CheckForUpdates)),
        Update::Latest => developer.push(choose("Up to date. Check again", Action::CheckForUpdates)),
        Update::Newer { latest, .. } => {
            developer.push(choose(format!("Update now to {latest}"), Action::UpdateNow));
            developer.push(choose("Check for updates", Action::CheckForUpdates));
        }
        Update::Unknown(_) => developer.push(choose("Could not check for updates. Try again", Action::CheckForUpdates)),
    }
    developer.push(choose("Open on GitHub", Action::OpenGitHub));
    entries.push(Entry::Submenu {
        label: "Developer".to_string(),
        entries: developer,
    });

    entries.push(Entry::Separator);
    entries.push(choose("Quit", Action::Quit));
    entries
}

/// The menu as plain text, one line each: what a test (or a person without Windows) reads.
pub fn menu_text(entries: &[Entry]) -> String {
    fn write(entries: &[Entry], depth: usize, out: &mut String) {
        for entry in entries {
            out.push_str(&"    ".repeat(depth));
            match entry {
                Entry::Separator => out.push_str("----\n"),
                Entry::Submenu { label, entries } => {
                    out.push_str(&format!("{label} >\n"));
                    write(entries, depth + 1, out);
                }
                Entry::Line {
                    label,
                    aside,
                    action,
                    checked,
                    dimmed,
                } => {
                    let mark = if *checked { "[x] " } else { "" };
                    let aside = aside.as_deref().map(|aside| format!("  |  {aside}")).unwrap_or_default();
                    let kind = match (action.is_some(), dimmed) {
                        (true, _) => "",
                        (false, false) => "  (to read)",
                        (false, true) => "  (gray)",
                    };
                    out.push_str(&format!("{mark}{label}{aside}{kind}\n"));
                }
            }
        }
    }
    let mut out = String::new();
    write(entries, 0, &mut out);
    out
}

/// What `zone doctor --json` found, as a few lines to read in a small window.
pub fn doctor_text(doctor: &Doctor) -> String {
    let troubled = doctor.checks.iter().filter(|check| check.result != "ok").count();
    let mut out = match (doctor.healthy, troubled) {
        (true, 0) => "Everything looks right.".to_string(),
        (true, 1) => "zone works. One thing to know:".to_string(),
        (true, n) => format!("zone works. {n} things to know:"),
        (false, 1) => "One thing needs fixing:".to_string(),
        (false, n) => format!("{n} things need fixing:"),
    };
    out.push('\n');
    for check in &doctor.checks {
        let mark = match check.result.as_str() {
            "ok" => "ok",
            "fail" => "FIX",
            _ => "note",
        };
        out.push_str(&format!("\n{mark}   {}: {}", check.check, check.detail));
        if let Some(fix) = check.fix.as_deref().filter(|fix| !fix.trim().is_empty()) {
            out.push_str(&format!("\n        {fix}"));
        }
    }
    out
}

// -------------------------------------------------------------------- the login entry

/// The program a login entry starts and the data folder it names (`--home`), as
/// `zone install` writes it: `"...\zonew.exe" serve --background [--port N] [--home "DIR"]`.
fn login_parts(command: &str) -> (Option<&str>, Option<&str>) {
    let program = command
        .trim_start()
        .strip_prefix('"')
        .and_then(|rest| rest.split_once('"'))
        .map(|(path, _)| path);
    let home = command
        .split_once(" --home \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(home, _)| home);
    (program, home)
}

fn same_folder(a: &str, b: &str) -> bool {
    a.trim_end_matches(['/', '\\']).eq_ignore_ascii_case(b.trim_end_matches(['/', '\\']))
}

fn folder_of(path: &str) -> &str {
    path.rsplit_once(['\\', '/']).map(|(folder, _)| folder).unwrap_or("")
}

/// How the login entry stands for this tray.
///
/// - `entry`: the text of the Run value named Zone, when there is one.
/// - `zone_home`: this tray's `ZONE_HOME`, when it has one.
/// - `folder`: the folder this tray (and its zone) is in.
/// - `turned_off`: Windows' own record says the person turned the entry off in Startup apps.
pub fn login_state(entry: Option<&str>, zone_home: Option<&str>, folder: &str, turned_off: bool) -> Login {
    let Some(entry) = entry.map(str::trim).filter(|entry| !entry.is_empty()) else {
        return Login::Off;
    };
    let (program, home) = login_parts(entry);
    let for_this_folder = match (home, zone_home) {
        (None, None) => true,
        (Some(a), Some(b)) => same_folder(a, b),
        _ => false,
    };
    if !for_this_folder {
        return Login::Other("set for another data folder".to_string());
    }
    if !program.is_some_and(|program| same_folder(folder_of(program), folder)) {
        return Login::Other("starts another copy of zone".to_string());
    }
    if turned_off {
        return Login::Other("turned off in Startup apps".to_string());
    }
    Login::On
}

// -------------------------------------------------------------------- words and numbers

/// 1764 becomes "1,764".
pub fn number(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// How long ago, in a few words: "just now", "5 min ago", "2 hr ago", "3 days ago".
pub fn ago(seconds: f64) -> String {
    let s = seconds.max(0.0);
    if s < 20.0 {
        "just now".to_string()
    } else if s < 60.0 {
        format!("{} sec ago", s as u64)
    } else if s < 3600.0 {
        format!("{} min ago", (s / 60.0) as u64)
    } else if s < 86400.0 {
        format!("{} hr ago", (s / 3600.0) as u64)
    } else {
        match (s / 86400.0) as u64 {
            1 => "yesterday".to_string(),
            days => format!("{days} days ago"),
        }
    }
}

/// How long something ran: "38 sec", "42 min", "1 hr 5 min".
pub fn length(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s < 60 {
        return format!("{s} sec");
    }
    let minutes = (s as f64 / 60.0).round() as u64;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

/// Days since a fixed day, for a date written `2026-11-19`.
fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');
    let (y, m, d): (i64, i64, i64) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // The usual count of days in the calendar we use, with the year starting in March.
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    Some(365 * y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400) + (153 * m + 2) / 5 + d)
}

/// When an item was said, from its local time as zone prints it (`2026-11-19T17:09:42-08:00`):
/// the time of day when that was today ("5:09 PM"), else "Yesterday", "Nov 11" or
/// "Nov 11, 2025". `today` is the day as `zone status` gives it.
pub fn when(created_local: &str, today: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (Some(day), Some(date)) = (day_number(created_local), created_local.get(..10)) else {
        return String::new();
    };
    let today_number = day_number(today);
    if today_number == Some(day) || today_number.is_none() {
        let time = created_local.get(11..16).and_then(|time| {
            let (hour, minute) = time.split_once(':')?;
            let hour: u32 = hour.parse().ok()?;
            let half = if hour < 12 { "AM" } else { "PM" };
            Some(format!("{}:{minute} {half}", if hour.is_multiple_of(12) { 12 } else { hour % 12 }))
        });
        if let (Some(time), Some(_)) = (time, today_number) {
            return time;
        }
    }
    if today_number == Some(day + 1) {
        return "Yesterday".to_string();
    }
    let month = date[5..7]
        .parse::<usize>()
        .ok()
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)))
        .copied()
        .unwrap_or("");
    let day_of_month = date[8..10].trim_start_matches('0');
    if today.get(..4) == date.get(..4) {
        format!("{month} {day_of_month}")
    } else {
        format!("{month} {day_of_month}, {}", &date[..4])
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether the tray was asked to close: `zone uninstall` writes the time it asked into
/// `tray.stop`, and a tray that started before that time closes. One that started after it
/// carries on, so nothing has to remember to delete the file (the same rule as `stop.request`).
pub fn quit_asked(file: Option<&str>, started_at: f64) -> bool {
    file.and_then(|text| text.trim().parse::<f64>().ok())
        .is_some_and(|asked| asked >= started_at)
}

/// Text on one line: line ends and tabs become spaces, runs of spaces one.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `limit` characters, ending in "..." when something was cut.
pub fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{
      "now": {"local": "Thu 2026-11-19 17:09:27 -08:00", "date": "2026-11-19"},
      "computer": {"name": "desk", "os": "windows", "zone_version": "0.1.4"},
      "daemon_running": true,
      "phones": [{"name": "Demo iPhone", "last_sync_age_s": 130.0, "items_received": 1768, "last_path": "direct"}],
      "last_sync": {"link": {"time": "x", "age_s": 130.0}, "flash": null, "backup_folder": null},
      "items": {"total": 1768},
      "data_directory": "C:\\Users\\Zed\\AppData\\Local\\Zone",
      "agents": {"agents": "on", "enabled": true},
      "daemon": {"running": true, "pid": 4, "version": "0.1.4", "relay_connected": true},
      "something_new": 7
    }"#;

    fn item(id: &str, kind: &str, source: &str, at: f64, local: &str, text: &str) -> Item {
        Item {
            id: id.into(),
            kind: kind.into(),
            source: Some(source.into()),
            created_at: at,
            created_local: local.into(),
            text: text.into(),
            title: None,
            duration_s: Some(4.0),
        }
    }

    fn snapshot() -> Snapshot {
        let mut meeting = item("M1", "meeting", "app", 900.0, "2026-11-18T10:00:00-08:00", "A: Hello.");
        meeting.title = Some("Launch review".into());
        meeting.duration_s = Some(2520.0);
        Snapshot {
            fetched_at: 1000.0,
            status: Some(serde_json::from_str(STATUS).unwrap()),
            latest: vec![
                item(
                    "C1",
                    "clip",
                    "clipboard",
                    999.0,
                    "2026-11-19T17:09:42-08:00",
                    "https://example.com/the-link",
                ),
                item(
                    "D1",
                    "dictation",
                    "keyboard",
                    998.0,
                    "2026-11-19T17:05:27-08:00",
                    "Running ten minutes late,\nstart without me.",
                ),
                item("N1", "note", "app", 990.0, "2026-11-19T09:12:00-08:00", "Groceries: oat milk & lemons"),
                item(
                    "R1",
                    "dictation",
                    "note",
                    986.0,
                    "2026-11-19T09:11:56-08:00",
                    "Groceries: oat milk & lemons",
                ),
                meeting.clone(),
                item("D2", "dictation", "app", 100.0, "2025-12-31T23:59:00-08:00", "Happy new year."),
            ],
            meeting: Some(meeting),
            clip: Some(Clip {
                id: "C1".into(),
                created_local: "2026-11-19T17:09:42-08:00".into(),
                first_line: "https://example.com/the-link".into(),
                text: "https://example.com/the-link".into(),
            }),
            clipboard: Some(ClipboardSetting {
                enabled: false,
                available: true,
            }),
            apps: Some(vec![]),
            login: Login::On,
            update: Update::NotAsked,
            receiving_until: 0.0,
            tray_version: "0.1.4".into(),
            zone_version: Some("0.1.4".into()),
            trouble: None,
            working: None,
        }
    }

    #[test]
    fn zones_json_is_read_and_what_is_new_in_it_is_passed_over() {
        let status: Status = serde_json::from_str(STATUS).unwrap();
        assert!(status.daemon_running && status.agents.unwrap().enabled);
        assert_eq!((status.phones[0].name.as_str(), status.phones[0].items_received), ("Demo iPhone", 1768));
        // An older zone says less: nothing it leaves out stops the reading.
        let bare: Status = serde_json::from_str("{}").unwrap();
        assert!(!bare.daemon_running && bare.agents.is_none() && bare.phones.is_empty());
        let none: ClipAnswer = serde_json::from_str(r#"{"clip": null}"#).unwrap();
        assert!(none.clip.is_none());
    }

    #[test]
    fn the_icon_has_four_faces() {
        let mut s = snapshot();
        assert_eq!(state(&s, 1000.0), State::Running);
        s.receiving_until = 1004.0;
        assert_eq!(state(&s, 1001.0), State::Receiving);
        assert_eq!(state(&s, 1005.0), State::Running);
        s.status.as_mut().unwrap().daemon_running = false;
        // Not running wins over receiving: nothing can arrive.
        assert_eq!(state(&s, 1001.0), State::Stopped);
        s.status = None;
        assert_eq!(state(&s, 1001.0), State::Attention);
    }

    #[test]
    fn the_line_says_who_synced_and_when_and_counts_on_between_askings() {
        let mut s = snapshot();
        assert_eq!(status_line(&s, 1000.0), "Demo iPhone, synced 2 min ago");
        assert_eq!(tooltip(&s, 1000.0), "Zone: synced 2 min ago");
        // Four minutes after zone was asked, with nothing asked since.
        assert_eq!(tooltip(&s, 1240.0), "Zone: synced 6 min ago");
        s.receiving_until = 1003.0;
        assert_eq!(
            (status_line(&s, 1001.0).as_str(), tooltip(&s, 1001.0).as_str()),
            ("Receiving", "Zone: receiving")
        );
        s.receiving_until = 0.0;

        // A sync that came another way says so, when it is the newer one.
        s.status.as_mut().unwrap().last_sync.flash = Some(Synced { age_s: Some(30.0) });
        assert_eq!(status_line(&s, 1000.0), "Synced by Flash 30 sec ago");
        assert_eq!(tooltip(&s, 1000.0), "Zone: synced by Flash 30 sec ago");
        s.status.as_mut().unwrap().last_sync = LastSync::default();
        s.status.as_mut().unwrap().phones[0].last_sync_age_s = None;
        assert_eq!(status_line(&s, 1000.0), "Demo iPhone, waiting for its first sync");
        assert_eq!(tooltip(&s, 1000.0), "Zone: waiting for the first sync");
        s.status.as_mut().unwrap().phones.clear();
        assert_eq!(
            (status_line(&s, 1000.0).as_str(), tooltip(&s, 1000.0).as_str()),
            ("Ready to pair", "Zone: ready to pair")
        );

        s.status.as_mut().unwrap().daemon_running = false;
        assert_eq!(
            (status_line(&s, 1000.0).as_str(), tooltip(&s, 1000.0).as_str()),
            ("Zone is not running", "Zone: not running")
        );
        s.status = None;
        s.trouble = Some("zone.exe is not beside the tray".into());
        assert_eq!(status_line(&s, 1000.0), "zone.exe is not beside the tray");
        assert_eq!(tooltip(&s, 1000.0), "Zone needs attention: zone.exe is not beside the tray");
    }

    #[test]
    fn nothing_a_person_said_is_in_the_tooltip() {
        let s = snapshot();
        for now in [1000.0, 1001.0, 5000.0] {
            let tip = tooltip(&s, now);
            assert!(tip.chars().count() <= 127);
            for item in &s.latest {
                assert!(!tip.contains(&item.text) && !tip.contains("example.com"), "{tip}");
            }
        }
        let mut long = snapshot();
        long.status = None;
        long.trouble = Some("x".repeat(400));
        assert_eq!(tooltip(&long, 0.0).chars().count(), 127);
    }

    #[test]
    fn the_menu_reads_top_to_bottom() {
        let s = snapshot();
        let text = menu_text(&menu(&s, 1000.0));
        let expected = "\
Demo iPhone, synced 2 min ago  (to read)
1,768 items from Demo iPhone, straight over this network  (to read)
----
Copy clip: https://example.com/the-link  |  5:09 PM
Latest  (gray)
Dictation: Running ten minutes late, start without me.  |  5:05 PM
Note: Groceries: oat milk & lemons  |  9:12 AM
Dictation: Happy new year.  |  Dec 31, 2025
Last meeting: Launch review  |  Yesterday, 42 min
----
Open Zone
Flash
Pair an iPhone
Agents >
    [x] Let AI agents read my library
    Agent settings
----
Copy what I send from my iPhone
[x] Start at login
Developer >
    Copy MCP settings
    Connected apps >
        No AI apps found on this computer  (to read)
        Agent settings
    ----
    Open the log
    Open the data folder
    Run zone doctor
    ----
    Restart zone
    Stop zone
    ----
    Zone 0.1.4  (to read)
    Check for updates
    Open on GitHub
----
Quit
";
        assert_eq!(text, expected);
    }

    #[test]
    fn choosing_a_row_copies_the_words_as_they_were_said() {
        let s = snapshot();
        let entries = menu(&s, 1000.0);
        let action_of = |wanted: &str| {
            entries.iter().find_map(|entry| match entry {
                Entry::Line { label, action, .. } if label.starts_with(wanted) => action.clone(),
                _ => None,
            })
        };
        // The line is flattened and may be cut short. What is copied is not.
        assert_eq!(
            action_of("Dictation: Running"),
            Some(Action::Copy("Running ten minutes late,\nstart without me.".into()))
        );
        assert_eq!(action_of("Copy clip"), Some(Action::Copy("https://example.com/the-link".into())));
        assert_eq!(action_of("Last meeting"), Some(Action::OpenPage(Route::Meeting("M1".into()))));
        assert_eq!(action_of("Start at login"), Some(Action::SetLogin(false)));
        assert_eq!(action_of("Copy what I send"), Some(Action::SetClipboard(true)));
        // The recording behind the note, the clip and the meeting are not listed a second time.
        let rows: Vec<&str> = latest_rows(&s).iter().map(|item| item.id.as_str()).collect();
        assert_eq!(rows, ["D1", "N1", "D2"]);
    }

    #[test]
    fn a_stopped_zone_offers_to_start_and_nothing_that_needs_the_page() {
        let mut s = snapshot();
        s.status.as_mut().unwrap().daemon_running = false;
        let text = menu_text(&menu(&s, 1000.0));
        assert!(text.starts_with("Zone is not running  (to read)\nStart zone\n"), "{text}");
        for line in [
            "Open Zone  (gray)",
            "Flash  (gray)",
            "Pair an iPhone  (gray)",
            "    Agent settings  (gray)",
            "Last meeting: Launch review  |  Yesterday, 42 min  (gray)",
        ] {
            assert!(text.contains(line), "{line} in {text}");
        }
        assert!(text.contains("    Start zone\n") && !text.contains("Restart zone") && !text.contains("Stop zone"));
        // What was said can still be copied: it is already here.
        assert!(text.contains("Dictation: Running ten minutes late, start without me.  |  5:05 PM\n"));

        // zone itself cannot be asked: the reason leads, and the tools to find out stay.
        s.status = None;
        s.trouble = Some("zone.exe is not beside the tray".into());
        let text = menu_text(&menu(&s, 1000.0));
        assert!(text.starts_with("zone.exe is not beside the tray  (to read)\n----\n"), "{text}");
        assert!(text.contains("Open the log") && text.contains("Run zone doctor") && text.ends_with("----\nQuit\n"));
    }

    #[test]
    fn updates_and_versions_say_what_is_known() {
        let mut s = snapshot();
        s.update = Update::Newer {
            latest: "0.1.5".into(),
            update_with: "zone update".into(),
        };
        assert!(menu_text(&menu(&s, 0.0)).contains("    Update now to 0.1.5\n    Check for updates\n"));
        s.update = Update::Latest;
        assert!(menu_text(&menu(&s, 0.0)).contains("    Up to date. Check again\n"));
        s.update = Update::Unknown("no answer".into());
        assert!(menu_text(&menu(&s, 0.0)).contains("    Could not check for updates. Try again\n"));

        let answer = |json: &str| Update::from_answer(&serde_json::from_str(json).unwrap());
        assert_eq!(
            answer(r#"{"current":"0.1.3","latest":"0.1.5","newer":true,"checked":true,"update_with":"zone update"}"#),
            Update::Newer {
                latest: "0.1.5".into(),
                update_with: "zone update".into()
            }
        );
        assert_eq!(
            answer(r#"{"current":"0.1.3","latest":"0.1.3","newer":false,"checked":true}"#),
            Update::Latest
        );
        assert_eq!(
            answer(r#"{"checked":false,"note":"GitHub did not answer"}"#),
            Update::Unknown("GitHub did not answer".into())
        );

        // The zone that runs is named only when it is another version than the one here.
        s.zone_version = Some("zone 0.1.5".trim_start_matches("zone ").into());
        assert_eq!(versions_line(&s), "Zone 0.1.4, with zone 0.1.5. Running: zone 0.1.4");
        s.zone_version = Some("0.1.4".into());
        s.status.as_mut().unwrap().daemon.version = Some("0.1.3".into());
        assert_eq!(versions_line(&s), "Zone 0.1.4. Running: zone 0.1.3");
    }

    #[test]
    fn connected_apps_are_named_once_each() {
        let client = |client: &str, label: &str, installed: bool, state: &str| Client {
            client: client.into(),
            label: label.into(),
            installed,
            state: state.into(),
        };
        let lines = app_lines(&[
            client("cursor", "Cursor", true, "not_registered"),
            client("claude-code", "Claude Code (default)", true, "registered"),
            client("claude-code", "Claude Code (~/.claude-work)", true, "registered_read_only"),
            client("codex", "Codex", true, "other_copy"),
            client("zed", "Zed", false, "not_registered"),
            client("vscode", "VS Code", true, "removed_by_you"),
        ]);
        assert_eq!(
            lines,
            [
                "Claude Code: Connected, 2 profiles",
                "Codex: Starts another copy of zone",
                "Cursor: Not connected",
                "VS Code: Removed by you"
            ]
        );
    }

    #[test]
    fn the_login_entry_is_this_trays_or_says_why_not() {
        let folder = r"C:\Users\Zed\AppData\Local\Zone\bin";
        let entry = r#""C:\Users\Zed\AppData\Local\Zone\bin\zonew.exe" serve --background"#;
        assert_eq!(login_state(None, None, folder, false), Login::Off);
        assert_eq!(login_state(Some("  "), None, folder, false), Login::Off);
        assert_eq!(login_state(Some(entry), None, folder, false), Login::On);
        assert_eq!(login_state(Some(entry), None, r"c:\users\zed\appdata\local\zone\BIN\", false), Login::On);
        assert_eq!(
            login_state(Some(entry), None, folder, true),
            Login::Other("turned off in Startup apps".into())
        );
        assert_eq!(
            login_state(Some(entry), Some(r"D:\test"), folder, false),
            Login::Other("set for another data folder".into())
        );
        assert_eq!(
            login_state(Some(entry), None, r"D:\elsewhere", false),
            Login::Other("starts another copy of zone".into())
        );
        let with_home = r#""D:\z\zonew.exe" serve --background --port 47913 --home "D:\zone data""#;
        assert_eq!(login_state(Some(with_home), Some(r"D:\zone data\"), r"D:\z", false), Login::On);
        assert_eq!(
            login_state(Some(with_home), None, r"D:\z", false),
            Login::Other("set for another data folder".into())
        );
    }

    #[test]
    fn events_that_brought_something_light_the_icon() {
        let event = |json: &str| serde_json::from_str::<Event>(json).unwrap();
        assert!(event(r#"{"seq":10,"event":"clip","length":42}"#).brought_items());
        assert!(event(r#"{"seq":9,"event":"link_session","items":67,"new":67,"changed":0}"#).brought_items());
        assert!(!event(r#"{"seq":9,"event":"link_session","items":3,"new":0,"changed":0}"#).brought_items());
        assert!(!event(r#"{"seq":2,"event":"relay_connected"}"#).brought_items());
        assert!(!event(r#"{"seq":3,"event":"agents_switched","on":false}"#).brought_items());
    }

    #[test]
    fn work_that_takes_a_while_is_said_and_the_tray_closes_when_asked() {
        let mut s = snapshot();
        s.working = Some("Updating zone".into());
        assert_eq!(
            (status_line(&s, 1000.0).as_str(), tooltip(&s, 1000.0).as_str()),
            ("Updating zone", "Zone: updating zone")
        );
        assert!(!quit_asked(None, 100.0) && !quit_asked(Some("soon"), 100.0));
        assert!(quit_asked(Some("150.5\n"), 100.0) && !quit_asked(Some("50"), 100.0));
    }

    #[test]
    fn pages_are_opened_at_a_place() {
        let page = "http://127.0.0.1:47813/?t=abc\n";
        assert_eq!(Route::Home.url(page), "http://127.0.0.1:47813/?t=abc");
        assert_eq!(Route::Pair.url(page), "http://127.0.0.1:47813/?t=abc#/pair");
        assert_eq!(Route::Settings.url(page), "http://127.0.0.1:47813/?t=abc#/settings");
        assert_eq!(
            Route::Meeting("A B/1".into()).url(page),
            "http://127.0.0.1:47813/?t=abc#/meeting/A%20B%2F1"
        );
    }

    #[test]
    fn times_and_numbers_read_as_a_person_says_them() {
        assert_eq!(
            [ago(3.0), ago(45.0), ago(130.0), ago(7300.0), ago(90000.0), ago(260000.0)],
            ["just now", "45 sec ago", "2 min ago", "2 hr ago", "yesterday", "3 days ago"]
        );
        assert_eq!(
            [length(38.0), length(2520.0), length(3900.0), length(7200.0)],
            ["38 sec", "42 min", "1 hr 5 min", "2 hr"]
        );
        assert_eq!([number(7), number(1764), number(1000000)], ["7", "1,764", "1,000,000"]);
        let today = "2026-03-01";
        assert_eq!(when("2026-03-01T00:05:00-08:00", today), "12:05 AM");
        assert_eq!(when("2026-03-01T12:00:09+01:00", today), "12:00 PM");
        assert_eq!(when("2026-02-28T23:59:00-08:00", today), "Yesterday");
        assert_eq!(when("2026-02-27T08:00:00-08:00", today), "Feb 27");
        assert_eq!(when("2025-11-09T08:00:00-08:00", today), "Nov 9, 2025");
        assert_eq!(when("2024-02-29T08:00:00Z", "2024-03-01"), "Yesterday");
        assert_eq!(when("nonsense", today), "");
        assert_eq!(shorten("abcdefghij", 8), "abcde...");
        assert_eq!(shorten("abc", 8), "abc");
        assert_eq!(one_line(" a\tb\r\n c  d "), "a b c d");
    }

    #[test]
    fn doctors_answer_is_a_few_lines() {
        let doctor: Doctor = serde_json::from_str(
            r#"{"healthy": true, "checks": [
                {"check": "Data folder", "result": "ok", "detail": "C:\\Zone", "fix": null},
                {"check": "Start at login", "result": "warn", "detail": "not set", "fix": "Run `zone install`."}]}"#,
        )
        .unwrap();
        assert_eq!(
            doctor_text(&doctor),
            "zone works. One thing to know:\n\nok   Data folder: C:\\Zone\nnote   Start at login: not set\n        Run `zone install`."
        );
        assert!(
            doctor_text(&Doctor {
                healthy: true,
                checks: vec![]
            })
            .starts_with("Everything looks right.")
        );
    }
}
