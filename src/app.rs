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
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    GetWindowLongPtrW, GetWindowLongW, KillTimer, MessageBoxW, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SetTimer, SetWindowLongPtrW, TranslateMessage,
    CREATESTRUCTW, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND, GWLP_USERDATA, GWL_EXSTYLE,
    GWL_STYLE, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MSG, OBJID_WINDOW,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONUP, WM_NCCREATE, WM_POWERBROADCAST,
    WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_POPUP,
};

use crate::autostart;
use crate::config::{self, Config};
use crate::hotkey::{self, Hotkey};
use crate::i18n::{self, tr, Lang, S};
use crate::log;
use crate::overlay::{should_reassert, Overlay};
use crate::tray::{self, MenuSnapshot, Tray};
use crate::win::{loword, wide};

pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_REASSERT: u32 = WM_APP + 2;
pub const WM_APP_SHOW_SETTINGS: u32 = WM_APP + 3;

const MSG_CLASS: &str = "NightSleepMsgWnd";
const MUTEX_NAME: &str = "Local\\NightSleep_SingleInstance";

const HK_DIM_MORE: i32 = 1;
const HK_DIM_LESS: i32 = 2;
const HK_TOGGLE: i32 = 3;
const HK_EMERGENCY: i32 = 4;

const TIMER_SAVE: usize = 1;
const TIMER_MONITORS: usize = 2;
const SAVE_DELAY_MS: u32 = 1000;
const MONITOR_DEBOUNCE_MS: u32 = 300;

// Values of constants that live in headers outside the enabled feature set.
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_UNLOCK: usize = 8;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Window handle for the WinEvent callback, which has no user-data parameter.
static HOOK_TARGET: AtomicIsize = AtomicIsize::new(0);
/// True while a reassert message is queued: bursts of events collapse into one.
static REASSERT_PENDING: AtomicBool = AtomicBool::new(false);

struct App {
    hwnd: HWND,
    cfg: Config,
    first_run: bool,
    overlay: Option<Overlay>,
    tray: Option<Tray>,
    /// Foreground-change hook, installed for the whole run.
    hook: HWINEVENTHOOK,
    /// Window-shown hook, installed only while dimming is active so that the
    /// idle app (0%) receives no system-wide events at all.
    hook_show: HWINEVENTHOOK,
    taskbar_created_msg: u32,
    session_registered: bool,
    shut_down: bool,
}

