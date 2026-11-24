//! Dimming overlay: one click-through, black, layered window per monitor.

use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_EXCLUDED_FROM_PEEK};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetStockObject, BLACK_BRUSH, HBRUSH, HDC, HMONITOR,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_SHOWWINDOW, SW_HIDE, WNDCLASSEXW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::level;
use crate::log;
use crate::win::wide;

const CLASS_NAME: &str = "NightSleepOverlay";

pub struct Overlay {
    hinstance: windows_sys::Win32::Foundation::HINSTANCE,
    class: Vec<u16>,
    windows: Vec<HWND>,
    visible: bool,
    alpha: u8,
}

impl Overlay {
    /// Registers the window class. Windows themselves are created lazily
    /// the first time a non-zero level is applied.
    pub fn new(hinstance: windows_sys::Win32::Foundation::HINSTANCE) -> Result<Self, u32> {
        let class = wide(CLASS_NAME);
        // SAFETY: plain Win32 calls with valid, live pointers; the class name
        // buffer outlives the call and the default window procedure is used.
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(DefWindowProcW),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: null_mut(),
                hCursor: null_mut(),
                hbrBackground: GetStockObject(BLACK_BRUSH) as HBRUSH,
                lpszMenuName: null(),
                lpszClassName: class.as_ptr(),
                hIconSm: null_mut(),
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(GetLastError());
            }
        }
        Ok(Self {
            hinstance,
            class,
            windows: Vec::new(),
            visible: false,
            alpha: 0,
        })
    }

    /// Applies a dimming level in percent. 0 hides every window.
    pub fn apply(&mut self, level: u8) {
        self.alpha = level::alpha(level);
        if level == 0 {
            self.visible = false;
            for &hwnd in &self.windows {
                // SAFETY: hwnd was created by us and is still alive.
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
            }
            return;
        }
        self.visible = true;
        self.sync();
    }

    /// Reconciles the windows with the current monitor layout.
    /// Does nothing while dimming is off; `apply` syncs before showing.
    pub fn sync(&mut self) {
        if !self.visible {
            return;
        }
        let rects = monitor_rects();

        while self.windows.len() > rects.len() {
            if let Some(hwnd) = self.windows.pop() {
                // SAFETY: hwnd was created by us and is still alive.
                unsafe {
                    DestroyWindow(hwnd);
                }
            }
        }

        for (i, rc) in rects.iter().enumerate() {
            let (x, y) = (rc.left, rc.top);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let hwnd = match self.windows.get(i) {
                Some(&hwnd) => hwnd,
                None => match self.create_window() {
                    Some(hwnd) => {
                        self.windows.push(hwnd);
                        hwnd
                    }
                    None => continue,
                },
            };
            // SAFETY: hwnd is a live window owned by this struct.
            unsafe {
                SetLayeredWindowAttributes(hwnd, 0, self.alpha, LWA_ALPHA);
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    /// Puts the overlay back on top of other topmost windows (e.g. taskbar).
    pub fn reassert(&self) {
        if !self.visible {
            return;
        }
        for &hwnd in &self.windows {
            // SAFETY: hwnd is a live window owned by this struct.
            unsafe {
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    pub fn destroy(&mut self) {
        for hwnd in self.windows.drain(..) {
            // SAFETY: hwnd was created by us; drained so it is destroyed once.
            unsafe {
                DestroyWindow(hwnd);
            }
        }
        self.visible = false;
    }

    fn create_window(&self) -> Option<HWND> {
        let title = wide("");
        // SAFETY: class and title buffers outlive the call; the window is
        // created hidden (no WS_VISIBLE) and shown later by `sync`.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW,
                self.class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                self.hinstance,
                null(),
            )
        };
        if hwnd.is_null() {
            // SAFETY: trivial FFI call.
            let code = unsafe { GetLastError() };
            log::error(&format!("overlay: CreateWindowExW failed ({code})"));
            None
        } else {
            exclude_from_peek(hwnd);
            Some(hwnd)
        }
    }
}

/// Aero Peek (hovering a taskbar thumbnail or the "Show desktop" corner)
/// hides every window except the previewed one, which would make the whole
/// screen flash at full brightness. This attribute keeps the overlay visible.
fn exclude_from_peek(hwnd: HWND) {
    let enable: i32 = 1; // BOOL TRUE
                         // SAFETY: `enable` outlives the call and the size matches a BOOL.
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_EXCLUDED_FROM_PEEK as u32,
            &enable as *const i32 as *const std::ffi::c_void,
            size_of::<i32>() as u32,
        )
    };
    if hr < 0 {
        log::warn(&format!(
            "overlay: DWMWA_EXCLUDED_FROM_PEEK failed (HRESULT {hr:#010X})"
        ));
    }
}

/// True if a window that has just been shown can end up above the overlay:
/// a top-level (not child) window that is topmost, such as a taskbar
/// thumbnail, tooltip or flyout. Used to filter system-wide show events.
pub fn should_reassert(style: u32, ex_style: u32) -> bool {
    style & WS_CHILD == 0 && ex_style & WS_EX_TOPMOST != 0
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.destroy();
    }
}

unsafe extern "system" fn collect_monitor(
    _monitor: HMONITOR,
    _dc: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> i32 {
    // SAFETY: `data` is the `&mut Vec<RECT>` passed by `monitor_rects`, and
    // `rect` is a valid pointer provided by the system for this callback.
    let list = unsafe { &mut *(data as *mut Vec<RECT>) };
    list.push(unsafe { *rect });
    1
}

/// Monitor rectangles in physical pixels (the process is Per-Monitor DPI v2).
fn monitor_rects() -> Vec<RECT> {
    let mut rects: Vec<RECT> = Vec::new();
    // SAFETY: the callback only runs during this call, while `rects` is alive.
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect_monitor),
            &mut rects as *mut Vec<RECT> as LPARAM,
        );
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_toplevel_topmost_windows_trigger() {
        assert!(should_reassert(WS_POPUP, WS_EX_TOPMOST));
        assert!(should_reassert(0, WS_EX_TOPMOST | WS_EX_TOOLWINDOW));
        assert!(!should_reassert(WS_POPUP, WS_EX_TOOLWINDOW));
        assert!(!should_reassert(0, 0));
        // a topmost-styled child is not a top-level window
        assert!(!should_reassert(WS_CHILD, WS_EX_TOPMOST));
    }
}
