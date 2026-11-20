//! Windows toast notifications, sent as "komotype".
//!
//! Unpackaged apps can show toasts under their own name by registering an AppUserModelID in
//! `HKCU\Software\Classes\AppUserModelId`. If that fails, PowerShell's id is used as a fallback.

use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use tauri_winrt_notification::{Sound, Toast};
use tracing::warn;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};

use crate::config;

const APP_ID: &str = "northkey.komotype";
const ICON: &[u8] = include_bytes!("../assets/icon.png");

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn set_string(key: HKEY, name: &str, value: &str) -> bool {
    let name = wide(name);
    let value = wide(value);
    let bytes = (value.len() * 2) as u32;
    unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, value.as_ptr().cast(), bytes) == 0 }
}

/// Registers the app id with display name and icon. Returns false if it could not be done.
fn register() -> bool {
    let icon = config::data_dir().join("icon.png");
    if std::fs::create_dir_all(config::data_dir()).is_err() || std::fs::write(&icon, ICON).is_err()
    {
        warn!("cannot write notification icon");
    }
    let subkey = wide(&format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
    let mut key: HKEY = null_mut();
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if created != 0 {
        warn!("cannot register notification app id (error {created})");
        return false;
    }
    let ok = set_string(key, "DisplayName", "komotype")
        && set_string(key, "IconUri", &icon.to_string_lossy());
    unsafe { RegCloseKey(key) };
    ok
}

fn app_id() -> &'static str {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    if *REGISTERED.get_or_init(register) {
        APP_ID
    } else {
        Toast::POWERSHELL_APP_ID
    }
}

/// Shows a toast without blocking the caller.
pub fn show(title: String, body: String) {
    std::thread::spawn(move || {
        let result = Toast::new(app_id())
            .title(&title)
            .text1(&body)
            .sound(Some(Sound::Default))
            .show();
        if let Err(e) = result {
            warn!("toast failed: {e}");
        }
    });
}
