use anyhow::{ensure, Result};
use hft_research_worker_contracts::campaign_result::ArchiveEntry;

/// Transport-level archive verification, independent of the worker's entry
/// claims. This does not evaluate a model or permit scientific promotion.
pub fn verify_archive_entries(
    source: impl std::io::Read + std::io::Seek,
    expected: &[ArchiveEntry],
    decoded_budget: u64,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    ensure!(
        !expected.is_empty()
            && expected.len() <= 256
            && decoded_budget > 0
            && decoded_budget <= 512 * 1024 * 1024,
        "unbounded native archive verification"
    );
    let mut archive = zip::ZipArchive::new(source)?;
    ensure!(archive.len() <= 256, "unbounded native archive entry count");
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let path = entry.name().to_owned();
        ensure!(
            seen.insert(path.clone())
                && !entry.is_dir()
                && !path.starts_with('/')
                && !path.contains("..")
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
                && entry
                    .unix_mode()
                    .is_none_or(|mode| mode & 0o170000 == 0 || mode & 0o170000 == 0o100000),
            "duplicate or unsafe native archive entry"
        );
        let reference = expected
            .iter()
            .find(|r| r.path == path)
            .ok_or_else(|| anyhow::anyhow!("archive entry omitted from native evidence"))?;
        ensure!(
            entry.size() == reference.bytes && crate::valid_digest(&reference.sha256),
            "native archive metadata changed"
        );
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| anyhow::anyhow!("native archive size overflow"))?;
        ensure!(
            total <= decoded_budget,
            "native archive exceeds decoded evidence budget"
        );
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        let mut buffer = [0_u8; 8192];
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            ensure!(
                count <= reference.bytes,
                "native archive entry exceeds declared size"
            );
            digest.update(&buffer[..n]);
        }
        ensure!(
            count == reference.bytes && format!("{:x}", digest.finalize()) == reference.sha256,
            "native archive entry bytes changed"
        );
    }
    ensure!(
        seen.len() == expected.len() && expected.iter().all(|r| seen.contains(&r.path)),
        "native archive evidence missing entries"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_zip_entry_claims_require_actual_bytes_and_bounded_coverage() -> Result<()> {
        use std::io::{Cursor, Write};
        let content = b"synthetic native verification receipt";
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive.start_file(
            "results/replay.json",
            zip::write::SimpleFileOptions::default(),
        )?;
        archive.write_all(content)?;
        let bytes = archive.finish()?.into_inner();
        let reference = ArchiveEntry {
            path: "results/replay.json".into(),
            sha256: crate::sha256(content),
            bytes: content.len() as u64,
        };
        verify_archive_entries(Cursor::new(&bytes), std::slice::from_ref(&reference), 1024)?;
        let mut forged = reference.clone();
        forged.sha256 = "a".repeat(64);
        assert!(verify_archive_entries(Cursor::new(&bytes), &[forged], 1024).is_err());
        assert!(
            verify_archive_entries(Cursor::new(&bytes), std::slice::from_ref(&reference), 1)
                .is_err()
        );
        let mut absent = reference.clone();
        absent.path = "missing.json".into();
        assert!(verify_archive_entries(Cursor::new(&bytes), &[absent], 1024).is_err());
        assert!(
            verify_archive_entries(Cursor::new(&bytes), &[reference.clone(), reference], 1024)
                .is_err()
        );
        Ok(())
    }
}
