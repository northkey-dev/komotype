//! Progression: XP, levels, titles, loyalty multiplier, streak, quests, seasons, prestige.
//!
//! Pure logic. The engine feeds it typing events and day summaries and persists [`GameState`].

use std::collections::{BTreeSet, VecDeque};

use chrono::{Datelike, NaiveDate};
use komotype_protocol::ipc::{self, GameEvent, GameEventKind};
use serde::{Deserialize, Serialize};

pub const MAX_LEVEL: u32 = 100;
/// One XP per word (5 characters).
const XP_PER_CHAR: f64 = 0.2;
const LOYALTY_MAX: f32 = 2.0;
const LOYALTY_STEP_UP: f32 = 0.1;
const LOYALTY_STEP_DOWN: f32 = 0.3;
/// Days with fewer keystrokes in total do not change the loyalty multiplier.
const LOYALTY_MIN_KEYS: u32 = 500;
const FREEZE_EVERY_DAYS: u32 = 7;
const MAX_FREEZES: u32 = 2;
const MAX_EVENTS: usize = 20;

/// Rank titles, one every 5 levels. The UI only reveals titles already reached.
const TITLES: &[(u32, &str)] = &[
    (1, "Hunt & Peck"),
    (5, "Key Tapper"),
    (10, "Home Row Rookie"),
    (15, "Layer Explorer"),
    (20, "Steady Striker"),
    (25, "Combo Crafter"),
    (30, "Flow Seeker"),
    (35, "Rhythm Keeper"),
    (40, "Swift Fingers"),
    (45, "Keystorm"),
    (50, "Clack Adept"),
    (55, "Thumb Cluster Sage"),
    (60, "Velocity Smith"),
    (65, "Ergonomist"),
    (70, "Split Master"),
    (75, "Keyboard Monk"),
    (80, "Clackstorm"),
    (85, "Glyph Weaver"),
    (90, "Grand Typist"),
    (95, "Mythic Hands"),
    (100, "Komotype Legend"),
];

/// XP needed to go from `level` to `level + 1`.
pub fn xp_for_level(level: u32) -> u64 {
    (100.0 * (level as f64).powf(1.5)).round() as u64
}

/// (level, xp inside the level, xp needed for the level) for a total amount of XP.
pub fn level_of(total: u64) -> (u32, u64, u64) {
    let mut level = 1;
    let mut rest = total;
    while level < MAX_LEVEL {
        let need = xp_for_level(level);
        if rest < need {
            return (level, rest, need);
        }
        rest -= need;
        level += 1;
    }
    (MAX_LEVEL, rest, 0)
}

pub fn title_for(level: u32) -> &'static str {
    TITLES
        .iter()
        .rev()
        .find(|(l, _)| *l <= level)
        .map_or("", |(_, t)| t)
}

fn next_title_level(level: u32) -> Option<u32> {
    TITLES.iter().map(|(l, _)| *l).find(|l| *l > level)
}

pub fn season_of(date: NaiveDate) -> String {
    format!("{}-Q{}", date.year(), date.month0() / 3 + 1)
}

pub fn week_start(date: NaiveDate) -> NaiveDate {
    date - chrono::Duration::days(date.weekday().num_days_from_monday() as i64)
}

/// Keystroke totals of one typing day.
#[derive(Debug, Default, Clone, Copy)]
pub struct DaySummary {
    pub main_keys: u32,
    pub all_keys: u32,
}

/// Personal averages used to size quests.
#[derive(Debug, Clone, Copy)]
pub struct Baseline {
    pub words_per_day: f32,
    pub wpm: f32,
}

impl Default for Baseline {
    fn default() -> Self {
        Self {
            words_per_day: 600.0,
            wpm: 35.0,
        }
    }
}

