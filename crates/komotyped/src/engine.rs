//! Combines key events and firmware reports into statistics and progression, persists them and
//! publishes [`Status`] snapshots for IPC clients.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use komotype_protocol::ipc::{
    Command, FirmwareState, GameEventKind, IPC_VERSION, KeyStats, Keyboard, Status, Transport,
    Typing,
};
use komotype_protocol::raw_hid::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::achievements::{self, Book, Snapshot};

use crate::config::{self, Config};
use crate::db::{Db, MinuteCounts};
use crate::devices::{self, DeviceInfo, DeviceMatcher};
use crate::game::{Baseline, DaySummary, DayView, Game, GameState};
use crate::input::{DeviceHandle, InputEvent};
use crate::notify;
use crate::stats::{Counted, FinishedBurst, KeyFilter, RECORD_MIN_DURATION, Session};

/// The typing day starts at 04:00 local time, so late-night sessions count for the evening.
const DAY_START_HOUR: u32 = 4;
const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);
const CONFIG_CHECK_INTERVAL: Duration = Duration::from_secs(3);
const BATTERY_SAMPLE_INTERVAL: Duration = Duration::from_secs(600);
const AVERAGES_INTERVAL: Duration = Duration::from_secs(300);
/// Firmware state older than this is treated as gone (heartbeat is 5 s).
const FIRMWARE_STALE: Duration = Duration::from_secs(15);
/// Bursts with fewer characters are noise (e.g. a single shortcut) and are not stored.
const MIN_BURST_CHARS: u32 = 5;
const GAME_STATE_KEY: &str = "game";
const ACHIEVEMENTS_KEY: &str = "achievements";
const METRICS_KEY: &str = "metrics";
const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;

pub enum Event {
    Input(InputEvent),
    Firmware(FirmwareState),
    /// Key or hold-tap statistics from the firmware.
    FirmwareStats(Message),
    Command(Command),
}

struct Known {
    info: DeviceInfo,
    db_id: i64,
    is_main: bool,
}

#[derive(Default)]
struct Day {
    date: NaiveDate,
    main: MinuteCounts,
    all_keys: u32,
    active_ms: i64,
    best_wpm: Option<f32>,
    /// Per key position: presses, taps, holds (from the firmware).
    positions: Vec<[u32; 3]>,
    /// Characters per local hour on the main keyboard.
    hour_chars: [u32; 24],
    /// Non-base layers used today.
    layers: BTreeSet<u8>,
    /// Presses per virtual key on the main keyboard.
    vkeys: HashMap<u16, u32>,
}

/// Lifetime totals of the main keyboard, loaded from the database and counted on.
#[derive(Default)]
struct Lifetime {
    chars: u64,
    keys: u64,
    backspaces: u64,
    taps: u64,
    holds: u64,
}

/// Counters that are not derivable from stored statistics.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Extra {
    perfect_bursts: u64,
    layer_switches: u64,
    new_bests: u64,
}

#[derive(Default, Clone, Copy)]
struct LastBurst {
    /// Only bursts of 30 s or more, else 0.
    wpm: f32,
    chars: u32,
    /// Only bursts of 200+ characters, else 0.
    accuracy: f32,
    minutes: f32,
}

pub struct Engine {
    db: Db,
    config: Config,
    config_mtime: Option<SystemTime>,
    status_tx: watch::Sender<Status>,
    known: HashMap<DeviceHandle, Known>,
    attached: HashSet<DeviceHandle>,
    filter: KeyFilter,
    session: Session,
    /// Typing on other keyboards, to measure the rival's speed.
    rival_session: Session,
    rival_db_id: Option<i64>,
    main_db_id: Option<i64>,
    day: Day,
    best_ever: Option<f32>,
    game: Game,
    book: Book,
    lifetime: Lifetime,
    extra: Extra,
    last_burst: LastBurst,
    /// Game events up to this time were already considered for notifications.
    last_toast_ms: i64,
    /// The next key press chooses the main keyboard.
    picking: bool,
    wpm_7d: Option<f32>,
    rival_wpm_7d: Option<f32>,
    pending_minutes: HashMap<(i64, i64), MinuteCounts>,
    pending_keys: HashMap<(i64, String, u16), u32>,
    pending_positions: HashMap<(i64, String, u8), [u32; 3]>,
    /// Stats reports already counted, by (message type, sequence). The keyboard sends each report
    /// over every connected transport, so USB + Bluetooth deliver it twice.
    seen_stats: HashMap<(u8, u8), Instant>,
    firmware: HashMap<Transport, (FirmwareState, Instant)>,
    last_flush: Instant,
    last_publish: Instant,
    last_config_check: Instant,
    last_averages: Instant,
    last_battery_sample: Option<Instant>,
}

