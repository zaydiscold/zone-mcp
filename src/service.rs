//! `zone install`, `zone uninstall`, `zone start` and `zone stop`: start `zone serve` at login,
//! without admin rights, and stop it on purpose.
//!
//! - macOS: a launchd user agent in `~/Library/LaunchAgents`.
//! - Linux: a systemd user unit in `~/.config/systemd/user`.
//! - Windows: a per-user Run registry value that starts `zonew.exe serve --background`: zone
//!   with no window, kept running by a keeper of its own (background.rs), since Windows has no
//!   per-user service manager. It used to start a VBScript, which newer Windows leaves out.
//!
//! The text of each file is built by a plain function so it can be checked on any system.

// Each system uses one of the three login items below. The other two are still compiled,
// so their text can be tested everywhere.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{background, paths};

/// The launchd label and the base name of the plist.
pub const LAUNCHD_LABEL: &str = "wtf.zayd.zone.link";
pub const SYSTEMD_UNIT: &str = "zone.service";
pub const WINDOWS_RUN_VALUE: &str = "Zone";
const WINDOWS_RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Explorer notes the login entries the person turned off in Startup apps.
const WINDOWS_STARTUP_APPROVED_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// Another key (under HKEY_CURRENT_USER) for the login entry, so the tests never touch the
/// real one. Startup apps' record is then looked for under it too.
const ENV_WINDOWS_RUN_KEY: &str = "ZONE_WINDOWS_RUN_KEY";

/// The key the Windows login entry is in, under HKEY_CURRENT_USER.
fn run_subkey() -> String {
    std::env::var(ENV_WINDOWS_RUN_KEY).ok().filter(|key| !key.trim().is_empty()).unwrap_or_else(|| WINDOWS_RUN_SUBKEY.to_string())
}

fn startup_approved_subkey() -> String {
    match std::env::var(ENV_WINDOWS_RUN_KEY).ok().filter(|key| !key.trim().is_empty()) {
        Some(key) => format!(r"{key}\StartupApproved"),
        None => WINDOWS_STARTUP_APPROVED_SUBKEY.to_string(),
    }
}

/// The login entry's full name, for people.
fn run_entry_name() -> String {
    format!(r"HKCU\{}\{WINDOWS_RUN_VALUE}", run_subkey())
}

pub enum Installed {
    Yes(PathBuf),
    /// Installed, but pointing at something other than this program.
    Stale(PathBuf, String),
    No,
}

/// What gets written into the login item.
pub struct Plan {
    /// Absolute path of the zone program.
    pub exe: PathBuf,
    /// The data directory, for log files.
    pub data_dir: PathBuf,
    /// `ZONE_HOME`, when the person overrode the data directory.
    pub zone_home: Option<PathBuf>,
}

