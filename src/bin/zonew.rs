//! `zonew.exe`: starts `zone.exe` from the same folder, with the same arguments, and no window.
//!
//! zone.exe is a console program, and Windows opens a console window for a console program
//! that is started from outside a terminal (the login entry) before that program can do
//! anything about it. zonew.exe is a windowed program that never makes a window, so starting it
//! shows nothing, and it starts zone.exe without a console window. It ends at once, holding no
//! file open, so an update can replace it at any time. The login entry `zone install` writes is
//!
//!     "...\Zone\bin\zonew.exe" serve --background
//!
//! On other systems it does nothing: zone runs without a window there anyway.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Some(zone) = std::env::current_exe().ok().map(|me| me.with_file_name("zone.exe")) else {
        return ExitCode::from(1);
    };
    let started = Command::new(zone)
        .args(std::env::args_os().skip(1))
        // Already without a window: zone need not start itself once more to lose one.
        .env("ZONE_HIDDEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    // Nothing can be shown here: zone's own log says how the start went.
    if started.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("zonew starts zone without a window on Windows. Here, run zone itself.");
    ExitCode::from(2)
}
