//! Printing for the command line that survives a closed pipe.
//!
//! The standard `println!` panics when standard output is a pipe whose reader has gone away
//! (`zone mcp status | head -4`). Here the first failed write turns printing off for the rest
//! of the run: the command finishes what it was doing, says nothing more, and exits with its
//! usual code. `main.rs` defines `println!`, `print!`, `eprintln!` and `eprint!` in terms of
//! these, for every module, so no command can bring the panic back.

use std::fmt::Arguments;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// Writes to standard output. After a write has failed, does nothing.
pub fn stdout(args: Arguments<'_>, newline: bool) {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    let mut out = std::io::stdout().lock();
    let written = if newline { writeln!(out, "{args}") } else { write!(out, "{args}") };
    if written.is_err() {
        STDOUT_CLOSED.store(true, Ordering::Relaxed);
    }
}

/// Writes to standard error. A failure is ignored: there is nowhere left to report it.
pub fn stderr(args: Arguments<'_>, newline: bool) {
    let mut err = std::io::stderr().lock();
    let _ = if newline { writeln!(err, "{args}") } else { write!(err, "{args}") };
}
