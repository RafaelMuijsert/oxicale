use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("secret key error: {0}")]
    Key(String),
    #[error("invalid encrypted secret")]
    InvalidSecret,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct SecretBox {
    cipher: XChaCha20Poly1305,
    key_id: String,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBox")
            .field("key_id", &self.key_id)
            .finish()
    }
}

impl SecretBox {
    pub fn load(
        environment: Option<&str>,
        key_file: &Path,
    ) -> Result<(Self, Option<String>), CryptoError> {
        let (key, warning) = if let Some(encoded) = environment.filter(|v| !v.trim().is_empty()) {
            (decode_key(encoded)?, None)
        } else if key_file.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(key_file)?.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(CryptoError::Key(
                        "secret key file must be mode 0600".to_string(),
                    ));
                }
            }
            let text = std::fs::read_to_string(key_file)?;
            (decode_key(text.trim())?, None)
        } else {
            let key = random_key();
            if let Some(parent) = key_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(key_file, STANDARD.encode(key))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(key_file, std::fs::Permissions::from_mode(0o600))?;
            }
            (
                key,
                Some(format!(
                    "generated a new secret key at {}; back it up separately",
                    key_file.display()
                )),
            )
        };
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
        let mut hasher = Sha256::new();
        hasher.update(key);
        let digest = hasher.finalize();
        let key_id = digest[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Ok((Self { cipher, key_id }, warning))
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn encrypt(&self, plaintext: &str) -> String {
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext.as_bytes())
            .expect("encryption with a valid key cannot fail");
        format!(
            "v1:{}:{}:{}",
            self.key_id,
            STANDARD.encode(nonce),
            STANDARD.encode(ciphertext)
        )
    }

    pub fn decrypt(&self, encoded: &str) -> Result<String, CryptoError> {
        let mut parts = encoded.split(':');
        let version = parts.next().ok_or(CryptoError::InvalidSecret)?;
        let key_id = parts.next().ok_or(CryptoError::InvalidSecret)?;
        let nonce = parts.next().ok_or(CryptoError::InvalidSecret)?;
        let ciphertext = parts.next().ok_or(CryptoError::InvalidSecret)?;
        if version != "v1" || parts.next().is_some() || key_id != self.key_id {
            return Err(CryptoError::InvalidSecret);
        }
        let nonce = STANDARD
            .decode(nonce)
            .map_err(|_| CryptoError::InvalidSecret)?;
        let ciphertext = STANDARD
            .decode(ciphertext)
            .map_err(|_| CryptoError::InvalidSecret)?;
        if nonce.len() != 24 {
            return Err(CryptoError::InvalidSecret);
        }
        let plaintext = self
            .cipher
            .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| CryptoError::InvalidSecret)?;
        String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidSecret)
    }
}

fn decode_key(value: &str) -> Result<[u8; 32], CryptoError> {
    let bytes = STANDARD
        .decode(value.trim())
        .map_err(|_| CryptoError::Key("secret key must be base64".to_string()))?;
    bytes
        .try_into()
        .map_err(|_| CryptoError::Key("secret key must be exactly 32 bytes".to_string()))
}

fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn key_file(dir: &Path) -> PathBuf {
        dir.join("key")
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let (secret, warning) = SecretBox::load(None, &key_file(dir.path())).unwrap();
        assert!(warning.is_some());
        let encrypted = secret.encrypt("hunter2");
        assert!(!encrypted.contains("hunter2"));
        assert_eq!(secret.decrypt(&encrypted).unwrap(), "hunter2");
        assert_ne!(secret.encrypt("hunter2"), encrypted);
    }

    #[test]
    fn wrong_key_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, _) = SecretBox::load(None, &key_file(&dir.path().join("a"))).unwrap();
        let (b, _) = SecretBox::load(None, &key_file(&dir.path().join("b"))).unwrap();
        let encrypted = a.encrypt("secret");
        assert!(b.decrypt(&encrypted).is_err());
    }

    #[test]
    fn environment_key_and_invalid_inputs() {
        let key = STANDARD.encode([7u8; 32]);
        let (secret, warning) = SecretBox::load(Some(&key), Path::new("/nonexistent")).unwrap();
        assert!(warning.is_none());
        assert!(!secret.encrypt("x").is_empty());
        assert!(SecretBox::load(Some("not base64"), Path::new("/nonexistent")).is_err());
        assert!(secret.decrypt("v1:bad:bad:bad").is_err());
    }
}
