//! IPC between `komotyped` and its clients.
//!
//! Transport: named pipe [`PIPE_NAME`], newline-delimited JSON. After connecting, the daemon
//! sends a [`Status`] line whenever something changes and at least once per second.

use serde::{Deserialize, Serialize};

use crate::raw_hid::KeyboardReport;

pub const PIPE_NAME: &str = r"\\.\pipe\komotype";

/// Bumped on breaking changes to [`Status`].
pub const IPC_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub ipc_version: u32,
    /// Display name of the configured main keyboard, `None` if none is configured yet.
    pub main_keyboard: Option<String>,
    /// Whether the main keyboard is currently attached (USB or Bluetooth).
    pub main_connected: bool,
    pub typing: Typing,
    /// Share of today's keystrokes typed on the main keyboard (0.0..=1.0).
    pub main_share_today: Option<f32>,
    /// Latest state from the komotype firmware module, `None` without module or when stale.
    pub firmware: Option<FirmwareState>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Typing {
    /// Words per minute over the last seconds, 0 when idle.
    pub wpm_live: f32,
    /// WPM of the current typing burst (continuous typing with pauses under 5 s).
    pub wpm_burst: Option<f32>,
    pub chars_today: u32,
    pub words_today: u32,
    pub backspaces_today: u32,
    /// 1 - backspaces / chars.
    pub accuracy_today: Option<f32>,
    pub active_seconds_today: u32,
    /// Best WPM of a burst lasting at least 30 s.
    pub best_wpm_today: Option<f32>,
    pub best_wpm_ever: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transport {
    Usb,
    Bluetooth,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirmwareState {
    pub transport: Transport,
    #[serde(flatten)]
    pub report: KeyboardReport,
}
