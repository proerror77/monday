//! Name-based discovery for hive-partitioned collector archives.
//!
//! Sealed tapes live under `date=YYYY-MM-DD/hour=HH`. Reference batches add
//! `batch={observed_at_ns}`. Walking every object with a per-file stat turns
//! one hour of preparation into a multi-day metadata scan. Directory entries
//! are classified from `d_type` and the partition name. Only partitions that
//! can overlap the requested receive-time window are opened, and a directory
//! index reuses those listings for later hours.
use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Production segments rotate every 300 seconds. The archiver default is one
/// hour. Two hours keeps a delayed rotation and the adjacent partition without
/// opening unrelated days.
const PARTITION_MARGIN_NS: u64 = 2 * 60 * 60 * 1_000_000_000;
const HOUR_NS: u64 = 60 * 60 * 1_000_000_000;
const DAY_NS: u64 = 24 * HOUR_NS;
const INDEX_SCHEMA: &str = "monday.research_discovery_index.v1";

#[derive(Debug, Clone, Copy)]
pub(crate) struct DiscoveryBounds {
    pub(crate) earliest_ns: u64,
    pub(crate) latest_ns: u64,
}

impl DiscoveryBounds {
    pub(crate) fn raw(start_ns: u64, end_ns: u64) -> Self {
        Self {
            earliest_ns: start_ns.saturating_sub(PARTITION_MARGIN_NS),
            latest_ns: end_ns,
        }
    }

