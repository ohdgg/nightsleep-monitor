//! Dimming through the Magnification API: a color matrix is applied to the
//! whole desktop at composition time, so it also covers the Start menu, the
//! taskbar, thumbnails, tooltips and Aero Peek (no window has to win a
//! z-order fight). This is the only dimming method of the app.
//!
//! Safety net: the effect is always reset to identity (and the API
//! uninitialised) when dimming goes back to 0%, on exit, on panic, and by the
//! standalone `--reset-screen` command.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::UI::Magnification::{
    MagInitialize, MagSetFullscreenColorEffect, MagUninitialize, MAGCOLOREFFECT,
};

use crate::level;

/// Set once the API has been initialised in this process, so the panic hook
/// does not load `Magnification.dll` needlessly.
static USED: AtomicBool = AtomicBool::new(false);

fn set_matrix(matrix: [f32; 25]) -> bool {
    let effect = MAGCOLOREFFECT { transform: matrix };
    // SAFETY: `effect` outlives the call; the API only reads it.
    unsafe { MagSetFullscreenColorEffect(&effect) != 0 }
}

pub struct Magnifier {
    initialized: bool,
}

impl Magnifier {
    pub fn new() -> Self {
        Self { initialized: false }
    }

    /// Dims to `level` percent. 0 restores the screen and releases the API
    /// (so the built-in Windows Magnifier is free to use again).
    /// An `Err` means the method does not work here: the caller falls back.
    pub fn apply(&mut self, level: u8) -> Result<(), String> {
        if level == 0 {
            self.reset();
            return Ok(());
        }
        if !self.initialized {
            // SAFETY: plain FFI call; paired with MagUninitialize in `reset`.
            if unsafe { MagInitialize() } == 0 {
                // SAFETY: trivial FFI call.
                let code = unsafe { GetLastError() };
                return Err(format!("MagInitialize failed (error {code})"));
            }
            self.initialized = true;
            USED.store(true, Ordering::Relaxed);
        }
        if set_matrix(level::color_matrix(level)) {
            Ok(())
        } else {
            // SAFETY: trivial FFI call.
            let code = unsafe { GetLastError() };
            self.reset();
            Err(format!("MagSetFullscreenColorEffect failed (error {code})"))
        }
    }

    /// Identity matrix, then release the API. Safe to call repeatedly.
    pub fn reset(&mut self) {
        if self.initialized {
            set_matrix(level::color_matrix(0));
            // SAFETY: balances the successful MagInitialize in `apply`.
            unsafe {
                MagUninitialize();
            }
            self.initialized = false;
        }
    }
}

impl Drop for Magnifier {
    fn drop(&mut self) {
        self.reset();
    }
}

/// Standalone recovery (`--reset-screen`): initialise, apply the identity
/// matrix, release. Returns whether the matrix could be set. Best effort when
/// another NightSleep process holds the effect.
pub fn force_reset() -> bool {
    // SAFETY: plain FFI calls; uninitialise only if we initialised.
    unsafe {
        let initialized = MagInitialize() != 0;
        let ok = set_matrix(level::color_matrix(0));
        if initialized {
            MagUninitialize();
        }
        ok
    }
}

/// Used by the panic hook: restores the screen if this process dimmed it.
pub fn emergency_reset() {
    if USED.load(Ordering::Relaxed) {
        force_reset();
    }
}
