//! Certificate fingerprints: the identity that paired devices pin.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

/// SHA-256 of a device certificate's DER encoding.
///
/// Displayed as 32 lowercase hex byte pairs joined by colons.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of_certificate(der: &[u8]) -> Self {
        let digest = Sha256::digest(der);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("expected 32 colon-separated hex bytes")]
pub struct ParseFingerprintError;

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; 32];
        let mut parts = s.split(':');
        for byte in &mut bytes {
            let part = parts.next().ok_or(ParseFingerprintError)?;
            if part.len() != 2 || !part.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(ParseFingerprintError);
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| ParseFingerprintError)?;
        }
        if parts.next().is_some() {
            return Err(ParseFingerprintError);
        }
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SHA-256("abc") from FIPS 180-2.
    const ABC: &str = "ba:78:16:bf:8f:01:cf:ea:41:41:40:de:5d:ae:22:23:\
                       b0:03:61:a3:96:17:7a:9c:b4:10:ff:61:f2:00:15:ad";

    #[test]
    fn hashes_and_formats() {
        assert_eq!(Fingerprint::of_certificate(b"abc").to_string(), ABC);
    }

    #[test]
    fn parses_what_it_formats_case_insensitively() {
        let fp = Fingerprint::of_certificate(b"abc");
        assert_eq!(fp.to_string().parse::<Fingerprint>(), Ok(fp));
        assert_eq!(fp.to_string().to_uppercase().parse::<Fingerprint>(), Ok(fp));
    }

    #[test]
    fn rejects_malformed_input() {
        let fp = Fingerprint::of_certificate(b"abc").to_string();
        assert_eq!("".parse::<Fingerprint>(), Err(ParseFingerprintError));
        assert_eq!(fp[3..].parse::<Fingerprint>(), Err(ParseFingerprintError));
        assert_eq!(
            format!("{fp}:00").parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
        assert_eq!(
            fp.replace(':', "").parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
        assert_eq!(
            fp.replacen("ba", "zz", 1).parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
        assert_eq!(
            fp.replacen("ba", "+a", 1).parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
    }
}
