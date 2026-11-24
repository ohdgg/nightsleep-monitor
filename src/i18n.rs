//! UI strings (ru/en) as static tables: no external files, no allocations
//! unless text has to be assembled.

use crate::config::Language;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

/// `Auto` follows the system UI language.
pub fn resolve(setting: Language, system_is_russian: bool) -> Lang {
    match setting {
        Language::Ru => Lang::Ru,
        Language::En => Lang::En,
        Language::Auto => {
            if system_is_russian {
                Lang::Ru
            } else {
                Lang::En
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum S {
    MenuHeader,
    MenuOff,
    MenuAutostart,
    MenuAbout,
    MenuExit,
    AboutDescription,
    AlreadyRunning,
    HotkeysFailed,
}

pub fn tr(lang: Lang, s: S) -> &'static str {
    match (lang, s) {
        (_, S::MenuHeader) => "NightSleep",
        (Lang::Ru, S::MenuOff) => "0%  (выкл)",
        (Lang::En, S::MenuOff) => "0%  (off)",
        (Lang::Ru, S::MenuAutostart) => "Запускать вместе с Windows",
        (Lang::En, S::MenuAutostart) => "Start with Windows",
        (Lang::Ru, S::MenuAbout) => "О программе",
        (Lang::En, S::MenuAbout) => "About",
        (Lang::Ru, S::MenuExit) => "Выход",
        (Lang::En, S::MenuExit) => "Exit",
        (Lang::Ru, S::AboutDescription) => "Утилита для глубокого затемнения экрана.",
        (Lang::En, S::AboutDescription) => "Utility for deep screen dimming.",
        (Lang::Ru, S::AlreadyRunning) => "NightSleep уже запущен.",
        (Lang::En, S::AlreadyRunning) => "NightSleep is already running.",
        (Lang::Ru, S::HotkeysFailed) => {
            "Не удалось зарегистрировать горячие клавиши (заняты другим приложением): "
        }
        (Lang::En, S::HotkeysFailed) => "Could not register hotkeys (already in use): ",
    }
}

/// First-run balloon text listing the hotkeys.
pub fn first_run(lang: Lang, more: &str, less: &str, toggle: &str, emergency: &str) -> String {
    match lang {
        Lang::Ru => format!(
            "{more} — темнее, {less} — светлее, {toggle} — вкл/выкл. Аварийный сброс: {emergency}."
        ),
        Lang::En => format!(
            "{more} — darker, {less} — lighter, {toggle} — on/off. Emergency reset: {emergency}."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_system() {
        assert_eq!(resolve(Language::Auto, true), Lang::Ru);
        assert_eq!(resolve(Language::Auto, false), Lang::En);
        assert_eq!(resolve(Language::En, true), Lang::En);
        assert_eq!(resolve(Language::Ru, false), Lang::Ru);
    }

    #[test]
    fn first_run_mentions_all_hotkeys() {
        for lang in [Lang::Ru, Lang::En] {
            let text = first_run(lang, "A", "B", "C", "D");
            for k in ["A", "B", "C", "D"] {
                assert!(text.contains(k));
            }
        }
    }
}
