//! The SHA-256 of the agent's own executable, which tells builds of the
//! same version apart. The macOS app compares it with the digest of the
//! agent it carries (`InputWeaveAgentDigest` in its LaunchAgent plist,
//! written by apps/macos/scripts/embed-agent.sh with `shasum -a 256`) and
//! restarts a service still running another build, as after an update.

use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// The digest of the running executable, in lowercase hex.
pub fn executable_digest() -> io::Result<String> {
    file_digest(&std::env::current_exe()?)
}

/// The SHA-256 of a file's contents, in lowercase hex, as `shasum -a 256`
/// prints it.
fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(crate::hex::hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_are_sha256_in_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent");
        std::fs::write(&path, "abc").unwrap();
        // What `printf abc | shasum -a 256` prints (FIPS 180-2's example).
        assert_eq!(
            file_digest(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Longer than one read.
        let long = vec![b'a'; 1_000_000];
        std::fs::write(&path, &long).unwrap();
        assert_eq!(
            file_digest(&path).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        std::fs::write(&path, "").unwrap();
        assert_eq!(
            file_digest(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_running_executable_has_a_digest() {
        let digest = executable_digest().unwrap();
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        // Where shasum is installed, it agrees.
        let exe = std::env::current_exe().unwrap();
        if let Ok(output) = std::process::Command::new("shasum")
            .args(["-a", "256"])
            .arg(&exe)
            .output()
            && output.status.success()
        {
            let printed = String::from_utf8(output.stdout).unwrap();
            assert_eq!(printed.split_whitespace().next(), Some(digest.as_str()));
        }
    }
}
