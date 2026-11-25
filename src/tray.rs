//! Notification-area icon, balloon notifications and the context menu.

use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::Controls::LoadIconMetric;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetCursorPos, InsertMenuItemW, LoadIconW, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION, MENUITEMINFOW, MFS_CHECKED,
    MFS_DISABLED, MFT_RADIOCHECK, MFT_SEPARATOR, MFT_STRING, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::i18n::{tr, Lang, S};
use crate::win::{copy_wide, wide};

pub const TRAY_ID: u32 = 1;

/// Resource ids from `nightsleep.rc`.
const ICON_NORMAL: u16 = 1;
const ICON_DIM: u16 = 2;
/// `LIM_SMALL`: the small-icon metric of the current DPI.
const LIM_SMALL: i32 = 0;

pub const ID_LEVEL_BASE: u32 = 1000; // 1000 + percent
pub const ID_AUTOSTART: u32 = 2000;
pub const ID_ABOUT: u32 = 2001;
pub const ID_EXIT: u32 = 2002;

pub struct Tray {
    hwnd: HWND,
    icon_normal: HICON,
    icon_dim: HICON,
}

fn load_icon(hinstance: windows_sys::Win32::Foundation::HINSTANCE, id: u16) -> HICON {
    let mut icon: HICON = null_mut();
    // SAFETY: `icon` is a valid out-pointer; MAKEINTRESOURCE ids are passed
    // as pointer-sized integers, as the API expects.
    unsafe {
        let hr = LoadIconMetric(hinstance, id as usize as *const u16, LIM_SMALL, &mut icon);
        if hr < 0 || icon.is_null() {
            icon = LoadIconW(null_mut(), IDI_APPLICATION);
        }
    }
    icon
}

impl Tray {
    pub fn new(hwnd: HWND, hinstance: windows_sys::Win32::Foundation::HINSTANCE) -> Self {
        Self {
            hwnd,
            icon_normal: load_icon(hinstance, ICON_NORMAL),
            icon_dim: load_icon(hinstance, ICON_DIM),
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        // SAFETY: NOTIFYICONDATAW is plain old data; all-zero is a valid start.
        let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_ID;
        nid
    }

    fn icon_for(&self, level: u8) -> HICON {
        if level > 0 {
            self.icon_dim
        } else {
            self.icon_normal
        }
    }

    fn tip(level: u8) -> String {
        format!("NightSleep-Monitor — {level}%")
    }

    /// Adds (or re-adds, after an Explorer restart) the icon.
    pub fn add(&self, level: u8, callback_message: u32) -> bool {
        let mut nid = self.data();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = callback_message;
        nid.hIcon = self.icon_for(level);
        copy_wide(&mut nid.szTip, &Self::tip(level));
        // SAFETY: nid is fully initialised and outlives the call.
        unsafe { Shell_NotifyIconW(NIM_ADD, &nid) != 0 }
    }

    pub fn update(&self, level: u8) {
        let mut nid = self.data();
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = self.icon_for(level);
        copy_wide(&mut nid.szTip, &Self::tip(level));
        // SAFETY: see `add`.
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn remove(&self) {
        let nid = self.data();
        // SAFETY: see `add`.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    /// Balloon / toast notification.
    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let mut nid = self.data();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, text);
        // SAFETY: see `add`.
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }
}

/// Everything the menu needs, copied out of the app state so that no borrow
/// is held while the (modal) menu loop runs.
pub struct MenuSnapshot {
    pub lang: Lang,
    pub current: u8,
    pub presets: Vec<u8>,
    pub autostart: bool,
}

fn insert_item(menu: windows_sys::Win32::UI::WindowsAndMessaging::HMENU, pos: u32, item: Item<'_>) {
    let text = wide(item.text);
    // SAFETY: MENUITEMINFOW is plain old data; the text buffer outlives the call.
    unsafe {
        let mut mii: MENUITEMINFOW = zeroed();
        mii.cbSize = size_of::<MENUITEMINFOW>() as u32;
        if item.separator {
            mii.fMask = MIIM_FTYPE;
            mii.fType = MFT_SEPARATOR;
        } else {
            mii.fMask = MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_STRING;
            mii.fType = MFT_STRING | if item.radio { MFT_RADIOCHECK } else { 0 };
            mii.wID = item.id;
            mii.fState = (if item.checked { MFS_CHECKED } else { 0 })
                | (if item.disabled { MFS_DISABLED } else { 0 });
            mii.dwTypeData = text.as_ptr() as *mut u16;
        }
        InsertMenuItemW(menu, pos, 1, &mii);
    }
}

struct Item<'a> {
    id: u32,
    text: &'a str,
    checked: bool,
    radio: bool,
    disabled: bool,
    separator: bool,
}

impl<'a> Item<'a> {
    fn text(id: u32, text: &'a str) -> Self {
        Self {
            id,
            text,
            checked: false,
            radio: false,
            disabled: false,
            separator: false,
        }
    }
    fn separator() -> Self {
        Self {
            separator: true,
            ..Self::text(0, "")
        }
    }
}

/// Shows the context menu at the cursor and returns the chosen command id
/// (0 if dismissed). Must be called from the message-loop thread.
pub fn show_menu(hwnd: HWND, snap: &MenuSnapshot) -> u32 {
    // SAFETY: standard popup-menu sequence; the menu is destroyed before return.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return 0;
        }
        let mut pos = 0u32;
        let mut push = |item: Item<'_>| {
            insert_item(menu, pos, item);
            pos += 1;
        };

        let mut header = Item::text(0, tr(snap.lang, S::MenuHeader));
        header.disabled = true;
        push(header);
        push(Item::separator());

        for &level in &snap.presets {
            let label = if level == 0 {
                tr(snap.lang, S::MenuOff).to_string()
            } else {
                format!("{level}%")
            };
            let mut item = Item::text(ID_LEVEL_BASE + u32::from(level), &label);
            item.radio = true;
            item.checked = level == snap.current;
            push(item);
        }

        push(Item::separator());
        let mut autostart = Item::text(ID_AUTOSTART, tr(snap.lang, S::MenuAutostart));
        autostart.checked = snap.autostart;
        push(autostart);
        push(Item::separator());
        push(Item::text(ID_ABOUT, tr(snap.lang, S::MenuAbout)));
        push(Item::text(ID_EXIT, tr(snap.lang, S::MenuExit)));

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        cmd as u32
    }
}
