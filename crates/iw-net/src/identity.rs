//! The device's long-term TLS identity: a self-signed certificate whose
//! fingerprint paired devices pin.
//!
//! Stored as DER files, with the private key readable only by its owner.
//! This is interim: the plan keeps the key in the macOS Keychain, or protects
//! it with DPAPI on Windows.

use std::io::{self, Write};
use std::path::Path;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::Fingerprint;

/// The files [`Identity::load_or_generate`] keeps in its folder: the
/// certificate, and the private key.
pub const CERT_FILE: &str = "identity.cert.der";
pub const KEY_FILE: &str = "identity.key.der";

#[derive(Debug)]
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    fingerprint: Fingerprint,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("could not generate a certificate: {0}")]
    Generate(#[from] rcgen::Error),
}

impl Identity {
    /// A new ECDSA P-256 key and self-signed certificate.
    pub fn generate() -> Result<Self, IdentityError> {
        let key = rcgen::KeyPair::generate()?;
        let cert =
            rcgen::CertificateParams::new(vec!["inputweave".to_owned()])?.self_signed(&key)?;
        Ok(Self::from_der(
            cert.der().clone(),
            PrivatePkcs8KeyDer::from(key.serialize_der()),
        ))
    }

    /// Loads the identity stored in `dir`, creating one on first use.
    pub fn load_or_generate(dir: &Path) -> Result<Self, IdentityError> {
        let (cert_path, key_path) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
        if cert_path.exists() && key_path.exists() {
            return Ok(Self::from_der(
                std::fs::read(cert_path)?.into(),
                std::fs::read(key_path)?.into(),
            ));
        }
        std::fs::create_dir_all(dir)?;
        let identity = Self::generate()?;
        write_private(&key_path, identity.key.secret_pkcs8_der())?;
        std::fs::write(&cert_path, identity.cert.as_ref())?;
        Ok(identity)
    }

    /// The identity whose certificate and PKCS #8 private key are `cert`
    /// and `key`, as [`CERT_FILE`] and [`KEY_FILE`] hold them, for a caller
    /// that reads them itself. Whether they belong together shows when an
    /// endpoint is bound with it.
    pub fn from_files(cert: Vec<u8>, key: Vec<u8>) -> Self {
        Self::from_der(cert.into(), key.into())
    }

    fn from_der(cert: CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static>) -> Self {
        let fingerprint = Fingerprint::of_certificate(&cert);
        Self {
            cert,
            key,
            fingerprint,
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub(crate) fn cert(&self) -> CertificateDer<'static> {
        self.cert.clone()
    }

    pub(crate) fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_read_elsewhere_are_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_generate(dir.path()).unwrap();
        let read = Identity::from_files(
            std::fs::read(dir.path().join(CERT_FILE)).unwrap(),
            std::fs::read(dir.path().join(KEY_FILE)).unwrap(),
        );
        assert_eq!(first.fingerprint(), read.fingerprint());
    }

    #[test]
    fn persists_and_reloads_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_generate(dir.path()).unwrap();
        let again = Identity::load_or_generate(dir.path()).unwrap();
        assert_eq!(first.fingerprint(), again.fingerprint());
        assert_ne!(
            first.fingerprint(),
            Identity::generate().unwrap().fingerprint()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
