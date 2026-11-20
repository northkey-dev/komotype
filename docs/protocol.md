# komotype raw HID protocol

Transport: [zmk-raw-hid](https://github.com/zzeneg/zmk-raw-hid), usage page `0xFF60`, usage `0x61`,
32-byte reports, over USB and Bluetooth. Multi-byte values are little endian.

Every packet starts with a 4-byte header:

| Offset | Field | Value |
|---|---|---|
| 0 | magic | `0x4B` (`'K'`) |
| 1 | protocol version | `1` |
| 2 | message type | see below |
| 3 | sequence | keyboard → host: increments per report (wraps). Host → keyboard: unused |

## Keyboard → host

### `0x01` STATE

Sent 50 ms after a layer or battery change, and as a heartbeat every 5 s.

| Offset | Size | Field |
|---|---|---|
| 4 | 1 | highest active layer index |
| 5 | 4 | active layer bitmask (bit n = layer n) |
| 9 | 1 | central battery % (`0xFF` = unknown) |
| 10 | 1 | number of split peripherals (0 for non-split) |
| 11 | 4 | peripheral battery % for peripherals 0..3 (`0xFF` = unknown/not connected) |
| 15 | 4 | keyboard uptime in seconds |

## Host → keyboard

### `0x80` SYNC

Asks the keyboard to send STATE now (e.g. after the daemon starts). No payload.

## Status

Version 1 is the phase-0 spike. Key statistics (positions, hold-tap, combos) follow in a later version.
