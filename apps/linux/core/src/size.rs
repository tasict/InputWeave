//! File sizes as GNOME writes them (GLib's `g_format_size`): decimal
//! units, 1000 bytes to the kB, one decimal place; bytes in words below
//! that. Each app follows its platform: Windows writes them as File
//! Explorer does, macOS as Finder does.

use crate::text::{UiText, keys};
use crate::ui;

const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];

pub fn format(bytes: u64) -> UiText {
    if bytes == 1 {
        return ui!(keys::SIZE_BYTE);
    }
    if bytes < 1000 {
        return ui!(keys::SIZE_BYTES, bytes.to_string());
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    UiText::verbatim(format!("{value:.1} {}", UNITS[unit]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_gnome_writes_them() {
        assert_eq!(format(0).to_string(), "0 bytes");
        assert_eq!(format(1).to_string(), "1 byte");
        assert_eq!(format(999).to_string(), "999 bytes");
        assert_eq!(format(1000).to_string(), "1.0 kB");
        assert_eq!(format(1234).to_string(), "1.2 kB");
        assert_eq!(format(2_340_000).to_string(), "2.3 MB");
        // Rounds as printf does, and moves up a unit rather than say 1000.0.
        assert_eq!(format(999_960).to_string(), "1.0 MB");
        assert_eq!(format(1_200_000_000).to_string(), "1.2 GB");
        assert_eq!(format(4_294_967_296).to_string(), "4.3 GB");
    }
}
