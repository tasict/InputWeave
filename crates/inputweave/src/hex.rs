//! Lowercase hex, for digests and ids written into files and file names.

use std::fmt::Write as _;

/// `bytes` in lowercase hex, two digits each.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The bytes `text` holds in hex (either case); none when it is not an even
/// number of hex digits.
pub(crate) fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digit = |c: u8| (c as char).to_digit(16);
            Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_go_to_hex_and_back() {
        assert_eq!(hex(&[0x00, 0x7f, 0xff]), "007fff");
        assert_eq!(hex(&[]), "");
        assert_eq!(from_hex("007fFF"), Some(vec![0x00, 0x7f, 0xff]));
        assert_eq!(from_hex(""), Some(vec![]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex("+1"), None);
    }
}
