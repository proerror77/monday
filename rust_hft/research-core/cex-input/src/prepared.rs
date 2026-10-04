//! Versioned binary blocks and explicit shared-file/object-store transports.
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use bincode::Options;

use crate::{
    data::{BlockRef, BlockSource, TypedBlock},
    valid_digest,
};

const MAX_BLOCK_BYTES: u64 = 16 * 1024 * 1024;

pub fn encode(block: &TypedBlock) -> Result<Vec<u8>> {
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BLOCK_BYTES)
        .serialize(&(1_u32, block))?)
}

pub fn decode(bytes: &[u8]) -> Result<TypedBlock> {
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= MAX_BLOCK_BYTES,
        "unbounded prepared block"
    );
    let (schema, block): (u32, TypedBlock) = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BLOCK_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)?;
    ensure!(schema == 1, "unsupported prepared block schema");
    Ok(block)
}

/// Only an accepted immutable publisher namespace may use this mount. We still
/// validate actual bytes on every cache miss, including after a Pod restart.
pub struct SharedFiles {
    root: PathBuf,
}

impl SharedFiles {
    pub fn new(root: &Path) -> Result<Self> {
        ensure!(
            root.is_absolute() && root.is_dir(),
            "shared input root is not mounted"
        );
        Ok(Self {
            root: root.canonicalize()?,
        })
    }
}

impl BlockSource for SharedFiles {
    fn read(&mut self, block: &BlockRef) -> Result<Vec<u8>> {
        ensure!(valid_digest(&block.sha256), "invalid block key");
        let path = self.root.join(format!("{}.mondaybin", block.sha256));
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.file_type().is_file()
                && metadata.len() == block.bytes
                && block.bytes <= MAX_BLOCK_BYTES,
            "invalid shared file"
        );
        let file = File::open(&path)?;
        let opened = file.metadata()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.dev() == opened.dev() && metadata.ino() == opened.ino(),
                "shared file changed during open"
            );
        }
        let mut bytes = Vec::new();
        file.take(MAX_BLOCK_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == block.bytes,
            "shared file changed during read"
        );
        Ok(bytes)
    }
}

/// Async object acquisition occurs once per bounded batch, then a synchronous
/// typed reader reuses immutable buffers. No per-epoch HTTP/SQL requery.
pub struct AcquiredBlocks {
    pub bytes: std::collections::BTreeMap<String, Vec<u8>>,
}
impl BlockSource for AcquiredBlocks {
    fn read(&mut self, block: &BlockRef) -> Result<Vec<u8>> {
        self.bytes
            .remove(&block.sha256)
            .context("block not acquired for this batch")
    }
}
