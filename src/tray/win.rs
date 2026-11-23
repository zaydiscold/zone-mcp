//! The part of the tray that talks to Windows: one hidden window, the icon beside the clock,
//! the menu, and the few things a menu line does. What is shown is decided in `model`.
//!
//! Three threads besides the window's own: one asks zone again (every half minute, and at
//! once when something happened), one reads `zone events --json --follow`, and each menu line
//! that takes a while runs on a thread of its own, so the icon never stops answering.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, HICON, HMENU, IDYES, KillTimer, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
    MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MF_CHECKED, MF_DISABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SW_SHOWNORMAL, SetForegroundWindow, SetTimer,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_NULL,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
};

use super::handover::{self, Step, Watch};
use super::ico::{self, Beat, Taskbar};
use super::model::{self, Action, Doctor, Entry, Event, Login, Route, Snapshot, State, Update, UpdateAnswer};
use super::zone::{Depth, Zone, refresh};

/// The window's class. One hidden window per tray.
const CLASS: &str = "ZoneTrayWindow";
/// What the icon tells the window: a click, a key, the menu being asked for.
const WM_TRAY: u32 = WM_APP + 1;
/// What the other threads tell the window: there is something new to show.
const WM_NEWS: u32 = WM_APP + 2;
const TIMER: usize = 1;
/// The window looks at the clock this often: the ring of "receiving" moves, the minutes in
/// the line count on, and every few looks it checks whether it should close or hand over.
const TICK: Duration = Duration::from_millis(500);
const ICON_ID: u32 = 1;
const REPO_PAGE: &str = "https://github.com/zaydiscold/zone-mcp";

const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const RUN_VALUE: &str = "Zone";

struct App {
    /// The hidden window, as a number so other threads can post to it.
    window: usize,
    zone: Zone,
    program: PathBuf,
    data_dir: PathBuf,
    started_at: f64,
    snapshot: Mutex<Snapshot>,
    ask: Mutex<Sender<Depth>>,
    /// Things in flight that must finish before the tray hands over to a new version: an
    /// update the tray started is still running the installer.
    busy: AtomicUsize,
    closing: AtomicBool,
    /// Held for as long as this tray runs: one tray per data folder.
    _lock: File,
}

static APP: OnceLock<Arc<App>> = OnceLock::new();

fn app() -> &'static Arc<App> {
    APP.get().expect("the tray is set up before its window gets messages")
}

/// What only the window's own thread touches.
struct Face {
    added: bool,
    shown: Option<(State, Taskbar, Beat, i32)>,
    tip: String,
    icons: HashMap<(State, Taskbar, Beat, i32), HICON>,
    taskbar: Taskbar,
    ticks: u64,
    watch: Watch,
    last_click: Option<Instant>,
    taskbar_created: u32,
}

thread_local! {
    static FACE: std::cell::RefCell<Option<Face>> = const { std::cell::RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// -------------------------------------------------------------------- where things are

/// The data folder: `ZONE_HOME`, else `%LOCALAPPDATA%\Zone`, as zone itself has it.
fn data_dir() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    env("ZONE_HOME")
        .or_else(|| env("LOCALAPPDATA").map(|local| local.join("Zone")))
        .or_else(|| env("USERPROFILE").map(|home| home.join("AppData").join("Local").join("Zone")))
}

/// This program's path as people write it, without the `\\?\` Windows puts before it.
fn current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.to_string_lossy();
    Some(match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => exe,
    })
}

/// The login entry, as the registry has it. The tests keep theirs under another key, as zone
/// itself does then.
pub fn login() -> Login {
    let other = std::env::var("ZONE_WINDOWS_RUN_KEY").ok().filter(|key| !key.trim().is_empty());
    let (run, approved) = match &other {
        Some(key) => (key.clone(), format!(r"{key}\StartupApproved")),
        None => (RUN_SUBKEY.to_string(), STARTUP_APPROVED_SUBKEY.to_string()),
    };
    let entry = crate::win::reg_get_string(&run, RUN_VALUE);
    // Explorer's flags: the first byte is odd when the person turned the entry off.
    let turned_off = crate::win::reg_get_binary(&approved, RUN_VALUE).is_some_and(|flags| flags.first().is_some_and(|first| first & 1 == 1));
    let home = std::env::var("ZONE_HOME").ok().filter(|home| !home.is_empty());
    let folder = current_exe()
        .and_then(|exe| exe.parent().map(|folder| folder.to_string_lossy().to_string()))
        .unwrap_or_default();
    model::login_state(entry.as_deref(), home.as_deref(), &folder, turned_off)
}