    pub(crate) fn reference(
        start_ns: u64,
        end_ns: u64,
        bucket_ms: u64,
        label_horizon_buckets: u64,
        gap_ns: u64,
    ) -> Result<Self> {
        let horizon_ns = bucket_ms
            .checked_mul(label_horizon_buckets)
            .and_then(|millis| millis.checked_mul(1_000_000))
            .context("discovery reference horizon overflows")?;
        let slack = PARTITION_MARGIN_NS
            .checked_add(gap_ns)
            .context("discovery reference margin overflows")?;
        Ok(Self {
            earliest_ns: start_ns.saturating_sub(slack),
            latest_ns: end_ns
                .checked_add(slack)
                .and_then(|end| end.checked_add(horizon_ns))
                .context("discovery reference tail overflows")?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Unknown,
    Other,
}

struct ListedEntry {
    name: String,
    kind: EntryKind,
}

#[derive(Debug, Clone, Copy)]
struct WalkPlace {
    date: Option<NaiveDate>,
    hour: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveryIndex {
    schema_version: String,
    root: String,
    partitions: BTreeMap<String, CachedPartition>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedPartition {
    scanned_at_ns: u64,
    hour_start_ns: u64,
    entries: Vec<CachedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEntry {
    name: String,
    named_ns: u64,
    /// Manifest paths relative to the archive root. Empty until a batch
    /// directory has been listed.
    manifests: Vec<String>,
    listed: bool,
}

pub(crate) fn discover_manifests(
    root: &Path,
    max_entries: usize,
    bounds: Option<DiscoveryBounds>,
    index_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    if max_entries == 0 {
        bail!("spool scan entry budget exceeded");
    }
    let env_index = std::env::var_os("MONDAY_RESEARCH_DISCOVERY_INDEX").map(PathBuf::from);
    let index_dir = index_dir.or(env_index.as_deref());
    let mut remaining = max_entries;
    let mut index = match index_dir {
        Some(dir) => load_index(dir, root)?,
        None => DiscoveryIndex {
            schema_version: INDEX_SCHEMA.to_string(),
            root: root.display().to_string(),
            partitions: BTreeMap::new(),
        },
    };
    let mut manifests = Vec::new();
    walk(
        root,
        root,
        WalkPlace {
            date: None,
            hour: None,
        },
        &mut remaining,
        bounds,
        &mut index,
        &mut manifests,
    )?;
    if let Some(dir) = index_dir {
        store_index(dir, root, &index)?;
    }
    manifests.sort();
    Ok(manifests)
}

fn walk(
    root: &Path,
    dir: &Path,
    place: WalkPlace,
    remaining: &mut usize,
    bounds: Option<DiscoveryBounds>,
    index: &mut DiscoveryIndex,
    manifests: &mut Vec<PathBuf>,
) -> Result<()> {
    if let (Some(hour), Some(date), Some(bounds)) = (place.hour, place.date, bounds) {
        let hour_start = hour_start_ns(date, hour)?;
        if !hour_overlaps(hour_start, bounds) {
            return Ok(());
        }
        let key = relative_key(root, dir)?;
        if let Some(paths) = reuse_partition(root, index, &key, hour_start, bounds, remaining)? {
            manifests.extend(paths);
            return Ok(());
        }
        let entries = list_dir(dir, remaining)?;
        let mut cached = cache_hour(root, dir, hour_start, &entries)?;
        cached = list_needed_batches(root, dir, &cached, bounds, remaining)?;
        manifests.extend(manifest_paths(root, &cached, bounds)?);
        index.partitions.insert(key, cached);
        return Ok(());
    }

    let entries = list_dir(dir, remaining)?;
    for entry in entries {
        if entry.name.starts_with('.') {
            continue;
        }
        let path = dir.join(&entry.name);
        if let Some(date) = entry.name.strip_prefix("date=") {
            if let Ok(date) = NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                if bounds.is_some_and(|bounds| !date_overlaps(date, bounds)) {
                    continue;
                }
                ensure_directory(&path, &entry.kind)?;
                walk(
                    root,
                    &path,
                    WalkPlace {
                        date: Some(date),
                        hour: None,
                    },
                    remaining,
                    bounds,
                    index,
                    manifests,
                )?;
                continue;
            }
        }
        if let Some(hour) = entry.name.strip_prefix("hour=") {
            if let Some(hour) = strict_hour(hour) {
                if let Some(date) = place.date {
                    if bounds.is_some_and(|bounds| {
                        hour_start_ns(date, hour)
                            .ok()
                            .is_some_and(|start| !hour_overlaps(start, bounds))
                    }) {
                        continue;
                    }
                }
                ensure_directory(&path, &entry.kind)?;
                walk(
                    root,
                    &path,
                    WalkPlace {
                        date: place.date,
                        hour: Some(hour),
                    },
                    remaining,
                    bounds,
                    index,
                    manifests,
                )?;
                continue;
            }
        }
        if entry.name.ends_with(".manifest.json") {
            if entry.kind == EntryKind::Symlink {
                bail!("refusing symlink while scanning spool: {}", path.display());
            }
            manifests.push(path);
            continue;
        }
        if entry.name.contains('.') {
            if entry.kind == EntryKind::Symlink {
                bail!("refusing symlink while scanning spool: {}", path.display());
            }
            if matches!(entry.kind, EntryKind::Other) {
                bail!(
                    "refusing non-regular entry while scanning spool: {}",
                    path.display()
                );
            }
            continue;
        }
        if entry.kind == EntryKind::Symlink {
            bail!("refusing symlink while scanning spool: {}", path.display());
        }
        if entry.kind == EntryKind::File {
            continue;
        }
        ensure_directory(&path, &entry.kind)?;
        walk(root, &path, place, remaining, bounds, index, manifests)?;
    }
    Ok(())
}

fn cache_hour(
    root: &Path,
    hour_dir: &Path,
    hour_start_ns: u64,
    entries: &[ListedEntry],
) -> Result<CachedPartition> {
    let mut cached = Vec::new();
    for entry in entries {
        if entry.name.starts_with('.') {
            continue;
        }
        if entry.name.ends_with(".manifest.json") {
            if entry.kind == EntryKind::Symlink {
                bail!(
                    "refusing symlink while scanning spool: {}",
                    hour_dir.join(&entry.name).display()
                );
            }
            cached.push(CachedEntry {
                named_ns: raw_part_start(&entry.name).unwrap_or(0),
                manifests: vec![relative_key(root, &hour_dir.join(&entry.name))?],
                name: entry.name.clone(),
                listed: true,
            });
            continue;
        }
        if let Some(batch) = entry.name.strip_prefix("batch=") {
            if let Ok(named_ns) = batch.parse::<u64>() {
                cached.push(CachedEntry {
                    name: entry.name.clone(),
                    named_ns,
                    manifests: Vec::new(),
                    listed: false,
                });
            }
        }
    }
    Ok(CachedPartition {
        scanned_at_ns: wall_ns()?,
        hour_start_ns,
        entries: cached,
    })
}

fn list_needed_batches(
    root: &Path,
    hour_dir: &Path,
    cached: &CachedPartition,
    bounds: DiscoveryBounds,
    remaining: &mut usize,
) -> Result<CachedPartition> {
    let mut updated = CachedPartition {
        scanned_at_ns: cached.scanned_at_ns,
        hour_start_ns: cached.hour_start_ns,
        entries: Vec::with_capacity(cached.entries.len()),
    };
    for entry in &cached.entries {
        let mut entry = entry.clone();
        if entry.name.starts_with("batch=")
            && !entry.listed
            && batch_overlaps(entry.named_ns, bounds)
        {
            let batch_dir = hour_dir.join(&entry.name);
            let children = list_dir(&batch_dir, remaining)?;
            let mut manifests = Vec::new();
            for child in children {
                if child.kind == EntryKind::Symlink {
                    bail!(
                        "refusing symlink while scanning spool: {}",
                        batch_dir.join(&child.name).display()
                    );
                }
                if child.name.ends_with(".manifest.json") {
                    manifests.push(relative_key(root, &batch_dir.join(&child.name))?);
                }
            }
            entry.manifests = manifests;
            entry.listed = true;
        }
        updated.entries.push(entry);
    }
    Ok(updated)
}

fn reuse_partition(
    root: &Path,
    index: &mut DiscoveryIndex,
    key: &str,
    hour_start: u64,
    bounds: DiscoveryBounds,
    remaining: &mut usize,
) -> Result<Option<Vec<PathBuf>>> {
    let Some(cached) = index.partitions.get(key) else {
        return Ok(None);
    };
    if !partition_reusable(cached.scanned_at_ns, hour_start) {
        return Ok(None);
    }
    let hour_dir = root.join(key);
    let updated = list_needed_batches(root, &hour_dir, cached, bounds, remaining)?;
    let paths = manifest_paths(root, &updated, bounds)?;
    index.partitions.insert(key.to_string(), updated);
    Ok(Some(paths))
}

fn manifest_paths(
    root: &Path,
    cached: &CachedPartition,
    bounds: DiscoveryBounds,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in &cached.entries {
        if entry.name.ends_with(".manifest.json") {
            if raw_name_overlaps(entry.named_ns, &entry.name, bounds) {
                let manifest = entry
                    .manifests
                    .first()
                    .context("cached raw manifest path is missing")?;
                paths.push(root.join(manifest));
            }
            continue;
        }
        if entry.listed && batch_overlaps(entry.named_ns, bounds) {
            for manifest in &entry.manifests {
                paths.push(root.join(manifest));
            }
        }
    }
    Ok(paths)
}

fn raw_name_overlaps(named_ns: u64, name: &str, bounds: DiscoveryBounds) -> bool {
    match raw_part_start(name) {
        Some(start) => {
            start < bounds.latest_ns
                && start.saturating_add(PARTITION_MARGIN_NS) >= bounds.earliest_ns
        }
        None if named_ns > 0 => {
            named_ns < bounds.latest_ns
                && named_ns.saturating_add(PARTITION_MARGIN_NS) >= bounds.earliest_ns
        }
        None => true,
    }
}

fn batch_overlaps(named_ns: u64, bounds: DiscoveryBounds) -> bool {
    named_ns >= bounds.earliest_ns && named_ns <= bounds.latest_ns
}

fn date_overlaps(date: NaiveDate, bounds: DiscoveryBounds) -> bool {
    let Ok(start) = day_start_ns(date) else {
        return true;
    };
    start < bounds.latest_ns.saturating_add(PARTITION_MARGIN_NS)
        && start
            .saturating_add(DAY_NS)
            .saturating_add(PARTITION_MARGIN_NS)
            > bounds.earliest_ns
}

fn hour_overlaps(hour_start: u64, bounds: DiscoveryBounds) -> bool {
    hour_start < bounds.latest_ns
        && hour_start
            .saturating_add(HOUR_NS)
            .saturating_add(PARTITION_MARGIN_NS)
            > bounds.earliest_ns
}

fn partition_reusable(scanned_at_ns: u64, hour_start_ns: u64) -> bool {
    scanned_at_ns.saturating_sub(hour_start_ns) >= HOUR_NS.saturating_add(PARTITION_MARGIN_NS)
}

fn raw_part_start(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("part-")?;
    let digits = rest.split_once('.')?.0;
    digits.parse().ok()
}

fn strict_hour(hour: &str) -> Option<u8> {
    if hour.len() != 2 {
        return None;
    }
    hour.parse::<u8>().ok().filter(|hour| *hour < 24)
}

fn day_start_ns(date: NaiveDate) -> Result<u64> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .context("discovery date is not a civil midnight")?;
    let seconds = naive.and_utc().timestamp();
    u64::try_from(seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .context("discovery date overflows the receive clock")
}

fn hour_start_ns(date: NaiveDate, hour: u8) -> Result<u64> {
    let naive = date
        .and_hms_opt(u32::from(hour), 0, 0)
        .context("discovery hour is not a civil time")?;
    let seconds = naive.and_utc().timestamp();
    u64::try_from(seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .context("discovery hour overflows the receive clock")
}

fn wall_ns() -> Result<u64> {
    u64::try_from(
        Utc::now()
            .timestamp_nanos_opt()
            .context("discovery wall clock is out of range")?,
    )
    .context("discovery wall clock is out of range")
}

fn relative_key(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_str().context("discovery path is not UTF-8")?;
                if part.is_empty() || part == "." || part == ".." {
                    bail!("discovery path escapes its archive root");
                }
                parts.push(part);
            }
            _ => bail!("discovery path escapes its archive root"),
        }
    }
    Ok(parts.join("/"))
}

fn ensure_directory(path: &Path, kind: &EntryKind) -> Result<()> {
    if *kind == EntryKind::Symlink {
        bail!("refusing symlink while scanning spool: {}", path.display());
    }
    Ok(())
}

fn list_dir(path: &Path, remaining: &mut usize) -> Result<Vec<ListedEntry>> {
    let mut listed = read_dir_kinds(path)?;
    if listed.len() > *remaining {
        bail!("spool scan entry budget exceeded");
    }
    *remaining -= listed.len();
    listed.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(listed)
}

fn read_dir_kinds(path: &Path) -> Result<Vec<ListedEntry>> {
    let bytes = path.as_os_str().as_bytes();
    let c_path = CString::new(bytes).context("discovery path contains an interior NUL")?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ELOOP) {
            bail!("refusing symlink while scanning spool: {}", path.display());
        }
        return Err(error).with_context(|| format!("scan spool directory {}", path.display()));
    }
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error).with_context(|| format!("scan spool directory {}", path.display()));
    }
    let mut listed = Vec::new();
    let read_result = (|| -> Result<()> {
        loop {
            errno_clear();
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error)
                        .with_context(|| format!("scan spool directory {}", path.display()));
                }
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_str()
                .context("spool entry name is not UTF-8")?
                .to_string();
            if name == "." || name == ".." {
                continue;
            }
            let kind = match unsafe { (*entry).d_type } {
                libc::DT_DIR => EntryKind::Directory,
                libc::DT_REG => EntryKind::File,
                libc::DT_LNK => EntryKind::Symlink,
                libc::DT_UNKNOWN => EntryKind::Unknown,
                _ => EntryKind::Other,
            };
            listed.push(ListedEntry { name, kind });
        }
        Ok(())
    })();
    unsafe { libc::closedir(dir) };
    read_result?;
    Ok(listed)
}

