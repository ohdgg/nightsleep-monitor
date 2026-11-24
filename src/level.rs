//! Pure dimming-level logic. No Win32 here, so it is fully unit-testable.

pub const DEFAULT_MAX: u8 = 90;
pub const MIN_MAX: u8 = 50;
pub const ABS_MAX: u8 = 100;
pub const DEFAULT_STEP: u8 = 10;
pub const STEPS: [u8; 4] = [5, 10, 15, 20];
/// Level used by "toggle on" when nothing has been remembered yet.
pub const DEFAULT_RESTORE: u8 = 30;
/// Distance between presets in the tray menu.
pub const PRESET_STEP: u8 = 10;

/// Window opacity (0..=255) for a dimming level in percent.
pub fn alpha(level: u8) -> u8 {
    ((u32::from(level.min(100)) * 255 + 50) / 100) as u8
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    current: u8,
    last_nonzero: u8,
    step: u8,
    max: u8,
}

impl Default for Level {
    fn default() -> Self {
        Self::new(0, DEFAULT_RESTORE, DEFAULT_STEP, DEFAULT_MAX)
    }
}

impl Level {
    /// Builds a level state, replacing invalid values with safe ones.
    pub fn new(current: u8, last_nonzero: u8, step: u8, max: u8) -> Self {
        let max = max.clamp(MIN_MAX, ABS_MAX);
        let step = if STEPS.contains(&step) {
            step
        } else {
            DEFAULT_STEP
        };
        let last_nonzero = if (1..=max).contains(&last_nonzero) {
            last_nonzero
        } else {
            DEFAULT_RESTORE.min(max)
        };
        Self {
            current: current.min(max),
            last_nonzero,
            step,
            max,
        }
    }

    pub fn current(&self) -> u8 {
        self.current
    }

    pub fn last_nonzero(&self) -> u8 {
        self.last_nonzero
    }

    pub fn step(&self) -> u8 {
        self.step
    }

    pub fn max(&self) -> u8 {
        self.max
    }

    pub fn is_on(&self) -> bool {
        self.current > 0
    }

    /// Sets the level (clamped to `max`). Non-zero values are remembered.
    pub fn set(&mut self, value: u8) {
        self.current = value.min(self.max);
        if self.current > 0 {
            self.last_nonzero = self.current;
        }
    }

    pub fn dim_more(&mut self) {
        self.set(self.current.saturating_add(self.step));
    }

    pub fn dim_less(&mut self) {
        self.set(self.current.saturating_sub(self.step));
    }

    /// 0% <-> last non-zero level.
    pub fn toggle(&mut self) {
        if self.is_on() {
            self.set(0);
        } else {
            self.set(self.last_nonzero);
        }
    }

    // Used by the settings dialog (stage 3).
    #[allow(dead_code)]
    pub fn set_max(&mut self, max: u8) {
        self.max = max.clamp(MIN_MAX, ABS_MAX);
        self.current = self.current.min(self.max);
        self.last_nonzero = self.last_nonzero.min(self.max).max(1);
    }

    // Used by the settings dialog (stage 3).
    #[allow(dead_code)]
    pub fn set_step(&mut self, step: u8) {
        if STEPS.contains(&step) {
            self.step = step;
        }
    }

    /// Tray menu presets: 0, 10, 20, ..., up to `max`.
    pub fn presets(&self) -> impl Iterator<Item = u8> {
        (0..=self.max).step_by(PRESET_STEP as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_mapping() {
        assert_eq!(alpha(0), 0);
        assert_eq!(alpha(50), 128);
        assert_eq!(alpha(90), 230);
        assert_eq!(alpha(100), 255);
        assert_eq!(alpha(200), 255);
    }

    #[test]
    fn alpha_is_monotonic() {
        let mut prev = 0;
        for l in 0..=100u8 {
            let a = alpha(l);
            assert!(a >= prev);
            prev = a;
        }
    }

    #[test]
    fn dim_more_and_less_use_step_and_clamp() {
        let mut l = Level::default();
        l.dim_more();
        assert_eq!(l.current(), 10);
        for _ in 0..20 {
            l.dim_more();
        }
        assert_eq!(l.current(), 90);
        for _ in 0..20 {
            l.dim_less();
        }
        assert_eq!(l.current(), 0);
    }

    #[test]
    fn step_15_does_not_snap_to_presets() {
        let mut l = Level::new(0, 30, 15, 90);
        l.dim_more();
        l.dim_more();
        assert_eq!(l.current(), 30);
        l.dim_more();
        assert_eq!(l.current(), 45);
        l.dim_less();
        l.dim_less();
        l.dim_less();
        assert_eq!(l.current(), 0);
    }

    #[test]
    fn step_not_dividing_max_ends_at_max() {
        let mut l = Level::new(75, 75, 20, 90);
        l.dim_more();
        assert_eq!(l.current(), 90);
    }

    #[test]
    fn toggle_remembers_last_nonzero() {
        let mut l = Level::new(40, 40, 10, 90);
        l.toggle();
        assert_eq!(l.current(), 0);
        assert_eq!(l.last_nonzero(), 40);
        l.toggle();
        assert_eq!(l.current(), 40);
    }

    #[test]
    fn toggle_from_fresh_state_uses_default() {
        let mut l = Level::default();
        l.toggle();
        assert_eq!(l.current(), DEFAULT_RESTORE);
    }

    #[test]
    fn invalid_values_are_sanitized() {
        let l = Level::new(250, 0, 7, 10);
        assert_eq!(l.max(), MIN_MAX);
        assert_eq!(l.step(), DEFAULT_STEP);
        assert_eq!(l.current(), MIN_MAX);
        assert_eq!(l.last_nonzero(), DEFAULT_RESTORE.min(MIN_MAX));
    }

    #[test]
    fn lowering_max_clamps_current() {
        let mut l = Level::new(90, 90, 10, 90);
        l.set_max(60);
        assert_eq!(l.current(), 60);
        assert_eq!(l.last_nonzero(), 60);
    }

    #[test]
    fn dim_less_at_zero_keeps_last_nonzero() {
        let mut l = Level::new(0, 40, 10, 90);
        l.dim_less();
        assert_eq!(l.current(), 0);
        assert_eq!(l.last_nonzero(), 40);
    }

    #[test]
    fn presets_follow_max() {
        let l = Level::new(0, 30, 10, 90);
        assert_eq!(l.presets().count(), 10);
        let l = Level::new(0, 30, 10, 100);
        assert_eq!(l.presets().count(), 11);
        assert_eq!(l.presets().last(), Some(100));
    }

    #[test]
    fn set_step_ignores_invalid() {
        let mut l = Level::default();
        l.set_step(13);
        assert_eq!(l.step(), 10);
        l.set_step(5);
        assert_eq!(l.step(), 5);
    }
}
