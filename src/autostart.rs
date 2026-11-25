//! "Start with Windows" via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! The registry is the only source of truth for this setting.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

use crate::win::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "NightSleep-Monitor";

/// `"C:\path\nightsleep-monitor.exe" --autostart`
fn expected_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --autostart", exe.display()))
}

fn read() -> Option<String> {
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    let mut hkey: HKEY = null_mut();
    // SAFETY: all pointers refer to live local buffers; the key handle is
    // closed on every path before returning.
    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut kind = 0u32;
        let mut size = 0u32;
        let mut result = None;
        if RegQueryValueExW(
            hkey,
            name.as_ptr(),
            null(),
            &mut kind,
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
            && kind == REG_SZ
            && size >= 2
        {
            let mut buf = vec![0u16; (size as usize).div_ceil(2)];
            let mut size2 = size;
            if RegQueryValueExW(
                hkey,
                name.as_ptr(),
                null(),
                &mut kind,
                buf.as_mut_ptr() as *mut u8,
                &mut size2,
            ) == ERROR_SUCCESS
            {
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                result = Some(String::from_utf16_lossy(&buf[..len]));
            }
        }
        RegCloseKey(hkey);
        result
    }
}

pub fn is_enabled() -> bool {
    read().is_some()
}

pub fn set_enabled(on: bool) -> bool {
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    let mut hkey: HKEY = null_mut();
    // SAFETY: see `read`.
    unsafe {
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut hkey,
            null_mut(),
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let ok = if on {
            match expected_command() {
                Some(cmd) => {
                    let data = wide(&cmd);
                    RegSetValueExW(
                        hkey,
                        name.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr() as *const u8,
                        (data.len() * 2) as u32,
                    ) == ERROR_SUCCESS
                }
                None => false,
            }
        } else {
            let status = RegDeleteValueW(hkey, name.as_ptr());
            // 2 = ERROR_FILE_NOT_FOUND: already disabled, which is fine.
            status == ERROR_SUCCESS || status == 2
        };
        RegCloseKey(hkey);
        ok
    }
}

pub fn toggle() -> bool {
    set_enabled(!is_enabled())
}

/// If autostart is on but points to another location (exe was moved),
/// rewrite it. Cheap: a single registry read in the common case.
pub fn sync_path() {
    if let (Some(current), Some(expected)) = (read(), expected_command()) {
        if current != expected {
            set_enabled(true);
        }
    }
}

/// `--cleanup`
pub fn remove() {
    set_enabled(false);
}
