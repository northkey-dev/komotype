//! IPC between `komotyped` and its clients.
//!
//! Transport: named pipe [`PIPE_NAME`], newline-delimited JSON. After connecting, the daemon
//! sends a [`Status`] line whenever something changes and at least once per second. Clients may
//! send [`Command`] lines.

use serde::{Deserialize, Serialize};

use crate::raw_hid::KeyboardReport;

pub const PIPE_NAME: &str = r"\\.\pipe\komotype";

/// Bumped on breaking changes to [`Status`].
pub const IPC_VERSION: u32 = 4;

/// Client → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Reset the level to 1 in exchange for a prestige star. Only allowed at the maximum level.
    Prestige,
    /// Make this keyboard the main keyboard.
    SetMain {
        vid: u16,
        pid: u16,
        product: Option<String>,
    },
    /// The next key press decides the main keyboard.
    PickNext,
    CancelPick,
}

/// An attached keyboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyboard {
    pub name: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub transport: Transport,
    pub main: bool,
}

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
    /// Progression: level, streak, quests. `None` until a main keyboard is configured.
    pub game: Option<Game>,
    /// Today's per-key counts from the firmware module, `None` without module data.
    pub key_stats: Option<KeyStats>,
    /// Achievements, `None` until a main keyboard is configured.
    pub achievements: Option<Achievements>,
    /// Attached keyboards, for choosing the main one.
    pub keyboards: Vec<Keyboard>,
    /// Waiting for a key press to choose the main keyboard ([`Command::PickNext`]).
    pub picking: bool,
}

/// Counts per physical key position (index = position in the keymap).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyStats {
    pub presses: Vec<u32>,
    /// Hold-tap keys only: resolved as tap.
    pub taps: Vec<u32>,
    /// Hold-tap keys only: resolved as hold (modifier).
    pub holds: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rarity {
    Bronze,
    Silver,
    Gold,
    Legendary,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Achievements {
    /// Newest first.
    pub unlocked: Vec<UnlockedAchievement>,
    /// Rarity of each locked achievement that is not secret. Secret ones are not listed at all.
    pub locked: Vec<Rarity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnlockedAchievement {
    pub name: String,
    pub description: String,
    pub rarity: Rarity,
    pub xp: u32,
    pub secret: bool,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Game {
    pub level: u32,
    pub max_level: u32,
    /// XP earned inside the current level.
    pub level_xp: u64,
    /// XP needed to finish the current level (0 at the maximum level).
    pub level_xp_needed: u64,
    pub total_xp: u64,
    pub prestige: u32,
    /// Rank title for the current level.
    pub title: String,
    /// Level at which the next (still secret) title unlocks.
    pub next_title_level: Option<u32>,
    /// XP multiplier for staying on the main keyboard (1.0..=2.0).
    pub loyalty: f32,
    pub streak_days: u32,
    pub streak_freezes: u32,
    /// Keystrokes on the main keyboard today, towards `streak_threshold`.
    pub streak_today: u32,
    pub streak_threshold: u32,
    pub quests: Vec<Quest>,
    pub weekly: Option<Quest>,
    /// Season id, e.g. "2026-Q4".
    pub season: String,
    pub season_xp: u64,
    /// Average WPM over the last 7 days (bursts of 30 s or more).
    pub wpm_7d: Option<f32>,
    /// Same for all other keyboards: the rival to beat.
    pub rival_wpm_7d: Option<f32>,
    /// Most recent first.
    pub events: Vec<GameEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quest {
    /// May contain `{layer}`, to be replaced with the name of [`Quest::layer`].
    pub title: String,
    pub layer: Option<u8>,
    pub progress: u32,
    pub target: u32,
    pub reward_xp: u32,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEvent {
    /// Unix time in milliseconds.
    pub at_ms: i64,
    pub kind: GameEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GameEventKind {
    LevelUp {
        level: u32,
    },
    TitleUnlocked {
        title: String,
    },
    QuestCompleted {
        title: String,
        layer: Option<u8>,
        xp: u32,
    },
    WeeklyCompleted {
        title: String,
        xp: u32,
    },
    StreakExtended {
        days: u32,
        xp: u32,
    },
    StreakFrozen {
        days: u32,
    },
    StreakLost {
        days: u32,
    },
    LoyaltyChanged {
        from: f32,
        to: f32,
    },
    NewBestWpm {
        wpm: f32,
    },
    SeasonStarted {
        season: String,
    },
    Prestige {
        prestige: u32,
    },
    AchievementUnlocked {
        name: String,
        rarity: Rarity,
        xp: u32,
    },
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
