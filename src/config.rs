//! Configuration: a tiny hand-written `key=value` format (no serde).
//!
//! The parsing/serialising part is pure and unit-tested; file I/O is at the bottom.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::hotkey::{self, Hotkey};
use crate::level::{self, Level};

pub const FILE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Auto,
    Ru,
    En,
}

impl Language {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ru" => Some(Self::Ru),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ru => "ru",
            Self::En => "en",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkeys {
    pub dim_more: Hotkey,
    pub dim_less: Hotkey,
    pub toggle: Hotkey,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            dim_more: hotkey::DIM_MORE,
            dim_less: hotkey::DIM_LESS,
            toggle: hotkey::TOGGLE,
        }
    }
}

impl Hotkeys {
    /// Falls back to the defaults if the set is unusable: invalid keys,
    /// duplicates, or a clash with the fixed emergency hotkey.
    fn sanitized(self) -> Self {
        let all = [self.dim_more, self.dim_less, self.toggle];
        let bad = all.iter().any(|h| !h.is_valid() || *h == hotkey::EMERGENCY)
            || all[0] == all[1]
            || all[0] == all[2]
            || all[1] == all[2];
        if bad {
            Self::default()
        } else {
            self
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub level: Level,
    pub restore_on_startup: bool,
    pub language: Language,
    pub hotkeys: Hotkeys,
    /// The file was written by a newer version: never overwrite it.
    pub preserve_file: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            level: Level::default(),
            restore_on_startup: true,
            language: Language::Auto,
            hotkeys: Hotkeys::default(),
            preserve_file: false,
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Lenient parser: every bad or missing field falls back to its default,
/// unknown keys and comments are ignored, later duplicates win.
pub fn parse(text: &str) -> Config {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut version: Option<u32> = None;
    let mut current: Option<u8> = None;
    let mut last_nonzero: Option<u8> = None;
    let mut step: Option<u8> = None;
    let mut max: Option<u8> = None;
    let mut restore = None;
    let mut language = None;
    let mut hk_more = None;
    let mut hk_less = None;
    let mut hk_toggle = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "version" => version = value.parse().ok(),
            "level" => current = value.parse().ok(),
            "last_nonzero_level" => last_nonzero = value.parse().ok(),
            "step" => step = value.parse().ok(),
            "max_level" => max = value.parse().ok(),
            "restore_on_startup" => restore = parse_bool(value),
            "language" => language = Language::parse(value),
            "hotkey_dim_more" => hk_more = Hotkey::parse(value),
            "hotkey_dim_less" => hk_less = Hotkey::parse(value),
            "hotkey_toggle" => hk_toggle = Hotkey::parse(value),
            _ => {}
        }
    }

    let defaults = Config::default();
    Config {
        level: Level::new(
            current.unwrap_or(0),
            last_nonzero.unwrap_or(level::DEFAULT_RESTORE),
            step.unwrap_or(level::DEFAULT_STEP),
            max.unwrap_or(level::DEFAULT_MAX),
        ),
        restore_on_startup: restore.unwrap_or(defaults.restore_on_startup),
        language: language.unwrap_or(defaults.language),
        hotkeys: Hotkeys {
            dim_more: hk_more.unwrap_or(defaults.hotkeys.dim_more),
            dim_less: hk_less.unwrap_or(defaults.hotkeys.dim_less),
            toggle: hk_toggle.unwrap_or(defaults.hotkeys.toggle),
        }
        .sanitized(),
        preserve_file: version.is_some_and(|v| v > FILE_VERSION),
    }
}

pub fn serialize(cfg: &Config) -> String {
    format!(
        "; NightSleep-Monitor configuration\n\
         version={FILE_VERSION}\n\
         level={}\n\
         last_nonzero_level={}\n\
         step={}\n\
         max_level={}\n\
         restore_on_startup={}\n\
         language={}\n\
         hotkey_dim_more={}\n\
         hotkey_dim_less={}\n\
         hotkey_toggle={}\n",
        cfg.level.current(),
        cfg.level.last_nonzero(),
        cfg.level.step(),
        cfg.level.max(),
        u8::from(cfg.restore_on_startup),
        cfg.language.as_str(),
        cfg.hotkeys.dim_more,
        cfg.hotkeys.dim_less,
        cfg.hotkeys.toggle,
    )
}

// ---------------------------------------------------------------- file I/O

/// `%APPDATA%\NightSleep-Monitor`
pub fn app_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("NightSleep-Monitor"))
}

fn config_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("config.ini"))
}

pub struct Loaded {
    pub config: Config,
    pub first_run: bool,
    pub warning: Option<String>,
}

