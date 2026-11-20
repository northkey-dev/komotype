//! Windows toast notifications.

use tauri_winrt_notification::{Sound, Toast};
use tracing::warn;

/// Shows a toast without blocking the caller.
pub fn show(title: String, body: String) {
    std::thread::spawn(move || {
        // komotype has no registered app id yet; PowerShell's is always present.
        let result = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(&title)
            .text1(&body)
            .sound(Some(Sound::Default))
            .show();
        if let Err(e) = result {
            warn!("toast failed: {e}");
        }
    });
}