fn unix_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn now_ms() -> i64 {
    unix_ms(SystemTime::now())
}

/// Start (inclusive) and end (exclusive) of a typing day, in Unix milliseconds.
fn day_bounds(date: NaiveDate) -> (i64, i64) {
    let start_of = |d: NaiveDate| {
        let t = d.and_hms_opt(DAY_START_HOUR, 0, 0).expect("valid time");
        Local
            .from_local_datetime(&t)
            .earliest()
            .map_or(0, |d| d.timestamp_millis())
    };
    (start_of(date), start_of(date.succ_opt().unwrap_or(date)))
}

fn current_day() -> NaiveDate {
    (Local::now() - chrono::Duration::hours(DAY_START_HOUR as i64)).date_naive()
}

impl Engine {
    pub fn new(db: Db, status_tx: watch::Sender<Status>) -> Result<Self> {
        let now = Instant::now();
        let game_state = match db.load_value(GAME_STATE_KEY)? {
            Some(json) => serde_json::from_str::<GameState>(&json).unwrap_or_else(|e| {
                warn!("game state unreadable, starting fresh: {e}");
                GameState::default()
            }),
            None => GameState::default(),
        };
        let unlocked: HashMap<String, i64> = db
            .load_value(ACHIEVEMENTS_KEY)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let extra: Extra = db
            .load_value(METRICS_KEY)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let mut engine = Self {
            db,
            config: config::load()?,
            config_mtime: config::modified(),
            status_tx,
            known: HashMap::new(),
            attached: HashSet::new(),
            filter: KeyFilter::default(),
            session: Session::default(),
            rival_session: Session::default(),
            rival_db_id: None,
            main_db_id: None,
            day: Day::default(),
            best_ever: None,
            game: Game::new(game_state),
            book: Book::new(achievements::builtin(), unlocked),
            lifetime: Lifetime::default(),
            extra,
            last_burst: LastBurst::default(),
            last_toast_ms: now_ms(),
            picking: false,
            wpm_7d: None,
            rival_wpm_7d: None,
            pending_minutes: HashMap::new(),
            pending_keys: HashMap::new(),
            pending_positions: HashMap::new(),
            seen_stats: HashMap::new(),
            firmware: HashMap::new(),
            last_flush: now,
            last_publish: now,
            last_config_check: now,
            last_averages: now,
            last_battery_sample: None,
        };
        for (handle, info) in devices::list() {
            engine.attached.insert(handle);
            engine.register(handle, info)?;
        }
        engine.apply_config()?;
        Ok(engine)
    }

    pub fn run(mut self, rx: Receiver<Event>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => {
                    if let Err(e) = self.handle(event) {
                        warn!("event handling failed: {e:#}");
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if let Err(e) = self.tick() {
                warn!("periodic work failed: {e:#}");
            }
        }
        let _ = self.flush();
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Input(InputEvent::Key {
                device,
                vkey,
                pressed,
                e0,
                at,
            }) => self.on_key(device, vkey, pressed, e0, at),
            Event::Input(InputEvent::DeviceAdded(handle)) => {
                self.attached.insert(handle);
                if let Some(info) = devices::resolve(handle) {
                    info!(name = info.display_name(), transport = ?info.transport, "keyboard attached");
                    self.register(handle, info)?;
                }
                Ok(())
            }
            Event::Input(InputEvent::DeviceRemoved(handle)) => {
                self.attached.remove(&handle);
                self.filter.forget_device(handle);
                Ok(())
            }
            Event::Firmware(state) => {
                // The keyboard may report over USB and Bluetooth at once. Both carry the same
                // state; react only when the shown layer changes.
                let layer_before = self.fresh_firmware().map(|(s, _)| s.report.layer);
                self.firmware
                    .insert(state.transport, (state, Instant::now()));
                let layer_after = self.fresh_firmware().map(|(s, _)| s.report.layer);
                if layer_before != layer_after {
                    if let Some(layer) = layer_after
                        && self.main_db_id.is_some()
                    {
                        self.game.on_layer(layer, now_ms());
                        if layer != 0 {
                            self.extra.layer_switches += 1;
                            self.day.layers.insert(layer);
                        }
                    }
                    self.publish();
                }
                Ok(())
            }
            Event::FirmwareStats(message) => {
                self.on_firmware_stats(message);
                Ok(())
            }
            Event::Command(Command::SetMain { vid, pid, product }) => {
                self.set_main(DeviceMatcher { vid, pid, product })?;
                self.publish();
                Ok(())
            }
            Event::Command(Command::PickNext) => {
                self.picking = true;
                self.publish();
                Ok(())
            }
            Event::Command(Command::CancelPick) => {
                self.picking = false;
                self.publish();
                Ok(())
            }
            Event::Command(Command::Prestige) => {
                if self.game.prestige(now_ms()) {
                    info!("prestige");
                    self.publish();
                } else {
                    warn!("prestige requested below the maximum level");
                }
                Ok(())
            }
        }
    }

