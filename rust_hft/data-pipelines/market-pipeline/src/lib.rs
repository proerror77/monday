//! Captured market-data conversion and batch publication, without acquisition,
//! training, research scheduling or execution-adapter dependencies.
#[cfg(feature = "import")]
use serde::Serialize;
#[cfg(feature = "import")]
use sha2::{Digest, Sha256};
#[cfg(feature = "import")]
pub mod market_clickhouse;
pub mod market_import;
#[cfg(feature = "import")]
pub mod market_postgres;
#[cfg(feature = "import")]
fn identity(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
#[cfg(feature = "import")]
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
#[cfg(feature = "import")]
fn read_secret(path: &str) -> anyhow::Result<String> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.len() <= 64 * 1024
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.uid() == unsafe { libc::geteuid() },
        "secret requires a bounded private file owned by the importer"
    );
    let mut secret = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut secret)?;
    anyhow::ensure!(
        secret.len() <= 64 * 1024 && !secret.trim().is_empty(),
        "invalid secret file"
    );
    Ok(secret.trim().into())
}
