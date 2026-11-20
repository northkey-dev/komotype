//! Shared types for komotype: the raw HID protocol spoken by the ZMK module and the IPC
//! messages the daemon (`komotyped`) sends to clients such as the komorebi-bar widget.

pub mod ipc;
pub mod raw_hid;