/// Light when Windows is set to a light taskbar ("Choose your default Windows mode").
fn taskbar_color() -> Taskbar {
    match crate::win::reg_get_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "SystemUsesLightTheme") {
        Some(1) => Taskbar::Light,
        _ => Taskbar::Dark,
    }
}

fn taskbar_exists() -> bool {
    let class = wide("Shell_TrayWnd");
    // SAFETY: a class name that ends in a zero; nothing is written.
    !unsafe { FindWindowW(class.as_ptr(), null()) }.is_null()
}

// -------------------------------------------------------------------- telling and opening

/// A small window with a few lines and OK. Only ever the answer to something the person just
/// chose in the menu: the tray never puts a window up by itself.
fn tell(text: &str, warning: bool) {
    let (text, caption) = (wide(text), wide("Zone"));
    let style = MB_OK | MB_SETFOREGROUND | MB_TOPMOST | if warning { MB_ICONWARNING } else { MB_ICONINFORMATION };
    // SAFETY: both strings end in a zero. No owner window: this runs on a thread of its own.
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), caption.as_ptr(), style) };
}

fn ask_yes(text: &str) -> bool {
    let (text, caption) = (wide(text), wide("Zone"));
    // SAFETY: as in `tell`.
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_YESNO | MB_SETFOREGROUND | MB_TOPMOST | MB_ICONINFORMATION,
        ) == IDYES
    }
}

/// Opens a link, a file or a folder with whatever Windows opens it with.
fn open(target: &str) -> bool {
    let (verb, target) = (wide("open"), wide(target));
    // SAFETY: both strings end in a zero. The answer is above 32 when it worked.
    unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL) as usize > 32 }
}

fn copy(text: &str) {
    if let Err(error) = crate::win::set_clipboard_text(text) {
        tell(&format!("Could not put that on the clipboard: {error}"), true);
    }
}

// -------------------------------------------------------------------- asking zone

impl App {
    fn post(&self, message: u32) {
        // SAFETY: posting to this program's own window, which outlives every thread.
        unsafe { PostMessageW(self.window as HWND, message, 0, 0) };
    }

    fn ask(&self, depth: Depth) {
        if let Ok(ask) = self.ask.lock() {
            let _ = ask.send(depth);
        }
    }

    fn change(&self, change: impl FnOnce(&mut Snapshot)) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            change(&mut snapshot);
        }
        self.post(WM_NEWS);
    }

    fn picture(&self) -> Snapshot {
        self.snapshot.lock().map(|snapshot| snapshot.clone()).unwrap_or_default()
    }
}

/// Asks zone again whenever asked to, and every half minute.
fn keep_asking(app: Arc<App>, asked: mpsc::Receiver<Depth>) {
    loop {
        let mut depth = match asked.recv_timeout(Duration::from_secs(30)) {
            Ok(depth) => depth,
            Err(RecvTimeoutError::Timeout) => Depth::Status,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Several things at once (a sync is a few events) are one asking.
        std::thread::sleep(Duration::from_millis(250));
        while let Ok(more) = asked.try_recv() {
            if more == Depth::Everything {
                depth = more;
            }
        }
        let before = app.picture();
        let mut next = refresh(&app.zone, &before, depth, now_unix(), &login);
        app.change(move |snapshot| {
            // What the other threads own is theirs: it may have changed while zone was asked.
            next.receiving_until = snapshot.receiving_until;
            next.update = snapshot.update.clone();
            next.working = snapshot.working.clone();
            *snapshot = next;
        });
    }
}

/// A job that ends what is in it when the tray ends, however the tray ends. Only the events
/// reader goes in: it would otherwise wait for the next event before noticing nobody reads.
fn job_that_ends_with_the_tray() -> usize {
    // SAFETY: a job with no name and default security; the handle is kept for the program's
    // whole life on purpose, and Windows closes it when the program ends.
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return 0;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        job as usize
    }
}

