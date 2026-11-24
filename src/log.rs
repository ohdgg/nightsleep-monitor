//! Minimal file logger: `%APPDATA%\NightSleep\log.txt`, one rotation to
//! `log.old.txt` at 256 KiB. The file is opened per write, so no handle is
//! kept and nothing here can ever bring the app down.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config;

const MAX_SIZE: u64 = 256 * 1024;

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn warn(msg: &str) {
    write("WARN", msg);
}

pub fn error(msg: &str) {
    write("ERROR", msg);
}

fn write(level: &str, msg: &str) {
    let Some(dir) = config::app_dir() else {
        return;
    };
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("log.txt");
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        // rename replaces an existing log.old.txt on Windows
        let _ = fs::rename(&path, dir.join("log.old.txt"));
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} UTC [{level}] {msg}", format_utc(secs));
    }
}

/// `YYYY-MM-DD HH:MM:SS` for a Unix timestamp (civil-from-days algorithm).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn known_timestamps() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20");
    }
}
