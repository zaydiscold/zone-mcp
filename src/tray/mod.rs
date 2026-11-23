//! Zone for Windows: the Z in the notification area (the tray), beside the clock.
//!
//! What it shows and decides is worked out in plain code that runs and is tested on every
//! system (`model`, `zone`, `handover`, `ico`). Only `win` talks to Windows.

pub mod handover;
pub mod ico;
pub mod model;
pub mod zone;

#[cfg(windows)]
pub mod win;