    fn on_firmware_stats(&mut self, message: Message) {
        let Some(main) = self.main_db_id else { return };
        let key = match &message {
            Message::KeyStats { seq, .. } => (2, *seq),
            Message::HoldTap { seq, .. } => (3, *seq),
            Message::State(_) => return,
        };
        self.seen_stats
            .retain(|_, at| at.elapsed() < Duration::from_secs(10));
        if self.seen_stats.insert(key, Instant::now()).is_some() {
            return; // same report via the other transport
        }
        let day = self.day.date.to_string();
        let mut add = |position: u8, slot: usize, n: u32| {
            let i = position as usize;
            if self.day.positions.len() <= i {
                self.day.positions.resize(i + 1, [0; 3]);
            }
            self.day.positions[i][slot] += n;
            self.pending_positions
                .entry((main, day.clone(), position))
                .or_default()[slot] += n;
        };
        match message {
            Message::KeyStats { presses, .. } => {
                for (position, n) in presses {
                    add(position, 0, n as u32);
                }
            }
            Message::HoldTap { outcomes, .. } => {
                for (position, taps, holds) in outcomes {
                    add(position, 1, taps as u32);
                    add(position, 2, holds as u32);
                    self.lifetime.taps += taps as u64;
                    self.lifetime.holds += holds as u64;
                }
            }
            Message::State(_) => {}
        }
    }

    fn register(&mut self, handle: DeviceHandle, info: DeviceInfo) -> Result<&Known> {
        let db_id = self
            .db
            .device_id(&info.key(), &info.display_name(), info.vid, info.pid)?;
        let is_main = self.config.main.as_ref().is_some_and(|m| m.matches(&info));
        if is_main && self.main_db_id.is_none() {
            self.main_db_id = Some(db_id);
        }
        Ok(self.known.entry(handle).or_insert(Known {
            info,
            db_id,
            is_main,
        }))
    }