impl Plan {
    pub fn current() -> Result<Plan> {
        let exe = paths::current_exe()?;
        Ok(Plan { exe, data_dir: paths::ensure_data_dir()?, zone_home: paths::env_path(paths::ENV_HOME) })
    }
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The launchd user agent.
pub fn launchd_plist(plan: &Plan) -> String {
    let mut env = String::new();
    if let Some(home) = &plan.zone_home {
        env = format!(
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>ZONE_HOME</key>\n        <string>{}</string>\n    </dict>\n",
            xml(&home.to_string_lossy())
        );
    }
    let log = plan.data_dir.join("launchd.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>serve</string>
        <string>--quiet</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ProcessType</key>
    <string>Background</string>
{env}    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        exe = xml(&plan.exe.to_string_lossy()),
        log = xml(&log.to_string_lossy()),
    )
}

/// The systemd user unit.
pub fn systemd_unit(plan: &Plan) -> String {
    // systemd reads ExecStart with its own quoting: double quotes, backslash escapes.
    let quote = |path: &Path| format!("\"{}\"", path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"));
    let mut unit = String::from(
        "[Unit]\nDescription=Zone Link: receives your iPhone's Zone library\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\n",
    );
    unit.push_str(&format!("ExecStart={} serve --quiet\n", quote(&plan.exe)));
    if let Some(home) = &plan.zone_home {
        unit.push_str(&format!("Environment={}\n", quote(Path::new(&format!("ZONE_HOME={}", home.to_string_lossy())))));
    }
    // `zone serve` exits 143 (or 129) when a signal stops it. `systemctl --user stop` is a
    // clean stop, not a failure; anything else that stops zone is followed by a restart.
    unit.push_str("Restart=always\nRestartSec=10\nSuccessExitStatus=129 143\n\n[Install]\nWantedBy=default.target\n");
    unit
}

/// The command the Windows login entry runs: `zonew.exe`, which starts zone with no window,
/// when it is beside zone (`zone.exe` itself otherwise, which shows a console for a moment).
pub fn windows_run_command(exe: &Path, windowless: Option<&Path>, zone_home: Option<&Path>) -> String {
    // A Windows path cannot hold a double quote, so quoting is only putting it in quotes.
    let program = windowless.unwrap_or(exe);
    let mut command = format!("\"{}\" serve --background", program.to_string_lossy());
    if let Some(home) = zone_home {
        command.push_str(&format!(" --home \"{}\"", home.to_string_lossy().trim_end_matches('\\')));
    }
    command
}

/// `zonew.exe` beside this zone, when it is there.
pub fn windowless_launcher(exe: &Path) -> Option<PathBuf> {
    Some(exe.with_file_name("zonew.exe")).filter(|path| path.is_file())
}

/// The program a login entry starts, and the data folder it gives zone (`--home`), if any.
pub fn windows_run_parts(command: &str) -> (Option<PathBuf>, Option<String>) {
    let program = command.trim_start().strip_prefix('"').and_then(|rest| rest.split_once('"')).map(|(path, _)| PathBuf::from(path));
    let home = command.split_once(" --home \"").and_then(|(_, rest)| rest.split_once('"')).map(|(home, _)| home.to_string());
    (program, home)
}

/// The script zone 0.1.3 and earlier started at login through `wscript.exe`.
pub fn windows_launcher_path(data_dir: &Path) -> PathBuf {
    data_dir.join("zone-serve.vbs")
}

/// What an old launcher script says: the program it starts, and the `ZONE_HOME` it sets.
pub fn vbs_parts(script: &str) -> (Option<PathBuf>, Option<String>) {
    // VBScript writes a double quote inside a string twice.
    let program = script.split_once("shell.Run \"\"\"").and_then(|(_, rest)| rest.split_once("\"\"")).map(|(path, _)| PathBuf::from(path));
    let home = script.split_once("(\"ZONE_HOME\") = \"").and_then(|(_, rest)| rest.split_once('"')).map(|(home, _)| home.to_string());
    (program, home)
}

/// True when a login entry's command is the old launcher script's.
pub fn is_vbs_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("wscript") || lower.contains(".vbs")
}

pub fn launchd_plist_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join("Library").join("LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")))
}

pub fn systemd_unit_path() -> Result<PathBuf> {
    let config = match paths::env_path("XDG_CONFIG_HOME") {
        Some(dir) => dir,
        None => paths::home_dir()?.join(".config"),
    };
    Ok(config.join("systemd").join("user").join(SYSTEMD_UNIT))
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("cannot write {}", path.display()))
}

fn run(program: &str, args: &[&str]) -> Result<std::process::Output> {
    background::hide(&mut std::process::Command::new(program)).args(args).output().with_context(|| format!("cannot run {program}"))
}

// -------------------------------------------------------------------- whose login item

/// The text of the login item, when there is one.
fn item_text() -> Option<String> {
    #[cfg(target_os = "macos")]
    return std::fs::read_to_string(launchd_plist_path().ok()?).ok();
    #[cfg(all(unix, not(target_os = "macos")))]
    return std::fs::read_to_string(systemd_unit_path().ok()?).ok();
    #[cfg(windows)]
    return crate::win::reg_get_string(&run_subkey(), WINDOWS_RUN_VALUE);
}

/// The `ZONE_HOME` a login item gives zone. None when it gives none (the usual data folder).
pub fn item_home(text: &str) -> Option<String> {
    // launchd
    if let Some((_, rest)) = text.split_once("<key>ZONE_HOME</key>") {
        let value = rest.split_once("<string>")?.1.split_once("</string>")?.0;
        return Some(value.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"));
    }
    // systemd
    if let Some((_, rest)) = text.split_once("Environment=\"ZONE_HOME=") {
        let mut value = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => value.extend(chars.next()),
                '"' => break,
                c => value.push(c),
            }
        }
        return Some(value.replace("%%", "%"));
    }
    // Windows: the login entry, or the old launcher script it starts.
    if is_vbs_command(text) {
        let script = text.split('"').find(|part| part.to_ascii_lowercase().ends_with(".vbs")).and_then(|path| std::fs::read_to_string(path).ok())?;
        return vbs_parts(&script).1;
    }
    windows_run_parts(text).1
}

