//! Read-only memory observations for research phase events.
//!
//! Process peaks are lifetime high-water marks, not per-phase peaks. Cgroup
//! counters include descendants/cache and are not interchangeable with RSS.
//! Missing or inaccessible counters stay unknown; sampling never changes limits
//! or makes an otherwise valid research operation fail.

use serde::Serialize;
#[cfg(any(target_os = "linux", test))]
use std::io::Read;
#[cfg(any(target_os = "linux", test))]
use std::path::{Component, Path, PathBuf};

#[cfg(any(target_os = "linux", test))]
const MAX_PROC_BYTES: u64 = 256 * 1024;

#[derive(Debug, Default, Serialize)]
pub struct ResearchMemoryObservation {
    pub process_rss_bytes: Option<u64>,
    pub process_peak_rss_bytes: Option<u64>,
    pub cgroup_version: Option<u8>,
    pub cgroup_memory_current_bytes: Option<u64>,
    pub cgroup_memory_peak_bytes: Option<u64>,
    /// The membership cgroup's setting, not physical RAM or an effective limit
    /// obtained by walking all ancestor cgroups.
    pub cgroup_memory_limit: Option<MemoryLimit>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "bytes", rename_all = "snake_case")]
pub enum MemoryLimit {
    Bytes(u64),
    Unlimited,
}

pub fn observe_research_memory() -> ResearchMemoryObservation {
    let observation = ResearchMemoryObservation {
        process_peak_rss_bytes: process_peak_rss_bytes(),
        ..ResearchMemoryObservation::default()
    };
    #[cfg(target_os = "linux")]
    {
        let mut observation = observation;
        observe_linux_memory(&mut observation, std::process::id(), read_bounded);
        observation
    }
    #[cfg(not(target_os = "linux"))]
    {
        observation
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage writes this valid rusage buffer on success; RUSAGE_SELF
    // observes this process and does not inspect another tenant's process.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: the successful getrusage call initialized usage.
    let value = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).ok()?;
    #[cfg(target_os = "macos")]
    {
        Some(value)
    }
    #[cfg(target_os = "linux")]
    {
        value.checked_mul(1024)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn observe_linux_memory(
    observation: &mut ResearchMemoryObservation,
    pid: u32,
    read: impl Fn(&Path) -> Option<String>,
) {
    if let Some(status) = read(Path::new("/proc/self/status")) {
        observation.process_rss_bytes = status_kib(&status, "VmRSS:");
        if let Some(peak) = status_kib(&status, "VmHWM:") {
            observation.process_peak_rss_bytes = Some(peak);
        }
    }
    let Some(membership) = read(Path::new("/proc/self/cgroup")) else {
        return;
    };
    let Some(mountinfo) = read(Path::new("/proc/self/mountinfo")) else {
        return;
    };
    let Some((version, directory)) = cgroup_directory(&membership, &mountinfo) else {
        return;
    };
    // A subtree/namespace mismatch must not report the host or another cgroup's
    // peak as this worker's usage. Re-check membership in the resolved directory.
    if !read(&directory.join("cgroup.procs")).is_some_and(|procs| {
        procs
            .lines()
            .any(|line| line.trim().parse::<u32>() == Ok(pid))
    }) {
        return;
    }
    observation.cgroup_version = Some(version);
    let (current, peak, limit) = if version == 2 {
        ("memory.current", "memory.peak", "memory.max")
    } else {
        (
            "memory.usage_in_bytes",
            "memory.max_usage_in_bytes",
            "memory.limit_in_bytes",
        )
    };
    observation.cgroup_memory_current_bytes =
        read(&directory.join(current)).and_then(|s| s.trim().parse().ok());
    observation.cgroup_memory_peak_bytes =
        read(&directory.join(peak)).and_then(|s| s.trim().parse().ok());
    observation.cgroup_memory_limit =
        read(&directory.join(limit)).and_then(|s| parse_limit(&s, version));
}

#[cfg(any(target_os = "linux", test))]
fn status_kib(status: &str, key: &str) -> Option<u64> {
    let mut fields = status
        .lines()
        .find_map(|line| line.strip_prefix(key))?
        .split_whitespace();
    let value: u64 = fields.next()?.parse().ok()?;
    if fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    value.checked_mul(1024)
}

#[cfg(any(target_os = "linux", test))]
fn parse_limit(value: &str, version: u8) -> Option<MemoryLimit> {
    let value = value.trim();
    if version == 2 && value == "max" {
        return Some(MemoryLimit::Unlimited);
    }
    let bytes: u64 = value.parse().ok()?;
    // cgroup v1 uses a page-aligned LONG_MAX sentinel on the 64-bit research
    // workers. Allow 4/16/64 KiB base pages, rather than displaying exabytes of RAM.
    if version == 1 && bytes >= (i64::MAX as u64 - 65535) {
        Some(MemoryLimit::Unlimited)
    } else {
        Some(MemoryLimit::Bytes(bytes))
    }
}

#[cfg(any(target_os = "linux", test))]
fn cgroup_directory(membership: &str, mountinfo: &str) -> Option<(u8, PathBuf)> {
    let memberships = membership
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            Some((fields.next()?, fields.next()?, fields.next()?))
        })
        .collect::<Vec<_>>();
    let (version, membership) = memberships
        .iter()
        .find_map(|(_, controllers, group)| {
            controllers
                .split(',')
                .any(|name| name == "memory")
                .then_some((1, *group))
        })
        .or_else(|| {
            memberships
                .iter()
                .find_map(|(hierarchy, controllers, group)| {
                    (*hierarchy == "0" && controllers.is_empty()).then_some((2, *group))
                })
        })?;
    let membership = Path::new(membership);
    if !safe_absolute(membership) {
        return None;
    }
    mountinfo
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let left = left.split_whitespace().collect::<Vec<_>>();
            let right = right.split_whitespace().collect::<Vec<_>>();
            if (version == 2 && right.first() != Some(&"cgroup2"))
                || (version == 1
                    && (right.first() != Some(&"cgroup")
                        || !right
                            .get(2)?
                            .split(',')
                            .any(|controller| controller == "memory")))
            {
                return None;
            }
            let root = mount_path(left.get(3)?)?;
            let mount = mount_path(left.get(4)?)?;
            let relative = if membership == Path::new("/") {
                Path::new("")
            } else {
                membership.strip_prefix(&root).ok()?
            };
            Some((root.components().count(), mount.join(relative)))
        })
        .max_by_key(|(specificity, _)| *specificity)
        .map(|(_, path)| (version, path))
}

