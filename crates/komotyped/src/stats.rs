//! Typing measurement: which keys count, WPM, typing bursts. Pure logic, no I/O.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use crate::input::DeviceHandle;

/// A burst ends after this much idle time.
pub const BURST_GAP: Duration = Duration::from_secs(5);
/// Bursts shorter than this do not count for best WPM.
pub const RECORD_MIN_DURATION: Duration = Duration::from_secs(30);
const LIVE_WINDOW: Duration = Duration::from_secs(10);
const LIVE_MIN_WINDOW: Duration = Duration::from_secs(3);
const CHARS_PER_WORD: f32 = 5.0;

// Windows virtual key codes.
const VK_BACK: u16 = 0x08;
const VK_RETURN: u16 = 0x0D;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_SPACE: u16 = 0x20;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counted {
    /// Produces a character: letters, digits, punctuation, space, enter.
    Char,
    Backspace,
    /// Any other key press: navigation, function keys, shortcuts, modifiers.
    Other,
}

fn is_char_key(vkey: u16) -> bool {
    matches!(vkey,
        VK_SPACE | VK_RETURN
        | 0x30..=0x39 // digits
        | 0x41..=0x5A // letters
        | 0x60..=0x6F // numpad
        | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2 // OEM punctuation, incl. German ü ö ä ß ^ < etc.
    )
}

fn is_modifier(vkey: u16) -> bool {
    matches!(
        vkey,
        VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | 0xA0..=0xA5
    )
}

#[derive(Default, Clone, Copy)]
struct Mods {
    ctrl: bool,
    left_alt: bool,
    /// Right Alt = AltGr on German layouts. Produces characters, not shortcuts.
    altgr: bool,
    win: bool,
}

/// Turns raw key events into counted key presses: drops releases and auto-repeat, and treats
/// Ctrl/Alt/Win combinations as shortcuts instead of characters.
#[derive(Default)]
pub struct KeyFilter {
    down: HashSet<(DeviceHandle, u16, bool)>,
    mods: HashMap<DeviceHandle, Mods>,
}

impl KeyFilter {
    pub fn process(
        &mut self,
        device: DeviceHandle,
        vkey: u16,
        pressed: bool,
        e0: bool,
    ) -> Option<Counted> {
        let mods = self.mods.entry(device).or_default();
        match vkey {
            VK_CONTROL => mods.ctrl = pressed,
            VK_MENU if e0 => mods.altgr = pressed,
            VK_MENU => mods.left_alt = pressed,
            VK_LWIN | VK_RWIN => mods.win = pressed,
            _ => {}
        }
        let mods = *mods;

        let key = (device, vkey, e0);
        if !pressed {
            self.down.remove(&key);
            return None;
        }
        if !self.down.insert(key) {
            return None; // auto-repeat
        }

        if vkey == VK_BACK {
            return Some(Counted::Backspace);
        }
        if is_modifier(vkey) || !is_char_key(vkey) {
            return Some(Counted::Other);
        }
        // AltGr is reported as Ctrl + right Alt on some layouts; with AltGr down it is typing.
        let shortcut = (mods.ctrl && !mods.altgr) || mods.left_alt || mods.win;
        Some(if shortcut {
            Counted::Other
        } else {
            Counted::Char
        })
    }

    pub fn forget_device(&mut self, device: DeviceHandle) {
        self.down.retain(|(d, _, _)| *d != device);
        self.mods.remove(&device);
    }
}

#[derive(Debug, Clone)]
pub struct FinishedBurst {
    pub start_wall: SystemTime,
    pub duration: Duration,
    pub chars: u32,
    pub backspaces: u32,
}

impl FinishedBurst {
    pub fn wpm(&self) -> Option<f32> {
        wpm(self.chars, self.duration)
    }

    pub fn counts_for_records(&self) -> bool {
        self.duration >= RECORD_MIN_DURATION
    }
}

fn wpm(chars: u32, duration: Duration) -> Option<f32> {
    let minutes = duration.as_secs_f32() / 60.0;
    (duration >= Duration::from_secs(1)).then(|| chars as f32 / CHARS_PER_WORD / minutes)
}

struct Burst {
    start: Instant,
    start_wall: SystemTime,
    last: Instant,
    chars: u32,
    backspaces: u32,
}

impl Burst {
    fn finish(self) -> FinishedBurst {
        FinishedBurst {
            start_wall: self.start_wall,
            duration: self.last - self.start,
            chars: self.chars,
            backspaces: self.backspaces,
        }
    }
}

/// Live WPM and bursts of continuous typing on the main keyboard.
#[derive(Default)]
pub struct Session {
    recent: VecDeque<Instant>,
    burst: Option<Burst>,
}