pub fn run() -> i32 {
    std::panic::set_hook(Box::new(|info| {
        log::error(&format!("panic: {info}"));
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--cleanup") {
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
        overlay: None,
        tray: None,
        hook: null_mut(),
        hook_show: null_mut(),
        taskbar_created_msg: 0,
        session_registered: false,
        shut_down: false,
    });
    let app_ptr = Box::into_raw(app);

    let class = wide(MSG_CLASS);
    let title = wide("NightSleep");
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
        HOOK_TARGET.store(hwnd as isize, Ordering::Relaxed);
        log::info(&format!("start v{APP_VERSION}"));

        let taskbar_name = wide("TaskbarCreated");
        // SAFETY: valid NUL-terminated name.
        self.taskbar_created_msg = unsafe { RegisterWindowMessageW(taskbar_name.as_ptr()) };

        match Overlay::new(hinstance) {
            Ok(overlay) => self.overlay = Some(overlay),
            Err(code) => log::error(&format!("overlay unavailable (error {code})")),
        }

        let tray = Tray::new(hwnd, hinstance);
        if !tray.add(self.cfg.level.current(), WM_APP_TRAY) {
            log::error("failed to add tray icon");
        }
        self.tray = Some(tray);

        let failed = self.register_hotkeys();

        // SAFETY: the callback only posts a message to our own window.
        unsafe {
            self.hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                null_mut(),
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            self.session_registered =
                WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) != 0;
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
            tray.notify("NightSleep", text, warning);
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

    /// Pushes the current level to the overlay and tray, and schedules a save.
    fn apply_level(&mut self) {
        let level = self.cfg.level.current();
        if let Some(overlay) = &mut self.overlay {
            overlay.apply(level);
        }
        if let Some(tray) = &self.tray {
            tray.update(level);
        }
        self.set_show_hook(level > 0);
        // SAFETY: restarts the one-shot save timer (debounce).
        unsafe {
            SetTimer(self.hwnd, TIMER_SAVE, SAVE_DELAY_MS, None);
        }
    }

    /// Installs or removes the hook that notices new topmost windows
    /// (taskbar thumbnails, tooltips, flyouts) so the overlay can go back on top.
    fn set_show_hook(&mut self, on: bool) {
        // SAFETY: the callback only filters and posts a message to our window;
        // the handle is owned by `self` and released exactly once.
        unsafe {
            if on && self.hook_show.is_null() {
                self.hook_show = SetWinEventHook(
                    EVENT_OBJECT_SHOW,
                    EVENT_OBJECT_SHOW,
                    null_mut(),
                    Some(show_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                if self.hook_show.is_null() {
                    log::warn(&format!(
                        "SetWinEventHook(EVENT_OBJECT_SHOW) failed ({})",
                        GetLastError()
                    ));
                }
            } else if !on && !self.hook_show.is_null() {
                UnhookWinEvent(self.hook_show);
                self.hook_show = null_mut();
            }
        }
    }

    fn save_now(&self) {
        if let Err(e) = config::save(&self.cfg) {
            log::error(&format!("saving config failed: {e}"));
        }
    }

    fn reassert(&self) {
        if let Some(overlay) = &self.overlay {
            overlay.reassert();
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
            TIMER_MONITORS => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.sync();
                }
            }
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
        // Save first, with the real level: the overlay disappears with the
        // process, so there is no need to reset to 0% (which would be saved).
        self.save_now();
        if let Some(tray) = &self.tray {
            tray.remove();
        }
        self.unregister_hotkeys();
        // SAFETY: handles were obtained in `init` and are released once.
        unsafe {
            if !self.hook.is_null() {
                UnhookWinEvent(self.hook);
                self.hook = null_mut();
            }
            if !self.hook_show.is_null() {
                UnhookWinEvent(self.hook_show);
                self.hook_show = null_mut();
            }
            if self.session_registered {
                WTSUnRegisterSessionNotification(self.hwnd);
                self.session_registered = false;
            }
        }
        HOOK_TARGET.store(0, Ordering::Relaxed);
        if let Some(overlay) = &mut self.overlay {
            overlay.destroy();
        }
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

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    request_reassert();
}

/// A window was shown somewhere in the system (another process: our own
/// events are skipped by `WINEVENT_SKIPOWNPROCESS`). Only top-level topmost
/// windows can cover the overlay, so everything else is ignored cheaply.
unsafe extern "system" fn show_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_null() || id_object != OBJID_WINDOW || id_child != 0 {
        return;
    }
    // SAFETY: plain queries on a window handle; a stale handle just fails.
    let (style, ex_style) = unsafe {
        (
            GetWindowLongW(hwnd, GWL_STYLE) as u32,
            GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
        )
    };
    if should_reassert(style, ex_style) {
        request_reassert();
    }
}

/// Asks the message loop to raise the overlay. At most one request is queued
/// at a time, so a burst of events costs a single `SetWindowPos` pass.
fn request_reassert() {
    let target = HOOK_TARGET.load(Ordering::Relaxed);
    if target == 0 || REASSERT_PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: posting to our own window.
    let posted = unsafe { PostMessageW(target as HWND, WM_APP_REASSERT, 0, 0) };
    if posted == 0 {
        REASSERT_PENDING.store(false, Ordering::Release);
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
            WM_APP_REASSERT | WM_SETTINGCHANGE => {
                if msg == WM_APP_REASSERT {
                    // Clear first: events arriving during the pass queue a new one.
                    REASSERT_PENDING.store(false, Ordering::Release);
                }
                (*app).reassert();
                if msg == WM_SETTINGCHANGE {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                0
            }
            WM_APP_SHOW_SETTINGS => {
                // The settings dialog is not implemented yet (stage 3).
                let lang = (*app).lang();
                (*app).notify(tr(lang, S::AlreadyRunning), false);
                0
            }
            WM_DISPLAYCHANGE => {
                // Arrives in bursts: debounce, then resync the overlay windows.
                SetTimer(hwnd, TIMER_MONITORS, MONITOR_DEBOUNCE_MS, None);
                0
            }
            WM_TIMER => {
                (*app).on_timer(wparam);
                0
            }
            WM_POWERBROADCAST => {
                if wparam == PBT_APMRESUMEAUTOMATIC {
                    (*app).reassert();
                }
                1 // TRUE
            }
            WM_WTSSESSION_CHANGE => {
                if wparam == WTS_SESSION_UNLOCK {
                    (*app).reassert();
                }
                0
            }
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                if wparam != 0 {
                    (*app).save_now();
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
