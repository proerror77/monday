//! Shared immutable, bounded JSONL frame storage for sequence readers.
use hft_research_manifest::sequence::{SequenceInputSpecV1, SequenceShardV1};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    marker::PhantomData,
    path::Path,
};

pub(crate) trait Frame: DeserializeOwned {
    fn clock(&self) -> i64;
    fn validate(&self, input: &SequenceInputSpecV1) -> Result<(), String>;
}
struct Pass {
    reader: BufReader<File>,
    digest: Sha256,
    bytes: u64,
    rows: u64,
    first: Option<i64>,
    last: Option<i64>,
}
pub(crate) struct Frames<F> {
    shards: Vec<SequenceShardV1>,
    files: Vec<File>,
    input: SequenceInputSpecV1,
    index: usize,
    pass: Option<Pass>,
    last: Option<i64>,
    marker: PhantomData<F>,
}
impl<F: Frame> Frames<F> {
    pub(crate) fn is_at_start(&self) -> bool {
        self.index == 0 && self.pass.is_none() && self.last.is_none()
    }

    pub(crate) fn open(
        root: &Path,
        shards: Vec<SequenceShardV1>,
        input: SequenceInputSpecV1,
    ) -> Result<Self, String> {
        let mut files = Vec::new();
        for shard in &shards {
            let path = root.join(&shard.file);
            let info = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !info.file_type().is_file() || info.len() != shard.bytes {
                return Err("invalid market shard file or size".into());
            }
            let mut file = File::open(&path).map_err(|e| e.to_string())?;
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata().map_err(|e| e.to_string())?;
            if opened.dev() != info.dev() || opened.ino() != info.ino() {
                return Err("market shard identity changed during open".into());
            }
            let mut hash = Sha256::new();
            let count = std::io::copy(&mut (&mut file).take(shard.bytes + 1), &mut hash)
                .map_err(|e| e.to_string())?;
            if count != shard.bytes || format!("{:x}", hash.finalize()) != shard.sha256 {
                return Err("market shard checksum mismatch".into());
            }
            files.push(file);
        }
        Ok(Self {
            shards,
            files,
            input,
            index: 0,
            pass: None,
            last: None,
            marker: PhantomData,
        })
    }
    pub(crate) fn next(&mut self) -> Result<Option<F>, String> {
        loop {
            if self.index == self.files.len() {
                return Ok(None);
            }
            if self.pass.is_none() {
                let mut file = self.files[self.index]
                    .try_clone()
                    .map_err(|e| e.to_string())?;
                file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                self.pass = Some(Pass {
                    reader: BufReader::new(file),
                    digest: Sha256::new(),
                    bytes: 0,
                    rows: 0,
                    first: None,
                    last: None,
                });
            }
            let pass = self.pass.as_mut().expect("opened pass");
            let mut line = Vec::new();
            let count = (&mut pass.reader)
                .take(32 * 1024 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if count == 0 {
                let pass = self.pass.take().expect("opened pass");
                let shard = &self.shards[self.index];
                if pass.bytes != shard.bytes
                    || pass.rows != shard.rows
                    || pass.first != Some(shard.first_observed_at_ms)
                    || pass.last != Some(shard.last_observed_at_ms)
                    || format!("{:x}", pass.digest.finalize()) != shard.sha256
                {
                    return Err("market shard changed or coverage is false".into());
                }
                self.index += 1;
                continue;
            }
            if count > 32 * 1024 || line.last() != Some(&b'\n') {
                return Err("invalid bounded market frame".into());
            }
            pass.bytes += count as u64;
            if pass.bytes > self.shards[self.index].bytes {
                return Err("market shard grew".into());
            }
            pass.digest.update(&line);
            let frame: F = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
            frame.validate(&self.input)?;
            let clock = frame.clock();
            if self.last.is_some_and(|t| clock <= t) {
                return Err("market frames duplicate or out of order".into());
            }
            self.last = Some(clock);
            pass.first.get_or_insert(clock);
            pass.last = Some(clock);
            pass.rows += 1;
            return Ok(Some(frame));
        }
    }
    pub(crate) fn finish(&mut self) -> Result<(), String> {
        while self.next()?.is_some() {}
        Ok(())
    }
    pub(crate) fn rewind(&mut self) -> Result<(), String> {
        if self.index != self.files.len() {
            return Err("cannot rewind an unverified market pass".into());
        }
        self.index = 0;
        self.pass = None;
        self.last = None;
        Ok(())
    }
}
