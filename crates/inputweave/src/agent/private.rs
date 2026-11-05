//! Folders only this user can open, for what the agent keeps of other
//! devices: their desktop pictures, files pasted from their clipboards.

use std::path::Path;

/// Creates `dir`, and the folders it is in, if missing; `dir` itself is
/// made, or made again, only this user's to open.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir)?;
        // One made before, or by someone else's umask, is tightened.
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    builder.create(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn folders_are_made_and_kept_private() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a/b");
        private_dir(&dir).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }
}