/// Reads what happens as it happens. Something arriving lights the icon for a few seconds,
/// and anything at all is a reason to ask zone again.
fn follow_events(app: Arc<App>) {
    let job = job_that_ends_with_the_tray();
    loop {
        if app.closing.load(Ordering::Relaxed) {
            return;
        }
        let begun = Instant::now();
        if let Ok(mut child) = app.zone.follow_events() {
            if job != 0 {
                // SAFETY: the job made above and the process just started, both open.
                unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
            }
            if let Some(output) = child.stdout.take() {
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    let Ok(event) = serde_json::from_str::<Event>(&line) else { continue };
                    if event.brought_items() {
                        app.change(|snapshot| snapshot.receiving_until = now_unix() + 4.0);
                    }
                    app.ask(Depth::Everything);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        // It ended: zone is missing, or was replaced by an update. Not in a tight loop.
        std::thread::sleep(if begun.elapsed() > Duration::from_secs(60) {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(15)
        });
    }
}

// -------------------------------------------------------------------- what a menu line does

/// The page's link with its key, pointed at a place.
fn open_page(app: &App, route: &Route, flash: bool) {
    let args: &[&str] = if flash { &["open", "--flash", "--print"] } else { &["open", "--print"] };
    match app.zone.run(args) {
        Ok(page) => {
            if !open(&route.url(&page)) {
                tell("The browser did not open. `zone open --print` prints the link to paste yourself.", true);
            }
        }
        Err(why) => tell(&why, true),
    }
}

/// Runs a zone command whose answer is only worth showing when it failed.
fn quietly(app: &App, args: &[&str]) -> bool {
    match app.zone.run(args) {
        Ok(_) => true,
        Err(why) => {
            tell(&why, true);
            false
        }
    }
}

fn check_for_updates(app: &Arc<App>) {
    let answer = match app.zone.json_whatever::<UpdateAnswer>(&["update", "--check", "--json"]) {
        Ok(answer) => answer,
        Err(why) => {
            app.change(|snapshot| snapshot.update = Update::Unknown(why.clone()));
            return tell(&format!("Could not find out whether a newer zone exists: {why}."), true);
        }
    };
    let update = Update::from_answer(&answer);
    app.change(|snapshot| snapshot.update = update.clone());
    match &update {
        Update::Newer { latest, .. } => {
            if ask_yes(&format!("zone {latest} is out. This is {}.\n\nUpdate now?", answer.current)) {
                update_now(app);
            }
        }
        Update::Latest => tell(&format!("zone {} is the latest.", answer.current), false),
        Update::Unknown(why) => tell(&format!("Could not find out whether a newer zone exists: {why}."), true),
        Update::NotAsked => {}
    }
}

/// `zone update`: the install script downloads the release, checks it and swaps the programs,
/// this one included. The tray then notices its own file changed and hands over.
fn update_now(app: &Arc<App>) {
    if let Update::Newer { update_with, .. } = &app.picture().update
        && update_with != "zone update"
    {
        return tell(
            &format!("This copy of zone was not put here by the install script, so zone will not replace it.\n\nTo update: {update_with}"),
            false,
        );
    }
    app.busy.fetch_add(1, Ordering::SeqCst);
    app.change(|snapshot| snapshot.working = Some("Updating zone".to_string()));
    let result = app.zone.run(&["update"]);
    app.change(|snapshot| {
        snapshot.working = None;
        if result.is_ok() {
            snapshot.update = Update::NotAsked;
        }
    });
    app.busy.fetch_sub(1, Ordering::SeqCst);
    if let Err(why) = result {
        tell(&format!("The update did not finish. The zone you have is unchanged.\n\n{why}"), true);
    }
    app.ask(Depth::Everything);
}

fn perform(action: Action) {
    let app = app().clone();
    // Copying is instant and Quit is the window's own business. The rest may take a moment
    // (zone is started, GitHub is asked), so each runs beside the window, not in it.
    match action {
        Action::Copy(text) => return copy(&text),
        Action::Quit => {
            // SAFETY: this program's own window, closed from its own thread.
            unsafe { PostMessageW(app.window as HWND, WM_CLOSE, 0, 0) };
            return;
        }
        _ => {}
    }
    std::thread::spawn(move || {
        match action {
            Action::Copy(_) | Action::Quit => {}
            Action::OpenPage(route) => open_page(&app, &route, false),
            Action::OpenFlash => open_page(&app, &Route::Home, true),
            Action::StartZone => {
                quietly(&app, &["start"]);
            }
            Action::StopZone => {
                quietly(&app, &["stop"]);
            }
            Action::RestartZone => {
                if quietly(&app, &["stop"]) {
                    quietly(&app, &["start"]);
                }
            }
            Action::SetAgents(on) => {
                quietly(&app, &["agents", if on { "on" } else { "off" }]);
            }
            Action::SetClipboard(on) => {
                quietly(&app, &["clipboard", if on { "on" } else { "off" }]);
            }
            Action::SetLogin(on) => {
                // The login entry and nothing else: AI clients are left as they are.
                quietly(
                    &app,
                    if on {
                        &["install", "--only-login-item"]
                    } else {
                        &["uninstall", "--only-login-item"]
                    },
                );
            }
            Action::CopyMcpSettings => match app.zone.run(&["mcp", "config", "--client", "other"]) {
                Ok(settings) => {
                    if let Err(error) = crate::win::set_clipboard_text(&settings) {
                        tell(&format!("Could not copy the MCP settings: {error}"), true);
                        return;
                    }
                    tell(
                        "Copied. Paste it into the MCP settings of the app that should use zone.\n\n`zone mcp config --client <name>` prints one app's own format.",
                        false,
                    );
                }
                Err(why) => tell(&why, true),
            },
            Action::OpenLog => {
                let log = app.data_dir.join("zone.log");
                if !log.is_file() {
                    tell(&format!("There is no log yet. zone writes it once it has run: {}", log.display()), false);
                } else if !open(&log.to_string_lossy()) {
                    tell(&format!("Could not open {}", log.display()), true);
                }
            }
            Action::OpenDataFolder => {
                if !open(&app.data_dir.to_string_lossy()) {
                    tell(&format!("Could not open {}", app.data_dir.display()), true);
                }
            }
            Action::RunDoctor => match app.zone.json_whatever::<Doctor>(&["doctor", "--json"]) {
                Ok(doctor) => tell(&model::doctor_text(&doctor), !doctor.healthy),
                Err(why) => tell(&why, true),
            },
            Action::CheckForUpdates => check_for_updates(&app),
            Action::UpdateNow => update_now(&app),
            Action::OpenGitHub => {
                open(REPO_PAGE);
            }
        }
        app.ask(Depth::Everything);
    });
}

// -------------------------------------------------------------------- the menu

/// Text for a menu line: an `&` would underline the next letter, and a tab starts the column
/// at the right edge.
fn menu_label(label: &str, aside: Option<&str>) -> Vec<u16> {
    let clean = |text: &str| text.replace('&', "&&").replace('\t', " ");
    match aside.filter(|aside| !aside.is_empty()) {
        Some(aside) => wide(&format!("{}\t{}", clean(label), clean(aside))),
        None => wide(&clean(label)),
    }
}

/// Builds the Windows menu. `actions` collects what each line does: a line's number is its
/// place in that list, plus one.
fn build_menu(entries: &[Entry], actions: &mut Vec<Action>) -> HMENU {
    // SAFETY: a new, empty menu. Every string given to it ends in a zero and is copied by it.
    let menu = unsafe { CreatePopupMenu() };
    for entry in entries {
        match entry {
            Entry::Separator => unsafe {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
            },
            Entry::Submenu { label, entries } => {
                let sub = build_menu(entries, actions);
                let label = menu_label(label, None);
                // SAFETY: the submenu belongs to this menu from here on and is freed with it.
                unsafe { AppendMenuW(menu, MF_POPUP | MF_STRING, sub as usize, label.as_ptr()) };
            }
            Entry::Line {
                label,
                aside,
                action,
                checked,
                dimmed,
            } => {
                let text = menu_label(label, aside.as_deref());
                let mut flags = MF_STRING;
                if *checked {
                    flags |= MF_CHECKED;
                }
                let id = match action {
                    Some(action) => {
                        actions.push(action.clone());
                        actions.len()
                    }
                    None => {
                        // A fact stays black and cannot be chosen. A heading is gray.
                        flags |= if *dimmed { MF_GRAYED } else { MF_DISABLED };
                        0
                    }
                };
                // SAFETY: as above.
                unsafe { AppendMenuW(menu, flags, id, text.as_ptr()) };
            }
        }
    }
    menu
}

fn show_menu(window: HWND, x: i32, y: i32) {
    let now = now_unix();
    let entries = model::menu(&app().picture(), now);
    let mut actions = Vec::new();
    let menu = build_menu(&entries, &mut actions);
    // The window comes to the front first, and is sent an empty message after: without the
    // two, a menu opened from the tray does not close when the person clicks elsewhere.
    // SAFETY: this thread's own window and the menu just built, which is freed here.
    let chosen = unsafe {
        SetForegroundWindow(window);
        let chosen = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, x, y, window, null());
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen
    };
    if chosen > 0
        && let Some(action) = actions.into_iter().nth(chosen as usize - 1)
    {
        perform(action);
    }
    // The menu showed what was known. Now find out what is.
    app().ask(Depth::Everything);
}

// -------------------------------------------------------------------- the icon

fn icon_side() -> i32 {
    // SAFETY: plain questions about the screen.
    let side = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
    if side > 0 { side } else { 16 }
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    // SAFETY: a plain structure of numbers and handles, all of which may be zero.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = ICON_ID;
    data
}

impl Face {
    fn icon(&mut self, key: (State, Taskbar, Beat, i32)) -> HICON {
        if let Some(icon) = self.icons.get(&key) {
            return *icon;
        }
        let (state, taskbar, beat, side) = key;
        let icon = match ico::best(ico::file(state, taskbar, beat), side as u32) {
            // SAFETY: the bytes are one whole picture from an icon file built into the
            // program. 0x00030000 is the format every Windows since 3.0 reads.
            Some(picture) => unsafe {
                CreateIconFromResourceEx(picture.bytes.as_ptr(), picture.bytes.len() as u32, 1, 0x0003_0000, side, side, 0)
            },
            None => null_mut(),
        };
        self.icons.insert(key, icon);
        icon
    }

    /// Shows what is true now, if it is not what is shown already.
    fn show(&mut self, window: HWND) {
        let now = now_unix();
        let snapshot = app().picture();
        let state = model::state(&snapshot, now);
        let beat = if state == State::Receiving && self.ticks % 2 == 1 {
            Beat::Second
        } else {
            Beat::First
        };
        let key = (state, self.taskbar, beat, icon_side());
        let tip = model::tooltip(&snapshot, now);
        if self.added && self.shown == Some(key) && self.tip == tip {
            return;
        }
        let mut data = icon_data(window);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icon(key);
        for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *slot = unit;
        }
        // SAFETY: `data` is whole and names this thread's window.
        unsafe {
            if self.added {
                Shell_NotifyIconW(NIM_MODIFY, &data);
            } else if Shell_NotifyIconW(NIM_ADD, &data) != 0 {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &data);
                self.added = true;
            }
        }
        if self.added {
            self.shown = Some(key);
            self.tip = tip;
        }
    }

    fn remove(&mut self, window: HWND) {
        if self.added {
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data(window)) };
            self.added = false;
        }
    }
}

