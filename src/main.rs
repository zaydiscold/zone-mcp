//! `zone`: the computer side of Zone Link.
//!
//! One program. It receives an iPhone's Zone library (dictations, notes, meeting
//! transcripts), keeps a copy in SQLite, and serves it to people and to AI agents.

// These take the place of the standard macros in every module below. The standard ones panic
// when the output is a closed pipe (`zone status | head -1`). See out.rs.
macro_rules! println {
    () => { $crate::out::stdout(format_args!(""), true) };
    ($($arg:tt)*) => { $crate::out::stdout(format_args!($($arg)*), true) };
}
macro_rules! print {
    ($($arg:tt)*) => { $crate::out::stdout(format_args!($($arg)*), false) };
}
macro_rules! eprintln {
    () => { $crate::out::stderr(format_args!(""), true) };
    ($($arg:tt)*) => { $crate::out::stderr(format_args!($($arg)*), true) };
}
macro_rules! eprint {
    ($($arg:tt)*) => { $crate::out::stderr(format_args!($($arg)*), false) };
}

mod admin;
mod agents;
mod backup;
mod cli;
mod clients;
mod clip;
mod codec;
mod daemon_info;
mod db;
mod events;
mod export;
mod fakephone;
mod flash;
mod item;
mod link;
mod log;
mod mcp;
mod out;
mod pairing;
mod paths;
mod prompts;
mod resources;
mod scope;
mod serve;
mod service;
mod stats;
mod timeparse;
mod tools;
mod update;
mod watch;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::cli::FilterArgs;
use crate::db::Db;

