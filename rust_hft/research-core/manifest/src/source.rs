//! Content identity for ordered immutable source segments.
use sha2::{Digest, Sha256};

/// Hash the ordered raw-segment digests that form a replay artifact.
pub fn source_revision<'a>(segment_hashes: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for hash in segment_hashes {
        digest.update(hash.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::source_revision;

    #[test]
    fn source_revision_preserves_published_ordered_digest_bytes() {
        assert_eq!(
            source_revision([]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            source_revision(["ab", "c"]),
            "1b10891da7a8bc413923775af7702b007cbf489c6f600255dca8fc1d7889d612"
        );
        assert_eq!(
            source_revision(["c", "ab"]),
            "b26177cc8ada189c304780e5d2c065e46fe0c40164ea040042e9e84136c810f9"
        );
    }
}