/// Starts the program that took this one's place, and closes. The new tray waits for this
/// one to let go of the lock.
fn hand_over(window: HWND) {
    let app = app();
    let mut command = Command::new(&app.program);
    command.arg("--handover").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if command.spawn().is_ok() {
        // SAFETY: this thread's own window.
        unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    }
}

fn on_tick(window: HWND, face: &mut Face) {
    face.ticks += 1;
    face.show(window);
    let app = app();
    // Every two seconds: was the tray asked to close (`zone uninstall`)?
    if face.ticks.is_multiple_of(4) {
        let asked = std::fs::read_to_string(app.data_dir.join("tray.stop")).ok();
        if model::quit_asked(asked.as_deref(), app.started_at) {
            // SAFETY: this thread's own window.
            unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
            return;
        }
    }
    // Every five seconds: did an update put another program at this one's name?
    if face.ticks.is_multiple_of(10) && face.watch.look(handover::identity(&app.program), app.busy.load(Ordering::SeqCst) > 0) == Step::HandOver
    {
        hand_over(window);
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Opening a popup and destroying a window dispatch nested messages synchronously.
    // Never hold the thread-local face borrow across either call.
    if message == WM_TRAY {
        // What happened is in the low half of `lparam`, where on the screen in `wparam`.
        let what = (lparam as u32) & 0xFFFF;
        let mut at = POINT {
            x: (wparam & 0xFFFF) as u16 as i16 as i32,
            y: ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32,
        };
        if at.x == 0 && at.y == 0 {
            // SAFETY: a place to write the pointer's position into.
            unsafe { GetCursorPos(&mut at) };
        }
        if what == WM_CONTEXTMENU {
            show_menu(window, at.x, at.y);
        } else if what == NIN_SELECT || what == NIN_SELECT | 1 {
            // A click, or Enter on the icon: the page. A double click is one click.
            let again = FACE.with(|slot| {
                slot.borrow_mut()
                    .as_mut()
                    .and_then(|face| face.last_click.replace(Instant::now()))
                    .is_some_and(|last| last.elapsed() < Duration::from_millis(700))
            });
            if again {
                return 0;
            }
            if app().picture().running() {
                perform(Action::OpenPage(Route::Home))
            } else {
                show_menu(window, at.x, at.y)
            }
        }
        return 0;
    }
    if message == WM_CLOSE {
        if let Some(app) = APP.get() {
            app.closing.store(true, Ordering::Relaxed);
        }
        FACE.with(|slot| {
            if let Some(face) = slot.borrow_mut().as_mut() {
                face.remove(window);
            }
        });
        unsafe {
            KillTimer(window, TIMER);
            DestroyWindow(window);
        }
        return 0;
    }
    if message == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    let handled = FACE.with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else { return false };
        let Some(face) = slot.as_mut() else { return false };
        match message {
            WM_NEWS => {
                face.show(window);
                true
            }
            WM_TIMER if wparam == TIMER => {
                on_tick(window, face);
                true
            }
            WM_SETTINGCHANGE => {
                // The person may have switched between a light and a dark taskbar.
                face.taskbar = taskbar_color();
                face.show(window);
                false
            }
            // Explorer started again (it crashed, or was restarted): the icon is gone and is
            // put back.
            other if other == face.taskbar_created && other != 0 => {
                face.added = false;
                face.shown = None;
                face.show(window);
                true
            }
            _ => false,
        }
    });
    if handled {
        return 0;
    }
    // SAFETY: what Windows asks every window to do with a message it does not handle itself.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

