//! `zone mcp install|uninstall|status`: registering this program with AI clients.
//!
//! Each client keeps its MCP servers in its own file, in its own format. Only the `zone`
//! entry is ever added, changed or removed. Everything else in the file is kept byte for
//! byte (comments included), and a copy of the original is saved beside it as
//! `<file>.before-zone` the first time zone changes it.
//!
//! Some clients have more than one place: Claude Code has a profile per `CLAUDE_CONFIG_DIR`,
//! Codex one per `CODEX_HOME`, Hermes one per profile. Each place is a [`Target`], and zone
//! registers with every one it finds, which is also how apps that run these agents with their
//! own profiles (T3 Code, for one) get it.
//!
//! What zone has done, and what the person undid, is kept in the data folder (see
//! [`state`]), so nothing automatic ever puts zone back where someone took it out.

pub mod jsonc;
pub mod state;
pub mod toml;
pub mod yaml;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::paths;
use state::State;
use yaml::Yaml;

/// The name the server is registered under.
pub const SERVER_NAME: &str = "zone";

// -------------------------------------------------------------------- where to look

/// The folders and settings that decide where clients keep their files, gathered once, so a
/// test can supply its own.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME`, or `~/.config`. Many command line tools use it on every system.
    pub xdg_config: PathBuf,
    /// Where desktop apps keep settings: `~/Library/Application Support` on macOS,
    /// `%APPDATA%` on Windows, the XDG config folder on Linux.
    pub app_config: PathBuf,
    /// `%LOCALAPPDATA%` on Windows, the XDG data folder elsewhere. Only Windows uses it so far.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub local_data: PathBuf,
    /// The settings each client lets an environment variable move.
    pub claude_config_dir: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub hermes_home: Option<PathBuf>,
    /// `GEMINI_CLI_HOME`: stands in for the home folder, so settings are in `<it>/.gemini`.
    pub gemini_home: Option<PathBuf>,
    /// `QWEN_HOME`: the `.qwen` folder itself.
    pub qwen_home: Option<PathBuf>,
    /// `COPILOT_HOME`: the `.copilot` folder itself.
    pub copilot_home: Option<PathBuf>,
    /// `GOOSE_PATH_ROOT`: Goose's settings are in `<it>/config`.
    pub goose_root: Option<PathBuf>,
    /// Folders to look for programs in: `PATH`, then any saved by `zone install`.
    pub search: Vec<PathBuf>,
}

impl Env {
    pub fn from_system() -> Result<Env> {
        let home = paths::home_dir()?;
        let xdg_config = paths::env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        #[cfg(target_os = "macos")]
        let (app_config, local_data) = (home.join("Library").join("Application Support"), home.join("Library").join("Application Support"));
        #[cfg(windows)]
        let (app_config, local_data) = (
            paths::env_path("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming")),
            paths::env_path("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local")),
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        let (app_config, local_data) = (xdg_config.clone(), paths::env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share")));
        let search = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        Ok(Env {
            home,
            xdg_config,
            app_config,
            local_data,
            claude_config_dir: paths::env_path("CLAUDE_CONFIG_DIR"),
            codex_home: paths::env_path("CODEX_HOME"),
            hermes_home: paths::env_path("HERMES_HOME"),
            gemini_home: paths::env_path("GEMINI_CLI_HOME"),
            qwen_home: paths::env_path("QWEN_HOME"),
            copilot_home: paths::env_path("COPILOT_HOME"),
            goose_root: paths::env_path("GOOSE_PATH_ROOT"),
            search,
        })
    }

    /// Also looks for programs in the folders of a saved `PATH`.
    pub fn with_saved_path(mut self, saved: Option<&str>) -> Env {
        if let Some(saved) = saved {
            for dir in std::env::split_paths(saved) {
                if !self.search.contains(&dir) {
                    self.search.push(dir);
                }
            }
        }
        self
    }

    /// A program in the search folders.
    pub fn find_program(&self, name: &str) -> Option<PathBuf> {
        #[cfg(windows)]
        let candidates = [format!("{name}.exe"), format!("{name}.cmd"), name.to_string()];
        #[cfg(not(windows))]
        let candidates = [name.to_string()];
        self.search.iter().flat_map(|dir| candidates.iter().map(move |c| dir.join(c))).find(|full| full.is_file())
    }

    fn has_program(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.find_program(n).is_some())
    }

    /// A path for people to read: under the home folder it starts with `~`.
    pub fn show(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) if !rest.as_os_str().is_empty() => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
            _ => path.display().to_string(),
        }
    }

    /// VS Code's settings folder, or one of its siblings (Insiders, VSCodium).
    fn vscode_user(&self, product: &str) -> PathBuf {
        self.app_config.join(product).join("User")
    }

    /// Hermes: `HERMES_HOME`, else `~/.hermes` (`%LOCALAPPDATA%\hermes` on Windows).
    fn hermes_dir(&self) -> PathBuf {
        if let Some(dir) = &self.hermes_home {
            return dir.clone();
        }
        #[cfg(windows)]
        return self.local_data.join("hermes");
        #[cfg(not(windows))]
        return self.home.join(".hermes");
    }

    /// Zed: `~/.config/zed` on macOS (whatever XDG says), the XDG folder on Linux,
    /// `%APPDATA%\Zed` on Windows.
    fn zed_dir(&self) -> PathBuf {
        #[cfg(windows)]
        return self.app_config.join("Zed");
        #[cfg(target_os = "macos")]
        return self.home.join(".config").join("zed");
        #[cfg(all(unix, not(target_os = "macos")))]
        return self.xdg_config.join("zed");
    }

    fn gemini_dir(&self) -> PathBuf {
        self.gemini_home.as_ref().unwrap_or(&self.home).join(".gemini")
    }

    fn qwen_dir(&self) -> PathBuf {
        self.qwen_home.clone().unwrap_or_else(|| self.home.join(".qwen"))
    }

    fn copilot_dir(&self) -> PathBuf {
        self.copilot_home.clone().unwrap_or_else(|| self.home.join(".copilot"))
    }

    /// LM Studio's folder can be moved; `~/.lmstudio-home-pointer` then says where it is.
    fn lmstudio_dir(&self) -> PathBuf {
        std::fs::read_to_string(self.home.join(".lmstudio-home-pointer"))
            .ok()
            .map(|text| PathBuf::from(text.trim()))
            .filter(|dir| dir.is_absolute() && dir.is_dir())
            .unwrap_or_else(|| self.home.join(".lmstudio"))
    }

    fn goose_config(&self) -> PathBuf {
        if let Some(root) = &self.goose_root {
            return root.join("config").join("config.yaml");
        }
        #[cfg(windows)]
        return self.app_config.join("Block").join("goose").join("config").join("config.yaml");
        #[cfg(not(windows))]
        return self.xdg_config.join("goose").join("config.yaml");
    }

    /// Crush: the XDG config folder, or `~/.config`, on every system.
    fn crush_config(&self) -> PathBuf {
        self.xdg_config.join("crush").join("crush.json")
    }

    /// Amp: `~/.config/amp/settings.json`, or `settings.jsonc`, on every system.
    fn amp_settings(&self) -> PathBuf {
        either(&self.home.join(".config").join("amp"), "settings.json", "settings.jsonc")
    }

    /// opencode: the XDG config folder, or `~/.config`, on every system (Windows too).
    fn opencode_config(&self) -> PathBuf {
        either(&self.xdg_config.join("opencode"), "opencode.json", "opencode.jsonc")
    }
}

