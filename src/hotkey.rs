//! Hotkey description, parsing and formatting. Pure logic, no Win32 calls.
//!
//! Virtual-key codes are plain numbers here so this module stays testable
//! everywhere; `app.rs` converts modifiers to `MOD_*` flags.

use std::fmt;

pub const CTRL: u32 = 1;
pub const ALT: u32 = 2;
pub const SHIFT: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

pub const DIM_MORE: Hotkey = Hotkey {
    mods: CTRL | ALT,
    vk: 0x22, // PageDown
};
pub const DIM_LESS: Hotkey = Hotkey {
    mods: CTRL | ALT,
    vk: 0x21, // PageUp
};
pub const TOGGLE: Hotkey = Hotkey {
    mods: CTRL | ALT,
    vk: 0x23, // End
};
/// Fixed, not configurable: always drops the dimming to 0%.
pub const EMERGENCY: Hotkey = Hotkey {
    mods: CTRL | ALT | SHIFT,
    vk: 0x23, // End
};

const NAMED_KEYS: &[(&str, u32)] = &[
    ("BACKSPACE", 0x08),
    ("TAB", 0x09),
    ("ENTER", 0x0D),
    ("SPACE", 0x20),
    ("PAGEUP", 0x21),
    ("PAGEDOWN", 0x22),
    ("END", 0x23),
    ("HOME", 0x24),
    ("LEFT", 0x25),
    ("UP", 0x26),
    ("RIGHT", 0x27),
    ("DOWN", 0x28),
    ("INSERT", 0x2D),
    ("DELETE", 0x2E),
    ("PLUS", 0xBB),
    ("COMMA", 0xBC),
    ("MINUS", 0xBD),
    ("PERIOD", 0xBE),
];

/// Pure modifier keys can never be the "main" key of a hotkey.
fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

impl Hotkey {
    /// A usable hotkey needs Ctrl or Alt (Shift alone would hijack typing)
    /// and a real, non-modifier key.
    pub fn is_valid(&self) -> bool {
        self.mods & (CTRL | ALT) != 0
            && self.mods & !(CTRL | ALT | SHIFT) == 0
            && (1..=0xFE).contains(&self.vk)
            && !is_modifier_vk(self.vk)
    }

    /// Parses e.g. `Ctrl+Alt+PageDown`. Case-insensitive.
    pub fn parse(text: &str) -> Option<Hotkey> {
        let mut mods = 0;
        let mut vk = None;
        for token in text.split('+') {
            let token = token.trim();
            if token.is_empty() {
                return None;
            }
            match token.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" => mods |= CTRL,
                "ALT" => mods |= ALT,
                "SHIFT" => mods |= SHIFT,
                other => {
                    if vk.is_some() {
                        return None;
                    }
                    vk = Some(vk_from_name(other)?);
                }
            }
        }
        let hotkey = Hotkey { mods, vk: vk? };
        hotkey.is_valid().then_some(hotkey)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods & CTRL != 0 {
            f.write_str("Ctrl+")?;
        }
        if self.mods & ALT != 0 {
            f.write_str("Alt+")?;
        }
        if self.mods & SHIFT != 0 {
            f.write_str("Shift+")?;
        }
        f.write_str(&vk_name(self.vk))
    }
}

/// Human-readable name of a virtual-key code (config format).
pub fn vk_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("Num{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => {
            if let Some((name, _)) = NAMED_KEYS.iter().find(|(_, code)| *code == vk) {
                // Stored in upper case; present as "PageDown", "End", ...
                return pretty(name);
            }
            format!("0x{vk:02X}")
        }
    }
}

fn pretty(upper: &str) -> String {
    match upper {
        "PAGEUP" => "PageUp".into(),
        "PAGEDOWN" => "PageDown".into(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_string() + &chars.as_str().to_ascii_lowercase(),
                None => String::new(),
            }
        }
    }
}

/// Inverse of [`vk_name`]. Expects upper-case input.
fn vk_from_name(upper: &str) -> Option<u32> {
    let bytes = upper.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
        return Some(u32::from(bytes[0]));
    }
    if let Some(rest) = upper.strip_prefix("NUM") {
        if let Ok(n) = rest.parse::<u32>() {
            if n <= 9 {
                return Some(0x60 + n);
            }
        }
    }
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }
    if let Some(hex) = upper.strip_prefix("0X") {
        return u32::from_str_radix(hex, 16).ok().filter(|v| *v <= 0xFE);
    }
    NAMED_KEYS
        .iter()
        .find(|(name, _)| *name == upper)
        .map(|(_, code)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_distinct() {
        for h in [DIM_MORE, DIM_LESS, TOGGLE, EMERGENCY] {
            assert!(h.is_valid(), "{h} must be valid");
        }
        assert_ne!(DIM_MORE, DIM_LESS);
        assert_ne!(TOGGLE, EMERGENCY);
    }

    #[test]
    fn display_of_defaults() {
        assert_eq!(DIM_MORE.to_string(), "Ctrl+Alt+PageDown");
        assert_eq!(DIM_LESS.to_string(), "Ctrl+Alt+PageUp");
        assert_eq!(TOGGLE.to_string(), "Ctrl+Alt+End");
        assert_eq!(EMERGENCY.to_string(), "Ctrl+Alt+Shift+End");
    }

    #[test]
    fn parse_round_trip() {
        for h in [DIM_MORE, DIM_LESS, TOGGLE, EMERGENCY] {
            assert_eq!(Hotkey::parse(&h.to_string()), Some(h));
        }
        for text in [
            "Ctrl+Alt+A",
            "Alt+F12",
            "Ctrl+Shift+Num5",
            "Ctrl+Minus",
            "Ctrl+Alt+7",
        ] {
            let h = Hotkey::parse(text).expect(text);
            assert_eq!(h.to_string(), text);
        }
    }

    #[test]
    fn parse_is_case_and_space_insensitive() {
        assert_eq!(Hotkey::parse(" ctrl + ALT + pagedown "), Some(DIM_MORE));
    }

    #[test]
    fn parse_rejects_invalid() {
        for text in [
            "",
            "PageDown",
            "Shift+A",
            "Shift+PageDown",
            "Ctrl+",
            "Ctrl+Alt",
            "Ctrl+A+B",
            "Ctrl+Nonsense",
            "Win+A",
            "Ctrl+0x00",
            "Ctrl+0xFF",
            "Ctrl+Shift",
        ] {
            assert_eq!(Hotkey::parse(text), None, "{text:?} must be rejected");
        }
    }

    #[test]
    fn unknown_vk_uses_hex() {
        let h = Hotkey {
            mods: CTRL,
            vk: 0xC0,
        };
        assert_eq!(h.to_string(), "Ctrl+0xC0");
        assert_eq!(Hotkey::parse("Ctrl+0xC0"), Some(h));
    }

    #[test]
    fn function_and_numpad_keys() {
        assert_eq!(vk_name(0x70), "F1");
        assert_eq!(vk_name(0x87), "F24");
        assert_eq!(vk_name(0x60), "Num0");
        assert_eq!(vk_from_name("F24"), Some(0x87));
        assert_eq!(vk_from_name("F25"), None);
    }
}