    fn on_key(
        &mut self,
        device: DeviceHandle,
        vkey: u16,
        pressed: bool,
        e0: bool,
        at: Instant,
    ) -> Result<()> {
        let Some(counted) = self.filter.process(device, vkey, pressed, e0) else {
            return Ok(());
        };
        if !self.known.contains_key(&device) {
            let Some(info) = devices::resolve(device) else {
                return Ok(());
            };
            self.register(device, info)?;
        }
        if self.picking
            && let Some(matcher) = DeviceMatcher::from_info(&self.known[&device].info)
        {
            self.picking = false;
            info!(
                name = self.known[&device].info.display_name(),
                "main keyboard picked"
            );
            self.set_main(matcher)?;
            self.publish();
        }
        let known = &self.known[&device];
        let (db_id, is_main, identified) = (known.db_id, known.is_main, known.info.vid.is_some());

        let wall = SystemTime::now();
        let counts = self
            .pending_minutes
            .entry((db_id, unix_ms(wall) / 60_000))
            .or_default();
        counts.keys += 1;
        match counted {
            Counted::Char => counts.chars += 1,
            Counted::Backspace => counts.backspaces += 1,
            Counted::Other => {}
        }
        *self
            .pending_keys
            .entry((db_id, self.day.date.to_string(), vkey))
            .or_default() += 1;

        self.day.all_keys += 1;
        if is_main {
            self.main_db_id = Some(db_id);
            self.day.main.keys += 1;
            self.lifetime.keys += 1;
            *self.day.vkeys.entry(vkey).or_default() += 1;
            match counted {
                Counted::Char => {
                    let hour = Local::now().hour();
                    self.day.main.chars += 1;
                    self.day.hour_chars[hour as usize] += 1;
                    self.lifetime.chars += 1;
                    self.game.on_char(hour, unix_ms(wall));
                }
                Counted::Backspace => {
                    self.day.main.backspaces += 1;
                    self.lifetime.backspaces += 1;
                }
                Counted::Other => {}
            }
            if let Some(burst) = self.session.on_key(counted, at, wall) {
                self.finish_burst(burst)?;
            }
        } else if identified && self.main_db_id.is_some() {
            if self.rival_db_id != Some(db_id) {
                // Another keyboard took over: close the previous one's burst.
                if let Some(burst) = self.rival_session.tick(at + Duration::from_secs(3600)) {
                    self.finish_rival_burst(burst)?;
                }
                self.rival_session = Session::default();
                self.rival_db_id = Some(db_id);
            }
            if let Some(burst) = self.rival_session.on_key(counted, at, wall) {
                self.finish_rival_burst(burst)?;
            }
        }
        Ok(())
    }

    fn finish_burst(&mut self, burst: FinishedBurst) -> Result<()> {
        let Some(main) = self.main_db_id else {
            return Ok(());
        };
        if burst.chars < MIN_BURST_CHARS {
            return Ok(());
        }
        let duration_ms = burst.duration.as_millis() as i64;
        self.db.add_burst(
            main,
            unix_ms(burst.start_wall),
            duration_ms,
            burst.chars,
            burst.backspaces,
        )?;
        self.day.active_ms += duration_ms;
        let record_wpm = burst.counts_for_records().then(|| burst.wpm()).flatten();
        let at = now_ms();
        self.last_burst = LastBurst {
            wpm: record_wpm.unwrap_or(0.0),
            chars: burst.chars,
            accuracy: if burst.chars >= 200 {
                1.0 - burst.backspaces as f32 / burst.chars as f32
            } else {
                0.0
            },
            minutes: burst.duration.as_secs_f32() / 60.0,
        };
        if burst.chars >= 100 && burst.backspaces == 0 {
            self.extra.perfect_bursts += 1;
        }
        self.game
            .on_burst(burst.chars, burst.backspaces, record_wpm, at);
        if let Some(wpm) = record_wpm {
            if self.day.best_wpm.is_none_or(|best| wpm > best) {
                self.day.best_wpm = Some(wpm);
            }
            if self.best_ever.is_none_or(|best| wpm > best) {
                info!(wpm, "new personal best");
                // The very first record is not news.
                if self.best_ever.is_some() {
                    self.game.on_new_best(wpm, at);
                    self.extra.new_bests += 1;
                }
                self.best_ever = Some(wpm);
            }
            self.refresh_averages()?;
        }
        Ok(())
    }

