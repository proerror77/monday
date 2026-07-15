use sha2::{Digest, Sha256};

/// Verify a pinned SHA-256 before deserializing an external catalog or snapshot.
pub fn verify_sha256_hex(bytes: &[u8], expected: &str) -> Result<(), String> {
    let expected = expected
        .split_whitespace()
        .next()
        .ok_or_else(|| "SHA-256 sidecar is empty".to_string())?;
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SHA-256 sidecar must contain one 64-character hex digest".to_string());
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected.to_ascii_lowercase() {
        return Err("SHA-256 sidecar does not match file contents".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_tampered_bytes_before_deserialization() {
        let expected = format!("{:x}", Sha256::digest(b"trusted"));
        assert!(verify_sha256_hex(b"trusted", &expected).is_ok());
        assert!(verify_sha256_hex(b"tampered", &expected).is_err());
    }
}
