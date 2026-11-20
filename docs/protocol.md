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
| 11 | 4 | peripheral battery % for peripherals 0..3 (`0xFF` = unknown, e.g. never received) |
| 15 | 4 | keyboard uptime in seconds |

### `0x02` KEYSTATS

Key presses per physical key position since the previous KEYSTATS report. Sent every 2 s while
typing (nothing when idle). Several reports if more positions changed than fit into one.

| Offset | Size | Field |
|---|---|---|
| 4 | 1 | entry count n |
| 5 | 2 × n | entries: (position, presses) |

### `0x03` HOLDTAP

Outcome of hold-tap keys (e.g. home row mods) per position since the previous HOLDTAP report.
The module matches each hold-tap press against the key code that follows: the tap key means tap,
the hold key means hold.

| Offset | Size | Field |
|---|---|---|
| 4 | 1 | entry count n |
| 5 | 3 × n | entries: (position, taps, holds) |

Note: a keyboard connected over USB and Bluetooth at the same time sends every report on both.
Hosts should count a KEYSTATS / HOLDTAP report once per sequence number.

## Host → keyboard

### `0x80` SYNC

Asks the keyboard to send STATE now (e.g. after the daemon starts). No payload.

## Status

Version 1. Message types are additive: hosts ignore types they do not know.
