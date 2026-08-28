//! Robot identity: an Ed25519 key pair stored as a 32-byte seed file.
//!
//! Created on first run (mode 0600 on Unix). The public key is what the cloud
//! pins to the `robot_id`; the private key never leaves the robot.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use std::path::Path;

#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut rand::rngs::OsRng),
        }
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// Load the seed from `path`, creating a new identity there if absent.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&bytes);
                Ok(Self::from_seed(seed))
            }
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: expected a 32-byte Ed25519 seed", path.display()),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate();
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, id.signing.to_bytes())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                }
                tracing::info!(path = %path.display(), "created new robot identity");
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn public_key_b64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.verifying_key().to_bytes())
    }

    pub fn sign_b64(&self, msg: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.signing.sign(msg).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn signature_verifies_with_public_key() {
        let id = Identity::generate();
        let sig = id.sign_b64(b"nonce");
        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(sig)
            .unwrap();
        let sig = ed25519_dalek::Signature::from_slice(&sig_bytes).unwrap();
        assert!(id.verifying_key().verify(b"nonce", &sig).is_ok());
    }

    #[test]
    fn load_or_create_roundtrip() {
        let dir = std::env::temp_dir().join(format!("seyd-id-{}", std::process::id()));
        let path = dir.join("robot.key");
        let a = Identity::load_or_create(&path).unwrap();
        let b = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.public_key_b64(), b.public_key_b64());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