// -------------------------------------------------------------------- the clients

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Cursor,
    Hermes,
    Gemini,
    Qwen,
    OpenCode,
    Windsurf,
    VsCode,
    Zed,
    Kiro,
    Droid,
    Copilot,
    LmStudio,
    Goose,
    Crush,
    Amp,
    Cline,
    Roo,
}

impl Client {
    pub const ALL: [Client; 20] = [
        Client::ClaudeCode,
        Client::ClaudeDesktop,
        Client::Codex,
        Client::Cursor,
        Client::Hermes,
        Client::Gemini,
        Client::Qwen,
        Client::OpenCode,
        Client::Windsurf,
        Client::VsCode,
        Client::Zed,
        Client::Kiro,
        Client::Droid,
        Client::Copilot,
        Client::LmStudio,
        Client::Goose,
        Client::Crush,
        Client::Amp,
        Client::Cline,
        Client::Roo,
    ];

    pub fn parse(name: &str) -> Result<Client> {
        let wanted = name.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        let found = match wanted.as_str() {
            "claude-code" | "claude" | "claudecode" => Client::ClaudeCode,
            "claude-desktop" | "claudedesktop" | "desktop" => Client::ClaudeDesktop,
            "codex" => Client::Codex,
            "cursor" => Client::Cursor,
            "hermes" | "hermes-agent" => Client::Hermes,
            "gemini" | "gemini-cli" => Client::Gemini,
            "qwen" | "qwen-code" => Client::Qwen,
            "opencode" | "open-code" => Client::OpenCode,
            "windsurf" => Client::Windsurf,
            "vscode" | "vs-code" | "code" => Client::VsCode,
            "zed" => Client::Zed,
            "kiro" => Client::Kiro,
            "droid" | "factory" | "factory-droid" => Client::Droid,
            "copilot" | "copilot-cli" | "github-copilot" => Client::Copilot,
            "lm-studio" | "lmstudio" => Client::LmStudio,
            "goose" => Client::Goose,
            "crush" => Client::Crush,
            "amp" => Client::Amp,
            "cline" => Client::Cline,
            "roo" | "roo-code" => Client::Roo,
            other => {
                let names: Vec<&str> = Client::ALL.iter().map(|c| c.name()).collect();
                bail!("unknown client \"{other}\". Choose one of: {}, or --all.", names.join(", "))
            }
        };
        Ok(found)
    }

    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeDesktop => "claude-desktop",
            Client::Codex => "codex",
            Client::Cursor => "cursor",
            Client::Hermes => "hermes",
            Client::Gemini => "gemini",
            Client::Qwen => "qwen",
            Client::OpenCode => "opencode",
            Client::Windsurf => "windsurf",
            Client::VsCode => "vscode",
            Client::Zed => "zed",
            Client::Kiro => "kiro",
            Client::Droid => "droid",
            Client::Copilot => "copilot",
            Client::LmStudio => "lm-studio",
            Client::Goose => "goose",
            Client::Crush => "crush",
            Client::Amp => "amp",
            Client::Cline => "cline",
            Client::Roo => "roo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Codex => "Codex",
            Client::Cursor => "Cursor",
            Client::Hermes => "Hermes",
            Client::Gemini => "Gemini CLI",
            Client::Qwen => "Qwen Code",
            Client::OpenCode => "opencode",
            Client::Windsurf => "Windsurf",
            Client::VsCode => "VS Code",
            Client::Zed => "Zed",
            Client::Kiro => "Kiro",
            Client::Droid => "Factory Droid",
            Client::Copilot => "Copilot CLI",
            Client::LmStudio => "LM Studio",
            Client::Goose => "Goose",
            Client::Crush => "Crush",
            Client::Amp => "Amp",
            Client::Cline => "Cline",
            Client::Roo => "Roo Code",
        }
    }

    /// Whether this client seems to be on this computer: its settings folder exists or its
    /// command is on the PATH.
    pub fn installed(self, env: &Env) -> bool {
        let h = &env.home;
        match self {
            Client::ClaudeCode => {
                env.has_program(&["claude"]) || h.join(".claude.json").exists() || h.join(".claude").is_dir() || claude_profiles(env).len() > 1
            }
            Client::ClaudeDesktop => claude_desktop_dir(env).is_dir(),
            Client::Codex => env.has_program(&["codex"]) || codex_homes(env).iter().any(|(_, dir)| dir.is_dir()),
            Client::Cursor => env.has_program(&["cursor", "cursor-agent"]) || h.join(".cursor").is_dir(),
            Client::Hermes => env.has_program(&["hermes"]) || env.hermes_dir().join("config.yaml").exists(),
            Client::Gemini => env.has_program(&["gemini"]) || env.gemini_dir().is_dir(),
            Client::Qwen => env.has_program(&["qwen"]) || env.qwen_dir().is_dir(),
            Client::OpenCode => env.has_program(&["opencode"]) || env.xdg_config.join("opencode").is_dir(),
            Client::Windsurf => env.has_program(&["windsurf"]) || h.join(".codeium").join("windsurf").is_dir(),
            Client::VsCode => env.has_program(&["code"]) || env.vscode_user("Code").is_dir() || env.vscode_user("Code - Insiders").is_dir(),
            Client::Zed => env.has_program(&["zed"]) || env.zed_dir().join("settings.json").exists(),
            Client::Kiro => env.has_program(&["kiro", "kiro-cli"]) || h.join(".kiro").is_dir(),
            Client::Droid => env.has_program(&["droid"]) || h.join(".factory").is_dir(),
            Client::Copilot => env.has_program(&["copilot"]) || env.copilot_dir().is_dir(),
            Client::LmStudio => env.has_program(&["lms"]) || env.lmstudio_dir().is_dir(),
            // These folders can exist for other reasons (shared skills), so only the
            // settings file or the command counts.
            Client::Goose => env.has_program(&["goose"]) || env.goose_config().exists(),
            Client::Crush => env.has_program(&["crush"]) || env.crush_config().exists(),
            Client::Amp => env.has_program(&["amp"]) || env.amp_settings().exists(),
            Client::Cline => env.has_program(&["cline"]) || cline_cli_file(env).exists() || cline_extension_dir(env).is_dir(),
            Client::Roo => roo_dir(env).is_dir(),
        }
    }

    /// Every place this client reads its MCP servers from.
    pub fn targets(self, env: &Env) -> Vec<Target> {
        let one = |path: PathBuf, format: Format| vec![Target { client: self, profile: None, path, format, claude_dir: None }];
        let h = &env.home;
        match self {
            Client::ClaudeCode => claude_profiles(env)
                .into_iter()
                .map(|(profile, dir, path)| Target { client: self, profile: Some(profile), path, format: Format::Json(&["mcpServers"], Style::ClaudeCode), claude_dir: Some(dir) })
                .collect(),
            Client::ClaudeDesktop => one(claude_desktop_dir(env).join("claude_desktop_config.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::Codex => {
                let homes = codex_homes(env);
                let several = homes.len() > 1;
                homes
                    .into_iter()
                    .map(|(profile, dir)| Target {
                        client: self,
                        profile: several.then_some(profile),
                        path: dir.join("config.toml"),
                        format: Format::Toml,
                        claude_dir: None,
                    })
                    .collect()
            }
            Client::Cursor => one(h.join(".cursor").join("mcp.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::Hermes => {
                let home = env.hermes_dir();
                let mut list = one(home.join("config.yaml"), Format::Yaml("mcp_servers", Style::Plain));
                for (name, dir) in subfolders(&home.join("profiles")) {
                    if dir.join("config.yaml").is_file() {
                        list.push(Target {
                            client: self,
                            profile: Some(format!("profile {name}")),
                            path: dir.join("config.yaml"),
                            format: Format::Yaml("mcp_servers", Style::Plain),
                            claude_dir: None,
                        });
                    }
                }
                if list.len() > 1 {
                    list[0].profile = Some("default".to_string());
                }
                list
            }
            Client::Gemini => one(env.gemini_dir().join("settings.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::Qwen => one(env.qwen_dir().join("settings.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::OpenCode => one(env.opencode_config(), Format::Json(&["mcp"], Style::OpenCode)),
            Client::Windsurf => one(h.join(".codeium").join("windsurf").join("mcp_config.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::VsCode => {
                let mut list = one(env.vscode_user("Code").join("mcp.json"), Format::Json(&["servers"], Style::Typed("stdio")));
                let insiders = env.vscode_user("Code - Insiders");
                if insiders.is_dir() {
                    list.push(Target {
                        client: self,
                        profile: Some("Insiders".to_string()),
                        path: insiders.join("mcp.json"),
                        format: Format::Json(&["servers"], Style::Typed("stdio")),
                        claude_dir: None,
                    });
                }
                list
            }
            Client::Zed => one(env.zed_dir().join("settings.json"), Format::Json(&["context_servers"], Style::Plain)),
            Client::Kiro => one(h.join(".kiro").join("settings").join("mcp.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::Droid => one(h.join(".factory").join("mcp.json"), Format::Json(&["mcpServers"], Style::Typed("stdio"))),
            Client::Copilot => one(env.copilot_dir().join("mcp-config.json"), Format::Json(&["mcpServers"], Style::Copilot)),
            Client::LmStudio => one(env.lmstudio_dir().join("mcp.json"), Format::Json(&["mcpServers"], Style::Plain)),
            Client::Goose => one(env.goose_config(), Format::Yaml("extensions", Style::Goose)),
            Client::Crush => one(env.crush_config(), Format::Json(&["mcp"], Style::Typed("stdio"))),
            Client::Amp => one(env.amp_settings(), Format::Json(&["amp.mcpServers"], Style::Plain)),
            Client::Cline => {
                let cli = Target { client: self, profile: Some("CLI".to_string()), path: cline_cli_file(env), format: Format::Json(&["mcpServers"], Style::Plain), claude_dir: None };
                let extension = cline_extension_dir(env);
                let cli_present = env.has_program(&["cline"]) || h.join(".cline").is_dir();
                let mut list = Vec::new();
                if cli_present || !extension.is_dir() {
                    list.push(cli);
                }
                if extension.is_dir() {
                    list.push(Target {
                        client: self,
                        profile: Some("VS Code".to_string()),
                        path: extension.join("settings").join("cline_mcp_settings.json"),
                        format: Format::Json(&["mcpServers"], Style::Plain),
                        claude_dir: None,
                    });
                }
                list
            }
            Client::Roo => one(roo_dir(env).join("settings").join("mcp_settings.json"), Format::Json(&["mcpServers"], Style::Plain)),
        }
    }
}

/// `dir/first`, unless only `dir/second` exists.
fn either(dir: &Path, first: &str, second: &str) -> PathBuf {
    let (a, b) = (dir.join(first), dir.join(second));
    if b.exists() && !a.exists() { b } else { a }
}

fn claude_desktop_dir(env: &Env) -> PathBuf {
    env.app_config.join("Claude")
}

fn cline_cli_file(env: &Env) -> PathBuf {
    env.home.join(".cline").join("data").join("settings").join("cline_mcp_settings.json")
}

fn cline_extension_dir(env: &Env) -> PathBuf {
    env.vscode_user("Code").join("globalStorage").join("saoudrizwan.claude-dev")
}

fn roo_dir(env: &Env) -> PathBuf {
    env.vscode_user("Code").join("globalStorage").join("rooveterinaryinc.roo-cline")
}

/// The folders inside a folder, by name, sorted.
fn subfolders(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut list: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().filter(|e| e.path().is_dir()).map(|e| (e.file_name().to_string_lossy().to_string(), e.path())).collect()
        })
        .unwrap_or_default();
    list.sort();
    list
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// The file a Claude Code profile keeps its settings in. Claude Code still prefers an older
/// `.config.json` in the profile's folder when that is there and not empty.
fn claude_file(config_home: &Path, main: PathBuf) -> PathBuf {
    let legacy = config_home.join(".config.json");
    match std::fs::metadata(&legacy) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => legacy,
        _ => main,
    }
}

/// Claude Code's profiles: (label, `CLAUDE_CONFIG_DIR` to use, the file it reads).
///
/// The default (no `CLAUDE_CONFIG_DIR`) reads `~/.claude.json`. With `CLAUDE_CONFIG_DIR` set,
/// Claude Code reads `.claude.json` inside that folder. A folder in the home folder whose name
/// starts with `.claude` and which holds a `.claude.json` is such a profile, and so is the
/// folder `CLAUDE_CONFIG_DIR` names right now.
pub fn claude_profiles(env: &Env) -> Vec<(String, Option<PathBuf>, PathBuf)> {
    let default = claude_file(&env.home.join(".claude"), env.home.join(".claude.json"));
    let mut list: Vec<(String, Option<PathBuf>, PathBuf)> = vec![("default".to_string(), None, default)];
    let mut dirs: Vec<PathBuf> = env.claude_config_dir.iter().cloned().collect();
    for (name, dir) in subfolders(&env.home) {
        if name.starts_with(".claude") && dir.join(".claude.json").is_file() {
            dirs.push(dir);
        }
    }
    for dir in dirs {
        let file = claude_file(&dir, dir.join(".claude.json"));
        if list.iter().any(|(_, _, existing)| same_file(existing, &file)) {
            continue;
        }
        list.push((env.show(&dir), Some(dir), file));
    }
    list
}

/// Codex's homes: (label, folder). `~/.codex` by default, `CODEX_HOME` when set, and any
/// folder in the home folder whose name starts with `.codex` and holds a `config.toml`.
pub fn codex_homes(env: &Env) -> Vec<(String, PathBuf)> {
    let mut list = vec![("default".to_string(), env.home.join(".codex"))];
    let mut dirs: Vec<PathBuf> = env.codex_home.iter().cloned().collect();
    for (name, dir) in subfolders(&env.home) {
        if name.starts_with(".codex") && dir.join("config.toml").is_file() {
            dirs.push(dir);
        }
    }
    for dir in dirs {
        if list.iter().any(|(_, existing)| same_file(existing, &dir)) {
            continue;
        }
        list.push((env.show(&dir), dir));
    }
    list
}

// -------------------------------------------------------------------- formats

/// How one client writes a server entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `{"command", "args", "env"}`, the shape most clients share.
    Plain,
    /// The same with `"type"` set to this.
    Typed(&'static str),
    /// What `claude mcp add` writes: `"type": "stdio"` and an `env` object, always.
    ClaudeCode,
    /// opencode: `{"type": "local", "command": [program, args...], "enabled": true, "environment"}`.
    OpenCode,
    /// Copilot CLI: `{"type": "local", "command", "args", "env", "tools": ["*"]}`.
    Copilot,
    /// Goose's extension: `{name, type: stdio, cmd, args, envs, enabled, timeout}`.
    Goose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A JSON (or JSONC) file. The entry goes at this path of keys, under `zone`.
    Json(&'static [&'static str], Style),
    /// Codex's TOML.
    Toml,
    /// A YAML file. The entry goes in the mapping under this top-level key, as `zone`.
    Yaml(&'static str, Style),
}

/// One place a client reads its servers from.
#[derive(Clone, Debug)]
pub struct Target {
    pub client: Client,
    /// Which one, for clients with several: `default`, `~/.claude_work`, `profile work`.
    pub profile: Option<String>,
    pub path: PathBuf,
    pub format: Format,
    /// Claude Code only: the `CLAUDE_CONFIG_DIR` to run `claude` with (None: unset).
    pub claude_dir: Option<Option<PathBuf>>,
}

impl Target {
    pub fn label(&self) -> String {
        match &self.profile {
            Some(profile) => format!("{} ({profile})", self.client.label()),
            None => self.client.label().to_string(),
        }
    }

    /// How the state file names this place.
    pub fn id(&self) -> String {
        format!("{}:{}", self.client.name(), self.path.display())
    }
}

/// How a client should start the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    /// Extra environment, only when the data directory is overridden.
    pub env: Vec<(String, String)>,
}

impl Launch {
    pub fn current(read_only: bool) -> Result<Launch> {
        let exe = paths::current_exe()?;
        let mut args = vec!["mcp".to_string()];
        if read_only {
            args.push("--read-only".to_string());
        }
        let env = match paths::env_path(paths::ENV_HOME) {
            Some(home) => vec![(paths::ENV_HOME.to_string(), home.to_string_lossy().to_string())],
            None => Vec::new(),
        };
        Ok(Launch { command: exe.to_string_lossy().to_string(), args, env })
    }

    pub fn read_only(&self) -> bool {
        self.args.iter().any(|a| a == "--read-only")
    }

    fn env_object(&self) -> Value {
        Value::Object(self.env.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<_, _>>())
    }

    /// The JSON entry in a client's style.
    pub fn json_entry(&self, style: Style) -> Value {
        let mut entry = Map::new();
        let has_env = !self.env.is_empty();
        match style {
            Style::Plain | Style::Typed(_) | Style::ClaudeCode | Style::Copilot => {
                match style {
                    Style::Typed(kind) => {
                        entry.insert("type".into(), json!(kind));
                    }
                    Style::ClaudeCode => {
                        entry.insert("type".into(), json!("stdio"));
                    }
                    Style::Copilot => {
                        entry.insert("type".into(), json!("local"));
                    }
                    _ => {}
                }
                entry.insert("command".into(), json!(self.command));
                entry.insert("args".into(), json!(self.args));
                if has_env || style == Style::ClaudeCode {
                    entry.insert("env".into(), self.env_object());
                }
                if style == Style::Copilot {
                    entry.insert("tools".into(), json!(["*"]));
                }
            }
            Style::OpenCode => {
                let mut command = vec![self.command.clone()];
                command.extend(self.args.iter().cloned());
                entry.insert("type".into(), json!("local"));
                entry.insert("command".into(), json!(command));
                entry.insert("enabled".into(), json!(true));
                if has_env {
                    entry.insert("environment".into(), self.env_object());
                }
            }
            Style::Goose => unreachable!("Goose is YAML"),
        }
        Value::Object(entry)
    }

    /// The YAML entry's fields in a client's style.
    pub fn yaml_fields(&self, style: Style) -> Vec<(&'static str, Yaml)> {
        let env = Yaml::Map(self.env.clone());
        match style {
            Style::Goose => vec![
                ("name", Yaml::Str(SERVER_NAME.to_string())),
                ("type", Yaml::Str("stdio".to_string())),
                ("enabled", Yaml::Bool(true)),
                ("cmd", Yaml::Str(self.command.clone())),
                ("args", Yaml::List(self.args.clone())),
                ("envs", env),
                ("timeout", Yaml::Int(300)),
                ("description", Yaml::Str("Your Zone dictations, notes and meeting transcripts".to_string())),
            ],
            _ => {
                let mut fields = vec![("command", Yaml::Str(self.command.clone())), ("args", Yaml::List(self.args.clone()))];
                if !self.env.is_empty() {
                    fields.push(("env", env));
                }
                fields
            }
        }
    }

    /// Reads an entry back from JSON, whichever of the known shapes it has.
    fn from_json(entry: &Value) -> Option<Launch> {
        let strings = |v: Option<&Value>| -> Vec<String> { v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default() };
        let env_of = |v: Option<&Value>| -> Vec<(String, String)> {
            v.and_then(Value::as_object).map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default()
        };
        let env = { let mut e = env_of(entry.get("env")); e.extend(env_of(entry.get("environment"))); e };
        match entry.get("command")? {
            Value::String(command) => Some(Launch { command: command.clone(), args: strings(entry.get("args")), env }),
            Value::Array(parts) => {
                let mut parts = parts.iter().filter_map(Value::as_str).map(str::to_string);
                Some(Launch { command: parts.next()?, args: parts.collect(), env })
            }
            // Zed's older shape, which Zed itself still migrates: {"command": {"path", "args", "env"}}.
            Value::Object(inner) => Some(Launch {
                command: inner.get("path")?.as_str()?.to_string(),
                args: strings(inner.get("args")),
                env: env_of(inner.get("env")),
            }),
            _ => None,
        }
    }

    fn from_yaml(fields: &[(String, Yaml)]) -> Option<Launch> {
        let field = |names: &[&str]| fields.iter().find(|(k, _)| names.contains(&k.as_str())).map(|(_, v)| v.clone());
        let Some(Yaml::Str(command)) = field(&["command", "cmd"]) else { return None };
        let args = match field(&["args"]) {
            Some(Yaml::List(args)) => args,
            _ => Vec::new(),
        };
        let env = match field(&["env", "envs"]) {
            Some(Yaml::Map(env)) => env,
            _ => Vec::new(),
        };
        Some(Launch { command, args, env })
    }
}

// -------------------------------------------------------------------- reading and writing files

fn read_text(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Keeps one copy of the file as it was before zone first touched it.
fn keep_original(path: &Path) {
    let backup = PathBuf::from(format!("{}.before-zone", path.display()));
    if path.exists() && !backup.exists() {
        let _ = std::fs::copy(path, &backup);
    }
}

/// Replaces a file's contents in one step. A link is followed, so a config kept elsewhere
/// (by a dotfiles manager, say) stays a link, and the file keeps its permissions.
fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let tmp = PathBuf::from(format!("{}.zone-tmp", real.display()));
    std::fs::write(&tmp, contents).with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Ok(metadata) = std::fs::metadata(&real) {
        let _ = std::fs::set_permissions(&tmp, metadata.permissions());
    }
    std::fs::rename(&tmp, &real).with_context(|| format!("cannot write {}", real.display()))
}

fn save(path: &Path, text: &str) -> Result<()> {
    keep_original(path);
    write_atomically(path, text)
}

/// Zone's entry as a target's file has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Absent,
    Present(Launch),
    /// The file cannot be read as its format.
    Unreadable(String),
}

pub fn read_entry(target: &Target, env: &Env) -> Found {
    let text = match read_text(&target.path) {
        Ok(text) => text,
        Err(error) => return Found::Unreadable(format!("{error:#}")),
    };
    match target.format {
        Format::Json(keys, _) => {
            let mut path: Vec<&str> = keys.to_vec();
            path.push(SERVER_NAME);
            match jsonc::get(&text, &path) {
                Ok(Some(entry)) => Launch::from_json(&entry).map(Found::Present).unwrap_or(Found::Present(Launch { command: String::new(), args: Vec::new(), env: Vec::new() })),
                Ok(None) => Found::Absent,
                Err(problem) => Found::Unreadable(format!("{} {problem}", env.show(&target.path))),
            }
        }
        Format::Toml => toml::registration(&text).map(Found::Present).unwrap_or(Found::Absent),
        Format::Yaml(key, _) => match yaml::get(&text, key, SERVER_NAME) {
            Some(fields) => Found::Present(Launch::from_yaml(&fields).unwrap_or(Launch { command: String::new(), args: Vec::new(), env: Vec::new() })),
            None => Found::Absent,
        },
    }
}

/// Writes zone's entry into a target's file. Returns false when it was already exactly so.
fn write_entry(target: &Target, launch: &Launch, env: &Env) -> Result<bool> {
    let text = read_text(&target.path)?;
    let shown = env.show(&target.path);
    let changed = match target.format {
        Format::Json(keys, style) => {
            let mut path: Vec<&str> = keys.to_vec();
            path.push(SERVER_NAME);
            jsonc::set(&text, &path, &launch.json_entry(style)).map_err(|problem| anyhow::anyhow!("{shown} {problem}, so zone will not change it"))?
        }
        Format::Toml => toml::with_zone(&text, launch),
        Format::Yaml(key, style) => yaml::set(&text, key, SERVER_NAME, &launch.yaml_fields(style)).map_err(|why| anyhow::anyhow!("in {shown}, {why}"))?,
    };
    match changed {
        Some(new_text) => {
            save(&target.path, &new_text)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Takes zone's entry out of a target's file. Returns false when it was not there.
fn erase_entry(target: &Target, env: &Env) -> Result<bool> {
    if !target.path.exists() {
        return Ok(false);
    }
    let text = read_text(&target.path)?;
    let changed = match target.format {
        Format::Json(keys, _) => {
            let mut path: Vec<&str> = keys.to_vec();
            path.push(SERVER_NAME);
            jsonc::remove(&text, &path).map_err(|problem| anyhow::anyhow!("{} {problem}, so zone will not change it", env.show(&target.path)))?
        }
        Format::Toml => {
            let (rest, found) = toml::without_zone(&text);
            found.then_some(rest)
        }
        Format::Yaml(key, _) => yaml::remove(&text, key, SERVER_NAME),
    };
    match changed {
        Some(new_text) => {
            save(&target.path, &new_text)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

// -------------------------------------------------------------------- Claude Code's own command

/// The command to type by hand, for when `claude` cannot be run.
fn claude_code_manual(launch: &Launch, dir: Option<&Path>) -> String {
    let mut command = String::new();
    if let Some(dir) = dir {
        command.push_str(&format!("CLAUDE_CONFIG_DIR=\"{}\" ", dir.display()));
    }
    command.push_str(&format!("claude mcp add --scope user {SERVER_NAME}"));
    for (key, value) in &launch.env {
        command.push_str(&format!(" --env {key}=\"{value}\""));
    }
    command.push_str(&format!(" -- \"{}\" {}", launch.command, launch.args.join(" ")));
    command
}

fn claude_command(claude: &Path, dir: &Option<PathBuf>) -> std::process::Command {
    let mut command = std::process::Command::new(claude);
    match dir {
        Some(dir) => command.env("CLAUDE_CONFIG_DIR", dir),
        None => command.env_remove("CLAUDE_CONFIG_DIR"),
    };
    command.stdin(std::process::Stdio::null());
    command
}

/// Registers with one Claude Code profile through `claude mcp add`, which takes Claude Code's
/// own lock on its settings. Without the command, the profile's `.claude.json` is edited
/// directly, the same way the command would.
fn claude_code_write(target: &Target, launch: &Launch, env: &Env, existing: &Found) -> Result<()> {
    let dir = target.claude_dir.clone().flatten();
    let Some(claude) = env.find_program("claude") else {
        write_entry(target, launch, env)?;
        return Ok(());
    };
    if matches!(existing, Found::Present(_)) {
        let _ = claude_command(&claude, &target.claude_dir.clone().flatten()).args(["mcp", "remove", "--scope", "user", SERVER_NAME]).output();
    }
    let mut command = claude_command(&claude, &dir);
    command.args(["mcp", "add", "--scope", "user", SERVER_NAME]);
    for (key, value) in &launch.env {
        command.arg("--env").arg(format!("{key}={value}"));
    }
    command.arg("--").arg(&launch.command).args(&launch.args);
    let output = command.output().with_context(|| format!("cannot run {}", claude.display()))?;
    if !output.status.success() {
        bail!("`claude mcp add` failed: {}. To run it yourself: {}", String::from_utf8_lossy(&output.stderr).trim(), claude_code_manual(launch, dir.as_deref()));
    }
    match read_entry(target, env) {
        Found::Present(found) if found.command == launch.command => Ok(()),
        _ => bail!("`claude mcp add` ran, but zone is not in {} afterwards", env.show(&target.path)),
    }
}

fn claude_code_erase(target: &Target, env: &Env) -> Result<bool> {
    let Some(claude) = env.find_program("claude") else {
        return erase_entry(target, env);
    };
    let output = claude_command(&claude, &target.claude_dir.clone().flatten())
        .args(["mcp", "remove", "--scope", "user", SERVER_NAME])
        .output()
        .with_context(|| format!("cannot run {}", claude.display()))?;
    if !output.status.success() || matches!(read_entry(target, env), Found::Present(_)) {
        // The command may refuse a file it does not like. Take the entry out directly.
        return erase_entry(target, env);
    }
    Ok(true)
}

// -------------------------------------------------------------------- registering

/// How hard to try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `zone mcp install <client>`: this client, whatever the state says, replacing any older
    /// entry exactly.
    Explicit,
    /// `zone mcp install --all`, `zone install`: every installed client except the ones the
    /// person took zone out of. An entry that already starts this program is left as it is
    /// (it may be read-only on purpose).
    All,
    /// `zone serve` at start: like `All`, and also never changes an entry that is there.
    Automatic,
}

/// What happened at one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Registered,
    /// It started another copy of zone, or another mode, and now starts this one.
    Updated,
    Already,
    NotInstalled,
    /// The person took zone out of this one.
    OptedOut,
    /// An entry is there that starts another program, and this mode does not change entries.
    LeftAlone(String),
    Failed(String),
    Removed,
    NotThere,
}

impl Outcome {
    fn words(&self, target: &Target) -> String {
        match self {
            Outcome::Registered => "registered".to_string(),
            Outcome::Updated => "registered (replaced an older entry)".to_string(),
            Outcome::Already => "already registered".to_string(),
            Outcome::NotInstalled => "skipped: not installed".to_string(),
            Outcome::OptedOut => format!("skipped: you removed zone here (`zone mcp install {}` adds it back)", target.client.name()),
            Outcome::LeftAlone(command) => format!("left alone: it starts {command}"),
            Outcome::Failed(why) => format!("failed: {why}"),
            Outcome::Removed => "removed".to_string(),
            Outcome::NotThere => "was not registered".to_string(),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Outcome::Failed(_))
    }
}

/// Registers with one target, following the mode and updating the state.
pub fn register(target: &Target, launch: &Launch, env: &Env, state: &mut State, mode: Mode) -> Outcome {
    let id = target.id();
    let existing = read_entry(target, env);
    if let Found::Unreadable(why) = &existing {
        return Outcome::Failed(why.clone());
    }
    if mode != Mode::Explicit {
        if state.removed.contains(&id) {
            return Outcome::OptedOut;
        }
        match &existing {
            Found::Present(found) if found.command == launch.command => {
                state.added(&id);
                return Outcome::Already;
            }
            Found::Present(found) if mode == Mode::Automatic => return Outcome::LeftAlone(found.command.clone()),
            // Zone added itself here once and it is gone now: someone took it out by hand.
            Found::Absent if state.registered.contains(&id) => {
                state.taken_out(&id);
                return Outcome::OptedOut;
            }
            _ => {}
        }
    }
    if existing == Found::Present(launch.clone()) {
        state.added(&id);
        return Outcome::Already;
    }
    let written = match target.client {
        Client::ClaudeCode => claude_code_write(target, launch, env, &existing).map(|()| true),
        _ => write_entry(target, launch, env),
    };
    match written {
        Ok(changed) => {
            state.added(&id);
            match (changed, &existing) {
                (false, _) => Outcome::Already,
                (true, Found::Present(_)) => Outcome::Updated,
                (true, _) => Outcome::Registered,
            }
        }
        Err(error) => Outcome::Failed(format!("{error:#}")),
    }
}

/// Takes zone out of one target and remembers that the person did.
pub fn unregister(target: &Target, env: &Env, state: &mut State, remember: bool) -> Outcome {
    let removed = match target.client {
        Client::ClaudeCode if matches!(read_entry(target, env), Found::Present(_)) => claude_code_erase(target, env),
        Client::ClaudeCode => Ok(false),
        _ => erase_entry(target, env),
    };
    match removed {
        Ok(removed) => {
            if remember {
                state.taken_out(&target.id());
            } else {
                state.registered.remove(&target.id());
            }
            if removed { Outcome::Removed } else { Outcome::NotThere }
        }
        Err(error) => Outcome::Failed(format!("{error:#}")),
    }
}

/// One line of a report: who, what happened, where.
fn report_line(env: &Env, target: &Target, outcome: &Outcome, width: usize) -> String {
    let words = outcome.words(target);
    match outcome {
        Outcome::Registered | Outcome::Updated | Outcome::Already | Outcome::Removed => {
            format!("  {:width$}  {:22}  {}", target.label(), words, env.show(&target.path))
        }
        _ => format!("  {:width$}  {words}", target.label()),
    }
}

/// Registers with every installed client, or with the given ones. Prints one line per place.
/// Returns how many places failed.
pub fn register_many(clients: &[Client], launch: &Launch, mode: Mode, env: &Env, state: &mut State, quiet_when_missing: bool) -> usize {
    let mut rows: Vec<(Target, Outcome)> = Vec::new();
    for &client in clients {
        if mode != Mode::Explicit && !client.installed(env) {
            if !quiet_when_missing {
                let target = Target { client, profile: None, path: PathBuf::new(), format: Format::Toml, claude_dir: None };
                rows.push((target, Outcome::NotInstalled));
            }
            continue;
        }
        for target in client.targets(env) {
            let outcome = register(&target, launch, env, state, mode);
            rows.push((target, outcome));
        }
    }
    let width = rows.iter().map(|(t, _)| t.label().chars().count()).max().unwrap_or(10);
    for (target, outcome) in &rows {
        println!("{}", report_line(env, target, outcome, width));
    }
    rows.iter().filter(|(_, o)| o.is_failure()).count()
}

fn data_dir() -> Result<PathBuf> {
    paths::data_dir()
}

fn save_state(state: &State) {
    if let Ok(dir) = data_dir()
        && let Err(error) = state.save(&dir)
    {
        eprintln!("zone could not save what it registered ({error:#}). It will still work.");
    }
}

/// Which clients a command means: the names given, or every one for `--all` (or `detected`).
pub fn chosen(names: &[String], all: bool) -> Result<(Vec<Client>, bool)> {
    let all = all || names.iter().any(|n| matches!(n.to_ascii_lowercase().as_str(), "all" | "detected" | "--all"));
    if all {
        return Ok((Client::ALL.to_vec(), true));
    }
    if names.is_empty() {
        let list: Vec<&str> = Client::ALL.iter().map(|c| c.name()).collect();
        bail!("say which client, or --all for every one on this computer. Clients: {}", list.join(", "));
    }
    let mut clients = Vec::new();
    for name in names {
        let client = Client::parse(name)?;
        if !clients.contains(&client) {
            clients.push(client);
        }
    }
    Ok((clients, false))
}

fn closing_lines(launch: &Launch) {
    println!();
    println!("It runs: {} {}", launch.command, launch.args.join(" "));
    if launch.read_only() {
        println!("Read-only: agents cannot flag items as processed.");
    }
    println!("New agent sessions have it. Apps that were already open (Claude Desktop, Cursor, an editor) need a restart.");
}

/// `zone mcp install`.
pub fn install(names: &[String], all: bool, read_only: bool) -> Result<()> {
    let (clients, all) = chosen(names, all)?;
    let launch = Launch::current(read_only)?;
    let env = Env::from_system()?;
    let mut state = data_dir().map(|d| State::load(&d)).unwrap_or_default();
    if all {
        println!("Registering zone with every AI client on this computer:");
        println!();
    }
    let failed = register_many(&clients, &launch, if all { Mode::All } else { Mode::Explicit }, &env, &mut state, false);
    save_state(&state);
    closing_lines(&launch);
    if failed > 0 {
        bail!("{failed} {} could not be registered (see above)", if failed == 1 { "place" } else { "places" });
    }
    Ok(())
}

/// `zone mcp uninstall`.
pub fn uninstall(names: &[String], all: bool) -> Result<()> {
    let (clients, all) = chosen(names, all)?;
    let env = Env::from_system()?;
    let mut state = data_dir().map(|d| State::load(&d)).unwrap_or_default();
    let mut rows = Vec::new();
    for client in clients {
        for target in client.targets(&env) {
            // After --all nothing is automatic any more, so there is nothing to remember: a
            // later `zone install` or `zone mcp install --all` may add zone back everywhere.
            let outcome = unregister(&target, &env, &mut state, !all);
            if all && outcome == Outcome::NotThere {
                continue;
            }
            rows.push((target, outcome));
        }
    }
    if all {
        state.auto = false;
        if rows.is_empty() {
            println!("zone was not registered with any client.");
        }
    }
    let width = rows.iter().map(|(t, _)| t.label().chars().count()).max().unwrap_or(10);
    for (target, outcome) in &rows {
        println!("{}", report_line(&env, target, outcome, width));
    }
    save_state(&state);
    if !all && rows.iter().any(|(_, o)| *o == Outcome::Removed) {
        println!();
        println!("zone will not add itself back there by itself. `zone mcp install <client>` does.");
    }
    let failed = rows.iter().filter(|(_, o)| o.is_failure()).count();
    if failed > 0 {
        bail!("{failed} {} could not be changed (see above)", if failed == 1 { "place" } else { "places" });
    }
    Ok(())
}

/// Where zone stands with one target, for `status` and `doctor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    Registered { read_only: bool },
    OtherCopy(String),
    NotRegistered,
    OptedOut,
    Unreadable(String),
}

pub struct Row {
    pub target: Target,
    pub installed: bool,
    pub standing: Standing,
}

/// Every place of every client, read without changing anything.
pub fn survey(env: &Env, state: &State, current: &Launch) -> Vec<Row> {
    let mut rows = Vec::new();
    for client in Client::ALL {
        let installed = client.installed(env);
        for target in client.targets(env) {
            let standing = match read_entry(&target, env) {
                Found::Present(found) if found.command == current.command => Standing::Registered { read_only: found.read_only() },
                Found::Present(found) => Standing::OtherCopy(found.command),
                Found::Absent if state.removed.contains(&target.id()) || state.registered.contains(&target.id()) => Standing::OptedOut,
                Found::Absent => Standing::NotRegistered,
                Found::Unreadable(why) => Standing::Unreadable(why),
            };
            rows.push(Row { target, installed, standing });
        }
    }
    rows
}

/// `zone mcp status`.
pub fn status(as_json: bool) -> Result<()> {
    let current = Launch::current(false)?;
    let env = Env::from_system()?;
    let state = data_dir().map(|d| State::load(&d)).unwrap_or_default();
    let rows = survey(&env, &state, &current);
    if as_json {
        let list: Vec<Value> = rows
            .iter()
            .map(|row| {
                let (standing, detail) = match &row.standing {
                    Standing::Registered { read_only } => (if *read_only { "registered_read_only" } else { "registered" }, None),
                    Standing::OtherCopy(command) => ("other_copy", Some(command.clone())),
                    Standing::NotRegistered => ("not_registered", None),
                    Standing::OptedOut => ("removed_by_you", None),
                    Standing::Unreadable(why) => ("unreadable", Some(why.clone())),
                };
                json!({
                    "client": row.target.client.name(),
                    "label": row.target.label(),
                    "profile": row.target.profile,
                    "path": row.target.path,
                    "installed": row.installed,
                    "state": standing,
                    "detail": detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "zone": current.command, "automatic": state.auto, "clients": list }))?);
        return Ok(());
    }
    println!("This zone: {}", current.command);
    println!();
    let shown: Vec<&Row> = rows.iter().filter(|r| r.installed || !matches!(r.standing, Standing::NotRegistered)).collect();
    let width = shown.iter().map(|r| r.target.label().chars().count()).max().unwrap_or(10);
    for row in &shown {
        let label = row.target.label();
        let name = row.target.client.name();
        let path = env.show(&row.target.path);
        match &row.standing {
            Standing::Registered { read_only } => {
                println!("{label:width$}  {:22}  {path}", if *read_only { "registered, read-only" } else { "registered" })
            }
            Standing::OtherCopy(command) => println!("{label:width$}  registered, but it starts another copy: {command} (`zone mcp install {name}` points it here)"),
            Standing::NotRegistered => println!("{label:width$}  not registered          (`zone mcp install {name}`)"),
            Standing::OptedOut => println!("{label:width$}  removed by you          (`zone mcp install {name}` adds it back)"),
            Standing::Unreadable(why) => println!("{label:width$}  cannot tell: {why}"),
        }
    }
    let missing: Vec<&str> = Client::ALL.iter().filter(|c| !rows.iter().any(|r| r.target.client == **c && (r.installed || !matches!(r.standing, Standing::NotRegistered)))).map(|c| c.name()).collect();
    if shown.is_empty() {
        println!("No AI clients found on this computer.");
    }
    if !missing.is_empty() {
        println!();
        println!("Not found on this computer: {}.", missing.join(", "));
    }
    println!();
    if state.auto {
        println!("zone install turned on registering by itself: when zone starts, it registers with clients installed since. It never touches a place you removed it from.");
    }
    Ok(())
}

// -------------------------------------------------------------------- automatic

/// `zone install`: registers with every installed client and lets `zone serve` do the same
/// for clients installed later.
pub fn register_on_install() -> Result<()> {
    let launch = Launch::current(false)?;
    let env = Env::from_system()?;
    let dir = paths::ensure_data_dir()?;
    let mut state = State::load(&dir);
    state.auto = true;
    state.exe = Some(launch.command.clone());
    state.search_path = std::env::var_os("PATH").map(|p| p.to_string_lossy().to_string());
    println!();
    println!("Registering zone with the AI clients on this computer (skip this with --no-mcp):");
    let failed = register_many(&Client::ALL, &launch, Mode::All, &env, &mut state, true);
    let any = Client::ALL.iter().any(|c| c.installed(&env));
    if !any {
        println!("  No AI clients found. When you install one, zone registers with it the next time it starts.");
    }
    state.save(&dir)?;
    if failed > 0 {
        println!("Some places could not be registered (see above). `zone mcp status` shows where zone stands.");
    }
    Ok(())
}

/// `zone uninstall` and `zone install --no-mcp`: the daemon no longer registers by itself.
/// Registrations already made stay, because the MCP server works without the daemon.
pub fn stop_registering_by_itself() {
    let Ok(dir) = data_dir() else { return };
    let mut state = State::load(&dir);
    if state.auto {
        state.auto = false;
        let _ = state.save(&dir);
    }
}

/// `zone serve` at start: registers with clients installed since `zone install`, when that
/// was asked for and this is the program it was asked from. It only ever adds an entry to a
/// place that never had one: it never changes an entry, and never adds one back where the
/// person took zone out. Returns one line per place it changed, for the log.
pub fn register_on_start(dir: &Path) -> Vec<String> {
    let mut state = State::load(dir);
    if !state.auto {
        return Vec::new();
    }
    let Ok(launch) = Launch::current(false) else { return Vec::new() };
    if state.exe.as_deref() != Some(launch.command.as_str()) {
        return vec![format!("not registering with new clients: this is {}, and zone install was run from {}", launch.command, state.exe.as_deref().unwrap_or("another copy"))];
    }
    let Ok(env) = Env::from_system() else { return Vec::new() };
    let env = env.with_saved_path(state.search_path.as_deref());
    let mut lines = Vec::new();
    for client in Client::ALL {
        if !client.installed(&env) {
            continue;
        }
        for target in client.targets(&env) {
            let before = state.clone();
            let outcome = register(&target, &launch, &env, &mut state, Mode::Automatic);
            match &outcome {
                Outcome::Registered | Outcome::Updated => lines.push(format!("registered with {} ({})", target.label(), env.show(&target.path))),
                Outcome::OptedOut if before.registered.contains(&target.id()) => {
                    lines.push(format!("zone was taken out of {} by hand, so it stays out", target.label()))
                }
                Outcome::Failed(why) => lines.push(format!("could not register with {}: {why}", target.label())),
                _ => {}
            }
        }
    }
    let _ = state.save(dir);
    lines
}

/// For `zone doctor`: where zone stands with the clients on this computer.
pub struct Summary {
    /// Places zone is registered in.
    pub registered: usize,
    /// Installed clients without zone (and not taken out on purpose).
    pub missing: Vec<String>,
    /// Places that start another copy of zone.
    pub other_copy: Vec<String>,
    /// Places whose file cannot be read, with why.
    pub unreadable: Vec<String>,
}

pub fn doctor_summary() -> Option<Summary> {
    let current = Launch::current(false).ok()?;
    let env = Env::from_system().ok()?;
    let state = data_dir().map(|d| State::load(&d)).unwrap_or_default();
    let rows = survey(&env, &state, &current);
    let labels = |keep: &dyn Fn(&Row) -> bool| -> Vec<String> { rows.iter().filter(|r| r.installed && keep(r)).map(|r| r.target.label()).collect() };
    Some(Summary {
        registered: rows.iter().filter(|r| matches!(r.standing, Standing::Registered { .. })).count(),
        missing: labels(&|r| r.standing == Standing::NotRegistered),
        other_copy: labels(&|r| matches!(r.standing, Standing::OtherCopy(_))),
        unreadable: rows
            .iter()
            .filter_map(|r| match &r.standing {
                Standing::Unreadable(why) if r.installed => Some(format!("{}: {why}", r.target.label())),
                _ => None,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests;