// -------------------------------------------------------------------- starting

/// Takes the lock that says "a tray runs for this data folder". None when another holds it.
fn take_lock(dir: &Path, patience: Duration) -> Option<File> {
    let _ = std::fs::create_dir_all(dir);
    let deadline = Instant::now() + patience;
    loop {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("tray.lock"))
            .ok()?;
        if file.try_lock().is_ok() {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `handover`: this tray was started by the one it replaces, which is about to close.
pub fn run(handover: bool) -> ExitCode {
    let started_at = now_unix();
    let (Some(program), Some(data_dir)) = (current_exe(), data_dir()) else {
        return ExitCode::from(1);
    };
    // Where there is no taskbar there is nowhere to put an icon: a session over SSH, or a
    // login that is still setting up. Wait a minute for one, then give up quietly.
    let waiting_since = Instant::now();
    while !taskbar_exists() {
        let stopped = std::fs::read_to_string(data_dir.join("tray.stop")).ok();
        if model::quit_asked(stopped.as_deref(), started_at) || waiting_since.elapsed() > Duration::from_secs(60) {
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    // One tray per data folder. A new version waits for the old one to close.
    let Some(lock) = take_lock(
        &data_dir,
        if handover {
            Duration::from_secs(20)
        } else {
            Duration::from_millis(1500)
        },
    ) else {
        return ExitCode::SUCCESS;
    };
    // What a keeper that started this tray had set for itself is not for what the tray starts.
    // SAFETY: nothing else runs yet: no other thread has been started.
    unsafe {
        std::env::remove_var("ZONE_HIDDEN");
        std::env::remove_var("ZONE_SUPERVISED");
        // Sharp icons on a screen that is not at 100%.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let class = wide(CLASS);
    let title = wide(&format!("Zone tray: {}", data_dir.display()));
    // SAFETY: the class and the window are this program's own; every string ends in a zero.
    let window = unsafe {
        let instance = GetModuleHandleW(null());
        let mut description: WNDCLASSW = std::mem::zeroed();
        description.lpfnWndProc = Some(window_proc);
        description.hInstance = instance;
        description.lpszClassName = class.as_ptr();
        RegisterClassW(&description);
        // A window that is never shown. Not a message-only one: those are not told when
        // Explorer starts again.
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if window.is_null() {
        return ExitCode::from(1);
    }

    let (ask, asked) = mpsc::channel();
    let app = Arc::new(App {
        window: window as usize,
        zone: Zone::beside(&program),
        program: program.clone(),
        data_dir,
        started_at,
        snapshot: Mutex::new(Snapshot {
            tray_version: env!("CARGO_PKG_VERSION").to_string(),
            trouble: Some("Starting".to_string()),
            ..Default::default()
        }),
        ask: Mutex::new(ask),
        busy: AtomicUsize::new(0),
        closing: AtomicBool::new(false),
        _lock: lock,
    });
    let _ = APP.set(app.clone());
    let created = wide("TaskbarCreated");
    FACE.with(|face| {
        *face.borrow_mut() = Some(Face {
            added: false,
            shown: None,
            tip: String::new(),
            icons: HashMap::new(),
            taskbar: taskbar_color(),
            ticks: 0,
            watch: Watch::new(handover::identity(&program)),
            last_click: None,
            // SAFETY: a name that ends in a zero.
            taskbar_created: unsafe { RegisterWindowMessageW(created.as_ptr()) },
        });
    });

    // Ask once before the icon shows, so its first face is a true one.
    {
        let first = refresh(&app.zone, &app.picture(), Depth::Everything, now_unix(), &login);
        if let Ok(mut snapshot) = app.snapshot.lock() {
            *snapshot = first;
        }
    }
    std::thread::spawn({
        let app = app.clone();
        move || keep_asking(app, asked)
    });
    std::thread::spawn({
        let app = app.clone();
        move || follow_events(app)
    });

    FACE.with(|face| {
        if let Some(face) = face.borrow_mut().as_mut() {
            face.show(window);
        }
    });
    // SAFETY: this thread's own window and the usual message loop.
    unsafe {
        SetTimer(window, TIMER, TICK.as_millis() as u32, None);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // The icons made along the way.
    FACE.with(|face| {
        if let Some(face) = face.borrow_mut().take() {
            for icon in face.icons.into_values().filter(|icon| !icon.is_null()) {
                // SAFETY: icons this thread made.
                unsafe { DestroyIcon(icon) };
            }
        }
    });
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, IsWindow, SendMessageW, UnregisterClassW};

    #[test]
    fn win32_builds_the_menu_and_closes_its_hidden_window() {
        let entries = model::menu(&Snapshot::default(), now_unix());
        let mut actions = Vec::new();
        let menu = build_menu(&entries, &mut actions);
        assert!(!menu.is_null());
        // SAFETY: handles and null-terminated strings belong to this test thread.
        unsafe {
            assert_eq!(GetMenuItemCount(menu), entries.len() as i32);
            assert!(actions.contains(&Action::CopyMcpSettings));
            DestroyMenu(menu);

            let class = wide("ZoneTrayCloseTest");
            let instance = GetModuleHandleW(null());
            let mut description: WNDCLASSW = std::mem::zeroed();
            description.lpfnWndProc = Some(window_proc);
            description.hInstance = instance;
            description.lpszClassName = class.as_ptr();
            assert_ne!(RegisterClassW(&description), 0);
            let window = CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, null_mut(), null_mut(), instance, null());
            assert!(!window.is_null());
            SendMessageW(window, WM_CLOSE, 0, 0);
            assert_eq!(IsWindow(window), 0);
            assert_ne!(UnregisterClassW(class.as_ptr(), instance), 0);
        }
    }
}
