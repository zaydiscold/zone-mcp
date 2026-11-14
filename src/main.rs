//! `zone`: the computer side of Zone Link.
//!
//! One program. It receives an iPhone's Zone library (dictations, notes, meeting
//! transcripts), keeps a copy in SQLite, and serves it to people and to AI agents.

mod admin;
mod backup;
mod cli;
mod clients;
mod codec;
mod daemon_info;
mod db;
mod fakephone;
mod flash;
mod item;
mod link;
mod log;
mod mcp;
mod pairing;
mod paths;
mod serve;
mod service;
mod timeparse;
mod tools;
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
                  zone mcp install claude-code   let an agent read them",
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
    /// Write items out as JSON Lines or Markdown, oldest first
    Export {
        #[command(flatten)]
        filter: FilterArgs,
        /// jsonl or md
        #[arg(long, default_value = "jsonl")]
        format: String,
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
    Mcp {
        /// Leave out mark_processed, the one tool that writes
        #[arg(long)]
        read_only: bool,
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// Start zone every time you log in (no admin rights needed), and start it now
    Install {
        /// Write the login item but do not load or start anything
        #[arg(long)]
        no_start: bool,
    },
    /// Stop zone and no longer start it at login. Your library is kept
    Uninstall {
        /// Remove the login item but do not stop or unload anything
        #[arg(long)]
        no_stop: bool,
    },
    /// Open the local web page with your transcriptions
    Open {
        /// Print the link (it contains the page's key) instead of opening a browser
        #[arg(long)]
        print: bool,
    },
    /// Tools for testing zone itself
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        action: DevAction,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Register zone with claude-code, claude-desktop, codex or cursor
    Install {
        client: String,
        /// Register the read-only server (no mark_processed)
        #[arg(long)]
        read_only: bool,
    },
    /// Remove zone from a client
    Uninstall { client: String },
    /// Show which clients have zone registered
    Status,
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
        /// Never try a direct address: everything goes through the relay
        #[arg(long)]
        relay_only: bool,
        /// A direct address of the computer to try (repeatable)
        #[arg(long)]
        addr: Vec<SocketAddr>,
        #[arg(long)]
        name: Option<String>,
        /// Protocol version to claim
        #[arg(long)]
        protocol_version: Option<i64>,
        /// Seconds to wait for the connection
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Print synthetic items as JSON Lines
    Synthetic {
        #[arg(long, default_value_t = 100)]
        count: usize,
        #[arg(long, default_value_t = 0)]
        start_index: usize,
    },
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Serve { port, quiet } => runtime()?.block_on(serve::run(serve::Options { port, quiet }))?,
        Command::Pair { no_qr, qr, relay, json } => {
            let show = if no_qr { Some(false) } else if qr { Some(true) } else { None };
            admin::pair(show, relay, json)?
        }
        Command::Status { json } => admin::status(json)?,
        Command::Devices { json } => admin::devices(json)?,
        Command::Unpair { device } => admin::unpair(&device)?,
        Command::Resync { device } => admin::resync(device.as_deref())?,
        Command::Doctor { json } => {
            if !admin::doctor(json)? {
                return Ok(ExitCode::from(1));
            }
        }
        Command::List { filter, order, limit, all, json } => cli::list(&Db::open_default()?, &filter, &order, limit, all, json)?,
        Command::Search { query, filter, order, limit, json } => {
            let query = query.join(" ");
            if query.trim().is_empty() {
                anyhow::bail!("say what to search for: zone search <words>");
            }
            cli::search(&Db::open_default()?, &query, &filter, order.as_deref(), limit, json)?
        }
        Command::Show { ids, tz, json } => cli::show(&Db::open_default()?, &ids, tz.as_deref(), json)?,
        Command::Stats { filter, by, json } => cli::stats(&Db::open_default()?, &filter, &by, json)?,
        Command::Export { filter, format, output } => cli::export(&Db::open_default()?, &filter, &format, output.as_deref())?,
        Command::Import { path } => admin::import(&path)?,
        Command::BackupFolder { path, clear } => admin::backup_folder(path.as_deref(), clear)?,
        Command::Mcp { read_only, action } => match action {
            None => runtime()?.block_on(mcp::serve(read_only))?,
            Some(McpAction::Install { client, read_only: install_read_only }) => {
                clients::install(clients::Client::parse(&client)?, read_only || install_read_only)?
            }
            Some(McpAction::Uninstall { client }) => clients::uninstall(clients::Client::parse(&client)?)?,
            Some(McpAction::Status) => clients::status()?,
        },
        Command::Install { no_start } => service::install(!no_start)?,
        Command::Uninstall { no_stop } => service::uninstall(!no_stop)?,
        Command::Open { print } => admin::open(print)?,
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
                relay_only,
                addr,
                name,
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
                    relay_only,
                    addrs: addr,
                    name,
                    version: protocol_version,
                    timeout: Duration::from_secs(timeout),
                };
                let report = runtime()?.block_on(fakephone::run(options))?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            DevAction::Synthetic { count, start_index } => {
                for item in fakephone::synthetic_items(start_index, count) {
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

fn main() -> ExitCode {
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
