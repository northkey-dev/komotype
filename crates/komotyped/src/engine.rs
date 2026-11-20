//! Combines key events and firmware reports into statistics, persists them and publishes
//! [`Status`] snapshots for IPC clients.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::{Local, NaiveDate, TimeZone};
use komotype_protocol::ipc::{FirmwareState, IPC_VERSION, Status, Transport, Typing};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::{self, Config};
use crate::db::{Db, MinuteCounts};
use crate::devices::{self, DeviceInfo};
use crate::input::{DeviceHandle, InputEvent};
use crate::stats::{Counted, FinishedBurst, KeyFilter, RECORD_MIN_DURATION, Session};

/// The typing day starts at 04:00 local time, so late-night sessions count for the evening.
const DAY_START_HOUR: u32 = 4;
const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);
const CONFIG_CHECK_INTERVAL: Duration = Duration::from_secs(3);
const BATTERY_SAMPLE_INTERVAL: Duration = Duration::from_secs(600);
/// Firmware state older than this is treated as gone (heartbeat is 5 s).
const FIRMWARE_STALE: Duration = Duration::from_secs(15);
/// Bursts with fewer characters are noise (e.g. a single shortcut) and are not stored.
const MIN_BURST_CHARS: u32 = 5;

pub enum Event {
    Input(InputEvent),
    Firmware(FirmwareState),
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
    main_db_id: Option<i64>,
    day: Day,
    best_ever: Option<f32>,
    pending_minutes: HashMap<(i64, i64), MinuteCounts>,
    pending_keys: HashMap<(i64, String, u16), u32>,
    firmware: HashMap<Transport, (FirmwareState, Instant)>,
    last_flush: Instant,
    last_publish: Instant,
    last_config_check: Instant,
    last_battery_sample: Option<Instant>,
}

fn unix_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn current_day() -> (NaiveDate, i64) {
    let now = Local::now();
    let date = (now - chrono::Duration::hours(DAY_START_HOUR as i64)).date_naive();
    let start = date.and_hms_opt(DAY_START_HOUR, 0, 0).expect("valid time");
    let start_ms = Local
        .from_local_datetime(&start)
        .earliest()
        .map_or_else(|| now.timestamp_millis(), |d| d.timestamp_millis());
    (date, start_ms)
}

impl Engine {
    pub fn new(db: Db, status_tx: watch::Sender<Status>) -> Result<Self> {
        let now = Instant::now();
        let mut engine = Self {
            db,
            config: config::load()?,
            config_mtime: config::modified(),
            status_tx,
            known: HashMap::new(),
            attached: HashSet::new(),
            filter: KeyFilter::default(),
            session: Session::default(),
            main_db_id: None,
            day: Day::default(),
            best_ever: None,
            pending_minutes: HashMap::new(),
            pending_keys: HashMap::new(),
            firmware: HashMap::new(),
            last_flush: now,
            last_publish: now,
            last_config_check: now,
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
                // state; publish right away only when the shown layer changes.
                let layer_before = self.fresh_firmware().map(|(s, _)| s.report.layer);
                self.firmware.insert(state.transport, (state, Instant::now()));
                let layer_after = self.fresh_firmware().map(|(s, _)| s.report.layer);
                if layer_before != layer_after {
                    self.publish();
                }
                Ok(())
            }
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
        let known = &self.known[&device];
        let (db_id, is_main) = (known.db_id, known.is_main);

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
            match counted {
                Counted::Char => self.day.main.chars += 1,
                Counted::Backspace => self.day.main.backspaces += 1,
                Counted::Other => {}
            }
            if let Some(burst) = self.session.on_key(counted, at, wall) {
                self.finish_burst(burst)?;
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
        if burst.counts_for_records()
            && let Some(wpm) = burst.wpm()
        {
            if self.day.best_wpm.is_none_or(|best| wpm > best) {
                self.day.best_wpm = Some(wpm);
            }
            if self.best_ever.is_none_or(|best| wpm > best) {
                info!(wpm, "new personal best");
                self.best_ever = Some(wpm);
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if let Some(burst) = self.session.tick(now) {
            self.finish_burst(burst)?;
        }
        if current_day().0 != self.day.date {
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
        if now - self.last_flush >= FLUSH_INTERVAL {
            self.flush()?;
        }
        if let Some((state, _)) = self.fresh_firmware()
            && self
                .last_battery_sample
                .is_none_or(|t| now - t >= BATTERY_SAMPLE_INTERVAL)
        {
            let peripheral = state.report.battery_peripherals.first().copied().flatten();
            self.db.add_battery(
                unix_ms(SystemTime::now()),
                state.report.battery_central,
                peripheral,
            )?;
            self.last_battery_sample = Some(now);
        }
        if now - self.last_publish >= PUBLISH_INTERVAL {
            self.publish();
        }
        Ok(())
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
        self.reload_day()
    }

    fn reload_day(&mut self) -> Result<()> {
        self.flush()?;
        let (date, start_ms) = current_day();
        let min_ms = RECORD_MIN_DURATION.as_millis() as i64;
        let totals = self.db.day_totals(self.main_db_id, start_ms, min_ms)?;
        self.day = Day {
            date,

            main: totals.main,
            all_keys: totals.all_keys,
            active_ms: totals.active_ms,
            best_wpm: totals.best_wpm,
        };
        self.best_ever = match self.main_db_id {
            Some(id) => self.db.best_wpm(id, None, min_ms)?,
            None => None,
        };
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.last_flush = Instant::now();
        if self.pending_minutes.is_empty() && self.pending_keys.is_empty() {
            return Ok(());
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

    fn publish(&mut self) {
        let now = Instant::now();
        self.last_publish = now;
        let day = &self.day;
        let chars = day.main.chars;
        let live_burst_ms = self.session.active().as_millis() as i64;
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
                active_seconds_today: ((day.active_ms + live_burst_ms) / 1000) as u32,
                best_wpm_today: day.best_wpm,
                best_wpm_ever: self.best_ever,
            },
            main_share_today: (day.all_keys > 0)
                .then(|| day.main.keys as f32 / day.all_keys as f32),
            firmware: self.fresh_firmware().map(|(s, _)| s.clone()),
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