impl Session {
    /// Records a char or backspace. Returns the previous burst if this key started a new one.
    pub fn on_key(
        &mut self,
        counted: Counted,
        at: Instant,
        wall: SystemTime,
    ) -> Option<FinishedBurst> {
        if counted == Counted::Other {
            return None;
        }
        let finished = match &self.burst {
            Some(b) if at.saturating_duration_since(b.last) > BURST_GAP => {
                self.burst.take().map(Burst::finish)
            }
            _ => None,
        };
        let burst = self.burst.get_or_insert(Burst {
            start: at,
            start_wall: wall,
            last: at,
            chars: 0,
            backspaces: 0,
        });
        burst.last = at;
        match counted {
            Counted::Char => {
                burst.chars += 1;
                self.recent.push_back(at);
            }
            Counted::Backspace => burst.backspaces += 1,
            Counted::Other => unreachable!(),
        }
        while self
            .recent
            .front()
            .is_some_and(|&t| at.saturating_duration_since(t) > LIVE_WINDOW)
        {
            self.recent.pop_front();
        }
        finished
    }

    /// Closes the burst once the user stopped typing.
    pub fn tick(&mut self, now: Instant) -> Option<FinishedBurst> {
        match &self.burst {
            Some(b) if now.saturating_duration_since(b.last) > BURST_GAP => {
                self.burst.take().map(Burst::finish)
            }
            _ => None,
        }
    }

    pub fn wpm_live(&self, now: Instant) -> f32 {
        let Some(burst) = &self.burst else { return 0.0 };
        if now.saturating_duration_since(burst.last) > BURST_GAP {
            return 0.0;
        }
        let window = now
            .saturating_duration_since(burst.start)
            .clamp(LIVE_MIN_WINDOW, LIVE_WINDOW);
        let chars = self
            .recent
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) <= window)
            .count();
        wpm(chars as u32, window).unwrap_or(0.0)
    }

    pub fn wpm_burst(&self) -> Option<f32> {
        let b = self.burst.as_ref()?;
        let duration = b.last - b.start;
        (duration >= Duration::from_secs(5))
            .then(|| wpm(b.chars, duration))
            .flatten()
    }

    /// Duration of the burst in progress.
    pub fn active(&self) -> Duration {
        self.burst
            .as_ref()
            .map_or(Duration::ZERO, |b| b.last - b.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KBD: DeviceHandle = 1;

    fn press(f: &mut KeyFilter, vkey: u16) -> Option<Counted> {
        let r = f.process(KBD, vkey, true, false);
        f.process(KBD, vkey, false, false);
        r
    }

    #[test]
    fn letters_count_as_chars_and_shortcuts_do_not() {
        let mut f = KeyFilter::default();
        assert_eq!(press(&mut f, 0x41), Some(Counted::Char)); // a
        assert_eq!(press(&mut f, VK_BACK), Some(Counted::Backspace));
        f.process(KBD, VK_CONTROL, true, false);
        assert_eq!(press(&mut f, 0x43), Some(Counted::Other)); // Ctrl+C
        f.process(KBD, VK_CONTROL, false, false);
        assert_eq!(press(&mut f, 0x70), Some(Counted::Other)); // F1
    }

    #[test]
    fn altgr_produces_chars() {
        let mut f = KeyFilter::default();
        f.process(KBD, VK_CONTROL, true, false);
        f.process(KBD, VK_MENU, true, true);
        assert_eq!(press(&mut f, 0x51), Some(Counted::Char)); // AltGr+Q = @
    }

    #[test]
    fn auto_repeat_is_ignored() {
        let mut f = KeyFilter::default();
        assert_eq!(f.process(KBD, 0x41, true, false), Some(Counted::Char));
        assert_eq!(f.process(KBD, 0x41, true, false), None);
        assert_eq!(f.process(KBD, 0x41, false, false), None);
    }

    #[test]
    fn burst_wpm_and_gap() {
        let t0 = Instant::now();
        let wall = SystemTime::now();
        let mut s = Session::default();
        // 300 chars in 60 s = 60 WPM
        for i in 0..=300u64 {
            assert!(
                s.on_key(Counted::Char, t0 + Duration::from_millis(i * 200), wall)
                    .is_none()
            );
        }
        let end = t0 + Duration::from_secs(60);
        let live = s.wpm_live(end);
        assert!((live - 60.0).abs() < 2.0, "live {live}");
        assert!(s.tick(end + Duration::from_secs(2)).is_none());
        let burst = s.tick(end + Duration::from_secs(6)).expect("burst closed");
        assert_eq!(burst.chars, 301);
        assert!(burst.counts_for_records());
        assert!((burst.wpm().unwrap() - 60.2).abs() < 0.5);
        assert_eq!(s.wpm_live(end + Duration::from_secs(6)), 0.0);
    }
}