fn errno_clear() {
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    unsafe {
        *libc::__errno_location() = 0;
    }
}

fn index_path(dir: &Path, root: &Path) -> PathBuf {
    let digest = hex::encode(Sha256::digest(root.as_os_str().as_bytes()));
    dir.join(format!("{digest}.json"))
}

fn load_index(dir: &Path, root: &Path) -> Result<DiscoveryIndex> {
    let path = index_path(dir, root);
    if !path.is_file() {
        return Ok(DiscoveryIndex {
            schema_version: INDEX_SCHEMA.to_string(),
            root: root.display().to_string(),
            partitions: BTreeMap::new(),
        });
    }
    let bytes =
        fs::read(&path).with_context(|| format!("read discovery index {}", path.display()))?;
    let index: DiscoveryIndex = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse discovery index {}", path.display()))?;
    if index.schema_version != INDEX_SCHEMA || index.root != root.display().to_string() {
        bail!("discovery index does not belong to this archive root");
    }
    Ok(index)
}

fn store_index(dir: &Path, root: &Path, index: &DiscoveryIndex) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("create discovery index directory {}", dir.display()))?;
    let path = index_path(dir, root);
    let bytes = serde_json::to_vec_pretty(index)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temporary)
            .with_context(|| format!("create discovery index {}", temporary.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, &path)
        .with_context(|| format!("publish discovery index {}", path.display()))?;
    let _ = File::open(dir).and_then(|directory| directory.sync_all());
    Ok(())
}
