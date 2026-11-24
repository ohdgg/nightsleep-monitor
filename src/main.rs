#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(any(windows, nightsleep_typecheck)), allow(dead_code))]

// Platform-independent logic (unit-tested on any host).
mod config;
mod hotkey;
mod i18n;
mod level;
mod log;

// Win32 layer. `nightsleep_typecheck` lets it be type-checked on a non-Windows
// host: RUSTFLAGS="--cfg nightsleep_typecheck" cargo check
#[cfg(any(windows, nightsleep_typecheck))]
mod app;
#[cfg(any(windows, nightsleep_typecheck))]
mod autostart;
#[cfg(any(windows, nightsleep_typecheck))]
mod magnifier;
#[cfg(any(windows, nightsleep_typecheck))]
mod overlay;
#[cfg(any(windows, nightsleep_typecheck))]
mod tray;
#[cfg(any(windows, nightsleep_typecheck))]
mod win;

#[cfg(any(windows, nightsleep_typecheck))]
fn main() {
    std::process::exit(app::run());
}

#[cfg(not(any(windows, nightsleep_typecheck)))]
fn main() {
    eprintln!("NightSleep supports Windows only.");
    std::process::exit(1);
}