fn same_folder(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_end_matches(['/', '\\']), b.trim_end_matches(['/', '\\']));
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// True when the login item starts zone for the data folder this process uses. A `ZONE_HOME`
/// pointed at a test folder then never stops, starts or removes the person's real zone.
fn item_is_for_this_folder(text: &str) -> bool {
    let wanted = paths::env_path(paths::ENV_HOME).map(|home| home.to_string_lossy().to_string());
    match (item_home(text), wanted) {
        (None, None) => true,
        (Some(a), Some(b)) => same_folder(&a, &b),
        _ => false,
    }
}

/// For an installer refreshing the login item after an update: the data folder is the one the
/// login item names, whatever this process was started with.
pub fn adopt_item_home() {
    let Some(text) = item_text() else { return };
    // SAFETY: called before anything else runs. The main thread only waits for this one, and no
    // other thread has been started.
    match item_home(&text) {
        Some(home) => unsafe { std::env::set_var(paths::ENV_HOME, home) },
        None => unsafe { std::env::remove_var(paths::ENV_HOME) },
    }
}

/// The login item, when it is this data folder's.
fn this_folders_item() -> Option<String> {
    item_text().filter(|text| item_is_for_this_folder(text))
}

// -------------------------------------------------------------------- install

/// Writes the login item. With `start` false nothing is loaded or started: only files are
/// written (and, on Windows, nothing is added to the registry).
pub fn install(start: bool) -> Result<()> {
    let plan = Plan::current()?;
    install_plan(&plan, start)
}