#[derive(Parser)]
#[command(
    name = "zone",
    version,
    about = "Your iPhone's Zone dictations, notes and meeting transcripts, on this computer",
    long_about = "Receives the library of the Zone iPhone app and keeps a copy on this computer, for you to read \
                  and search and for AI agents to use.\n\nGetting started:\n  zone install      start zone every time you log in\n  \
                  zone pair         show the code to scan with your iPhone\n  zone open         see your transcriptions in the browser\n  \
                  zone mcp install --all   let your AI agents read them",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run zone: accept paired phones, watch the backup folder, serve the local web page
    Serve {
        /// Port for the local web page on 127.0.0.1 (default 47813, or a free one if taken)
        #[arg(long)]
        port: Option<u16>,
        /// Write the log file only, nothing to the terminal
        #[arg(long)]
        quiet: bool,
    },
    /// Show a one-use code (and link) to pair an iPhone. Valid for ten minutes
    Pair {
        /// Print the link without the QR code
        #[arg(long, conflicts_with = "qr")]
        no_qr: bool,
        /// Print the QR code even when the output is not a terminal
        #[arg(long)]
        qr: bool,
        /// Put this computer's current relay address in the link
        #[arg(long)]
        relay: bool,
        /// Stay until a phone has used the code or it has run out (what a terminal does anyway).
        /// With --json: one line for the code, then one line saying what became of it
        #[arg(long, conflicts_with = "no_wait")]
        wait: bool,
        /// Print the code and return at once
        #[arg(long)]
        no_wait: bool,
        /// Instead of making a code: say whether a phone has used the code with this id
        /// (the "id" that `zone pair --json` printed)
        #[arg(long, value_name = "ID", conflicts_with_all = ["qr", "no_qr", "relay", "wait", "no_wait"])]
        check: Option<String>,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Is zone running, which phones are paired, how much is in the library
    Status {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// List paired phones
    Devices {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Forget a paired phone (its items stay)
    Unpair {
        /// The phone's name, or the beginning of its id from `zone devices`
        device: String,
    },
    /// Ask a phone to send its whole library again at its next sync
    Resync {
        /// The phone's name or id. Leave out for every phone
        device: Option<String>,
    },
    /// What happened lately: syncs, pairings, flashes, backup imports, zone starting and stopping
    #[command(long_about = "What happened lately, one line each: a phone synced over Link, a phone paired or was unpaired, a \
                            Flash arrived, the backup folder was read, zone started or stopped, the relay came or went.\n\n\
                            Names and counts only, never your words. The newest 2000 are kept.\n\n\
                            With --json each line is one JSON object: seq (a number that only goes up), at (Unix seconds), \
                            time (local RFC 3339), event (the kind), then that kind's fields. The README lists them.")]
    Events {
        /// Print one JSON object per line instead of text
        #[arg(long)]
        json: bool,
        /// Keep running and print events as they happen
        #[arg(long, short)]
        follow: bool,
        /// At most this many of the newest events (0: none, only what follows)
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Only events at or after this time: 2h, today, 2026-11-11, "2026-11-11 14:30"
        #[arg(long, value_name = "TIME")]
        since: Option<String>,
        /// Only events numbered above this one (the "seq" of the last line you read)
        #[arg(long, value_name = "SEQ")]
        after: Option<i64>,
    },
    /// Print the newest text you sent from your iPhone (a clip), or list recent ones
    #[command(
        args_conflicts_with_subcommands = true,
        long_about = "Print the newest text you sent to this computer from Zone on your iPhone (a clip): its text and nothing \
                      else, so it pipes cleanly.\n\n  zone clip              print the newest clip\n  zone clip --copy       \
                      also put it on this computer's clipboard\n  zone clip list         recent clips as short cards\n  \
                      zone clip --id <id>    another clip, by its id from the list\n\n\
                      Exits 2 when there is no clip. `zone clipboard on` puts clips on the clipboard by themselves as they arrive."
    )]
    Clip {
        /// Also put the text on this computer's clipboard
        #[arg(long)]
        copy: bool,
        /// Print this clip instead of the newest: an id (or its beginning) from `zone clip list`
        #[arg(long, value_name = "ID")]
        id: Option<String>,
        /// Print JSON instead of the text
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<ClipAction>,
    },
    /// Put text you send from your iPhone on this computer's clipboard as it arrives: on, off, status
    #[command(long_about = "Whether text you send to this computer from Zone on your iPhone (a clip) goes on this computer's \
                            clipboard as it arrives. Off until you turn it on.\n\n  zone clipboard on\n  zone clipboard off\n  \
                            zone clipboard status\n\n\
                            Only a clip that comes over Link and was sent in the last two minutes is copied. An older one (a \
                            phone that was offline, a Flash) is kept and the clipboard is left alone. Set ZONE_CLIPBOARD_COMMAND \
                            to a command that reads the text on its standard input to use another clipboard tool.")]
    Clipboard {
        #[command(subcommand)]
        action: ClipboardAction,
        /// Print JSON instead of text
        #[arg(long, global = true)]
        json: bool,
    },
    /// Let AI agents use your library, or stop every one of them at once: on, off, status
    #[command(long_about = "One switch for every AI agent on this computer. On until you turn it off.\n\n  zone agents off\n  \
                            zone agents on\n  zone agents status [--json]\n\n\
                            Off does not unregister zone from any client. Each client still starts zone and connects, and is \
                            told that agent access is turned off on this computer and how to turn it back on. Servers that are \
                            already running notice within a few seconds. Each switch is recorded in `zone events`.")]
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
        /// Print JSON instead of text
        #[arg(long, global = true)]
        json: bool,
    },
    /// Things to ask your agent: example prompts that use your library, to copy into any agent
    #[command(
        long_about = "Things to ask your agent: example prompts that use your library, to copy into any agent. Agents that \
                      support MCP prompts offer the same ones as slash commands.\n\n  zone prompts                 the list\n  \
                      zone prompts show <id>       one prompt's text, ready to paste\n  \
                      zone prompts show what-did-i-say day=\"Wednesday 12pm to 5pm\"\n\n\
                      The view flags (--notes-only, --kinds, --no-keyboard, --read-only) show the prompts, and the wording, \
                      that an agent with that view gets."
    )]
    Prompts {
        #[command(flatten)]
        view: scope::ViewArgs,
        /// Print JSON instead of text
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        action: Option<PromptsAction>,
    },
    /// Check the setup and explain how to fix anything that is wrong
    Doctor {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// List items, newest first
    List {
        #[command(flatten)]
        filter: FilterArgs,
        /// newest or oldest
        #[arg(long, default_value = "newest")]
        order: String,
        /// How many to show
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Show every matching item
        #[arg(long)]
        all: bool,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Search the text of everything you said
    Search {
        /// Words to find. "Quoted words" must appear together, OR means either, word* matches beginnings
        query: Vec<String>,
        #[command(flatten)]
        filter: FilterArgs,
        /// newest or oldest (default: best match first)
        #[arg(long)]
        order: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Continue after an earlier page: the cursor that search printed
        #[arg(long)]
        cursor: Option<String>,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Show items in full, by id (or the beginning of an id)
    Show {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long, value_name = "ZONE")]
        tz: Option<String>,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Counts, words and recorded time by day, kind or source
    Stats {
        #[command(flatten)]
        filter: FilterArgs,
        /// day, kind or source
        #[arg(long, default_value = "day")]
        by: String,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Write items out: JSON Lines, JSON, CSV, Markdown or plain text. In Markdown and text,
    /// notes alone become a checklist, one meeting a transcript, anything else a document by day
    Export {
        #[command(flatten)]
        filter: FilterArgs,
        /// jsonl, json, csv, md or txt
        #[arg(long, default_value = "jsonl")]
        format: String,
        /// oldest or newest first
        #[arg(long, default_value = "oldest")]
        order: String,
        /// Only these items: an id or the beginning of one (repeatable). Other filters are
        /// then left aside
        #[arg(long, value_name = "ID")]
        id: Vec<String>,
        /// Write to this file instead of the terminal
        #[arg(long, short, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Import a Zone backup once: a .zonebackup folder, its library.json, or the folder holding it
    Import { path: PathBuf },
    /// Choose a backup folder for zone to watch and re-import whenever it changes
    BackupFolder {
        path: Option<PathBuf>,
        /// Stop watching
        #[arg(long, conflicts_with = "path")]
        clear: bool,
    },
    /// Serve the library to an AI client over MCP (stdio), or register zone with a client
    #[command(long_about = "Serve the library to an AI client over MCP (stdio), or register zone with a client.\n\n\
                            By default an agent can read everything. To show it less: --kinds note,meeting shows only \
                            those kinds, --exclude-sources keyboard hides what you typed by voice into other apps \
                            (--notes-only and --no-keyboard are short for the usual two). `zone mcp install <client>` \
                            takes the same flags, so each client can have its own view.")]
    Mcp {
        #[command(flatten)]
        view: scope::ViewArgs,
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// Start zone every time you log in (no admin rights needed), start it now, and register
    /// it with the AI clients on this computer
    Install {
        /// Write the login item but do not load or start anything
        #[arg(long)]
        no_start: bool,
        /// Do not register zone with AI clients, now or when it starts
        #[arg(long)]
        no_mcp: bool,
    },
    /// Stop zone and no longer start it at login. Your library is kept
    Uninstall {
        /// Remove the login item but do not stop or unload anything
        #[arg(long)]
        no_stop: bool,
    },
    /// See whether a newer zone is out, and install it
    #[command(long_about = "See whether a newer zone is out, and install it.\n\n\
                            Asks GitHub once which release is the latest. If it is newer than this one, downloads it, checks \
                            it against the release's checksums and replaces this program, the same way the install script \
                            does. zone never looks for updates by itself. Set ZONE_NO_UPDATE_CHECK=1 to turn asking off.")]
    Update {
        /// Only say whether a newer version is out
        #[arg(long)]
        check: bool,
        /// Install the latest release again even when this is it
        #[arg(long, conflicts_with = "check")]
        force: bool,
        /// Print JSON instead of text (and install nothing)
        #[arg(long)]
        json: bool,
    },
    /// Open the local web page with your transcriptions
    Open {
        /// Print the link (it contains the page's key) instead of opening a browser
        #[arg(long)]
        print: bool,
        /// Open the Flash page, where the camera reads the phone's codes
        #[arg(long)]
        flash: bool,
    },
    /// Tools for testing zone itself
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        action: DevAction,
    },
}

#[derive(Subcommand)]
enum ClipAction {
    /// Recent clips as short cards, newest first: when, how it was sent, how long, its first line
    List {
        /// How many to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ClipboardAction {
    /// Put clips on this computer's clipboard as they arrive
    On,
    /// Leave the clipboard alone (clips are still kept)
    Off,
    /// Say whether it is on, and which clipboard tool zone would use
    Status,
}

#[derive(Subcommand)]
enum PromptsAction {
    /// Print one prompt's text, ready to paste. Arguments are name=value: day=Wednesday, topic="the lease"
    Show {
        /// The prompt's id from `zone prompts`
        id: String,
        /// name=value pairs for the prompt's arguments
        args: Vec<String>,
        #[command(flatten)]
        view: scope::ViewArgs,
    },
}

#[derive(Subcommand)]
enum AgentsAction {
    /// Let AI agents use the library (each within the view it was registered with)
    On,
    /// Stop every AI agent from reading the library, without unregistering anything
    Off,
    /// Say whether agents can use the library
    Status,
}

#[derive(Subcommand)]
enum McpAction {
    /// Register zone with AI clients: name them, or --all for every one on this computer
    #[command(long_about = "Register zone with AI clients: name them, or use --all for every one on this computer.\n\n\
                            Clients: claude-code (every profile), claude-desktop, codex (every CODEX_HOME), cursor, hermes \
                            (every profile), gemini, qwen, opencode, windsurf, vscode, zed, kiro, droid, copilot, lm-studio, \
                            goose, crush, amp, cline, roo, antigravity, warp, junie. The ChatGPT desktop app reads Codex's \
                            settings, so `codex` (or `chatgpt`) covers it. For Raycast, `zone mcp config --client raycast` \
                            prints the link that installs zone there.\n\n\
                            Only zone's own entry is written. Everything else in each file is kept, and the original is saved \
                            beside it as <file>.before-zone the first time.")]
    Install {
        /// Client names, or `detected` for every one on this computer
        clients: Vec<String>,
        /// Every client found on this computer, except ones you removed zone from
        #[arg(long)]
        all: bool,
        /// What the client's agents may do and see. A client that already has a limited view,
        /// or is read-only, keeps that unless you say otherwise (--kinds all, --read-write).
        /// With --all, giving one of the view flags (--kinds, --exclude-sources, --notes-only,
        /// --no-keyboard) applies it to every client
        #[command(flatten)]
        view: scope::ViewArgs,
    },
    /// Remove zone from clients: name them, or --all for every one
    Uninstall {
        clients: Vec<String>,
        /// Every client, and stop registering by itself
        #[arg(long)]
        all: bool,
    },
    /// Print what to paste into an app's MCP settings to use this zone: the generic JSON, or one
    /// client's own format
    #[command(long_about = "Print what to paste into an app's MCP settings to use this zone, with this program's full path.\n\n\
                            Without --client: the generic \"mcpServers\" JSON that most apps take, and the TOML and YAML forms \
                            Codex, Hermes and Goose use. --client <name> prints one client's own snippet and where it goes; \
                            --client other prints only the generic JSON. --json prints every form at once.\n\n\
                            The view flags (--notes-only, --kinds, --no-keyboard, --read-only) go into the snippet.")]
    Config {
        /// One client (a name from `zone mcp status`), or `other` for the generic snippet only
        #[arg(long)]
        client: Option<String>,
        #[command(flatten)]
        view: scope::ViewArgs,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Show which clients have zone registered
    Status {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
}

// Parsed once per run, so the size of the largest variant does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum DevAction {
    /// Act as a phone over iroh: pair or reconnect, send items, report the acks as JSON
    FakePhone {
        /// A zone://pair?... link
        #[arg(long)]
        url: Option<String>,
        /// File that keeps this phone's identity between runs
        #[arg(long)]
        identity: Option<PathBuf>,
        /// The computer's endpoint id, when there is no link and no saved identity
        #[arg(long)]
        computer: Option<String>,
        /// Do not present the link's pairing secret
        #[arg(long)]
        no_secret: bool,
        /// Synthetic items to send
        #[arg(long, default_value_t = 0)]
        count: usize,
        /// Index of the first synthetic item
        #[arg(long, default_value_t = 0)]
        start_index: usize,
        /// Items per frame (at most 500)
        #[arg(long, default_value_t = 500)]
        batch: usize,
        /// Send the items in this JSONL file instead
        #[arg(long)]
        file: Option<PathBuf>,
        /// Send a deletion for this item id (repeatable)
        #[arg(long)]
        delete: Vec<String>,
        /// Send this text as a clip, as if the person had just sent it (repeatable)
        #[arg(long, value_name = "TEXT")]
        clip: Vec<String>,
        /// The source the clips claim: clipboard, share, shortcut or typed
        #[arg(long, value_name = "SOURCE")]
        clip_source: Option<String>,
        /// How many seconds ago the clips say they were sent
        #[arg(long, value_name = "SECONDS", default_value_t = 0.0, allow_hyphen_values = true)]
        clip_age: f64,
        /// Never try a direct address: everything goes through the relay
        #[arg(long)]
        relay_only: bool,
        /// A direct address of the computer to try (repeatable)
        #[arg(long)]
        addr: Vec<SocketAddr>,
        #[arg(long)]
        name: Option<String>,
        /// Say in hello how Zone looks on this phone: dark or light
        #[arg(long)]
        appearance: Option<String>,
        /// Say in hello which accent is chosen on this phone: violet, ice, ember, mint or rose
        #[arg(long)]
        accent: Option<String>,
        /// Protocol version to claim
        #[arg(long)]
        protocol_version: Option<i64>,
        /// Seconds to wait for the connection
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Draw a flash as numbered PNG pictures of QR codes, sealed with a pairing secret
    FlashEncode {
        /// Folder to write frame-000000.png, frame-000001.png, ... into
        #[arg(long, short)]
        out: PathBuf,
        /// The pairing secret, 64 hex characters
        #[arg(long)]
        secret: String,
        /// Items as JSON Lines. Without it, synthetic items are used
        #[arg(long)]
        file: Option<PathBuf>,
        /// Synthetic items to flash
        #[arg(long, default_value_t = 100)]
        count: usize,
        /// Index of the first synthetic item
        #[arg(long, default_value_t = 0)]
        start_index: usize,
        /// Bytes per frame body
        #[arg(long, default_value_t = flash::fountain::DEFAULT_CHUNK)]
        chunk: usize,
        /// How many frames to draw (default: enough to rebuild, with some to spare)
        #[arg(long)]
        frames: Option<usize>,
        /// Index of the first frame drawn
        #[arg(long, default_value_t = 0)]
        first_frame: u32,
        /// Pixels per module
        #[arg(long, default_value_t = 4)]
        scale: usize,
        /// Modules of white around the code
        #[arg(long, default_value_t = 4)]
        quiet: usize,
        /// The session number, as hex (default: random)
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        phone_id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Color Flash: draw three frames to a picture, in red, green and blue
        #[arg(long)]
        color: bool,
        /// With --color, the palette: the dark then the light level of red, green and blue,
        /// as r0,r1,g0,g1,b0,b1 (default 0,255,0,255,0,255)
        #[arg(long, value_name = "LEVELS", requires = "color", allow_hyphen_values = true)]
        levels: Option<String>,
    },
    /// Read a folder of PNG pictures of flash frames, rebuild the flash and report counts
    FlashDecode {
        /// The folder. File names and their order do not matter
        dir: PathBuf,
        /// Open with this pairing secret (64 hex characters) instead of the library's keys
        #[arg(long)]
        secret: Option<String>,
        /// Store the items in the library
        #[arg(long)]
        import: bool,
    },
    /// Print the codes in each picture of a folder, as hexadecimal JSON lines
    FlashRead {
        dir: PathBuf,
        /// Read a color picture's channels as they are, without undoing the crosstalk
        #[arg(long)]
        no_unmix: bool,
    },
    /// Time the QR reader on webcam-like pictures of flash frames of a given size
    FlashBench {
        /// Bytes per frame body
        #[arg(long, default_value_t = flash::fountain::DEFAULT_CHUNK)]
        chunk: usize,
        /// Width of the code in the picture, in pixels, quiet zone included
        #[arg(long, default_value_t = 520)]
        code_px: usize,
        /// Side of the square picture, as the page posts it
        #[arg(long, default_value_t = 720)]
        picture: usize,
        /// Frames to read
        #[arg(long, default_value_t = 90)]
        frames: usize,
        /// Degrees the phone is turned
        #[arg(long, default_value_t = 4.0)]
        tilt: f64,
        /// Blur radius in pixels
        #[arg(long, default_value_t = 1)]
        blur: usize,
        /// Sensor noise (plus or minus, per pixel)
        #[arg(long, default_value_t = 6)]
        noise: u8,
        /// Write the first picture as a PNG here
        #[arg(long)]
        sample: Option<PathBuf>,
        /// Color Flash: three frames to a picture
        #[arg(long)]
        color: bool,
        /// With --color, how much of each channel the camera sees in its neighbours
        #[arg(long, default_value_t = 0.15)]
        crosstalk: f64,
        /// With --color, the palette as r0,r1,g0,g1,b0,b1 (default 0,255,0,255,0,255)
        #[arg(long, value_name = "LEVELS", conflicts_with = "span")]
        levels: Option<String>,
        /// With --color, a palette of this contrast in every channel: light 255, dark 255 less this
        #[arg(long, value_name = "SPAN")]
        span: Option<u8>,
        /// Uneven brightness: how much darker the picture gets toward its far corner (0.25 is a quarter)
        #[arg(long, default_value_t = 0.0)]
        shade: f64,
        /// Use one of the cameras of PROTOCOL.md's tables (normal, harsh, blurred) for
        /// --tilt, --blur, --noise, --crosstalk and --shade
        #[arg(long, value_name = "NAME", conflicts_with_all = ["tilt", "blur", "noise", "crosstalk", "shade"])]
        camera: Option<String>,
    },
    /// Measure how many codes of a color picture are read, for each contrast, code width and camera
    FlashSweep {
        /// Contrast spans to measure: light minus dark, the same in every channel
        #[arg(long, value_delimiter = ',', default_value = "255,200,170,150,120")]
        spans: Vec<u8>,
        /// Where a span sits: light (light is 255), dark (dark is 0) or middle
        #[arg(long, default_value = "light")]
        anchor: String,
        /// A palette to measure as well, as r0,r1,g0,g1,b0,b1 (repeatable)
        #[arg(long, value_name = "LEVELS")]
        levels: Vec<String>,
        /// Widths of the code in the picture, in pixels. The picture is 1.4 times as wide
        #[arg(long, value_delimiter = ',', default_value = "450,550,700")]
        widths: Vec<usize>,
        /// Cameras: normal, harsh, blurred
        #[arg(long, value_delimiter = ',', default_value = "normal,harsh")]
        cameras: Vec<String>,
        /// Bytes per frame body
        #[arg(long, default_value_t = flash::fountain::DEFAULT_CHUNK)]
        chunk: usize,
        /// Frames read in each cell, three to a picture
        #[arg(long, default_value_t = 90)]
        frames: usize,
        /// Print a table for PROTOCOL.md instead of JSON
        #[arg(long)]
        table: bool,
    },
    /// Print synthetic items as JSON Lines
    Synthetic {
        #[arg(long, default_value_t = 100)]
        count: usize,
        #[arg(long, default_value_t = 0)]
        start_index: usize,
        /// A made-up library that reads like a real one (to-do notes, meetings with several
        /// speakers, clips, voice notes with their recordings), ending a few minutes ago
        #[arg(long)]
        demo: bool,
    },
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Serve { port, quiet } => return runtime()?.block_on(serve::run(serve::Options { port, quiet })),
        Command::Pair { no_qr, qr, relay, wait, no_wait, check, json } => {
            let show = if no_qr { Some(false) } else if qr { Some(true) } else { None };
            let wait = if wait { Some(true) } else if no_wait { Some(false) } else { None };
            let claimed = match check {
                Some(id) => admin::pair_check(&id, json)?,
                None => admin::pair(show, relay, json, wait)?,
            };
            if !claimed {
                return Ok(ExitCode::from(2));
            }
        }
        Command::Events { json, follow, limit, since, after } => events::run(&events::Options { json, follow, limit, since, after })?,
        Command::Status { json } => admin::status(json)?,
        Command::Devices { json } => admin::devices(json)?,
        Command::Unpair { device } => admin::unpair(&device)?,
        Command::Resync { device } => admin::resync(device.as_deref())?,
        Command::Clip { copy, id, json, action } => match action {
            Some(ClipAction::List { limit, json }) => clip::list(limit, json)?,
            None => match clip::show(id.as_deref(), copy, json)? {
                clip::Shown::Ok => {}
                // With --json, "there is none" is an answer, not a failure.
                clip::Shown::Nothing if json && !copy => {}
                clip::Shown::Nothing => return Ok(ExitCode::from(2)),
                clip::Shown::NotCopied => return Ok(ExitCode::from(1)),
            },
        },
        Command::Clipboard { action, json } => {
            let action = match action {
                ClipboardAction::On => clip::Setting::On,
                ClipboardAction::Off => clip::Setting::Off,
                ClipboardAction::Status => clip::Setting::Status,
            };
            clip::clipboard(action, json)?
        }
        Command::Agents { action, json } => {
            let action = match action {
                AgentsAction::On => agents::Action::On,
                AgentsAction::Off => agents::Action::Off,
                AgentsAction::Status => agents::Action::Status,
            };
            agents::run(action, json)?
        }
        Command::Prompts { view, json, action } => match action {
            None => prompts::list(&tools::Access { read_only: view.read_only, scope: view.scope()? }, json)?,
            Some(PromptsAction::Show { id, args, view: inner }) => {
                let view = inner.or(&view);
                prompts::show(&id, &args, &tools::Access { read_only: view.read_only, scope: view.scope()? }, json)?
            }
        },
        Command::Doctor { json } => {
            if !admin::doctor(json)? {
                return Ok(ExitCode::from(1));
            }
        }
        Command::List { filter, order, limit, all, json } => cli::list(&Db::open_default()?, &filter, &order, limit, all, json)?,
        Command::Search { query, filter, order, limit, cursor, json } => {
            let query = query.join(" ");
            if query.trim().is_empty() {
                anyhow::bail!("say what to search for: zone search <words>");
            }
            cli::search(&Db::open_default()?, &query, &filter, order.as_deref(), limit, cursor.as_deref(), json)?
        }
        Command::Show { ids, tz, json } => cli::show(&Db::open_default()?, &ids, tz.as_deref(), json)?,
        Command::Stats { filter, by, json } => cli::stats(&Db::open_default()?, &filter, &by, json)?,
        Command::Export { filter, format, order, id, output } => cli::export(&Db::open_default()?, &filter, &format, &order, &id, output.as_deref())?,
        Command::Import { path } => admin::import(&path)?,
        Command::BackupFolder { path, clear } => admin::backup_folder(path.as_deref(), clear)?,
        Command::Mcp { view, action } => match action {
            None => runtime()?.block_on(mcp::serve(tools::Access { read_only: view.read_only, scope: view.scope()? }))?,
            Some(McpAction::Install { clients, all, view: install_view }) => {
                let view = install_view.or(&view);
                let wish = clients::Wish { read_only: view.read_only, read_write: view.read_write, scope: view.scope()?, view_named: view.names_a_view() };
                clients::install(&clients, all, &wish)?
            }
            Some(McpAction::Uninstall { clients, all }) => clients::uninstall(&clients, all)?,
            Some(McpAction::Status { json }) => clients::status(json)?,
            Some(McpAction::Config { client, view: inner, json }) => {
                let view = inner.or(&view);
                clients::config(client.as_deref(), view.read_only, &view.scope()?, json)?
            }
        },
        Command::Install { no_start, no_mcp } => {
            service::install(!no_start)?;
            if no_mcp {
                clients::stop_registering_by_itself();
                println!("Not registering zone with AI clients (--no-mcp). `zone mcp install --all` does it later.");
            } else {
                clients::register_on_install()?;
            }
        }
        Command::Uninstall { no_stop } => {
            service::uninstall(!no_stop)?;
            clients::stop_registering_by_itself();
            println!("AI clients keep zone registered (it works without the daemon). `zone mcp uninstall --all` takes it out of them.");
        }
        Command::Open { print, flash } => admin::open(print, flash)?,
        Command::Update { check, force, json } => {
            if !update::run(check, force, json)? {
                return Ok(ExitCode::from(1));
            }
        }
        Command::Dev { action } => match action {
            DevAction::FakePhone {
                url,
                identity,
                computer,
                no_secret,
                count,
                start_index,
                batch,
                file,
                delete,
                clip,
                clip_source,
                clip_age,
                relay_only,
                addr,
                name,
                appearance,
                accent,
                protocol_version,
                timeout,
            } => {
                let options = fakephone::Options {
                    url,
                    identity,
                    computer,
                    no_secret,
                    count,
                    start_index,
                    batch,
                    file,
                    delete,
                    clips: clip,
                    clip_source,
                    clip_age,
                    relay_only,
                    addrs: addr,
                    name,
                    appearance,
                    accent,
                    version: protocol_version,
                    timeout: Duration::from_secs(timeout),
                };
                let report = runtime()?.block_on(fakephone::run(options))?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            DevAction::FlashEncode { out, secret, file, count, start_index, chunk, frames, first_frame, scale, quiet, session, phone_id, name, color, levels } => {
                let levels = levels.as_deref().map(flash::color::Levels::parse).transpose()?;
                let session = match session {
                    Some(text) => Some(u32::from_str_radix(text.trim_start_matches("0x"), 16).map_err(|_| anyhow::anyhow!("--session is up to 8 hex characters"))?),
                    None => None,
                };
                let options = flash::dev::EncodeOptions { out, secret, file, count, start_index, chunk, frames, first_frame, scale, quiet, session, phone_id, name, color, levels };
                println!("{}", serde_json::to_string_pretty(&flash::dev::encode(&options)?)?);
            }
            DevAction::FlashBench { chunk, code_px, picture, frames, tilt, blur, noise, sample, color, crosstalk, levels, span, shade, camera } => {
                let levels = match (levels, span) {
                    (Some(text), _) => flash::color::Levels::parse(&text)?,
                    (None, Some(0)) => anyhow::bail!("--span is 1 to 255"),
                    (None, Some(span)) => flash::color::Levels::with_span(span),
                    (None, None) => flash::color::Levels::FULL,
                };
                let (tilt, blur, noise, crosstalk, shade) = match camera {
                    Some(name) => {
                        let camera = flash::dev::Camera::named(&name)?;
                        (camera.tilt, camera.blur, camera.noise, camera.crosstalk, camera.shade)
                    }
                    None => (tilt, blur, noise, crosstalk, shade),
                };
                let options = flash::dev::BenchOptions { chunk, code_px, picture, frames, tilt, blur, noise, sample, color, crosstalk, levels, shade };
                println!("{}", serde_json::to_string_pretty(&flash::dev::bench(&options)?)?);
            }
            DevAction::FlashSweep { spans, anchor, levels, widths, cameras, chunk, frames, table } => {
                if spans.contains(&0) {
                    anyhow::bail!("--spans are 1 to 255");
                }
                let options = flash::dev::SweepOptions {
                    spans,
                    anchor,
                    palettes: levels.iter().map(|text| flash::color::Levels::parse(text)).collect::<Result<_>>()?,
                    widths,
                    cameras: cameras.iter().map(|name| flash::dev::Camera::named(name)).collect::<Result<_>>()?,
                    chunk,
                    frames,
                };
                let report = flash::dev::sweep(&options)?;
                if table {
                    print!("{}", flash::dev::sweep_table(&report));
                } else {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
            }
            DevAction::FlashRead { dir, no_unmix } => {
                for line in flash::dev::read_folder(&dir, !no_unmix)? {
                    println!("{line}");
                }
            }
            DevAction::FlashDecode { dir, secret, import } => {
                let report = flash::dev::decode(&flash::dev::DecodeOptions { dir, secret, import })?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                if report["complete"] != true {
                    return Ok(ExitCode::from(1));
                }
            }
            DevAction::Synthetic { count, start_index, demo } => {
                let items = if demo { fakephone::demo_items(start_index, count, paths::now_unix()) } else { fakephone::synthetic_items(start_index, count) };
                for item in items {
                    println!("{item}");
                }
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}

/// Writing to a pipe that was closed (`zone list | head`) is not a failure.
fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.downcast_ref::<std::io::Error>().map(|e| e.kind() == std::io::ErrorKind::BrokenPipe).unwrap_or(false))
}

fn real_main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zone: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// The stack every command runs on. Windows gives a program's first thread 1 MB, which an
/// unoptimized build outgrows just reading the command line (the command enum is large), so
/// the work is done on a thread whose stack is the same generous size on every system.
const STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> ExitCode {
    match std::thread::Builder::new().name("zone".to_string()).stack_size(STACK_BYTES).spawn(real_main) {
        // A panic has already said what went wrong. 101 is what Rust itself exits with.
        Ok(thread) => thread.join().unwrap_or(ExitCode::from(101)),
        Err(_) => real_main(),
    }
}
