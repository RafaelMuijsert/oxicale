use sha2::{Digest, Sha256};

/// Strong ETag value (without surrounding quotes) for stored bytes.
pub fn content_etag(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_is_sha256_hex() {
        let etag = content_etag(b"hello");
        assert_eq!(
            etag,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(etag, content_etag(b"hello"));
        assert_ne!(etag, content_etag(b"hello!"));
    }
}
