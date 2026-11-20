//! Raw HID protocol between the komotype ZMK module and the host. See `docs/protocol.md`.

use serde::{Deserialize, Serialize};

pub const USAGE_PAGE: u16 = 0xFF60;
pub const USAGE: u16 = 0x61;
pub const REPORT_SIZE: usize = 32;

pub const MAGIC: u8 = 0x4B;
pub const VERSION: u8 = 1;

pub const MSG_STATE: u8 = 0x01;
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

/// Decodes a report as read from the device (without report ID).
pub fn decode(data: &[u8]) -> Result<KeyboardReport, DecodeError> {
    if data.len() < 19 {
        return Err(DecodeError::TooShort);
    }
    if data[0] != MAGIC {
        return Err(DecodeError::NotKomotype);
    }
    if data[1] != VERSION {
        return Err(DecodeError::UnsupportedVersion(data[1]));
    }
    if data[2] != MSG_STATE {
        return Err(DecodeError::UnknownMessage(data[2]));
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

        let report = decode(&r).unwrap();
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
}