pub fn load() -> Loaded {
    let mut loaded = Loaded {
        config: Config::default(),
        first_run: false,
        warning: None,
    };
    let Some(path) = config_path() else {
        loaded.warning = Some("APPDATA is not set; settings will not be saved".into());
        return loaded;
    };
    match fs::read(&path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => loaded.config = parse(&text),
            Err(_) => {
                let backup = path.with_extension("ini.bak");
                let moved = fs::rename(&path, &backup).is_ok();
                loaded.warning = Some(if moved {
                    "config.ini is not valid UTF-8; moved to config.ini.bak, using defaults".into()
                } else {
                    "config.ini is not valid UTF-8; using defaults".into()
                });
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => loaded.first_run = true,
        Err(e) => loaded.warning = Some(format!("cannot read config.ini: {e}")),
    }
    loaded
}

/// Atomic save: write a temporary file, then rename over the target
/// (`std::fs::rename` replaces an existing file on Windows).
pub fn save(cfg: &Config) -> io::Result<()> {
    if cfg.preserve_file {
        return Ok(());
    }
    let path = config_path().ok_or_else(|| io::Error::other("APPDATA is not set"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("ini.tmp");
    fs::write(&tmp, serialize(cfg))?;
    fs::rename(&tmp, &path)
}

/// `--cleanup`: delete `%APPDATA%\NightSleep-Monitor` entirely.
pub fn remove_all() -> io::Result<()> {
    match app_dir() {
        Some(dir) => match fs::remove_dir_all(dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_gives_defaults() {
        assert_eq!(parse(""), Config::default());
    }

    #[test]
    fn parses_a_full_file() {
        let cfg = parse(
            "; comment\n\
             version=1\n\
             level=40\n\
             last_nonzero_level=40\n\
             step=5\n\
             max_level=100\n\
             restore_on_startup=0\n\
             language=ru\n\
             hotkey_dim_more=Ctrl+Alt+Down\n\
             hotkey_dim_less=Ctrl+Alt+Up\n\
             hotkey_toggle=Ctrl+Alt+Home\n",
        );
        assert_eq!(cfg.level.current(), 40);
        assert_eq!(cfg.level.step(), 5);
        assert_eq!(cfg.level.max(), 100);
        assert!(!cfg.restore_on_startup);
        assert_eq!(cfg.language, Language::Ru);
        assert_eq!(cfg.hotkeys.dim_more.to_string(), "Ctrl+Alt+Down");
        assert!(!cfg.preserve_file);
    }

    #[test]
    fn round_trip() {
        let cfg = Config {
            level: Level::new(60, 60, 15, 95),
            language: Language::En,
            restore_on_startup: false,
            ..Config::default()
        };
        assert_eq!(parse(&serialize(&cfg)), cfg);
        assert_eq!(parse(&serialize(&Config::default())), Config::default());
    }

    #[test]
    fn bad_values_fall_back_per_field() {
        let cfg = parse("level=abc\nstep=7\nmax_level=500\nlanguage=xx\nhotkey_toggle=Shift+A\nrestore_on_startup=maybe\n");
        let d = Config::default();
        assert_eq!(cfg.level, d.level);
        assert_eq!(cfg.language, d.language);
        assert_eq!(cfg.hotkeys.toggle, d.hotkeys.toggle);
        assert_eq!(cfg.restore_on_startup, d.restore_on_startup);
    }

    #[test]
    fn duplicate_hotkeys_reset_to_defaults() {
        let cfg = parse(
            "hotkey_dim_more=Ctrl+Alt+A\nhotkey_dim_less=Ctrl+Alt+A\nhotkey_toggle=Ctrl+Alt+B\n",
        );
        assert_eq!(cfg.hotkeys, Hotkeys::default());
    }

    #[test]
    fn emergency_hotkey_cannot_be_reused() {
        let cfg = parse("hotkey_toggle=Ctrl+Alt+Shift+End\n");
        assert_eq!(cfg.hotkeys, Hotkeys::default());
    }

    #[test]
    fn bom_unknown_keys_and_duplicates() {
        let cfg = parse("\u{feff}level=10\nfoo=bar\nlevel=20\n# note\nnot a pair\n");
        assert_eq!(cfg.level.current(), 20);
    }

    #[test]
    fn newer_version_is_preserved() {
        let cfg = parse("version=2\nlevel=10\n");
        assert!(cfg.preserve_file);
        assert_eq!(cfg.level.current(), 10);
    }

    #[test]
    fn level_above_max_is_clamped() {
        let cfg = parse("level=100\nmax_level=90\n");
        assert_eq!(cfg.level.current(), 90);
    }
}
