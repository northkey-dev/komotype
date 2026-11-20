//! Hidden achievements.
//!
//! Definitions ship encrypted (`achievements.enc`) so that a glance at the repository or a diff
//! does not spoil them. This is obfuscation, not security: the key is in this file.
//! Maintainers edit them with `komotyped dev decrypt|encrypt|check`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use komotype_protocol::ipc::{Achievements, Rarity, UnlockedAchievement};
use serde::{Deserialize, Serialize};

const ENCRYPTED: &[u8] = include_bytes!("../achievements.enc");
const KEY: u64 = 0x6B6F_6D6F_2D61_6368; // "komo-ach"

/// Metrics a requirement can name. Two more are parameterized:
/// `words_hours_<from>_<to>` (words typed today between those local hours, wrapping past
/// midnight) and `vkey_today_<code>` (presses of a Windows virtual key today).
pub const METRICS: &[&str] = &[
    "lifetime_words",
    "lifetime_keys",
    "lifetime_backspaces",
    "perfect_bursts",
    "layer_switches",
    "hrm_holds",
    "hrm_taps",
    "today_words",
    "today_keys",
    "today_backspaces",
    "today_active_minutes",
    "today_accuracy",
    "today_share",
    "today_layers",
    "today_hrm_holds",
    "today_max_key",
    "today_distinct_keys",
    "burst_wpm",
    "burst_chars",
    "burst_accuracy",
    "burst_minutes",
    "best_wpm",
    "new_bests",
    "wpm_live",
    "rival_gap",
    "level",
    "prestige",
    "loyalty",
    "streak_days",
    "max_streak",
    "days_active",
    "days_since_start",
    "idle_return_days",
    "quests_completed",
    "weekly_completed",
    "perfect_quest_days",
    "season_xp",
    "seasons_played",
    "achievements_unlocked",
    "battery_min",
    "battery_max",
    "keyboard_uptime_hours",
    "on_bluetooth",
    "month",
    "day_of_month",
    "weekday",
    "hour",
];

pub fn is_known_metric(name: &str) -> bool {
    if METRICS.contains(&name) {
        return true;
    }
    if let Some(rest) = name.strip_prefix("words_hours_") {
        let mut parts = rest.split('_');
        let ok = |p: Option<&str>| {
            p.and_then(|v| v.parse::<u32>().ok())
                .is_some_and(|h| h <= 24)
        };
        return ok(parts.next()) && ok(parts.next()) && parts.next().is_none();
    }
    name.strip_prefix("vkey_today_")
        .is_some_and(|v| v.parse::<u16>().is_ok())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Definition {
    pub id: String,
    pub name: String,
    /// Shown once unlocked.
    pub description: String,
    pub rarity: Rarity,
    /// Secret achievements do not even show up as locked placeholders.
    #[serde(default)]
    pub secret: bool,
    /// All requirements must hold at the same moment.
    pub all: Vec<Requirement>,
}

pub fn xp_for(rarity: Rarity) -> u32 {
    match rarity {
        Rarity::Bronze => 100,
        Rarity::Silver => 300,
        Rarity::Gold => 1000,
        Rarity::Legendary => 5000,
    }
}

/// XOR with a SplitMix64 keystream. Symmetric: encrypts and decrypts.
pub fn crypt(data: &[u8]) -> Vec<u8> {
    let mut state = KEY;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(8) {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend(chunk.iter().zip(z.to_le_bytes()).map(|(b, k)| b ^ k));
    }
    out
}

pub fn parse(encrypted: &[u8]) -> Result<Vec<Definition>> {
    if encrypted.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&crypt(encrypted)).context("achievement definitions are corrupt")
}

pub fn builtin() -> Vec<Definition> {
    parse(ENCRYPTED).unwrap_or_else(|e| {
        tracing::warn!("{e:#}");
        Vec::new()
    })
}

/// Source of metric values. Returns `None` for metrics that are unknown or currently unavailable.
pub trait Metrics {
    fn value(&self, name: &str) -> Option<f64>;
}

/// Metric values at one moment.
pub struct Snapshot {
    pub scalars: HashMap<&'static str, f64>,
    /// Characters per local hour today.
    pub hour_chars: [u32; 24],
    /// Presses per Windows virtual key today.
    pub vkeys: HashMap<u16, u32>,
}

impl Metrics for Snapshot {
    fn value(&self, name: &str) -> Option<f64> {
        if let Some(v) = self.scalars.get(name) {
            return Some(*v);
        }
        if let Some(rest) = name.strip_prefix("words_hours_") {
            let (from, to) = rest.split_once('_')?;
            let (from, to): (usize, usize) = (from.parse().ok()?, to.parse().ok()?);
            let mut chars = 0;
            let mut h = from % 24;
            while h != to % 24 {
                chars += self.hour_chars[h];
                h = (h + 1) % 24;
            }
            return Some(chars as f64 / 5.0);
        }
        if let Some(code) = name.strip_prefix("vkey_today_") {
            let code: u16 = code.parse().ok()?;
            return Some(self.vkeys.get(&code).copied().unwrap_or(0) as f64);
        }
        None
    }
}

