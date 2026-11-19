//! Where Zone keeps its files on this computer.
//!
//! Everything lives in one data directory. `ZONE_HOME` overrides the platform default, which
//! is what every test uses so the real library is never touched.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// Name of the environment variable that overrides the data directory.
pub const ENV_HOME: &str = "ZONE_HOME";

/// The data directory, without creating it.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = env_path(ENV_HOME) {
        return Ok(dir);
    }
    default_data_dir()
}

/// The data directory, created if missing, readable by its owner only.
pub fn ensure_data_dir() -> Result<PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    restrict_dir(&dir);
    Ok(dir)
}

#[cfg(target_os = "macos")]
fn default_data_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library").join("Application Support").join("Zone"))
}

#[cfg(windows)]
fn default_data_dir() -> Result<PathBuf> {
    if let Some(local) = env_path("LOCALAPPDATA") {
        return Ok(local.join("Zone"));
    }
    Ok(home_dir()?.join("AppData").join("Local").join("Zone"))
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn default_data_dir() -> Result<PathBuf> {
    if let Some(xdg) = env_path("XDG_DATA_HOME") {
        return Ok(xdg.join("zone"));
    }
    Ok(home_dir()?.join(".local").join("share").join("zone"))
}

/// Where this program is, as a path people and other programs can use.
///
/// On Windows the fully resolved form of a path starts with `\\?\`, which the system itself
/// accepts but scripts and some programs do not, so that prefix is taken off.
pub fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find where zone is installed")?;
    let resolved = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(PathBuf::from(without_verbatim_prefix(&resolved.to_string_lossy())))
}

/// `\\?\C:\dir\file` becomes `C:\dir\file`, and `\\?\UNC\server\share` becomes
/// `\\server\share`. Anything else is returned as it is.
pub fn without_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => path.to_string(),
    }
}

/// A non-empty environment variable as a path.
pub fn env_path(name: &str) -> Option<PathBuf> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// The person's home directory, from the environment (so a temporary `HOME` works in tests).
pub fn home_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let names = ["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    let names = ["HOME"];
    for name in names {
        if let Some(dir) = env_path(name) {
            return Ok(dir);
        }
    }
    Err(anyhow!("cannot find your home folder: the HOME environment variable is not set"))
}

pub fn database(dir: &Path) -> PathBuf {
    dir.join("zone.db")
}

/// The iroh secret key of this computer (32 bytes as lowercase hex).
pub fn secret_key(dir: &Path) -> PathBuf {
    dir.join("endpoint.key")
}

/// The random token that unlocks the local web page.
pub fn web_token(dir: &Path) -> PathBuf {
    dir.join("web.token")
}

/// Held with an exclusive lock for as long as the daemon runs.
pub fn daemon_lock(dir: &Path) -> PathBuf {
    dir.join("daemon.lock")
}

/// What the running daemon says about itself (port, endpoint id, relay state).
pub fn daemon_info(dir: &Path) -> PathBuf {
    dir.join("daemon.json")
}

pub fn log_file(dir: &Path) -> PathBuf {
    dir.join("zone.log")
}

/// Makes a directory readable by its owner only. Best effort.
pub fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        // On Windows the data directory is inside the user profile, which already limits
        // access to that user through inherited ACLs.
        let _ = path;
    }
}

/// Writes a file that only its owner can read, replacing any existing one atomically.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&tmp).with_context(|| format!("cannot write {}", tmp.display()))?;
        file.write_all(contents)?;
        file.sync_all().ok();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// True when the file can be read by someone other than its owner (always false off Unix).
pub fn is_shared_with_others(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// 32 random bytes as 64 lowercase hex characters.
pub fn random_hex32() -> String {
    hex(&random_bytes::<32>())
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system could not provide random bytes");
    bytes
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let digit = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    bytes.chunks(2).map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?)).collect()
}

/// A random version 4 UUID, uppercase with hyphens (the form the phone uses for ids).
pub fn new_uuid() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b).to_uppercase();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Seconds since 1970-01-01 UTC, fractional.
pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// This computer's short name, for the pairing link and the welcome message.
pub fn computer_name() -> String {
    if let Ok(name) = std::env::var("ZONE_NAME") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    // On a Mac, the name the person gave it (shown in Finder and AirDrop).
    #[cfg(target_os = "macos")]
    {
        let friendly = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok();
        if let Some(out) = friendly.filter(|out| out.status.success()) {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    let from_command = crate::background::hide(&mut std::process::Command::new("hostname"))
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let raw = from_command
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default();
    let short = raw.split('.').next().unwrap_or("").trim();
    if short.is_empty() { "computer".to_string() } else { short.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_verbatim_prefix_is_taken_off() {
        assert_eq!(without_verbatim_prefix(r"\\?\C:\Users\Zed\zone.exe"), r"C:\Users\Zed\zone.exe");
        assert_eq!(without_verbatim_prefix(r"\\?\UNC\server\share\zone.exe"), r"\\server\share\zone.exe");
        assert_eq!(without_verbatim_prefix(r"C:\Users\Zed\zone.exe"), r"C:\Users\Zed\zone.exe");
        assert_eq!(without_verbatim_prefix("/usr/local/bin/zone"), "/usr/local/bin/zone");
        // A device path is not a file path with a prefix: left alone.
        assert_eq!(without_verbatim_prefix(r"\\?\Volume{1234}\zone.exe"), r"\\?\Volume{1234}\zone.exe");
        assert!(current_exe().unwrap().is_absolute());
    }

    #[test]
    fn hex_round_trip() {
        let bytes = random_bytes::<32>();
        let text = hex(&bytes);
        assert_eq!(text.len(), 64);
        assert_eq!(unhex(&text).unwrap(), bytes);
        assert!(unhex("zz").is_none());
        assert!(unhex("abc").is_none());
    }

    #[test]
    fn uuid_shape() {
        let id = new_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(id, id.to_uppercase());
        assert_eq!(id.as_bytes()[14], b'4');
        assert_eq!(id.matches('-').count(), 4);
    }
}
