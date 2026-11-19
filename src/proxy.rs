//! Which web proxy zone's own connections go through: the link's relay (iroh) and `zone update`.
//!
//! First the usual environment variables (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, in either
//! case). On Windows, when none is set, the proxy Windows itself is set to use (Settings,
//! Network, Proxy, "Use a proxy server"), which is how most offices set one. A proxy given
//! only by a setup script (a PAC file, "Use setup script") cannot be followed: zone says so in
//! its log, and the link still works wherever the relay can be reached directly.

/// A proxy address as `http://host:port` (or as the environment gave it), when there is one.
pub fn proxy_url() -> Option<String> {
    from_environment().or_else(system_proxy)
}

/// The system's proxy when the environment names none: what to hand `curl`, which reads the
/// environment itself but not the Windows setting.
pub fn system_proxy_only() -> Option<String> {
    if from_environment().is_some() { None } else { system_proxy() }
}

fn from_environment() -> Option<String> {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty()))
}

/// The proxy for https in the form Windows keeps it: `host:port`, or one entry per protocol,
/// `http=host:port;https=host:port;socks=host:port`.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn from_windows_setting(setting: &str) -> Option<String> {
    let setting = setting.trim();
    if setting.is_empty() {
        return None;
    }
    let address = if setting.contains('=') {
        let entries: Vec<(&str, &str)> = setting.split(';').filter_map(|entry| entry.split_once('=')).map(|(kind, address)| (kind.trim(), address.trim())).collect();
        let pick = |wanted: &str| entries.iter().find(|(kind, address)| kind.eq_ignore_ascii_case(wanted) && !address.is_empty()).map(|(_, address)| *address);
        pick("https").or_else(|| pick("http"))?
    } else {
        setting.split(';').next()?.trim()
    };
    if address.contains("://") { Some(address.to_string()) } else { Some(format!("http://{address}")) }
}

#[cfg(windows)]
fn system_proxy() -> Option<String> {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    if crate::win::reg_get_dword(KEY, "ProxyEnable") != Some(1) {
        return None;
    }
    from_windows_setting(&crate::win::reg_get_string(KEY, "ProxyServer")?)
}

#[cfg(not(windows))]
fn system_proxy() -> Option<String> {
    None
}

/// Windows is set to find its proxy through a setup script, which zone cannot follow.
pub fn uses_setup_script() -> bool {
    #[cfg(windows)]
    {
        crate::win::reg_get_string(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings", "AutoConfigURL").is_some_and(|url| !url.trim().is_empty())
    }
    #[cfg(not(windows))]
    false
}

/// A proxy address for the log: the host and port, never a user name or password in it.
pub fn for_log(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let host = rest.rsplit_once('@').map(|(_, host)| host).unwrap_or(rest);
    format!("{scheme}://{}", host.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_setting_is_read_as_people_write_it() {
        assert_eq!(from_windows_setting("proxy.corp.example:8080").as_deref(), Some("http://proxy.corp.example:8080"));
        assert_eq!(from_windows_setting("http=web:80;https=secure:443;socks=s:1080").as_deref(), Some("http://secure:443"));
        assert_eq!(from_windows_setting("http=web:80;ftp=f:21").as_deref(), Some("http://web:80"));
        assert_eq!(from_windows_setting("socks=s:1080"), None);
        assert_eq!(from_windows_setting("  "), None);
        assert_eq!(from_windows_setting("https://p:3128").as_deref(), Some("https://p:3128"));
    }

    #[test]
    fn a_proxy_in_the_log_carries_no_password() {
        assert_eq!(for_log("http://me:secret@proxy:8080/"), "http://proxy:8080");
        assert_eq!(for_log("proxy:3128"), "http://proxy:3128");
    }
}
