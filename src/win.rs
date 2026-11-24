//! Tiny helpers shared by the Win32 modules.

/// UTF-16, NUL-terminated.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed UTF-16 buffer, truncating if needed and always
/// leaving it NUL-terminated (for NOTIFYICONDATAW text fields).
pub fn copy_wide(dst: &mut [u16], s: &str) {
    if dst.is_empty() {
        return;
    }
    let mut n = 0;
    for unit in s.encode_utf16().take(dst.len() - 1) {
        dst[n] = unit;
        n += 1;
    }
    dst[n] = 0;
}

pub fn loword(v: isize) -> u32 {
    (v as usize & 0xFFFF) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("ab"), vec![97, 98, 0]);
        assert_eq!(wide(""), vec![0]);
    }

    #[test]
    fn copy_wide_truncates_and_terminates() {
        let mut buf = [0xFFFFu16; 4];
        copy_wide(&mut buf, "abcdef");
        assert_eq!(buf, [97, 98, 99, 0]);
        let mut buf = [0xFFFFu16; 4];
        copy_wide(&mut buf, "a");
        assert_eq!(&buf[..2], &[97, 0]);
    }

    #[test]
    fn loword_masks() {
        assert_eq!(loword(0x1234_5678), 0x5678);
    }
}