/// Today's numbers, as far as quests need them.
#[derive(Debug, Default, Clone, Copy)]
pub struct DayView {
    pub main_chars: u32,
    pub main_keys: u32,
    pub all_keys: u32,
    pub active_seconds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum QuestKind {
    Words,
    AccurateWords { min_accuracy_pct: u8 },
    SpeedBurst { wpm: u32 },
    ActiveMinutes,
    Share { pct: u8, min_keys: u32 },
    Layer { layer: u8 },
    MorningWords { before_hour: u8 },
    WeeklyWords,
    WeeklyStreakDays,
    WeeklySpeed { wpm: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Quest {
    kind: QuestKind,
    target: u32,
    /// Characters for word quests, otherwise in the quest's own unit.
    progress: u32,
    reward: u32,
    done: bool,
}

impl Quest {
    fn counts_words(&self) -> bool {
        matches!(
            self.kind,
            QuestKind::Words
                | QuestKind::AccurateWords { .. }
                | QuestKind::MorningWords { .. }
                | QuestKind::WeeklyWords
        )
    }

    fn shown_progress(&self) -> u32 {
        let p = if self.counts_words() {
            self.progress / 5
        } else {
            self.progress
        };
        p.min(self.target)
    }

    fn title(&self) -> String {
        let t = self.target;
        match self.kind {
            QuestKind::Words => format!("Type {t} words"),
            QuestKind::AccurateWords { min_accuracy_pct } => {
                format!("Type {t} words in bursts with {min_accuracy_pct}%+ accuracy")
            }
            QuestKind::SpeedBurst { wpm } => format!("Hold {wpm}+ WPM for 30 seconds"),
            QuestKind::ActiveMinutes => format!("Type actively for {t} minutes"),
            QuestKind::Share { pct, min_keys } => {
                format!("Keep {pct}% of {min_keys}+ keystrokes on your main keyboard")
            }
            QuestKind::Layer { .. } => format!("Use layer {{layer}} {t} times"),
            QuestKind::MorningWords { before_hour } => {
                format!("Type {t} words before {before_hour}:00")
            }
            QuestKind::WeeklyWords => format!("Type {t} words this week"),
            QuestKind::WeeklyStreakDays => format!("Hit your daily goal on {t} days this week"),
            QuestKind::WeeklySpeed { wpm } => format!("Hold {wpm}+ WPM for 30 seconds this week"),
        }
    }

    fn view(&self) -> ipc::Quest {
        ipc::Quest {
            title: self.title(),
            layer: match self.kind {
                QuestKind::Layer { layer } => Some(layer),
                _ => None,
            },
            progress: self.shown_progress(),
            target: self.target,
            reward_xp: self.reward,
            done: self.done,
        }
    }

    fn is_complete(&self) -> bool {
        match self.kind {
            QuestKind::Share { pct, .. } => self.progress >= pct as u32,
            _ => self.shown_progress() >= self.target,
        }
    }
}

/// Small deterministic RNG (SplitMix64), so quests of a day are stable across restarts.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x6B6F_6D6F_7479_7065) // "komotype"
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn round_to(value: f32, step: u32) -> u32 {
    ((value / step as f32).round() as u32 * step).max(step)
}

fn day_seed(date: NaiveDate) -> u64 {
    date.num_days_from_ce() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameState {
    total_xp: f64,
    prestige: u32,
    loyalty: f32,
    streak: u32,
    freezes: u32,
    days_toward_freeze: u32,
    last_evaluated: Option<NaiveDate>,
    quest_day: Option<NaiveDate>,
    quests: Vec<Quest>,
    week: Option<NaiveDate>,
    weekly: Option<Quest>,
    week_chars: u32,
    week_goal_days: u32,
    season: String,
    season_xp: f64,
    layers_seen: BTreeSet<u8>,
    events: VecDeque<GameEvent>,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            total_xp: 0.0,
            prestige: 0,
            loyalty: 1.0,
            streak: 0,
            freezes: 0,
            days_toward_freeze: 0,
            last_evaluated: None,
            quest_day: None,
            quests: Vec::new(),
            week: None,
            weekly: None,
            week_chars: 0,
            week_goal_days: 0,
            season: String::new(),
            season_xp: 0.0,
            layers_seen: BTreeSet::new(),
            events: VecDeque::new(),
        }
    }
}

pub struct Game {
    pub state: GameState,
    /// Set when the state changed and should be saved.
    pub dirty: bool,
}

impl Game {
    pub fn new(state: GameState) -> Self {
        Self {
            state,
            dirty: false,
        }
    }

    fn level(&self) -> u32 {
        level_of(self.state.total_xp as u64).0
    }

    fn push(&mut self, at_ms: i64, kind: GameEventKind) {
        self.state.events.push_front(GameEvent { at_ms, kind });
        self.state.events.truncate(MAX_EVENTS);
        self.dirty = true;
    }

    /// Adds XP and reports level-ups and new titles.
    fn add_xp(&mut self, xp: f64, at_ms: i64) {
        if xp <= 0.0 {
            return;
        }
        let before = self.level();
        self.state.total_xp += xp;
        self.state.season_xp += xp;
        self.dirty = true;
        let after = self.level();
        for level in before + 1..=after {
            self.push(at_ms, GameEventKind::LevelUp { level });
            if TITLES.iter().any(|(l, _)| *l == level) {
                self.push(
                    at_ms,
                    GameEventKind::TitleUnlocked {
                        title: title_for(level).into(),
                    },
                );
            }
        }
    }

    fn reward_scale(&self) -> f32 {
        1.0 + self.level() as f32 / 25.0
    }

    /// A character typed on the main keyboard.
    pub fn on_char(&mut self, hour: u32, at_ms: i64) {
        self.add_xp(XP_PER_CHAR * self.state.loyalty as f64, at_ms);
        self.state.week_chars += 1;
        for q in &mut self.state.quests {
            if let QuestKind::MorningWords { before_hour } = q.kind
                && hour < before_hour as u32
                && hour >= 4
            {
                q.progress += 1;
            }
        }
        self.check_quests(at_ms);
    }

    /// A finished typing burst on the main keyboard.
    pub fn on_burst(&mut self, chars: u32, backspaces: u32, wpm_30s: Option<f32>, at_ms: i64) {
        if chars >= 50 {
            let accuracy = 1.0 - backspaces as f64 / chars as f64;
            let bonus = match accuracy {
                a if a >= 0.97 => 0.2,
                a if a >= 0.94 => 0.1,
                _ => 0.0,
            };
            self.add_xp(
                chars as f64 * XP_PER_CHAR * self.state.loyalty as f64 * bonus,
                at_ms,
            );
        }
        let accuracy_pct = if chars > 0 {
            100.0 * (1.0 - backspaces as f32 / chars as f32)
        } else {
            0.0
        };
        let quests = self
            .state
            .quests
            .iter_mut()
            .chain(self.state.weekly.as_mut());
        for q in quests {
            match q.kind {
                QuestKind::AccurateWords { min_accuracy_pct }
                    if accuracy_pct >= min_accuracy_pct as f32 =>
                {
                    q.progress += chars;
                }
                QuestKind::SpeedBurst { wpm } | QuestKind::WeeklySpeed { wpm }
                    if wpm_30s.is_some_and(|w| w >= wpm as f32) =>
                {
                    q.progress = 1;
                }
                _ => {}
            }
        }
        self.check_quests(at_ms);
    }

    /// The keyboard switched to `layer` (from firmware reports).
    pub fn on_layer(&mut self, layer: u8, at_ms: i64) {
        if layer == 0 {
            return;
        }
        if self.state.layers_seen.insert(layer) {
            self.dirty = true;
        }
        for q in &mut self.state.quests {
            if q.kind == (QuestKind::Layer { layer }) {
                q.progress += 1;
            }
        }
        self.check_quests(at_ms);
    }

    pub fn on_new_best(&mut self, wpm: f32, at_ms: i64) {
        self.push(at_ms, GameEventKind::NewBestWpm { wpm });
    }

    /// Updates quests whose progress comes from today's totals.
    pub fn update_day(&mut self, day: &DayView, streak_threshold: u32, at_ms: i64) {
        for q in &mut self.state.quests {
            match q.kind {
                QuestKind::Words => q.progress = q.progress.max(day.main_chars),
                QuestKind::ActiveMinutes => q.progress = q.progress.max(day.active_seconds / 60),
                QuestKind::Share { min_keys, .. }
                    if day.all_keys > 0 && day.main_keys >= min_keys =>
                {
                    q.progress = (100 * day.main_keys / day.all_keys).max(q.progress);
                }
                _ => {}
            }
        }
        if let Some(q) = &mut self.state.weekly {
            match q.kind {
                QuestKind::WeeklyWords => q.progress = self.state.week_chars,
                QuestKind::WeeklyStreakDays => {
                    let today = u32::from(day.main_keys >= streak_threshold);
                    q.progress = self.state.week_goal_days + today;
                }
                _ => {}
            }
        }
        self.check_quests(at_ms);
    }

    fn check_quests(&mut self, at_ms: i64) {
        let scale = self.reward_scale();
        let mut rewards = Vec::new();
        for q in &mut self.state.quests {
            if !q.done && q.is_complete() {
                q.done = true;
                let xp = (q.reward as f32 * scale).round() as u32;
                rewards.push(GameEventKind::QuestCompleted {
                    title: q.title(),
                    layer: q.view().layer,
                    xp,
                });
            }
        }
        if let Some(q) = &mut self.state.weekly
            && !q.done
            && q.is_complete()
        {
            q.done = true;
            let xp = (q.reward as f32 * scale).round() as u32;
            rewards.push(GameEventKind::WeeklyCompleted {
                title: q.title(),
                xp,
            });
        }
        for kind in rewards {
            let xp = match &kind {
                GameEventKind::QuestCompleted { xp, .. }
                | GameEventKind::WeeklyCompleted { xp, .. } => *xp,
                _ => 0,
            };
            self.push(at_ms, kind);
            self.add_xp(xp as f64, at_ms);
        }
    }

    /// Settles every day since the last call (streak, loyalty) and prepares `today`
    /// (quests, weekly challenge, season). `history` returns the totals of a past day.
    pub fn start_day(
        &mut self,
        today: NaiveDate,
        history: impl Fn(NaiveDate) -> DaySummary,
        baseline: Baseline,
        streak_threshold: u32,
        at_ms: i64,
    ) {
        let yesterday = today.pred_opt().unwrap_or(today);
        // First run: nothing to settle, history before komotype does not count.
        let mut day = match self.state.last_evaluated {
            Some(d) => d.succ_opt().unwrap_or(d),
            None => today,
        };
        // Week of the days being settled; `week_goal_days` counts goal days within it.
        let mut settling_week = self.state.week;
        while day <= yesterday {
            if settling_week != Some(week_start(day)) {
                settling_week = Some(week_start(day));
                self.state.week_goal_days = 0;
            }
            self.settle_day(history(day), streak_threshold, at_ms);
            day = day.succ_opt().unwrap_or(day);
        }
        self.state.last_evaluated = Some(yesterday);

        let season = season_of(today);
        if self.state.season != season {
            if !self.state.season.is_empty() {
                self.push(
                    at_ms,
                    GameEventKind::SeasonStarted {
                        season: season.clone(),
                    },
                );
            }
            self.state.season = season;
            self.state.season_xp = 0.0;
        }
        let this_week = week_start(today);
        if self.state.week != Some(this_week) {
            if settling_week != Some(this_week) {
                // No settled day belongs to this week yet.
                self.state.week_goal_days = 0;
            }
            self.state.week_chars = 0;
            self.state.week = Some(this_week);
            self.state.weekly = Some(weekly_quest(this_week, baseline));
        } else if self.state.weekly.is_none() {
            self.state.weekly = Some(weekly_quest(this_week, baseline));
        }
        if self.state.quest_day != Some(today) {
            self.state.quest_day = Some(today);
            self.state.quests = daily_quests(today, baseline, &self.state.layers_seen);
        }
        self.dirty = true;
    }

    fn settle_day(&mut self, day: DaySummary, threshold: u32, at_ms: i64) {
        let s = &mut self.state;
        if day.main_keys >= threshold {
            s.streak += 1;
            s.week_goal_days += 1;
            s.days_toward_freeze += 1;
            if s.days_toward_freeze >= FREEZE_EVERY_DAYS {
                s.days_toward_freeze = 0;
                s.freezes = (s.freezes + 1).min(MAX_FREEZES);
            }
            let days = s.streak;
            let xp = (100 + 10 * days).min(500);
            self.push(at_ms, GameEventKind::StreakExtended { days, xp });
            self.add_xp(xp as f64, at_ms);
        } else if s.streak > 0 {
            if s.freezes > 0 {
                s.freezes -= 1;
                let days = s.streak;
                self.push(at_ms, GameEventKind::StreakFrozen { days });
            } else {
                let days = s.streak;
                s.streak = 0;
                s.days_toward_freeze = 0;
                self.push(at_ms, GameEventKind::StreakLost { days });
            }
        }

        if day.all_keys >= LOYALTY_MIN_KEYS {
            let share = day.main_keys as f32 / day.all_keys as f32;
            let from = self.state.loyalty;
            let to = if share >= 0.95 {
                (from + LOYALTY_STEP_UP).min(LOYALTY_MAX)
            } else if share < 0.5 {
                1.0
            } else {
                (from - LOYALTY_STEP_DOWN).max(1.0)
            };
            let to = (to * 10.0).round() / 10.0;
            if (to - from).abs() > f32::EPSILON {
                self.state.loyalty = to;
                self.push(at_ms, GameEventKind::LoyaltyChanged { from, to });
            }
        }
    }

    /// Resets the level for a prestige star. Returns false below the maximum level.
    pub fn prestige(&mut self, at_ms: i64) -> bool {
        if self.level() < MAX_LEVEL {
            return false;
        }
        self.state.total_xp = 0.0;
        self.state.prestige += 1;
        let prestige = self.state.prestige;
        self.push(at_ms, GameEventKind::Prestige { prestige });
        true
    }

    pub fn view(
        &self,
        day: &DayView,
        streak_threshold: u32,
        wpm_7d: Option<f32>,
        rival_wpm_7d: Option<f32>,
    ) -> ipc::Game {
        let total = self.state.total_xp as u64;
        let (level, level_xp, level_xp_needed) = level_of(total);
        ipc::Game {
            level,
            max_level: MAX_LEVEL,
            level_xp,
            level_xp_needed,
            total_xp: total,
            prestige: self.state.prestige,
            title: title_for(level).into(),
            next_title_level: next_title_level(level),
            loyalty: self.state.loyalty,
            streak_days: self.state.streak,
            streak_freezes: self.state.freezes,
            streak_today: day.main_keys,
            streak_threshold,
            quests: self.state.quests.iter().map(Quest::view).collect(),
            weekly: self.state.weekly.as_ref().map(Quest::view),
            season: self.state.season.clone(),
            season_xp: self.state.season_xp as u64,
            wpm_7d,
            rival_wpm_7d,
            events: self.state.events.iter().take(5).cloned().collect(),
        }
    }
}

fn daily_quests(date: NaiveDate, b: Baseline, layers_seen: &BTreeSet<u8>) -> Vec<Quest> {
    let mut rng = Rng::new(day_seed(date));
    let words = b.words_per_day.max(200.0);
    let wpm = b.wpm.max(15.0);
    let quest = |kind, target, reward| Quest {
        kind,
        target,
        progress: 0,
        reward,
        done: false,
    };

    let mut pool: Vec<Quest> = vec![
        quest(
            QuestKind::Words,
            round_to(words * rng.range(0.6, 1.1), 50),
            150,
        ),
        quest(
            QuestKind::AccurateWords {
                min_accuracy_pct: 96,
            },
            round_to(words * rng.range(0.3, 0.5), 50),
            200,
        ),
        quest(
            QuestKind::SpeedBurst {
                wpm: (wpm * rng.range(0.95, 1.08)).round() as u32,
            },
            1,
            250,
        ),
        quest(
            QuestKind::ActiveMinutes,
            round_to(words / wpm * rng.range(0.5, 0.9), 5).max(10),
            150,
        ),
        quest(
            QuestKind::Share {
                pct: 98,
                min_keys: 3000,
            },
            98,
            200,
        ),
        quest(
            QuestKind::MorningWords { before_hour: 10 },
            round_to(rng.range(150.0, 300.0), 50),
            200,
        ),
    ];
    if !layers_seen.is_empty() {
        let layers: Vec<u8> = layers_seen.iter().copied().collect();
        let layer = layers[rng.below(layers.len() as u64) as usize];
        pool.push(quest(
            QuestKind::Layer { layer },
            round_to(rng.range(20.0, 60.0), 5),
            150,
        ));
    }

    let mut picked = Vec::new();
    while picked.len() < 3 && !pool.is_empty() {
        let i = rng.below(pool.len() as u64) as usize;
        picked.push(pool.swap_remove(i));
    }
    picked
}

fn weekly_quest(week: NaiveDate, b: Baseline) -> Quest {
    let mut rng = Rng::new(day_seed(week).wrapping_mul(31));
    let words = b.words_per_day.max(200.0);
    let (kind, target, reward) = match rng.below(3) {
        0 => (
            QuestKind::WeeklyWords,
            round_to(words * 5.0 * rng.range(0.9, 1.2), 500),
            1500,
        ),
        1 => (QuestKind::WeeklyStreakDays, 5, 1500),
        _ => (
            QuestKind::WeeklySpeed {
                wpm: (b.wpm.max(15.0) * rng.range(1.03, 1.08)).round() as u32,
            },
            1,
            2000,
        ),
    };
    Quest {
        kind,
        target,
        progress: 0,
        reward,
        done: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn level_curve() {
        assert_eq!(level_of(0), (1, 0, 100));
        assert_eq!(level_of(100), (2, 0, 283));
        // About 1700 XP (words) reach level 5, i.e. on the first day.
        assert_eq!(level_of(1703).0, 5);
        assert_eq!(level_of(u64::MAX / 2).0, MAX_LEVEL);
        assert_eq!(title_for(1), "Hunt & Peck");
        assert_eq!(next_title_level(12), Some(15));
    }

    #[test]
    fn chars_give_xp_and_level_ups() {
        let mut g = Game::new(GameState::default());
        for _ in 0..500 {
            g.on_char(12, 0);
        }
        assert_eq!(g.state.total_xp.round() as u64, 100);
        assert!(
            g.state
                .events
                .iter()
                .any(|e| e.kind == GameEventKind::LevelUp { level: 2 })
        );
    }

    #[test]
    fn streak_freeze_and_loss() {
        let mut g = Game::new(GameState::default());
        let good = DaySummary {
            main_keys: 3000,
            all_keys: 3000,
        };
        let none = DaySummary::default();
        g.start_day(date("2026-11-01"), |_| good, Baseline::default(), 2000, 0);
        // Eight good days: streak 8, one freeze earned after 7.
        g.start_day(date("2026-11-09"), |_| good, Baseline::default(), 2000, 0);
        assert_eq!(g.state.streak, 8);
        assert_eq!(g.state.freezes, 1);
        // One missed day uses the freeze, the second one ends the streak.
        g.start_day(date("2026-11-10"), |_| none, Baseline::default(), 2000, 0);
        assert_eq!((g.state.streak, g.state.freezes), (8, 0));
        g.start_day(date("2026-11-11"), |_| none, Baseline::default(), 2000, 0);
        assert_eq!(g.state.streak, 0);
    }

    #[test]
    fn loyalty_rises_and_drops() {
        let mut g = Game::new(GameState::default());
        let loyal = DaySummary {
            main_keys: 2000,
            all_keys: 2000,
        };
        g.start_day(date("2026-11-01"), |_| loyal, Baseline::default(), 2000, 0);
        g.start_day(date("2026-11-06"), |_| loyal, Baseline::default(), 2000, 0);
        assert!((g.state.loyalty - 1.5).abs() < 1e-6);
        let cheating = DaySummary {
            main_keys: 400,
            all_keys: 2000,
        };
        g.start_day(
            date("2026-11-07"),
            |_| cheating,
            Baseline::default(),
            2000,
            0,
        );
        assert!((g.state.loyalty - 1.0).abs() < 1e-6);
    }

    #[test]
    fn quests_are_stable_per_day() {
        let a = daily_quests(date("2026-11-20"), Baseline::default(), &BTreeSet::new());
        let b = daily_quests(date("2026-11-20"), Baseline::default(), &BTreeSet::new());
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn word_quest_completes_and_rewards() {
        let mut g = Game::new(GameState::default());
        g.state.quests = vec![Quest {
            kind: QuestKind::Words,
            target: 10,
            progress: 0,
            reward: 100,
            done: false,
        }];
        let day = DayView {
            main_chars: 50,
            ..Default::default()
        };
        g.update_day(&day, 2000, 0);
        assert!(g.state.quests[0].done);
        assert!(g.state.total_xp >= 100.0);
    }
}
