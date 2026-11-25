//! Application core: the hidden message window, hotkeys, tray, timers and
//! system notifications. Everything runs on one thread.
//!
//! Re-entrancy rule: `TrackPopupMenu` and `MessageBoxW` run nested message
//! loops that dispatch back into `wndproc`. Therefore no `&mut App` is ever
//! held across such a call: state is snapshotted first, the modal call runs
//! without a borrow, and the result is applied afterwards.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    GetWindowLongPtrW, KillTimer, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SetTimer, SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW,
    GWLP_USERDATA, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MSG, WM_APP, WM_CLOSE,
    WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONUP,
    WM_NCCREATE, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSEXW, WS_POPUP,
};

use crate::autostart;
use crate::config::{self, Config};
use crate::hotkey::{self, Hotkey};
use crate::i18n::{self, tr, Lang, S};
use crate::log;
use crate::magnifier::{self, Magnifier};
use crate::tray::{self, MenuSnapshot, Tray};
use crate::win::{loword, wide};

pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_SHOW_SETTINGS: u32 = WM_APP + 3;

const MSG_CLASS: &str = "NightSleepMsgWnd";
const MUTEX_NAME: &str = "Local\\NightSleep_SingleInstance";

const HK_DIM_MORE: i32 = 1;
const HK_DIM_LESS: i32 = 2;
const HK_TOGGLE: i32 = 3;
const HK_EMERGENCY: i32 = 4;

const TIMER_SAVE: usize = 1;
const TIMER_REFRESH: usize = 2;
const SAVE_DELAY_MS: u32 = 1000;
const REFRESH_DEBOUNCE_MS: u32 = 300;

// Values of constants that live in headers outside the enabled feature set.
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_UNLOCK: usize = 8;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

struct App {
    hwnd: HWND,
    cfg: Config,
    first_run: bool,
    /// The only dimming method: a color matrix of the Magnification API.
    magnifier: Magnifier,
    tray: Option<Tray>,
    taskbar_created_msg: u32,
    session_registered: bool,
    shut_down: bool,
}

pub fn run() -> i32 {
    std::panic::set_hook(Box::new(|info| {
        log::error(&format!("panic: {info}"));
        // The process aborts after this hook: never leave the screen dimmed.
        magnifier::emergency_reset();
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--reset-screen") {
        // Standalone recovery: drop any leftover Magnification color effect.
        let ok = magnifier::force_reset();
        log::info(&format!(
            "--reset-screen: {}",
            if ok { "ok" } else { "failed" }
        ));
        return if ok { 0 } else { 1 };
    }
    if args.iter().any(|a| a == "--cleanup") {
        magnifier::force_reset();
        autostart::remove();
        if let Err(e) = config::remove_all() {
            log::error(&format!("cleanup failed: {e}"));
            return 1;
        }
        return 0;
    }

    // Single instance: keep the mutex handle open for the process lifetime.
    let name = wide(MUTEX_NAME);
    // SAFETY: valid NUL-terminated name; null security attributes.
    let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    // SAFETY: trivial FFI call, must directly follow CreateMutexW.
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_running {
        let class = wide(MSG_CLASS);
        // SAFETY: valid NUL-terminated class name.
        unsafe {
            let other = FindWindowW(class.as_ptr(), null());
            if !other.is_null() {
                PostMessageW(other, WM_APP_SHOW_SETTINGS, 0, 0);
            }
        }
        return 0;
    }

    let loaded = config::load();
    if let Some(warning) = &loaded.warning {
        log::warn(warning);
    }
    let mut cfg = loaded.config;
    if args.iter().any(|a| a == "--reset") || !cfg.restore_on_startup {
        cfg.level.set(0);
    }
    if args.iter().any(|a| a == "--autostart") {
        log::info("started from autostart");
    }
    autostart::sync_path();

    // SAFETY: null module name returns the handle of the current executable.
    let hinstance = unsafe { GetModuleHandleW(null()) };
    let app = Box::new(App {
        hwnd: null_mut(),
        cfg,
        first_run: loaded.first_run,
        magnifier: Magnifier::new(),
        tray: None,
        taskbar_created_msg: 0,
        session_registered: false,
        shut_down: false,
    });
    let app_ptr = Box::into_raw(app);

    let class = wide(MSG_CLASS);
    let title = wide("NightSleep-Monitor");
    // SAFETY: the class/title buffers outlive the calls; `app_ptr` stays valid
    // until reclaimed below, after the message loop has ended.
    let exit_code = unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        if RegisterClassExW(&wc) == 0 {
            log::error(&format!("RegisterClassExW failed ({})", GetLastError()));
            drop(Box::from_raw(app_ptr));
            return 1;
        }
        // A hidden top-level window (not message-only): message-only windows
        // do not receive broadcasts such as WM_DISPLAYCHANGE or TaskbarCreated.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            app_ptr as *const c_void,
        );
        if hwnd.is_null() {
            log::error(&format!("CreateWindowExW failed ({})", GetLastError()));
            drop(Box::from_raw(app_ptr));
            return 1;
        }

        (*app_ptr).init(hwnd, hinstance);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        msg.wParam as i32
    };

    // SAFETY: the window is destroyed and the loop has ended, so nothing can
    // reach `app_ptr` any more.
    unsafe {
        drop(Box::from_raw(app_ptr));
        if !mutex.is_null() {
            CloseHandle(mutex);
        }
    }
    log::info("exit");
    exit_code
}