#[cfg(any(target_os = "linux", test))]
fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
}

#[cfg(any(target_os = "linux", test))]
fn mount_path(encoded: &str) -> Option<PathBuf> {
    let mut decoded = String::new();
    let mut chars = encoded.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escape: String = chars.by_ref().take(3).collect();
        decoded.push(match escape.as_str() {
            "040" => ' ',
            "011" => '\t',
            "012" => '\n',
            "134" => '\\',
            _ => return None,
        });
    }
    let path = PathBuf::from(decoded);
    safe_absolute(&path).then_some(path)
}

#[cfg(any(target_os = "linux", test))]
fn read_bounded(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_PROC_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_PROC_BYTES).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn units_missing_values_overflow_and_unlimited_are_distinct() {
        assert_eq!(
            status_kib("VmRSS:\t12 kB\nVmHWM:\t15 kB\n", "VmRSS:"),
            Some(12 * 1024)
        );
        assert_eq!(status_kib("VmRSS: 12 MB", "VmRSS:"), None);
        assert_eq!(status_kib("VmRSS: 18446744073709551615 kB", "VmRSS:"), None);
        assert_eq!(status_kib("VmRSS: 0 kB", "VmRSS:"), Some(0));
        assert_eq!(parse_limit("max\n", 2), Some(MemoryLimit::Unlimited));
        assert_eq!(
            parse_limit("12884901888", 2),
            Some(MemoryLimit::Bytes(12 * 1024 * 1024 * 1024))
        );
        assert_eq!(
            parse_limit("9223372036854771712", 1),
            Some(MemoryLimit::Unlimited)
        );
        assert_eq!(parse_limit("-1", 2), None);
        assert_eq!(parse_limit("", 1), None);
    }

    #[test]
    fn resolves_v1_v2_subtree_and_namespaced_mounts_without_parent_escape() {
        let mount = "31 20 0:27 / /sys/fs/cgroup rw - cgroup2 cgroup rw";
        assert_eq!(
            cgroup_directory("0::/pods/a", mount),
            Some((2, "/sys/fs/cgroup/pods/a".into()))
        );
        let subtree = "31 20 0:27 /pods/a /sys/fs/cgroup rw - cgroup2 cgroup rw";
        assert_eq!(
            cgroup_directory("0::/pods/a", subtree),
            Some((2, "/sys/fs/cgroup".into()))
        );
        assert_eq!(
            cgroup_directory("0::/", subtree),
            Some((2, "/sys/fs/cgroup".into()))
        );
        assert_eq!(cgroup_directory("0::/pods/b", subtree), None);
        assert_eq!(cgroup_directory("0::/../../secrets", mount), None);
        let v1 = "31 20 0:27 / /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory";
        assert_eq!(
            cgroup_directory("7:cpu:/a\n8:memory:/pods/a", v1),
            Some((1, "/sys/fs/cgroup/memory/pods/a".into()))
        );
        assert_eq!(
            cgroup_directory("0::/unified\n8:memory:/pods/a", v1),
            Some((1, "/sys/fs/cgroup/memory/pods/a".into()))
        );
        assert_eq!(mount_path("/a\\040b"), Some("/a b".into()));
        assert_eq!(mount_path("/a\\999"), None);
    }

    #[test]
    fn records_real_membership_and_preserves_unknown_peak_instead_of_zero() {
        let mut files = BTreeMap::from([
            ("/proc/self/status", "VmRSS: 1024 kB\nVmHWM: 2048 kB"),
            ("/proc/self/cgroup", "0::/pods/a"),
            (
                "/proc/self/mountinfo",
                "31 20 0:27 / /sys/fs/cgroup rw - cgroup2 cgroup rw",
            ),
            ("/sys/fs/cgroup/pods/a/cgroup.procs", "42\n"),
            ("/sys/fs/cgroup/pods/a/memory.current", "4194304"),
            ("/sys/fs/cgroup/pods/a/memory.max", "12884901888"),
        ]);
        let mut snapshot = ResearchMemoryObservation::default();
        observe_linux_memory(&mut snapshot, 42, |path| {
            files.get(path.to_str()?).map(|s| (*s).into())
        });
        assert_eq!(snapshot.process_rss_bytes, Some(1024 * 1024));
        assert_eq!(snapshot.cgroup_memory_current_bytes, Some(4194304));
        assert_eq!(snapshot.cgroup_memory_peak_bytes, None);
        files.insert("/sys/fs/cgroup/pods/a/cgroup.procs", "999\n");
        let mut wrong = ResearchMemoryObservation::default();
        observe_linux_memory(&mut wrong, 42, |path| {
            files.get(path.to_str()?).map(|s| (*s).into())
        });
        assert_eq!(wrong.cgroup_version, None);
        assert_eq!(wrong.cgroup_memory_current_bytes, None);
    }

    #[test]
    fn proc_reads_are_bounded_and_v1_memory_counters_are_observed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counter");
        std::fs::write(&path, b"1024\n").unwrap();
        assert_eq!(read_bounded(&path).as_deref(), Some("1024\n"));
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_PROC_BYTES + 1)
            .unwrap();
        assert_eq!(read_bounded(&path), None);
        let files = BTreeMap::from([
            ("/proc/self/cgroup", "8:memory:/pod"),
            (
                "/proc/self/mountinfo",
                "31 20 0:27 / /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory",
            ),
            ("/sys/fs/cgroup/memory/pod/cgroup.procs", "42\n"),
            ("/sys/fs/cgroup/memory/pod/memory.usage_in_bytes", "2048"),
            (
                "/sys/fs/cgroup/memory/pod/memory.max_usage_in_bytes",
                "4096",
            ),
            ("/sys/fs/cgroup/memory/pod/memory.limit_in_bytes", "8192"),
        ]);
        let mut observed = ResearchMemoryObservation::default();
        observe_linux_memory(&mut observed, 42, |path| {
            files.get(path.to_str()?).map(|s| (*s).into())
        });
        assert_eq!(observed.cgroup_version, Some(1));
        assert_eq!(observed.cgroup_memory_peak_bytes, Some(4096));
        assert_eq!(observed.cgroup_memory_limit, Some(MemoryLimit::Bytes(8192)));
    }

    #[test]
    fn unreadable_proc_is_unknown_and_sampling_is_observational_only() {
        let mut snapshot = ResearchMemoryObservation::default();
        observe_linux_memory(&mut snapshot, 42, |_| None);
        assert!(serde_json::to_value(snapshot)
            .unwrap()
            .as_object()
            .unwrap()
            .values()
            .all(serde_json::Value::is_null));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let resident = vec![1_u8; 4 * 1024 * 1024];
            std::hint::black_box(&resident);
            let current = observe_research_memory();
            assert!(current
                .process_peak_rss_bytes
                .is_some_and(|peak| peak >= resident.len() as u64));
        }
    }
}
