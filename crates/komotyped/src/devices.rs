//! Identifies keyboards: Raw Input handle → path, VID/PID, product name, transport.

use std::collections::HashMap;
use std::mem::size_of;
use std::ptr::null_mut;

use komotype_protocol::ipc::Transport;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Input::{
    GetRawInputDeviceInfoW, GetRawInputDeviceList, RAWINPUTDEVICELIST, RIDI_DEVICENAME,
    RIM_TYPEKEYBOARD,
};

use crate::input::DeviceHandle;

/// HID over GATT service UUID, present in the device path of Bluetooth LE keyboards.
const HOGP_UUID: &str = "00001812-0000-1000-8000-00805f9b34fb";

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub product: Option<String>,
    pub transport: Transport,
}

impl DeviceInfo {
    /// Stable identity across USB and Bluetooth of the same keyboard.
    pub fn key(&self) -> String {
        match (self.vid, self.pid) {
            (Some(vid), Some(pid)) => {
                format!(
                    "{vid:04x}:{pid:04x}:{}",
                    self.product.as_deref().unwrap_or("")
                )
            }
            _ => self.path.to_lowercase(),
        }
    }

    pub fn display_name(&self) -> String {
        match (&self.product, self.vid, self.pid) {
            (Some(p), _, _) => p.clone(),
            (None, Some(vid), Some(pid)) => format!("Keyboard {vid:04X}:{pid:04X}"),
            _ => "Unknown keyboard".into(),
        }
    }
}

/// Which keyboard counts as "main". Matches by VID/PID, optionally narrowed by product name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceMatcher {
    pub vid: u16,
    pub pid: u16,
    /// Case-insensitive substring of the product name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

impl DeviceMatcher {
    pub fn from_info(info: &DeviceInfo) -> Option<Self> {
        Some(Self {
            vid: info.vid?,
            pid: info.pid?,
            product: info.product.clone(),
        })
    }

    pub fn matches(&self, info: &DeviceInfo) -> bool {
        info.vid == Some(self.vid)
            && info.pid == Some(self.pid)
            && match (&self.product, &info.product) {
                (None, _) => true,
                (Some(want), Some(have)) => have.to_lowercase().contains(&want.to_lowercase()),
                // Product name not readable: VID/PID decide.
                (Some(_), None) => true,
            }
    }
}

/// Extracts VID/PID from USB (`VID_1D50&PID_615E`) and Bluetooth (`VID&011d50_PID&615e`) paths.
pub fn parse_vid_pid(path: &str) -> (Option<u16>, Option<u16>) {
    let upper = path.to_uppercase();
    let find = |tags: &[&str], digits: usize| {
        tags.iter().find_map(|tag| {
            let start = upper.find(tag)? + tag.len();
            let hex: String = upper[start..].chars().take(digits).collect();
            u16::from_str_radix(&hex, 16).ok()
        })
    };
    // Bluetooth: "VID&01" + 4 hex digits (01 = Bluetooth SIG vendor ID source).
    let vid = find(&["VID_"], 4).or_else(|| find(&["VID&01", "VID&02"], 4));
    let pid = find(&["PID_", "PID&"], 4);
    (vid, pid)
}

pub fn transport_of(path: &str) -> Transport {
    let lower = path.to_lowercase();
    if lower.contains(HOGP_UUID) || lower.contains("bthledevice") {
        Transport::Bluetooth
    } else if lower.contains("vid_") {
        Transport::Usb
    } else {
        Transport::Other
    }
}

pub fn device_path(handle: DeviceHandle) -> Option<String> {
    unsafe {
        let mut len = 0u32;
        GetRawInputDeviceInfoW(handle as _, RIDI_DEVICENAME, null_mut(), &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        let read = GetRawInputDeviceInfoW(
            handle as _,
            RIDI_DEVICENAME,
            buf.as_mut_ptr().cast(),
            &mut len,
        );
        if read == u32::MAX {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// Device path without the trailing interface class GUID. Raw Input ends keyboard paths with the
/// keyboard class GUID, hidapi with the HID class GUID; the rest is identical.
fn path_key(path: &str) -> String {
    let lower = path.to_lowercase();
    match lower.rfind("#{") {
        Some(i) => lower[..i].to_string(),
        None => lower,
    }
}

/// Product names of HID collections, keyed by [`path_key`].
fn product_names() -> HashMap<String, String> {
    let Ok(api) = hidapi::HidApi::new() else {
        return HashMap::new();
    };
    api.device_list()
        .filter_map(|d| {
            let name = d.product_string()?.trim().to_string();
            (!name.is_empty()).then(|| (path_key(&d.path().to_string_lossy()), name))
        })
        .collect()
}

/// Resolves device details. Product names come from hidapi, which enumerates all HID devices,
/// so callers should cache the result.
pub fn resolve(handle: DeviceHandle) -> Option<DeviceInfo> {
    let path = device_path(handle)?;
    let (vid, pid) = parse_vid_pid(&path);
    let product = product_names().remove(&path_key(&path));
    Some(DeviceInfo {
        transport: transport_of(&path),
        path,
        vid,
        pid,
        product,
    })
}

/// All keyboards Raw Input currently knows about.
pub fn list() -> Vec<(DeviceHandle, DeviceInfo)> {
    let names = product_names();
    let mut count = 0u32;
    let entry = size_of::<RAWINPUTDEVICELIST>() as u32;
    unsafe {
        GetRawInputDeviceList(null_mut(), &mut count, entry);
        let mut devices = vec![std::mem::zeroed::<RAWINPUTDEVICELIST>(); count as usize];
        let n = GetRawInputDeviceList(devices.as_mut_ptr(), &mut count, entry);
        if n == u32::MAX {
            return Vec::new();
        }
        devices.truncate(n as usize);
        devices
            .into_iter()
            .filter(|d| d.dwType == RIM_TYPEKEYBOARD)
            .filter_map(|d| {
                let handle = d.hDevice as DeviceHandle;
                let path = device_path(handle)?;
                let (vid, pid) = parse_vid_pid(&path);
                let product = names.get(&path_key(&path)).cloned();
                Some((
                    handle,
                    DeviceInfo {
                        transport: transport_of(&path),
                        path,
                        vid,
                        pid,
                        product,
                    },
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_path() {
        let p = r"\\?\HID#VID_1D50&PID_615E&MI_00#8&1251ff1&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
        assert_eq!(parse_vid_pid(p), (Some(0x1D50), Some(0x615E)));
        assert_eq!(transport_of(p), Transport::Usb);
    }

    #[test]
    fn parses_bluetooth_path() {
        let p = r"\\?\HID#{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&011d50_PID&615e_REV&0001_d73634a90eb8#b&37e1d174&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
        assert_eq!(parse_vid_pid(p), (Some(0x1D50), Some(0x615E)));
        assert_eq!(transport_of(p), Transport::Bluetooth);
    }

    #[test]
    fn matcher_uses_product_when_given() {
        let info = |product: Option<&str>| DeviceInfo {
            path: String::new(),
            vid: Some(0x1D50),
            pid: Some(0x615E),
            product: product.map(String::from),
            transport: Transport::Usb,
        };
        let m = DeviceMatcher {
            vid: 0x1D50,
            pid: 0x615E,
            product: Some("cornix".into()),
        };
        assert!(m.matches(&info(Some("Cornix"))));
        assert!(!m.matches(&info(Some("Corne"))));
        assert!(m.matches(&info(None)));
    }
}