fn system_is_russian() -> bool {
    // SAFETY: trivial FFI call. Primary language id 0x19 = LANG_RUSSIAN.
    (unsafe { GetUserDefaultUILanguage() } & 0x3FF) == 0x19
}

impl App {
    fn lang(&self) -> Lang {
        i18n::resolve(self.cfg.language, system_is_russian())
    }

    fn init(&mut self, hwnd: HWND, hinstance: windows_sys::Win32::Foundation::HINSTANCE) {
        self.hwnd = hwnd;
        log::info(&format!("start v{APP_VERSION}"));

        let taskbar_name = wide("TaskbarCreated");
        // SAFETY: valid NUL-terminated name.
        self.taskbar_created_msg = unsafe { RegisterWindowMessageW(taskbar_name.as_ptr()) };

        let tray = Tray::new(hwnd, hinstance);
        if !tray.add(self.cfg.level.current(), WM_APP_TRAY) {
            log::error("failed to add tray icon");
        }
        self.tray = Some(tray);

        let failed = self.register_hotkeys();

        // SAFETY: plain FFI call on our own window.
        self.session_registered =
            unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } != 0;

        if self.cfg.level.current() == 0 {
            // A previous run may have died with the color effect still applied.
            magnifier::force_reset();
        }
        self.apply_level();