    fn finish_rival_burst(&mut self, burst: FinishedBurst) -> Result<()> {
        let Some(device) = self.rival_db_id else {
            return Ok(());
        };
        if burst.chars < MIN_BURST_CHARS {
            return Ok(());
        }
        let duration_ms = burst.duration.as_millis() as i64;
        self.db.add_burst(
            device,
            unix_ms(burst.start_wall),
            duration_ms,
            burst.chars,
            burst.backspaces,
        )?;
        if burst.counts_for_records() {
            self.refresh_averages()?;
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if let Some(burst) = self.session.tick(now) {
            self.finish_burst(burst)?;
        }
        if let Some(burst) = self.rival_session.tick(now) {
            self.finish_rival_burst(burst)?;
        }
        if current_day() != self.day.date {
            info!("new day");
            self.reload_day()?;
        }
        if now - self.last_config_check >= CONFIG_CHECK_INTERVAL {
            self.last_config_check = now;
            let mtime = config::modified();
            if mtime != self.config_mtime {
                self.config_mtime = mtime;
                match config::load() {
                    Ok(config) => {
                        self.config = config;
                        info!("configuration reloaded");
                        self.apply_config()?;
                    }
                    Err(e) => warn!("keeping old configuration: {e:#}"),
                }
            }
        }
        if now - self.last_averages >= AVERAGES_INTERVAL {
            self.refresh_averages()?;
        }
        if now - self.last_flush >= FLUSH_INTERVAL {
            self.flush()?;
        }
        if let Some((state, _)) = self.fresh_firmware()
            && self
                .last_battery_sample
                .is_none_or(|t| now - t >= BATTERY_SAMPLE_INTERVAL)
        {
            let peripheral = state.report.battery_peripherals.first().copied().flatten();
            self.db
                .add_battery(now_ms(), state.report.battery_central, peripheral)?;
            self.last_battery_sample = Some(now);
        }
        if now - self.last_publish >= PUBLISH_INTERVAL {
            self.publish();
        }
        Ok(())
    }

    /// Saves a new main keyboard to the config file and applies it.
    fn set_main(&mut self, matcher: DeviceMatcher) -> Result<()> {
        self.config.main = Some(matcher);
        config::save(&self.config)?;
        self.config_mtime = config::modified();
        self.apply_config()
    }

    fn keyboards(&self) -> Vec<Keyboard> {
        let mut seen = HashSet::new();
        let mut list: Vec<Keyboard> = self
            .attached
            .iter()
            .filter_map(|h| self.known.get(h))
            .filter(|k| seen.insert((k.info.key(), k.info.transport)))
            .map(|k| Keyboard {
                name: k.info.display_name(),
                vid: k.info.vid,
                pid: k.info.pid,
                transport: k.info.transport,
                main: k.is_main,
            })
            .collect();
        list.sort_by(|a, b| b.main.cmp(&a.main).then(a.name.cmp(&b.name)));
        list
    }

    fn apply_config(&mut self) -> Result<()> {
        self.main_db_id = None;
        for known in self.known.values_mut() {
            known.is_main = self
                .config
                .main
                .as_ref()
                .is_some_and(|m| m.matches(&known.info));
            if known.is_main && self.main_db_id.is_none() {
                self.main_db_id = Some(known.db_id);
            }
        }
        if self.main_db_id.is_none()
            && let Some(m) = &self.config.main
        {
            self.main_db_id = self.db.find_device(m.vid, m.pid, m.product.as_deref())?;
        }
        self.session = Session::default();
        self.rival_session = Session::default();
        self.reload_day()
    }

    fn reload_day(&mut self) -> Result<()> {
        self.flush()?;
        let date = current_day();
        let (start_ms, _) = day_bounds(date);
        let min_ms = RECORD_MIN_DURATION.as_millis() as i64;
        let totals = self.db.day_totals(self.main_db_id, start_ms, min_ms)?;
        let mut positions = Vec::new();
        let mut hour_chars = [0u32; 24];
        let mut vkeys = HashMap::new();
        self.lifetime = Lifetime::default();
        if let Some(main) = self.main_db_id {
            let (chars, keys, backspaces, taps, holds) = self.db.lifetime(main)?;
            self.lifetime = Lifetime {
                chars,
                keys,
                backspaces,
                taps,
                holds,
            };
            for (minute, n) in self.db.minute_chars(main, start_ms / 60_000)? {
                if let Some(t) = Local.timestamp_opt(minute * 60, 0).single() {
                    hour_chars[t.hour() as usize] += n;
                }
            }
            vkeys = self
                .db
                .vkeys(main, &date.to_string())?
                .into_iter()
                .collect();
            for (position, counts) in self.db.positions(main, &date.to_string())? {
                let i = position as usize;
                if positions.len() <= i {
                    positions.resize(i + 1, [0; 3]);
                }
                positions[i] = counts;
            }
        }
        self.day = Day {
            date,
            main: totals.main,
            all_keys: totals.all_keys,
            active_ms: totals.active_ms,
            best_wpm: totals.best_wpm,
            positions,
            hour_chars,
            layers: BTreeSet::new(),
            vkeys,
        };
        self.best_ever = match self.main_db_id {
            Some(id) => self.db.best_wpm(id, None, min_ms)?,
            None => None,
        };
        self.refresh_averages()?;
        self.start_game_day()
    }

    /// Settles past days for streak and loyalty and prepares today's quests.
    fn start_game_day(&mut self) -> Result<()> {
        let Some(main) = self.main_db_id else {
            return Ok(());
        };
        let today = self.day.date;

        // Personal baseline for sizing quests: last 7 days with typing.
        let mut active_days = 0u32;
        let mut words = 0u32;
        let mut day = today;
        for _ in 0..7 {
            day = day.pred_opt().unwrap_or(day);
            let (from, to) = day_bounds(day);
            let (_, _, chars) = self.db.range_keys(Some(main), from, to)?;
            if chars > 0 {
                active_days += 1;
                words += chars / 5;
            }
        }
        let mut baseline = Baseline::default();
        if active_days > 0 {
            baseline.words_per_day = words as f32 / active_days as f32;
        }
        if let Some(wpm) = self.wpm_7d {
            baseline.wpm = wpm;
        }

        let db = &self.db;
        let history = |d: NaiveDate| {
            let (from, to) = day_bounds(d);
            let (main_keys, all_keys, _) = db.range_keys(Some(main), from, to).unwrap_or_default();
            DaySummary {
                main_keys,
                all_keys,
            }
        };
        self.game.start_day(
            today,
            history,
            baseline,
            self.config.streak_threshold(),
            now_ms(),
        );
        self.save_game()
    }

    fn refresh_averages(&mut self) -> Result<()> {
        self.last_averages = Instant::now();
        let Some(main) = self.main_db_id else {
            return Ok(());
        };
        let since = now_ms() - WEEK_MS;
        let min_ms = RECORD_MIN_DURATION.as_millis() as i64;
        self.wpm_7d = self.db.average_wpm(main, false, since, min_ms)?;
        self.rival_wpm_7d = self.db.average_wpm(main, true, since, min_ms)?;
        Ok(())
    }

    fn save_game(&mut self) -> Result<()> {
        if self.game.dirty {
            self.db
                .save_value(GAME_STATE_KEY, &serde_json::to_string(&self.game.state)?)?;
            self.game.dirty = false;
        }
        if self.book.dirty {
            let json = serde_json::to_string(self.book.unlocked_ids())?;
            self.db.save_value(ACHIEVEMENTS_KEY, &json)?;
            self.book.dirty = false;
        }
        self.db
            .save_value(METRICS_KEY, &serde_json::to_string(&self.extra)?)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.last_flush = Instant::now();
        self.save_game()?;
        if self.pending_minutes.is_empty()
            && self.pending_keys.is_empty()
            && self.pending_positions.is_empty()
        {
            return Ok(());
        }
        if !self.pending_positions.is_empty() {
            let positions: Vec<_> = self.pending_positions.drain().collect();
            self.db.add_positions(&positions)?;
        }
        let minutes: Vec<_> = self.pending_minutes.drain().collect();
        let keys: Vec<_> = self.pending_keys.drain().collect();
        self.db.flush(&minutes, &keys)
    }

    /// Latest firmware state, preferring USB over Bluetooth when both are current.
    fn fresh_firmware(&self) -> Option<&(FirmwareState, Instant)> {
        [Transport::Usb, Transport::Bluetooth, Transport::Other]
            .iter()
            .filter_map(|t| self.firmware.get(t))
            .find(|(_, at)| at.elapsed() < FIRMWARE_STALE)
    }

    fn main_name(&self) -> Option<String> {
        let main = self.config.main.as_ref()?;
        let attached = self
            .known
            .values()
            .find(|k| k.is_main)
            .map(|k| k.info.display_name());
        Some(
            attached
                .or_else(|| main.product.clone())
                .unwrap_or_else(|| format!("{:04X}:{:04X}", main.vid, main.pid)),
        )
    }

    /// All values achievements can be checked against, at this moment.
    fn snapshot(&self, now: Instant, active_seconds: u32) -> Snapshot {
        let day = &self.day;
        let c = self.game.counters(day.date);
        let local = Local::now();
        let chars = day.main.chars as f64;
        let firmware = self.fresh_firmware().map(|(s, _)| s);
        let batteries: Vec<f64> = firmware
            .map(|f| {
                std::iter::once(f.report.battery_central)
                    .chain(f.report.battery_peripherals.iter().copied())
                    .flatten()
                    .map(f64::from)
                    .collect()
            })
            .unwrap_or_default();
        let on_bluetooth = firmware.is_some_and(|f| f.transport == Transport::Bluetooth);
        let mut v: HashMap<&'static str, f64> = HashMap::from([
            ("lifetime_words", self.lifetime.chars as f64 / 5.0),
            ("lifetime_keys", self.lifetime.keys as f64),
            ("lifetime_backspaces", self.lifetime.backspaces as f64),
            ("perfect_bursts", self.extra.perfect_bursts as f64),
            ("layer_switches", self.extra.layer_switches as f64),
            ("hrm_holds", self.lifetime.holds as f64),
            ("hrm_taps", self.lifetime.taps as f64),
            ("today_words", chars / 5.0),
            ("today_keys", day.main.keys as f64),
            ("today_backspaces", day.main.backspaces as f64),
            ("today_active_minutes", active_seconds as f64 / 60.0),
            // Accuracy and share only count on real typing days, not after three keystrokes.
            (
                "today_accuracy",
                if chars >= 2500.0 {
                    (1.0 - day.main.backspaces as f64 / chars).max(0.0)
                } else {
                    0.0
                },
            ),
            (
                "today_share",
                if day.main.keys >= 1000 && day.all_keys > 0 {
                    day.main.keys as f64 / day.all_keys as f64
                } else {
                    0.0
                },
            ),
            ("today_layers", day.layers.len() as f64),
            (
                "today_hrm_holds",
                day.positions.iter().map(|p| p[2] as f64).sum(),
            ),
            (
                "today_max_key",
                day.positions.iter().map(|p| p[0]).max().unwrap_or(0) as f64,
            ),
            (
                "today_distinct_keys",
                day.positions.iter().filter(|p| p[0] > 0).count() as f64,
            ),
            ("burst_wpm", self.last_burst.wpm as f64),
            ("burst_chars", self.last_burst.chars as f64),
            ("burst_accuracy", self.last_burst.accuracy as f64),
            ("burst_minutes", self.last_burst.minutes as f64),
            ("best_wpm", self.best_ever.unwrap_or(0.0) as f64),
            ("new_bests", self.extra.new_bests as f64),
            ("wpm_live", self.session.wpm_live(now) as f64),
            ("level", c.level as f64),
            ("prestige", c.prestige as f64),
            ("loyalty", c.loyalty as f64),
            ("streak_days", c.streak as f64),
            ("max_streak", c.max_streak as f64),
            ("days_active", c.days_active as f64),
            ("days_since_start", c.days_since_start as f64),
            ("idle_return_days", c.idle_return_days as f64),
            ("quests_completed", c.quests_completed as f64),
            ("weekly_completed", c.weekly_completed as f64),
            ("perfect_quest_days", c.perfect_quest_days as f64),
            ("season_xp", c.season_xp as f64),
            ("seasons_played", c.seasons_played as f64),
            ("achievements_unlocked", self.book.unlocked_count() as f64),
            ("on_bluetooth", if on_bluetooth { 1.0 } else { 0.0 }),
            ("month", local.month() as f64),
            ("day_of_month", local.day() as f64),
            ("weekday", local.weekday().number_from_monday() as f64),
            ("hour", local.hour() as f64),
        ]);
        if let (Some(me), Some(rival)) = (self.wpm_7d, self.rival_wpm_7d) {
            v.insert("rival_gap", (me - rival) as f64);
        }
        if !batteries.is_empty() {
            v.insert(
                "battery_min",
                batteries.iter().copied().fold(f64::MAX, f64::min),
            );
            v.insert("battery_max", batteries.iter().copied().fold(0.0, f64::max));
        }
        if let Some(f) = firmware {
            v.insert("keyboard_uptime_hours", f.report.uptime_s as f64 / 3600.0);
        }
        Snapshot {
            scalars: v,
            hour_chars: day.hour_chars,
            vkeys: day.vkeys.clone(),
        }
    }

    fn check_achievements(&mut self, now: Instant, active_seconds: u32) {
        let snapshot = self.snapshot(now, active_seconds);
        let at = now_ms();
        for def in self.book.check(&snapshot, at) {
            info!(rarity = ?def.rarity, "achievement unlocked");
            let xp = achievements::xp_for(def.rarity);
            self.game.award(
                xp,
                GameEventKind::AchievementUnlocked {
                    name: def.name,
                    rarity: def.rarity,
                    xp,
                },
                at,
            );
        }
    }

    /// Shows Windows notifications for new game events, as configured.
    fn notify_events(&mut self) {
        let Some(newest) = self.game.newest_event_ms() else {
            return;
        };
        if newest <= self.last_toast_ms {
            return;
        }
        let since = self.last_toast_ms;
        self.last_toast_ms = newest;
        let n = &self.config.notifications;
        if !n.enabled {
            return;
        }
        for event in self.game.events_since(since) {
            let toast = match &event.kind {
                GameEventKind::AchievementUnlocked { name, rarity, xp } if n.achievements => {
                    Some((
                        format!("Achievement unlocked: {name}"),
                        format!("{rarity:?} · +{xp} XP"),
                    ))
                }
                GameEventKind::TitleUnlocked { title } if n.titles => {
                    Some(("New title".into(), title.clone()))
                }
                GameEventKind::Prestige { prestige } if n.titles => {
                    Some(("Prestige!".into(), format!("Prestige {prestige} reached")))
                }
                GameEventKind::NewBestWpm { wpm } if n.records => {
                    Some(("New personal best".into(), format!("{wpm:.0} WPM")))
                }
                GameEventKind::WeeklyCompleted { title, xp } if n.weekly => Some((
                    "Weekly challenge done".into(),
                    format!("{title} · +{xp} XP"),
                )),
                GameEventKind::StreakLost { days } if n.streak => Some((
                    "Streak lost".into(),
                    format!("Your {days}-day streak ended"),
                )),
                GameEventKind::StreakFrozen { days } if n.streak => Some((
                    "Streak saved".into(),
                    format!("A freeze kept your {days}-day streak"),
                )),
                GameEventKind::QuestCompleted { title, xp, .. } if n.quests => {
                    Some(("Quest done".into(), format!("{title} · +{xp} XP")))
                }
                _ => None,
            };
            if let Some((title, body)) = toast {
                notify::show(title, body);
            }
        }
    }

    fn publish(&mut self) {
        let now = Instant::now();
        self.last_publish = now;
        let chars = self.day.main.chars;
        let live_burst_ms = self.session.active().as_millis() as i64;
        let active_seconds = ((self.day.active_ms + live_burst_ms) / 1000) as u32;
        let threshold = self.config.streak_threshold();

        let game = self.main_db_id.map(|_| {
            let view = DayView {
                main_chars: chars,
                main_keys: self.day.main.keys,
                all_keys: self.day.all_keys,
                active_seconds,
            };
            self.game.update_day(&view, threshold, now_ms());
            self.check_achievements(now, active_seconds);
            self.notify_events();
            self.game
                .view(&view, threshold, self.wpm_7d, self.rival_wpm_7d)
        });

        let day = &self.day;
        let status = Status {
            ipc_version: IPC_VERSION,
            main_keyboard: self.main_name(),
            main_connected: self
                .attached
                .iter()
                .any(|h| self.known.get(h).is_some_and(|k| k.is_main)),
            typing: Typing {
                wpm_live: self.session.wpm_live(now),
                wpm_burst: self.session.wpm_burst(),
                chars_today: chars,
                words_today: chars / 5,
                backspaces_today: day.main.backspaces,
                accuracy_today: (chars > 0)
                    .then(|| (1.0 - day.main.backspaces as f32 / chars as f32).clamp(0.0, 1.0)),
                active_seconds_today: active_seconds,
                best_wpm_today: day.best_wpm,
                best_wpm_ever: self.best_ever,
            },
            main_share_today: (day.all_keys > 0)
                .then(|| day.main.keys as f32 / day.all_keys as f32),
            firmware: self.fresh_firmware().map(|(s, _)| s.clone()),
            game,
            key_stats: (!day.positions.is_empty()).then(|| KeyStats {
                presses: day.positions.iter().map(|p| p[0]).collect(),
                taps: day.positions.iter().map(|p| p[1]).collect(),
                holds: day.positions.iter().map(|p| p[2]).collect(),
            }),
            achievements: self.main_db_id.map(|_| self.book.view()),
            keyboards: self.keyboards(),
            picking: self.picking,
        };
        self.status_tx.send_if_modified(|current| {
            if *current == status {
                false
            } else {
                *current = status;
                true
            }
        });
    }
}