fn satisfied(req: &Requirement, metrics: &impl Metrics) -> bool {
    let Some(v) = metrics.value(&req.metric) else {
        return false;
    };
    req.min.is_none_or(|m| v >= m)
        && req.max.is_none_or(|m| v <= m)
        && req.eq.is_none_or(|e| (v - e).abs() < 1e-9)
}

pub struct Book {
    defs: Vec<Definition>,
    /// Achievement id → unlock time (Unix ms).
    unlocked: HashMap<String, i64>,
    pub dirty: bool,
}

impl Book {
    pub fn new(defs: Vec<Definition>, unlocked: HashMap<String, i64>) -> Self {
        Self {
            defs,
            unlocked,
            dirty: false,
        }
    }

    pub fn unlocked_ids(&self) -> &HashMap<String, i64> {
        &self.unlocked
    }

    pub fn unlocked_count(&self) -> usize {
        self.defs
            .iter()
            .filter(|d| self.unlocked.contains_key(&d.id))
            .count()
    }

    /// Unlocks every achievement whose requirements hold now and returns them.
    pub fn check(&mut self, metrics: &impl Metrics, at_ms: i64) -> Vec<Definition> {
        let mut new = Vec::new();
        for def in &self.defs {
            if self.unlocked.contains_key(&def.id) || def.all.is_empty() {
                continue;
            }
            if def.all.iter().all(|r| satisfied(r, metrics)) {
                self.unlocked.insert(def.id.clone(), at_ms);
                new.push(def.clone());
            }
        }
        if !new.is_empty() {
            self.dirty = true;
        }
        new
    }

    pub fn view(&self) -> Achievements {
        let mut unlocked: Vec<UnlockedAchievement> = self
            .defs
            .iter()
            .filter_map(|d| {
                let at_ms = *self.unlocked.get(&d.id)?;
                Some(UnlockedAchievement {
                    name: d.name.clone(),
                    description: d.description.clone(),
                    rarity: d.rarity,
                    xp: xp_for(d.rarity),
                    secret: d.secret,
                    at_ms,
                })
            })
            .collect();
        unlocked.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
        let mut locked: Vec<Rarity> = self
            .defs
            .iter()
            .filter(|d| !d.secret && !self.unlocked.contains_key(&d.id))
            .map(|d| d.rarity)
            .collect();
        locked.sort();
        Achievements { unlocked, locked }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(HashMap<&'static str, f64>);

    impl Metrics for Fixed {
        fn value(&self, name: &str) -> Option<f64> {
            self.0.get(name).copied()
        }
    }

    fn def(id: &str, metric: &str, min: f64) -> Definition {
        Definition {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            rarity: Rarity::Bronze,
            secret: false,
            all: vec![Requirement {
                metric: metric.into(),
                min: Some(min),
                max: None,
                eq: None,
            }],
        }
    }

    #[test]
    fn crypt_roundtrip() {
        let data = br#"[{"id":"x"}]"#;
        assert_ne!(crypt(data), data.to_vec());
        assert_eq!(crypt(&crypt(data)), data.to_vec());
    }

    #[test]
    fn builtin_definitions_are_valid() {
        let defs = builtin();
        let mut ids = std::collections::HashSet::new();
        for d in &defs {
            assert!(ids.insert(&d.id), "duplicate id {}", d.id);
            assert!(!d.all.is_empty(), "{} has no requirements", d.id);
            for r in &d.all {
                assert!(
                    is_known_metric(&r.metric),
                    "{}: unknown metric {}",
                    d.id,
                    r.metric
                );
            }
        }
    }

    #[test]
    fn unlocks_once() {
        let mut book = Book::new(vec![def("a", "today_words", 100.0)], HashMap::new());
        let low = Fixed(HashMap::from([("today_words", 50.0)]));
        let high = Fixed(HashMap::from([("today_words", 150.0)]));
        assert!(book.check(&low, 0).is_empty());
        assert_eq!(book.check(&high, 1).len(), 1);
        assert!(book.check(&high, 2).is_empty());
        assert_eq!(book.view().unlocked.len(), 1);
    }

    #[test]
    fn parameterized_metrics_are_known() {
        assert!(is_known_metric("words_hours_0_4"));
        assert!(is_known_metric("vkey_today_13"));
        assert!(!is_known_metric("words_hours_25_4"));
        assert!(!is_known_metric("nope"));
    }
}
