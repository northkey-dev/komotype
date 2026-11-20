//! Raw HID protocol between the komotype ZMK module and the host. See `docs/protocol.md`.

use serde::{Deserialize, Serialize};

pub const USAGE_PAGE: u16 = 0xFF60;
pub const USAGE: u16 = 0x61;
pub const REPORT_SIZE: usize = 32;

pub const MAGIC: u8 = 0x4B;
pub const VERSION: u8 = 1;

pub const MSG_STATE: u8 = 0x01;
pub const MSG_KEYSTATS: u8 = 0x02;
pub const MSG_HOLDTAP: u8 = 0x03;
pub const MSG_SYNC: u8 = 0x80;

const BATTERY_UNKNOWN: u8 = 0xFF;
const MAX_PERIPHERALS: usize = 4;

/// Keyboard state as reported by the firmware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardReport {
    pub seq: u8,
    /// Highest active layer index (0 = base).
    pub layer: u8,
    /// Bitmask of active layers. The base layer is never set; treat it as always active.
    pub layers_active: u32,
    pub battery_central: Option<u8>,
    /// One entry per split peripheral. `None` = unknown.
    pub battery_peripherals: Vec<Option<u8>>,
    pub uptime_s: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    NotKomotype,
    UnsupportedVersion(u8),
    UnknownMessage(u8),
}

fn battery(value: u8) -> Option<u8> {
    (value != BATTERY_UNKNOWN).then_some(value)
}

/// A decoded keyboard → host message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    State(KeyboardReport),
    /// Key presses per position since the previous report.
    KeyStats {
        seq: u8,
        presses: Vec<(u8, u8)>,
    },
    /// Hold-tap outcomes per position since the previous report: (position, taps, holds).
    HoldTap {
        seq: u8,
        outcomes: Vec<(u8, u8, u8)>,
    },
}

/// Decodes a report as read from the device (without report ID).
pub fn decode(data: &[u8]) -> Result<Message, DecodeError> {
    if data.len() < 5 {
        return Err(DecodeError::TooShort);
    }
    if data[0] != MAGIC {
        return Err(DecodeError::NotKomotype);
    }
    if data[1] != VERSION {
        return Err(DecodeError::UnsupportedVersion(data[1]));
    }
    let seq = data[3];
    match data[2] {
        MSG_STATE => decode_state(data).map(Message::State),
        MSG_KEYSTATS => {
            let n = (data[4] as usize).min((data.len() - 5) / 2);
            let presses = data[5..5 + 2 * n]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| (c[0], c[1]))
                .collect();
            Ok(Message::KeyStats { seq, presses })
        }
        MSG_HOLDTAP => {
            let n = (data[4] as usize).min((data.len() - 5) / 3);
            let outcomes = data[5..5 + 3 * n]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| (c[0], c[1], c[2]))
                .collect();
            Ok(Message::HoldTap { seq, outcomes })
        }
        other => Err(DecodeError::UnknownMessage(other)),
    }
}

fn decode_state(data: &[u8]) -> Result<KeyboardReport, DecodeError> {
    if data.len() < 19 {
        return Err(DecodeError::TooShort);
    }
    let count = (data[10] as usize).min(MAX_PERIPHERALS);
    Ok(KeyboardReport {
        seq: data[3],
        layer: data[4],
        layers_active: u32::from_le_bytes([data[5], data[6], data[7], data[8]]),
        battery_central: battery(data[9]),
        battery_peripherals: data[11..11 + count].iter().map(|&b| battery(b)).collect(),
        uptime_s: u32::from_le_bytes([data[15], data[16], data[17], data[18]]),
    })
}

/// SYNC request, prefixed with report ID 0 as hidapi expects for devices without report IDs.
pub fn sync_request() -> [u8; REPORT_SIZE + 1] {
    let mut packet = [0u8; REPORT_SIZE + 1];
    packet[1] = MAGIC;
    packet[2] = VERSION;
    packet[3] = MSG_SYNC;
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_state_report() {
        let mut r = [0u8; REPORT_SIZE];
        r[..5].copy_from_slice(&[MAGIC, VERSION, MSG_STATE, 7, 5]);
        r[5..9].copy_from_slice(&0b10_0000u32.to_le_bytes());
        r[9] = 81;
        r[10] = 1;
        r[11..15].copy_from_slice(&[97, 0xFF, 0xFF, 0xFF]);
        r[15..19].copy_from_slice(&250u32.to_le_bytes());

        let Message::State(report) = decode(&r).unwrap() else {
            panic!("not a state")
        };
        assert_eq!(report.seq, 7);
        assert_eq!(report.layer, 5);
        assert_eq!(report.layers_active, 0b10_0000);
        assert_eq!(report.battery_central, Some(81));
        assert_eq!(report.battery_peripherals, vec![Some(97)]);
        assert_eq!(report.uptime_s, 250);
    }

    #[test]
    fn rejects_foreign_reports() {
        assert_eq!(decode(&[0u8; REPORT_SIZE]), Err(DecodeError::NotKomotype));
        assert_eq!(decode(&[MAGIC; 4]), Err(DecodeError::TooShort));
    }

    #[test]
    fn decodes_stats_reports() {
        let mut r = [0u8; REPORT_SIZE];
        r[..8].copy_from_slice(&[MAGIC, VERSION, MSG_KEYSTATS, 9, 2, 13, 4, 40]);
        r[8] = 1;
        assert_eq!(
            decode(&r),
            Ok(Message::KeyStats {
                seq: 9,
                presses: vec![(13, 4), (40, 1)]
            })
        );
        let mut r = [0u8; REPORT_SIZE];
        r[..8].copy_from_slice(&[MAGIC, VERSION, MSG_HOLDTAP, 10, 1, 13, 7, 2]);
        assert_eq!(
            decode(&r),
            Ok(Message::HoldTap {
                seq: 10,
                outcomes: vec![(13, 7, 2)]
            })
        );
    }
}