        let lang = self.lang();
        if !failed.is_empty() {
            let text = format!("{}{}", tr(lang, S::HotkeysFailed), failed.join(", "));
            self.notify(&text, true);
        } else if self.first_run {
            let hk = &self.cfg.hotkeys;
            let text = i18n::first_run(
                lang,
                &hk.dim_more.to_string(),
                &hk.dim_less.to_string(),
                &hk.toggle.to_string(),
                &hotkey::EMERGENCY.to_string(),
            );
            self.notify(&text, false);
        }
    }

    fn notify(&self, text: &str, warning: bool) {
        if let Some(tray) = &self.tray {
            tray.notify("NightSleep-Monitor", text, warning);
        }
    }

    /// Returns the names of hotkeys that could not be registered.
    fn register_hotkeys(&self) -> Vec<String> {
        let hk = &self.cfg.hotkeys;
        let table: [(i32, Hotkey, bool); 4] = [
            (HK_DIM_MORE, hk.dim_more, false), // hold to repeat
            (HK_DIM_LESS, hk.dim_less, false),
            (HK_TOGGLE, hk.toggle, true),
            (HK_EMERGENCY, hotkey::EMERGENCY, true),
        ];
        let mut failed = Vec::new();
        for (id, key, no_repeat) in table {
            let mut mods = 0;
            if key.mods & hotkey::CTRL != 0 {
                mods |= MOD_CONTROL;
            }
            if key.mods & hotkey::ALT != 0 {
                mods |= MOD_ALT;
            }
            if key.mods & hotkey::SHIFT != 0 {
                mods |= MOD_SHIFT;
            }
            if no_repeat {
                mods |= MOD_NOREPEAT;
            }
            // SAFETY: plain FFI call on our own window.
            let ok = unsafe { RegisterHotKey(self.hwnd, id, mods, key.vk) } != 0;
            if !ok {
                // SAFETY: trivial FFI call.
                let code = unsafe { GetLastError() };
                log::warn(&format!("RegisterHotKey({key}) failed ({code})"));
                failed.push(key.to_string());
            }
        }
        failed
    }

    fn unregister_hotkeys(&self) {
        for id in [HK_DIM_MORE, HK_DIM_LESS, HK_TOGGLE, HK_EMERGENCY] {
            // SAFETY: plain FFI call; unregistering an unknown id is harmless.
            unsafe {
                UnregisterHotKey(self.hwnd, id);
            }
        }
    }

    /// Pushes the current level to the screen and tray, and schedules a save.
    fn apply_level(&mut self) {
        let level = self.cfg.level.current();
        self.apply_dimming(level);
        // The level may have been reset to 0 if dimming failed.
        let level = self.cfg.level.current();
        if let Some(tray) = &self.tray {
            tray.update(level);
        }
        // SAFETY: restarts the one-shot save timer (debounce).
        unsafe {
            SetTimer(self.hwnd, TIMER_SAVE, SAVE_DELAY_MS, None);
        }
    }

    /// Applies the color matrix. There is no fallback method: if the
    /// Magnification API refuses, the level goes back to 0% so that the tray
    /// and the saved settings never claim a dimming that is not on screen.
    /// Returns whether dimming succeeded.
    fn apply_dimming(&mut self, level: u8) -> bool {
        match self.magnifier.apply(level) {
            Ok(()) => true,
            Err(e) => {
                log::error(&format!("magnifier: {e}"));
                self.cfg.level.set(0);
                let lang = self.lang();
                self.notify(tr(lang, S::DimmingFailed), true);
                false
            }
        }
    }

    fn save_now(&self) {
        if let Err(e) = config::save(&self.cfg) {
            log::error(&format!("saving config failed: {e}"));
        }
    }

    /// Resume, unlock, settings or display change: apply the matrix again.
    fn refresh(&mut self) {
        let level = self.cfg.level.current();
        if !self.apply_dimming(level) {
            self.apply_level(); // update tray and schedule a save
        }
    }

    fn on_hotkey(&mut self, id: i32) {
        match id {
            HK_DIM_MORE => self.cfg.level.dim_more(),
            HK_DIM_LESS => self.cfg.level.dim_less(),
            HK_TOGGLE => self.cfg.level.toggle(),
            HK_EMERGENCY => self.cfg.level.set(0),
            _ => return,
        }
        self.apply_level();
    }

    fn on_timer(&mut self, id: usize) {
        // SAFETY: both timers are one-shot.
        unsafe {
            KillTimer(self.hwnd, id);
        }
        match id {
            TIMER_SAVE => self.save_now(),
            TIMER_REFRESH => self.refresh(),
            _ => {}
        }
    }

    fn menu_snapshot(&self) -> MenuSnapshot {
        MenuSnapshot {
            lang: self.lang(),
            current: self.cfg.level.current(),
            presets: self.cfg.level.presets().collect(),
            autostart: autostart::is_enabled(),
        }
    }

    fn on_menu_command(&mut self, cmd: u32) {
        if (tray::ID_LEVEL_BASE..=tray::ID_LEVEL_BASE + 100).contains(&cmd) {
            self.cfg.level.set((cmd - tray::ID_LEVEL_BASE) as u8);
            self.apply_level();
        } else if cmd == tray::ID_AUTOSTART && !autostart::toggle() {
            log::warn("changing autostart failed");
        }
    }

    /// Idempotent: runs on WM_CLOSE and, as a fallback, on session end.
    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        // Save first, with the real level: the screen is restored below without
        // touching the level, so the next start can bring the dimming back.
        self.save_now();
        if let Some(tray) = &self.tray {
            tray.remove();
        }
        self.unregister_hotkeys();
        if self.session_registered {
            // SAFETY: registered in `init`; released once.
            unsafe {
                WTSUnRegisterSessionNotification(self.hwnd);
            }
            self.session_registered = false;
        }
        // Restore the screen last: identity matrix, API released.
        self.magnifier.reset();
    }
}

