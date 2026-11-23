//! `zone-tray.exe`: Zone for Windows, the Z in the notification area beside the clock.
//!
//! It shows whether zone is running and what arrived lately, and has the menu a command line
//! cannot have: the newest items to copy, the page, Flash, pairing, the agents switch, and a
//! Developer menu. Everything it shows comes from running the `zone.exe` beside it
//! (`zone status --json` and the like), as Zone for Mac does. It never opens the library.
//!
//! `zone install` starts it at login together with zone. It is a windowed program that makes
//! no window, so starting it shows nothing but the icon, and nothing it starts shows a
//! console window. On other systems it only prints the menu as text (`--print-menu`), which is
//! how the menu is looked at where there is no Windows.

#![cfg_attr(windows, windows_subsystem = "windows")]
// Off Windows only the plain parts are built, for their tests and for `--print-menu`.
#![cfg_attr(not(windows), allow(dead_code))]

#[path = "../tray/mod.rs"]
mod tray;

// The clipboard and the registry, as zone itself calls them.
#[cfg(windows)]
#[allow(dead_code)]
#[path = "../win.rs"]
mod win;

use std::process::ExitCode;

use tray::model;
use tray::zone::{Depth, Zone, refresh};

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The menu as it would be shown right now, as text.
fn print_menu() -> ExitCode {
    let Ok(me) = std::env::current_exe() else { return ExitCode::from(1) };
    let zone = Zone::beside(&me);
    let now = now_unix();
    #[cfg(windows)]
    let login = || tray::win::login();
    #[cfg(not(windows))]
    let login = model::Login::default;
    let snapshot = refresh(&zone, &model::Snapshot::default(), Depth::Everything, now, &login);
    println!("{:?}: {}", model::state(&snapshot, now), model::tooltip(&snapshot, now));
    println!();
    print!("{}", model::menu_text(&model::menu(&snapshot, now)));
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        // Shown only where the output goes somewhere (a pipe, a file): there is no console.
        println!("zone-tray {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--print-menu") {
        return print_menu();
    }
    #[cfg(windows)]
    {
        tray::win::run(args.iter().any(|arg| arg == "--handover"))
    }
    #[cfg(not(windows))]
    {
        eprintln!("zone-tray is Zone's icon beside the clock on Windows. Here it only prints the menu: zone-tray --print-menu");
        ExitCode::from(2)
    }
}
