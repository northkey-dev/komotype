//! Reads state reports from keyboards running the komotype ZMK module (raw HID).

use std::collections::HashSet;
use std::ffi::CString;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};
use komotype_protocol::ipc::{FirmwareState, Transport};
use komotype_protocol::raw_hid::{self, REPORT_SIZE};
use tracing::{debug, info, warn};

use crate::devices::transport_of;
use crate::engine::Event;

const SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Starts a thread that watches for komotype keyboards and forwards their reports.
pub fn spawn(tx: Sender<Event>) {
    thread::Builder::new()
        .name("firmware-scan".into())
        .spawn(move || scan_loop(tx))
        .expect("spawning firmware scan thread");
}

fn scan_loop(tx: Sender<Event>) {
    let open: Arc<Mutex<HashSet<CString>>> = Arc::default();
    let mut api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            warn!("hidapi unavailable, firmware data disabled: {e}");
            return;
        }
    };
    loop {
        if let Err(e) = api.refresh_devices() {
            warn!("HID enumeration failed: {e}");
        }
        for info in api.device_list() {
            if info.usage_page() != raw_hid::USAGE_PAGE || info.usage() != raw_hid::USAGE {
                continue;
            }
            let path = info.path().to_owned();
            if !open.lock().unwrap().insert(path.clone()) {
                continue;
            }
            let transport = transport_of(&path.to_string_lossy());
            match api.open_path(&path) {
                Ok(device) => {
                    info!(?transport, "komotype keyboard found");
                    let tx = tx.clone();
                    let open = open.clone();
                    thread::spawn(move || {
                        read_loop(&device, transport, &tx);
                        open.lock().unwrap().remove(&path);
                        info!(?transport, "komotype keyboard gone");
                    });
                }
                Err(e) => {
                    debug!("cannot open raw HID interface: {e}");
                    open.lock().unwrap().remove(&path);
                }
            }
        }
        thread::sleep(SCAN_INTERVAL);
    }
}

fn read_loop(device: &HidDevice, transport: Transport, tx: &Sender<Event>) {
    if let Err(e) = device.write(&raw_hid::sync_request()) {
        debug!("SYNC failed: {e}");
    }
    let mut buf = [0u8; REPORT_SIZE];
    loop {
        match device.read_timeout(&mut buf, 1000) {
            Ok(0) => continue,
            Ok(n) => match raw_hid::decode(&buf[..n]) {
                Ok(report) => {
                    if tx
                        .send(Event::Firmware(FirmwareState { transport, report }))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(e) => debug!("ignoring report: {e:?}"),
            },
            Err(e) => {
                debug!("raw HID read failed: {e}");
                return;
            }
        }
    }
}
