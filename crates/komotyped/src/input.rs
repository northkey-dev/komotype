//! Keyboard input via the Windows Raw Input API.
//!
//! Raw Input reports every key event together with the device it came from, also while other
//! applications have focus. Only key codes are forwarded, never text.

use std::cell::RefCell;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use crate::engine::Event;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
    RegisterClassW, TranslateMessage, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WNDCLASSW,
};

/// Opaque Raw Input device handle.
pub type DeviceHandle = usize;

const RI_KEY_BREAK: u16 = 1;
const RI_KEY_E0: u16 = 2;
const GIDC_ARRIVAL: usize = 1;
const GIDC_REMOVAL: usize = 2;

#[derive(Debug, Clone, Copy)]
pub enum InputEvent {
    Key {
        device: DeviceHandle,
        vkey: u16,
        pressed: bool,
        /// Extended-key flag (e.g. right Alt = AltGr, right Ctrl).
        e0: bool,
        at: Instant,
    },
    DeviceAdded(DeviceHandle),
    DeviceRemoved(DeviceHandle),
}

thread_local! {
    static SENDER: RefCell<Option<Sender<Event>>> = const { RefCell::new(None) };
}

/// Starts the Raw Input thread. Events stop when the receiver is dropped.
pub fn spawn(tx: Sender<Event>) -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("raw-input".into())
        .spawn(move || {
            SENDER.with(|s| *s.borrow_mut() = Some(tx));
            match unsafe { create_window() } {
                Ok(_) => {
                    let _ = ready_tx.send(Ok(()));
                    unsafe { message_loop() };
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .context("spawning raw input thread")?;
    ready_rx.recv().context("raw input thread died")?
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn create_window() -> Result<HWND> {
    unsafe {
        let class_name = wide("komotyped-raw-input");
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&class) == 0 {
            bail!("RegisterClassW failed: {}", std::io::Error::last_os_error());
        }
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            bail!(
                "CreateWindowExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01, // Generic Desktop
            usUsage: 0x06,     // Keyboard
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        };
        if RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            bail!(
                "RegisterRawInputDevices failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(hwnd)
    }
}

unsafe fn message_loop() {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn send(event: InputEvent) {
    SENDER.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.send(Event::Input(event));
        }
    });
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_INPUT => {
                tracing::trace!("WM_INPUT received");
                if let Some(event) = read_key(lparam as HRAWINPUT) {
                    send(event);
                }
                // WM_INPUT must still reach DefWindowProc so the system can clean up.
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_INPUT_DEVICE_CHANGE => {
                let device = lparam as DeviceHandle;
                match wparam {
                    GIDC_ARRIVAL => send(InputEvent::DeviceAdded(device)),
                    GIDC_REMOVAL => send(InputEvent::DeviceRemoved(device)),
                    _ => {}
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn read_key(handle: HRAWINPUT) -> Option<InputEvent> {
    unsafe {
        let mut raw: RAWINPUT = std::mem::zeroed();
        let mut size = size_of::<RAWINPUT>() as u32;
        let read = GetRawInputData(
            handle,
            RID_INPUT,
            (&mut raw as *mut RAWINPUT).cast(),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        );
        if read == u32::MAX || raw.header.dwType != RIM_TYPEKEYBOARD {
            return None;
        }
        let device = raw.header.hDevice as DeviceHandle;
        // Injected input (SendInput, macros) has no device. It is not typing on a keyboard.
        if device == 0 {
            return None;
        }
        let kb = raw.data.keyboard;
        // 0xFF is sent as part of escaped sequences, not a real key.
        if kb.VKey == 0xFF {
            return None;
        }
        Some(InputEvent::Key {
            device,
            vkey: kb.VKey,
            pressed: kb.Flags & RI_KEY_BREAK == 0,
            e0: kb.Flags & RI_KEY_E0 != 0,
            at: Instant::now(),
        })
    }
}
