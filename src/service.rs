//! `zone install` and `zone uninstall`: start `zone serve` at login, without admin rights.
//!
//! - macOS: a launchd user agent in `~/Library/LaunchAgents`.
//! - Linux: a systemd user unit in `~/.config/systemd/user`.
//! - Windows: a per-user Run registry value that starts a small hidden launcher script.
//!
//! The text of each file is built by a plain function so it can be checked on any system.

// Each system uses one of the three login items below. The other two are still compiled,
// so their text can be tested everywhere.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::paths;

/// The launchd label and the base name of the plist.
pub const LAUNCHD_LABEL: &str = "wtf.zayd.zone.link";
pub const SYSTEMD_UNIT: &str = "zone.service";
pub const WINDOWS_RUN_VALUE: &str = "Zone";
const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

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
        let exe = std::env::current_exe().context("cannot find where zone is installed")?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
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
    unit.push_str("Restart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n");
    unit
}

/// The Windows launcher: starts `zone serve` with no console window.
pub fn windows_launcher(plan: &Plan) -> String {
    // In VBScript a double quote inside a string is written twice.
    let quoted = |path: &Path| path.to_string_lossy().replace('"', "\"\"");
    let mut script = String::from("' Starts Zone Link at login without showing a window. Written by `zone install`.\r\n");
    script.push_str("Set shell = CreateObject(\"WScript.Shell\")\r\n");
    if let Some(home) = &plan.zone_home {
        script.push_str(&format!("shell.Environment(\"PROCESS\")(\"ZONE_HOME\") = \"{}\"\r\n", quoted(home)));
    }
    script.push_str(&format!("shell.Run \"\"\"{}\"\" serve --quiet\", 0, False\r\n", quoted(&plan.exe)));
    script
}

/// The command stored in the Run registry value.
pub fn windows_run_command(launcher: &Path) -> String {
    format!("wscript.exe \"{}\"", launcher.to_string_lossy())
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

pub fn windows_launcher_path(data_dir: &Path) -> PathBuf {
    data_dir.join("zone-serve.vbs")
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("cannot write {}", path.display()))
}

fn run(program: &str, args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new(program).args(args).output().with_context(|| format!("cannot run {program}"))
}

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
    let uid = String::from_utf8_lossy(&run("id", &["-u"])?.stdout).trim().to_string();
    let domain = format!("gui/{uid}");
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
    let launcher = windows_launcher_path(&plan.data_dir);
    write_file(&launcher, &windows_launcher(plan))?;
    println!("Wrote {}", launcher.display());
    let command = windows_run_command(&launcher);
    if !start {
        println!("Not started (--no-start). To start at login: reg add {WINDOWS_RUN_KEY} /v {WINDOWS_RUN_VALUE} /t REG_SZ /d \"{command}\" /f");
        return Ok(());
    }
    let added = run("reg", &["add", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f"])?;
    if !added.status.success() {
        bail!("could not add the login item to the registry: {}", String::from_utf8_lossy(&added.stderr).trim());
    }
    if !crate::daemon_info::is_running(&plan.data_dir) {
        std::process::Command::new("wscript.exe").arg(&launcher).spawn().context("cannot start zone now")?;
    }
    println!("Zone now starts when you log in, and it is running. Next: `zone pair`.");
    Ok(())
}

/// Removes the login item. With `stop` false nothing is unloaded or stopped: only the file
/// is removed.
pub fn uninstall(stop: bool) -> Result<()> {
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
        let uid = String::from_utf8_lossy(&run("id", &["-u"])?.stdout).trim().to_string();
        let _ = run("launchctl", &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")]);
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
    let launcher = windows_launcher_path(&data_dir);
    if stop {
        let _ = run("reg", &["delete", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE, "/f"]);
        if let Some(daemon) = crate::daemon_info::DaemonInfo::read_if_running(&data_dir) {
            let _ = run("taskkill", &["/PID", &daemon.pid.to_string(), "/F"]);
        }
    }
    let _ = std::fs::remove_file(&launcher);
    println!("Zone no longer starts at login{}.", if stop { " and has been stopped" } else { "" });
    print_leftovers();
    Ok(())
}

fn print_leftovers() {
    if let Ok(dir) = paths::data_dir() {
        println!("Your library is still in {}. Delete that folder to remove it too.", dir.display());
    }
}

/// Whether the login item exists and points at this program.
pub fn status() -> Result<Installed> {
    #[cfg(target_os = "macos")]
    let path = launchd_plist_path()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let path = systemd_unit_path()?;
    #[cfg(windows)]
    let path = windows_launcher_path(&paths::data_dir()?);

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(Installed::No);
    };
    let exe = std::env::current_exe().ok().map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    match exe {
        Some(exe) if !contents.contains(&*exe.to_string_lossy()) && !contents.contains(&xml(&exe.to_string_lossy())) => {
            Ok(Installed::Stale(path, "starts a different copy of zone than the one you are running".to_string()))
        }
        _ => Ok(Installed::Yes(path)),
    }
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
        assert!(text.contains("<string>/Users/zed/Library/Application Support/Zone/launchd.log</string>"));
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
        assert!(text.contains("Restart=on-failure"));
        assert!(text.contains("[Install]\nWantedBy=default.target\n"));
        assert!(!systemd_unit(&plan(None)).contains("Environment="));
    }

    #[test]
    fn windows_launcher_hides_the_window() {
        let plan = Plan {
            exe: PathBuf::from(r"C:\Users\Zed\Tools\zone.exe"),
            data_dir: PathBuf::from(r"C:\Users\Zed\AppData\Local\Zone"),
            zone_home: Some(PathBuf::from(r"D:\zone data")),
        };
        let text = windows_launcher(&plan);
        // Window style 0 is hidden. False means do not wait.
        assert!(text.contains("shell.Run \"\"\"C:\\Users\\Zed\\Tools\\zone.exe\"\" serve --quiet\", 0, False\r\n"));
        assert!(text.contains("shell.Environment(\"PROCESS\")(\"ZONE_HOME\") = \"D:\\zone data\"\r\n"));
        let command = windows_run_command(&windows_launcher_path(&plan.data_dir));
        assert!(command.starts_with("wscript.exe \"") && command.ends_with("zone-serve.vbs\""));
    }
}