#[cfg(target_os = "macos")]
fn install_plan(plan: &Plan, start: bool) -> Result<()> {
    let path = launchd_plist_path()?;
    write_file(&path, &launchd_plist(plan))?;
    println!("Wrote {}", path.display());
    if !start {
        println!("Not started (--no-start). To start it: launchctl bootstrap gui/$(id -u) \"{}\"", path.display());
        return Ok(());
    }
    let domain = launchd_domain()?;
    // Replace a copy that is already loaded.
    let _ = run("launchctl", &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")]);
    let loaded = run("launchctl", &["bootstrap", &domain, &path.to_string_lossy()])?;
    if !loaded.status.success() {
        bail!(
            "launchctl could not load the login item: {}\nTry: launchctl bootstrap {domain} \"{}\"",
            String::from_utf8_lossy(&loaded.stderr).trim(),
            path.display()
        );
    }
    println!("Zone now starts when you log in, and it is running. Next: `zone pair`.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String> {
    let uid = String::from_utf8_lossy(&run("id", &["-u"])?.stdout).trim().to_string();
    Ok(format!("gui/{uid}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_plan(plan: &Plan, start: bool) -> Result<()> {
    let path = systemd_unit_path()?;
    write_file(&path, &systemd_unit(plan))?;
    println!("Wrote {}", path.display());
    if !start {
        println!("Not started (--no-start). To start it: systemctl --user daemon-reload && systemctl --user enable --now {SYSTEMD_UNIT}");
        return Ok(());
    }
    let _ = run("systemctl", &["--user", "daemon-reload"]);
    let enabled = run("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT])?;
    if !enabled.status.success() {
        bail!(
            "systemctl could not start the unit: {}\nTry: systemctl --user enable --now {SYSTEMD_UNIT}",
            String::from_utf8_lossy(&enabled.stderr).trim()
        );
    }
    println!("Zone now starts when you log in, and it is running. Next: `zone pair`.");
    println!("To keep it running while you are logged out: loginctl enable-linger");
    Ok(())
}

#[cfg(windows)]
fn install_plan(plan: &Plan, start: bool) -> Result<()> {
    let windowless = windowless_launcher(&plan.exe);
    let command = windows_run_command(&plan.exe, windowless.as_deref(), plan.zone_home.as_deref());
    if !start {
        println!("Not started (--no-start). To start at login: reg add HKCU\\{} /v {WINDOWS_RUN_VALUE} /t REG_SZ /d \"{}\" /f", run_subkey(), command.replace('"', "\\\""));
        return Ok(());
    }
    crate::win::reg_set_string(&run_subkey(), WINDOWS_RUN_VALUE, &command).context("could not add the login entry to the registry")?;
    println!("Login entry: {} = {command}", run_entry_name());
    // What zone 0.1.3 and earlier started at login, through wscript.exe.
    let old = windows_launcher_path(&plan.data_dir);
    if old.exists() {
        match std::fs::remove_file(&old) {
            Ok(()) => println!("Removed the old launcher script {}", old.display()),
            Err(error) => println!("Could not remove the old launcher script {} ({error}). It is no longer used.", old.display()),
        }
    }
    if windowless.is_none() {
        println!(
            "zonew.exe is not beside {}, so a console window shows for a moment when you log in. Install zone with install.ps1 to have it.",
            plan.exe.display()
        );
    }
    run_this_version(&plan.data_dir)?;
    println!("Zone now starts when you log in, and it is running. Next: `zone pair`.");
    Ok(())
}

/// Makes sure the zone running for this data folder is this version, kept running by its
/// keeper: an older zone, or one started by the old launcher, is stopped first.
#[cfg(windows)]
fn run_this_version(dir: &Path) -> Result<()> {
    use crate::background::{self, KeeperInfo};
    use crate::daemon_info::DaemonInfo;
    let this = crate::update::current_version();
    let daemon = DaemonInfo::read_if_running(dir);
    let keeper = KeeperInfo::read_if_running(dir);
    let other = daemon.as_ref().is_some_and(|d| d.version != this || !d.stops_on_request) || keeper.as_ref().is_some_and(|k| k.version != this);
    if other {
        let stopped = background::stop(dir)?;
        if stopped == background::Stopped::StillRunning {
            bail!("the zone that is running would not stop, so this version cannot start. `zone status` names it.");
        }
        println!("Stopped the zone that was running, to start this version.");
    }
    if !background::keeper_running(dir) {
        background::start_keeper(None)?;
    }
    if !background::wait_until_running(dir, std::time::Duration::from_secs(20)) {
        bail!("zone did not start within 20 seconds. Its log may say why: {}", paths::log_file(dir).display());
    }
    Ok(())
}

// -------------------------------------------------------------------- uninstall

/// Removes the login item. With `stop` false nothing is unloaded or stopped: only the file
/// is removed.
pub fn uninstall(stop: bool) -> Result<()> {
    if let Some(text) = item_text().filter(|text| !item_is_for_this_folder(text)) {
        let home = item_home(&text).unwrap_or_else(|| "the usual data folder".to_string());
        println!("The login item starts zone for another data folder ({home}), so it was left alone.");
        if stop {
            stop_this_folder()?;
        }
        return Ok(());
    }
    uninstall_platform(stop)
}

#[cfg(target_os = "macos")]
fn uninstall_platform(stop: bool) -> Result<()> {
    let path = launchd_plist_path()?;
    if !path.exists() {
        println!("Zone was not set to start at login.");
        return Ok(());
    }
    if stop {
        let _ = run("launchctl", &["bootout", &format!("{}/{LAUNCHD_LABEL}", launchd_domain()?)]);
    }
    std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
    println!("Removed {}. Zone no longer starts at login{}.", path.display(), if stop { " and has been stopped" } else { "" });
    print_leftovers();
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn uninstall_platform(stop: bool) -> Result<()> {
    let path = systemd_unit_path()?;
    if !path.exists() {
        println!("Zone was not set to start at login.");
        return Ok(());
    }
    if stop {
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
    }
    std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
    if stop {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    println!("Removed {}. Zone no longer starts at login{}.", path.display(), if stop { " and has been stopped" } else { "" });
    print_leftovers();
    Ok(())
}

#[cfg(windows)]
fn uninstall_platform(stop: bool) -> Result<()> {
    let data_dir = paths::data_dir()?;
    let mut stopped = String::new();
    if stop {
        stopped = match crate::background::stop(&data_dir)? {
            crate::background::Stopped::NotRunning => String::new(),
            crate::background::Stopped::StillRunning => " (zone itself would not stop: `zone status` names it)".to_string(),
            _ => " and has been stopped".to_string(),
        };
    }
    let removed = crate::win::reg_delete(&run_subkey(), WINDOWS_RUN_VALUE).context("could not remove the login entry from the registry")?;
    // Explorer's record of whether the person turned the entry off in Startup apps.
    let _ = crate::win::reg_delete(&startup_approved_subkey(), WINDOWS_RUN_VALUE);
    let old = windows_launcher_path(&data_dir);
    let _ = std::fs::remove_file(&old);
    if removed {
        println!("Removed the login entry {}. Zone no longer starts at login{stopped}.", run_entry_name());
    } else {
        println!("Zone was not set to start at login{}.", if stopped.is_empty() { String::new() } else { format!(". It{stopped}") });
    }
    print_leftovers();
    Ok(())
}

fn print_leftovers() {
    if let Ok(dir) = paths::data_dir() {
        println!("Your library is still in {}. Delete that folder to remove it too.", dir.display());
    }
}

// -------------------------------------------------------------------- start and stop

/// `zone start`: through the login item when there is one for this data folder, else by a
/// keeper in the background (see background.rs).
pub fn start() -> Result<()> {
    let dir = paths::ensure_data_dir()?;
    if let Some(daemon) = crate::daemon_info::DaemonInfo::read_if_running(&dir) {
        println!("zone is already running (pid {}). `zone status` says more.", daemon.pid);
        return Ok(());
    }
    if !start_through_item()? {
        crate::background::start_keeper(None)?;
    }
    if crate::background::wait_until_running(&dir, std::time::Duration::from_secs(20)) {
        println!("zone is running. `zone status` says more, `zone stop` stops it.");
        Ok(())
    } else {
        bail!("zone did not start within 20 seconds. Its log may say why: {}", paths::log_file(&dir).display())
    }
}

/// Starts zone through the service manager, when the login item is this data folder's. False
/// when there is none to start it through.
fn start_through_item() -> Result<bool> {
    if this_folders_item().is_none() {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        let domain = launchd_domain()?;
        // Loaded: start it. Unloaded (by `launchctl bootout`): load it, which starts it.
        if !run("launchctl", &["kickstart", &format!("{domain}/{LAUNCHD_LABEL}")])?.status.success() {
            let loaded = run("launchctl", &["bootstrap", &domain, &launchd_plist_path()?.to_string_lossy()])?;
            if !loaded.status.success() {
                bail!("launchctl could not start zone: {}", String::from_utf8_lossy(&loaded.stderr).trim());
            }
        }
        Ok(true)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let started = run("systemctl", &["--user", "start", SYSTEMD_UNIT])?;
        if !started.status.success() {
            bail!("systemctl could not start zone: {}", String::from_utf8_lossy(&started.stderr).trim());
        }
        Ok(true)
    }
    // On Windows the login entry starts a keeper, which is what starting without one does too.
    #[cfg(windows)]
    Ok(false)
}

/// `zone stop`: stops zone for this data folder until the next login or `zone start`. False
/// when it would not stop.
pub fn stop() -> Result<bool> {
    let stopped = stop_this_folder()?;
    let dir = paths::data_dir()?;
    match stopped {
        crate::background::Stopped::NotRunning => println!("zone is not running for {}.", dir.display()),
        crate::background::Stopped::StillRunning => {
            println!("zone did not stop. `zone status` names its process.");
            return Ok(false);
        }
        _ => {
            let again = if this_folders_item().is_some() { "when you log in, or with `zone start`" } else { "with `zone start`" };
            println!("Stopped zone. It starts again {again}.");
        }
    }
    Ok(true)
}

fn stop_this_folder() -> Result<crate::background::Stopped> {
    let dir = paths::data_dir()?;
    if this_folders_item().is_some() {
        // systemd starts zone again whatever its exit, unless it is the one stopping it.
        #[cfg(all(unix, not(target_os = "macos")))]
        if crate::daemon_info::is_running(&dir) {
            let _ = run("systemctl", &["--user", "stop", SYSTEMD_UNIT]);
        }
        // launchd starts again a zone that a signal ended (an older zone that does not know
        // about stop requests is ended that way): it is unloaded until `zone start` instead.
        #[cfg(target_os = "macos")]
        if crate::daemon_info::DaemonInfo::read_if_running(&dir).is_some_and(|d| !d.stops_on_request) {
            let _ = run("launchctl", &["bootout", &format!("{}/{LAUNCHD_LABEL}", launchd_domain()?)]);
        }
    }
    crate::background::stop(&dir)
}

// -------------------------------------------------------------------- status

/// Whether the login item exists and points at this program.
pub fn status() -> Result<Installed> {
    #[cfg(windows)]
    return windows_status();
    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        let path = launchd_plist_path()?;
        #[cfg(all(unix, not(target_os = "macos")))]
        let path = systemd_unit_path()?;
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Ok(Installed::No);
        };
        let exe = paths::current_exe().ok();
        match exe {
            Some(exe) if !contents.contains(&*exe.to_string_lossy()) && !contents.contains(&xml(&exe.to_string_lossy())) => {
                Ok(Installed::Stale(path, "starts a different copy of zone than the one you are running".to_string()))
            }
            _ => Ok(Installed::Yes(path)),
        }
    }
}

#[cfg(windows)]
fn windows_status() -> Result<Installed> {
    let Some(command) = crate::win::reg_get_string(&run_subkey(), WINDOWS_RUN_VALUE) else {
        return Ok(Installed::No);
    };
    let entry = PathBuf::from(run_entry_name());
    if is_vbs_command(&command) {
        return Ok(Installed::Stale(entry, "starts zone through a VBScript (zone-serve.vbs), which newer versions of Windows leave out: run `zone install` to replace it".to_string()));
    }
    let exe = paths::current_exe()?;
    let (program, _) = windows_run_parts(&command);
    let folder = |path: &Path| path.parent().map(|p| p.to_string_lossy().to_lowercase());
    if program.as_deref().and_then(folder) != folder(&exe) {
        return Ok(Installed::Stale(entry, "starts a different copy of zone than the one you are running".to_string()));
    }
    if crate::win::reg_get_binary(&startup_approved_subkey(), WINDOWS_RUN_VALUE).is_some_and(|flags| startup_turned_off(&flags)) {
        return Ok(Installed::Stale(entry, "is turned off in Startup apps (Settings, Apps, Startup, or Task Manager): turn Zone on there".to_string()));
    }
    if program.as_ref().is_some_and(|p| !p.file_name().is_some_and(|name| name.eq_ignore_ascii_case("zonew.exe"))) {
        return Ok(Installed::Stale(entry, "shows a console window for a moment at login (zonew.exe was missing when it was made): run `zone install` again".to_string()));
    }
    Ok(Installed::Yes(entry))
}

/// Explorer keeps whether the person turned a login entry off in Startup apps: the first byte
/// is even when it is on (2, or 6), odd when off (3).
pub fn startup_turned_off(flags: &[u8]) -> bool {
    flags.first().is_some_and(|first| first & 1 == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(zone_home: Option<&str>) -> Plan {
        Plan {
            exe: PathBuf::from("/Users/zed/My Tools/zone & co/zone"),
            data_dir: PathBuf::from("/Users/zed/Library/Application Support/Zone"),
            zone_home: zone_home.map(PathBuf::from),
        }
    }

    #[test]
    fn launchd_plist_is_well_formed() {
        let text = launchd_plist(&plan(None));
        assert!(text.starts_with("<?xml version=\"1.0\""));
        assert!(text.contains("<string>wtf.zayd.zone.link</string>"));
        // The ampersand in the path is escaped, the spaces are kept as they are.
        assert!(text.contains("<string>/Users/zed/My Tools/zone &amp; co/zone</string>"));
        assert!(text.contains("<string>serve</string>\n        <string>--quiet</string>"));
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(text.contains("<key>SuccessfulExit</key>\n        <false/>"));
        // Joined with this platform's separator, so the test also passes when run on Windows.
        let log = Path::new("/Users/zed/Library/Application Support/Zone").join("launchd.log");
        assert!(text.contains(&format!("<string>{}</string>", log.display())));
        assert!(!text.contains("EnvironmentVariables"));
        assert_eq!(text.matches("<dict>").count(), text.matches("</dict>").count());
        assert_eq!(text.matches("<key>").count(), text.matches("</key>").count());
        // No root-only keys: this is a per-user agent.
        assert!(!text.contains("UserName") && !text.contains("LaunchDaemons"));

        let with_home = launchd_plist(&plan(Some("/tmp/zone <test>")));
        assert!(with_home.contains("<key>ZONE_HOME</key>\n        <string>/tmp/zone &lt;test&gt;</string>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_plist_passes_plutil() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.plist");
        std::fs::write(&path, launchd_plist(&plan(Some("/tmp/zone <test>")))).unwrap();
        let out = std::process::Command::new("plutil").arg("-lint").arg(&path).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    }

    #[test]
    fn systemd_unit_quotes_paths() {
        let text = systemd_unit(&plan(Some("/tmp/zone home")));
        assert!(text.contains("ExecStart=\"/Users/zed/My Tools/zone & co/zone\" serve --quiet\n"));
        assert!(text.contains("Environment=\"ZONE_HOME=/tmp/zone home\"\n"));
        assert!(text.contains("Restart=always"));
        assert!(text.contains("SuccessExitStatus=129 143"));
        assert!(text.contains("[Install]\nWantedBy=default.target\n"));
        assert!(!systemd_unit(&plan(None)).contains("Environment="));
    }

    #[test]
    fn the_windows_login_entry_starts_zone_without_a_window() {
        let exe = PathBuf::from("C:\\Users\\Zo\u{eb}\\AppData\\Local\\Zone\\bin\\zone.exe");
        // Spelled out: off Windows a backslash does not separate folders.
        let zonew = PathBuf::from("C:\\Users\\Zo\u{eb}\\AppData\\Local\\Zone\\bin\\zonew.exe");
        let command = windows_run_command(&exe, Some(&zonew), None);
        assert_eq!(command, "\"C:\\Users\\Zo\u{eb}\\AppData\\Local\\Zone\\bin\\zonew.exe\" serve --background");
        assert_eq!(windows_run_parts(&command), (Some(zonew.clone()), None));
        assert!(!is_vbs_command(&command));
        // A data folder of its own goes with it. A trailing backslash would escape the quote.
        let with_home = windows_run_command(&exe, Some(&zonew), Some(Path::new(r"D:\zone data\")));
        assert!(with_home.ends_with(r#" serve --background --home "D:\zone data""#), "{with_home}");
        assert_eq!(windows_run_parts(&with_home).1.as_deref(), Some(r"D:\zone data"));
        assert_eq!(item_home(&with_home).as_deref(), Some(r"D:\zone data"));
        // Without zonew.exe, zone.exe itself.
        assert!(windows_run_command(&exe, None, None).starts_with("\"C:\\Users\\Zo\u{eb}\\AppData\\Local\\Zone\\bin\\zone.exe\" "));
    }

    #[test]
    fn the_old_launcher_script_is_recognized_and_read() {
        // What zone 0.1.3 wrote, byte for byte.
        let script = "' Starts Zone Link at login without showing a window. Written by `zone install`.\r\nSet shell = CreateObject(\"WScript.Shell\")\r\nshell.Environment(\"PROCESS\")(\"ZONE_HOME\") = \"D:\\zone data\"\r\nshell.Run \"\"\"C:\\Users\\Zed\\Tools\\zone.exe\"\" serve --quiet\", 0, False\r\n";
        assert_eq!(vbs_parts(script), (Some(PathBuf::from(r"C:\Users\Zed\Tools\zone.exe")), Some(r"D:\zone data".to_string())));
        let plain = script.replace("shell.Environment(\"PROCESS\")(\"ZONE_HOME\") = \"D:\\zone data\"\r\n", "");
        assert_eq!(vbs_parts(&plain).1, None);
        assert!(is_vbs_command(r#"wscript.exe "C:\Users\Zed\AppData\Local\Zone\zone-serve.vbs""#));
        assert!(windows_launcher_path(Path::new(r"C:\Zone")).ends_with("zone-serve.vbs"));
    }

    #[test]
    fn a_login_item_says_which_data_folder_it_is_for() {
        assert_eq!(item_home(&launchd_plist(&plan(None))), None);
        assert_eq!(item_home(&launchd_plist(&plan(Some("/tmp/zone <test> & co")))).as_deref(), Some("/tmp/zone <test> & co"));
        assert_eq!(item_home(&systemd_unit(&plan(None))), None);
        assert_eq!(item_home(&systemd_unit(&plan(Some("/tmp/a \"b\" 100%")))).as_deref(), Some("/tmp/a \"b\" 100%"));
        assert!(same_folder("/tmp/x/", "/tmp/x"));
    }

    #[test]
    fn startup_apps_off_is_read_from_explorers_flags() {
        assert!(!startup_turned_off(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!startup_turned_off(&[6, 0, 0, 0]));
        assert!(startup_turned_off(&[3, 0, 0, 0, 0x10, 0x20, 0, 0, 0, 0, 0, 0]));
        assert!(!startup_turned_off(&[]));
    }
}