fn show_about(lang: Lang) {
    let text = format!(
        "NightSleep {APP_VERSION}\n\n{}\n\nMIT OR Apache-2.0\n{APP_REPOSITORY}",
        tr(lang, S::AboutDescription)
    );
    let text = wide(&text);
    let title = wide(tr(lang, S::MenuAbout));
    // SAFETY: modal box; called without any borrow of `App` held (see module docs).
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

fn on_tray_context_menu(hwnd: HWND, app: *mut App) {
    // SAFETY: `app` is the live App; the shared borrow ends before the menu runs.
    let snapshot = unsafe { (*app).menu_snapshot() };
    let lang = snapshot.lang;
    let cmd = tray::show_menu(hwnd, &snapshot);
    match cmd {
        0 => {}
        tray::ID_ABOUT => show_about(lang),
        // SAFETY: WM_CLOSE is handled later by the message loop, not here.
        tray::ID_EXIT => unsafe {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        },
        // SAFETY: no modal call is active, so this is the only live reference.
        other => unsafe { (*app).on_menu_command(other) },
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: every block below dereferences the `App` pointer stored by
    // WM_NCCREATE. It is valid from then until after the message loop ends,
    // and handlers never hold a reference across a modal call.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if app.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        match msg {
            WM_HOTKEY => {
                (*app).on_hotkey(wparam as i32);
                0
            }
            WM_APP_TRAY => {
                match loword(lparam) {
                    WM_LBUTTONUP => {
                        (*app).cfg.level.toggle();
                        (*app).apply_level();
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => on_tray_context_menu(hwnd, app),
                    _ => {}
                }
                0
            }
            WM_SETTINGCHANGE => {
                (*app).refresh();
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_APP_SHOW_SETTINGS => {
                // The settings dialog is not implemented yet (stage 3).
                let lang = (*app).lang();
                (*app).notify(tr(lang, S::AlreadyRunning), false);
                0
            }
            WM_DISPLAYCHANGE => {
                // Arrives in bursts: debounce, then apply the matrix again.
                SetTimer(hwnd, TIMER_REFRESH, REFRESH_DEBOUNCE_MS, None);
                0
            }
            WM_TIMER => {
                (*app).on_timer(wparam);
                0
            }
            WM_POWERBROADCAST => {
                if wparam == PBT_APMRESUMEAUTOMATIC {
                    (*app).refresh();
                }
                1 // TRUE
            }
            WM_WTSSESSION_CHANGE => {
                if wparam == WTS_SESSION_UNLOCK {
                    (*app).refresh();
                }
                0
            }
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                if wparam != 0 {
                    // The session is ending: save and restore the screen now.
                    (*app).shutdown();
                }
                0
            }
            WM_CLOSE => {
                (*app).shutdown();
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => {
                let created = (*app).taskbar_created_msg;
                if created != 0 && msg == created {
                    // Explorer restarted: the tray icon is gone, add it again.
                    if let Some(tray) = &(*app).tray {
                        tray.add((*app).cfg.level.current(), WM_APP_TRAY);
                    }
                    return 0;
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }
}
