use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, Dir, Mode, OFlags};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

const OUTPUT_LOCK_NAME: &str = ".prediction-research-loop.lock";
static OUTPUT_TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactRef {
    pub path: String,
    pub sha256: String,
}

/// A stable capability for one LoopRun output directory.
///
/// Every governed read, write, directory traversal, and lock acquisition is
/// resolved relative to this descriptor. Replacing the path after this object
/// is opened therefore cannot redirect the LoopRun to a different directory.
#[derive(Debug)]
pub(crate) struct OutputRoot {
    fd: OwnedFd,
    canonical_path: PathBuf,
}

pub(crate) struct OutputLock {
    file: File,
}

#[derive(Debug)]
pub(crate) struct VerifiedArtifact {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl VerifiedArtifact {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn parse_json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.bytes)
            .map_err(|error| format!("parse JSON {}: {error}", self.path.display()))
    }
}

impl OutputRoot {
    pub(crate) fn open(output_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(output_dir).map_err(|error| {
            format!("create output directory {}: {error}", output_dir.display())
        })?;
        let canonical_path = fs::canonicalize(output_dir).map_err(|error| {
            format!(
                "canonicalize prediction output root {}: {error}",
                output_dir.display()
            )
        })?;
        let fd = rustix::fs::open(
            &canonical_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            format!(
                "open canonical prediction output root without following symlinks {}: {error}",
                canonical_path.display()
            )
        })?;
        Ok(Self { fd, canonical_path })
    }

    pub(crate) fn path(&self, relative: &Path) -> Result<PathBuf, String> {
        validate_relative_path(relative, true)?;
        Ok(self.canonical_path.join(relative))
    }

    pub(crate) fn entry_exists(&self, relative: &Path) -> Result<bool, String> {
        let (parent, file_name) = self.open_parent(relative, false)?;
        match rustix::fs::openat(
            &parent,
            &file_name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(_) => Ok(true),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
            Err(error) => Err(format!(
                "inspect prediction output entry {}: {error}",
                self.path(relative)?.display()
            )),
        }
    }

    pub(crate) fn list_directory(&self, relative: &Path) -> Result<Option<Vec<String>>, String> {
        if !relative.as_os_str().is_empty() && !self.entry_exists(relative)? {
            return Ok(None);
        }
        let directory = self.open_directory(relative, false)?;
        let mut entries = Vec::new();
        let mut directory = Dir::read_from(&directory).map_err(|error| {
            format!(
                "read prediction output directory {}: {error}",
                self.path(relative).map_or_else(
                    |_| relative.display().to_string(),
                    |path| path.display().to_string()
                )
            )
        })?;
        for entry in &mut directory {
            let entry = entry.map_err(|error| {
                format!(
                    "read prediction output directory entry under {}: {error}",
                    self.canonical_path.display()
                )
            })?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| "prediction output contains a non-UTF-8 filename".to_string())?;
            if !matches!(name, "." | "..") {
                entries.push(name.to_string());
            }
        }
        Ok(Some(entries))
    }

    fn read_file(&self, relative: &Path) -> Result<Vec<u8>, String> {
        let (parent, file_name) = self.open_parent(relative, false)?;
        let fd = rustix::fs::openat(
            &parent,
            &file_name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            format!(
                "open prediction output file with no-follow traversal {}: {error}",
                self.path(relative).map_or_else(
                    |_| relative.display().to_string(),
                    |path| path.display().to_string()
                )
            )
        })?;
        let mut file = File::from(fd);
        if !file
            .metadata()
            .map_err(|error| format!("inspect prediction output {}: {error}", relative.display()))?
            .is_file()
        {
            return Err(format!(
                "prediction output must be a regular file: {}",
                relative.display()
            ));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("read prediction output {}: {error}", relative.display()))?;
        Ok(bytes)
    }

    fn write_atomic(&self, relative: &Path, body: &[u8]) -> Result<(), String> {
        let (parent, file_name) = self.open_parent(relative, true)?;
        let (temporary_name, temporary_fd) = create_temporary_file(&parent, "state")?;
        let mut renamed = false;
        let result = (|| -> Result<(), String> {
            let mut temporary = File::from(temporary_fd);
            temporary.write_all(body).map_err(|error| {
                format!("write temporary output {}: {error}", relative.display())
            })?;
            temporary.sync_all().map_err(|error| {
                format!("sync temporary output {}: {error}", relative.display())
            })?;
            drop(temporary);
            rustix::fs::renameat(&parent, temporary_name.as_str(), &parent, &file_name).map_err(
                |error| format!("atomically replace output {}: {error}", relative.display()),
            )?;
            renamed = true;
            sync_directory(&parent, relative)?;
            Ok(())
        })();
        if result.is_err() && !renamed {
            let _ = rustix::fs::unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
        }
        result
    }

    fn write_content_addressed(
        &self,
        directory: &Path,
        prefix: &str,
        extension: &str,
        body: &[u8],
    ) -> Result<ArtifactRef, String> {
        validate_file_fragment(prefix, "evidence prefix")?;
        validate_file_fragment(extension, "evidence extension")?;
        let parent = self.open_directory(directory, true)?;
        let digest = sha256_hex(body);
        let file_name = format!("{prefix}-{digest}.{extension}");
        let relative = directory.join(&file_name);

        match read_regular_file_at(&parent, file_name.as_str()) {
            Ok(existing) => {
                if existing != body {
                    return Err(format!(
                        "content-address collision at {}",
                        self.path(&relative)?.display()
                    ));
                }
                // A previous publication may have completed its rename but
                // reported a directory-sync failure. Re-sync before treating
                // the existing entry as durable so a retry can heal that
                // uncertain outcome instead of silently accepting it.
                sync_directory(&parent, &relative)?;
                return Ok(ArtifactRef {
                    path: relative_path_string(&relative)?,
                    sha256: digest,
                });
            }
            Err(AtReadError::NotFound) => {}
            Err(AtReadError::Other(error)) => {
                return Err(format!(
                    "read existing evidence {}: {error}",
                    self.path(&relative)?.display()
                ));
            }
        }

        let (temporary_name, temporary_fd) = create_temporary_file(&parent, "evidence")?;
        let mut renamed = false;
        let result = (|| -> Result<(), String> {
            let mut temporary = File::from(temporary_fd);
            temporary.write_all(body).map_err(|error| {
                format!(
                    "write evidence {}: {error}",
                    self.path(&relative).map_or_else(
                        |_| relative.display().to_string(),
                        |path| path.display().to_string()
                    )
                )
            })?;
            temporary.sync_all().map_err(|error| {
                format!(
                    "sync evidence {}: {error}",
                    self.path(&relative).map_or_else(
                        |_| relative.display().to_string(),
                        |path| path.display().to_string()
                    )
                )
            })?;
            drop(temporary);
            rustix::fs::renameat(
                &parent,
                temporary_name.as_str(),
                &parent,
                file_name.as_str(),
            )
            .map_err(|error| {
                format!(
                    "publish evidence {}: {error}",
                    self.path(&relative).map_or_else(
                        |_| relative.display().to_string(),
                        |path| path.display().to_string()
                    )
                )
            })?;
            renamed = true;
            sync_directory(&parent, &relative)?;
            Ok(())
        })();
        if result.is_err() && !renamed {
            let _ = rustix::fs::unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
        }
        result?;
        Ok(ArtifactRef {
            path: relative_path_string(&relative)?,
            sha256: digest,
        })
    }

    fn create_next_directory(&self, parent: &Path) -> Result<PathBuf, String> {
        let parent_fd = self.open_directory(parent, true)?;
        for index in 1..=10_000_u32 {
            let file_name = format!("attempt-{index:03}");
            match rustix::fs::mkdirat(&parent_fd, file_name.as_str(), Mode::RWXU) {
                Ok(()) => {
                    sync_directory(&parent_fd, parent)?;
                    return Ok(parent.join(file_name));
                }
                Err(error) if error == rustix::io::Errno::EXIST => continue,
                Err(error) => {
                    return Err(format!(
                        "create attempt directory {}: {error}",
                        self.path(&parent.join(file_name))?.display()
                    ));
                }
            }
        }
        Err(format!(
            "attempt directory limit exceeded under {}",
            self.path(parent)?.display()
        ))
    }

    fn open_parent(
        &self,
        relative: &Path,
        create_directories: bool,
    ) -> Result<(OwnedFd, OsString), String> {
        let mut components = validate_relative_path(relative, false)?;
        let file_name = components
            .pop()
            .ok_or_else(|| format!("output path has no file name: {}", relative.display()))?;
        let parent = self.open_components(&components, create_directories)?;
        Ok((parent, file_name))
    }

    fn open_directory(&self, relative: &Path, create_directories: bool) -> Result<OwnedFd, String> {
        let components = validate_relative_path(relative, true)?;
        self.open_components(&components, create_directories)
    }

    fn open_components(
        &self,
        components: &[OsString],
        create_directories: bool,
    ) -> Result<OwnedFd, String> {
        let mut parent = rustix::io::dup(&self.fd)
            .map_err(|error| format!("duplicate prediction output root descriptor: {error}"))?;
        for component in components {
            let name = component.as_os_str();
            let directory = match rustix::fs::openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(directory) => directory,
                Err(error) if create_directories && error == rustix::io::Errno::NOENT => {
                    match rustix::fs::mkdirat(&parent, name, Mode::RWXU) {
                        Ok(()) => {}
                        Err(error) if error == rustix::io::Errno::EXIST => {}
                        Err(error) => {
                            return Err(format!(
                                "create prediction output directory {:?}: {error}",
                                name
                            ));
                        }
                    }
                    rustix::fs::openat(
                        &parent,
                        name,
                        OFlags::RDONLY
                            | OFlags::DIRECTORY
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|error| {
                        format!(
                            "open newly-created prediction directory {:?} without following symlinks: {error}",
                            name
                        )
                    })?
                }
                Err(error) => {
                    return Err(format!(
                        "open prediction directory {:?} with no-follow dir-FD traversal: {error}",
                        name
                    ));
                }
            };
            if create_directories {
                // Conservatively sync even an existing component. It may be a
                // directory left behind by an earlier mkdirat whose parent
                // fsync failed; retrying must establish that durability before
                // publishing descendants beneath it.
                sync_directory(&parent, Path::new(name))?;
            }
            parent = directory;
        }
        Ok(parent)
    }
}

impl OutputLock {
    pub(crate) fn acquire(output_root: &OutputRoot) -> Result<Self, String> {
        let fd = rustix::fs::openat(
            &output_root.fd,
            OUTPUT_LOCK_NAME,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| {
            format!(
                "open output lock under {} without following symlinks: {error}",
                output_root.canonical_path.display()
            )
        })?;
        let file = File::from(fd);
        if !file
            .metadata()
            .map_err(|error| format!("inspect prediction output lock: {error}"))?
            .is_file()
        {
            return Err("prediction output lock must be a regular file".to_string());
        }
        file.try_lock_exclusive().map_err(|error| {
            format!(
                "prediction research output is already locked at {}: {error}",
                output_root.canonical_path.display()
            )
        })?;
        sync_directory(&output_root.fd, Path::new(OUTPUT_LOCK_NAME))?;
        Ok(Self { file })
    }
}

impl Drop for OutputLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(value)
        .map_err(|error| format!("serialize canonical JSON value: {error}"))?;
    let normalized = canonicalize_json(value);
    let mut body = serde_json::to_vec(&normalized)
        .map_err(|error| format!("serialize canonical JSON bytes: {error}"))?;
    body.push(b'\n');
    Ok(body)
}

fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(values) => {
            let values = values
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect::<std::collections::BTreeMap<_, _>>();
            serde_json::Value::Object(values.into_iter().collect())
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonicalize_json).collect())
        }
        value => value,
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn write_content_addressed_json<T: Serialize>(
    output_root: &OutputRoot,
    directory: &Path,
    prefix: &str,
    value: &T,
) -> Result<ArtifactRef, String> {
    let body = canonical_json_bytes(value)?;
    output_root.write_content_addressed(directory, prefix, "json", &body)
}

pub(crate) fn write_content_addressed_text(
    output_root: &OutputRoot,
    directory: &Path,
    prefix: &str,
    body: &str,
) -> Result<ArtifactRef, String> {
    output_root.write_content_addressed(directory, prefix, "txt", body.as_bytes())
}

pub(crate) fn atomic_write_json<T: Serialize>(
    output_root: &OutputRoot,
    relative: &Path,
    value: &T,
) -> Result<(), String> {
    let mut body = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize loop state: {error}"))?;
    body.push(b'\n');
    output_root.write_atomic(relative, &body)
}

pub(crate) fn read_json<T: DeserializeOwned>(
    output_root: &OutputRoot,
    relative: &Path,
) -> Result<T, String> {
    let body = output_root.read_file(relative)?;
    serde_json::from_slice(&body)
        .map_err(|error| format!("parse JSON {}: {error}", relative.display()))
}

#[cfg(test)]
pub(crate) fn artifact_path(
    output_root: &OutputRoot,
    artifact: &ArtifactRef,
) -> Result<PathBuf, String> {
    let relative = Path::new(&artifact.path);
    validate_relative_path(relative, false)?;
    output_root.path(relative)
}

pub(crate) fn verify_artifact(
    output_root: &OutputRoot,
    artifact: &ArtifactRef,
) -> Result<VerifiedArtifact, String> {
    let relative = Path::new(&artifact.path);
    validate_relative_path(relative, false)?;
    let body = output_root.read_file(relative)?;
    let digest = sha256_hex(&body);
    if digest != artifact.sha256 {
        return Err(format!("evidence hash mismatch for {}", artifact.path));
    }
    let file_name = relative
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !file_name.ends_with(&artifact.sha256) {
        return Err(format!(
            "evidence filename is not content-addressed: {}",
            artifact.path
        ));
    }
    Ok(VerifiedArtifact {
        path: output_root.path(relative)?,
        bytes: body,
    })
}

pub(crate) fn next_attempt_dir(output_root: &OutputRoot, parent: &Path) -> Result<PathBuf, String> {
    output_root.create_next_directory(parent)
}

fn validate_relative_path(relative: &Path, allow_empty: bool) -> Result<Vec<OsString>, String> {
    if relative.is_absolute() {
        return Err(format!(
            "unsafe prediction output path {}",
            relative.display()
        ));
    }
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!(
                "unsafe prediction output path {}",
                relative.display()
            ));
        };
        components.push(component.to_os_string());
    }
    if components.is_empty() && !allow_empty {
        return Err(format!(
            "unsafe prediction output path {}",
            relative.display()
        ));
    }
    Ok(components)
}

fn relative_path_string(relative: &Path) -> Result<String, String> {
    validate_relative_path(relative, false)?;
    relative.to_str().map(str::to_string).ok_or_else(|| {
        format!(
            "prediction output path is not UTF-8: {}",
            relative.display()
        )
    })
}

fn validate_file_fragment(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(format!("{label} must be a safe filename fragment"));
    }
    Ok(())
}

fn create_temporary_file(parent: &OwnedFd, label: &str) -> Result<(String, OwnedFd), String> {
    for _ in 0..128 {
        let sequence = OUTPUT_TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            ".prediction-loop-{label}.tmp.{}.{}",
            std::process::id(),
            sequence
        );
        match rustix::fs::openat(
            parent,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => return Ok((name, fd)),
            Err(error) if error == rustix::io::Errno::EXIST => continue,
            Err(error) => return Err(format!("create unique {label} temporary file: {error}")),
        }
    }
    Err(format!("exhausted unique {label} temporary file names"))
}

enum AtReadError {
    NotFound,
    Other(String),
}

fn read_regular_file_at(parent: &OwnedFd, name: &str) -> Result<Vec<u8>, AtReadError> {
    let fd = match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT => return Err(AtReadError::NotFound),
        Err(error) => return Err(AtReadError::Other(error.to_string())),
    };
    let mut file = File::from(fd);
    if !file
        .metadata()
        .map_err(|error| AtReadError::Other(error.to_string()))?
        .is_file()
    {
        return Err(AtReadError::Other(
            "referenced entry is not a regular file".to_string(),
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| AtReadError::Other(error.to_string()))?;
    Ok(bytes)
}

fn sync_directory(directory: &OwnedFd, context: &Path) -> Result<(), String> {
    #[cfg(test)]
    if fail_next_directory_sync_requested() {
        return Err(format!(
            "sync prediction output directory for {}: injected test failure",
            context.display()
        ));
    }
    rustix::fs::fsync(directory).map_err(|error| {
        format!(
            "sync prediction output directory for {}: {error}",
            context.display()
        )
    })
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_directory_sync_requested() -> bool {
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false))
}

#[cfg(test)]
fn inject_next_directory_sync_failure() {
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ploy-prediction-loop-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn content_addressed_artifact_rejects_tampering_and_escape() {
        let path = fixture_root("artifact");
        let root = OutputRoot::open(&path).expect("open output root");
        let artifact = write_content_addressed_json(
            &root,
            Path::new("evidence"),
            "record",
            &serde_json::json!({"b": 2, "a": 1}),
        )
        .expect("write artifact");
        let verified = verify_artifact(&root, &artifact).expect("verify artifact");

        let governed_path = artifact_path(&root, &artifact).expect("artifact path");
        fs::write(&governed_path, b"tampered").expect("tamper artifact");
        let original: serde_json::Value = verified.parse_json().expect("parse verified bytes");
        assert_eq!(original, serde_json::json!({"a": 1, "b": 2}));
        assert!(verify_artifact(&root, &artifact)
            .expect_err("tampering must fail")
            .contains("hash mismatch"));

        let escaped = ArtifactRef {
            path: "../escape.json".to_string(),
            sha256: "0".repeat(64),
        };
        assert!(artifact_path(&root, &escaped).is_err());
        drop(root);
        fs::remove_dir_all(path).expect("remove artifact fixture");
    }

    #[test]
    fn root_path_replacement_cannot_redirect_lock_state_or_evidence() {
        let path = fixture_root("root-replacement");
        let moved = path.with_extension("moved");
        let root = OutputRoot::open(&path).expect("open original output root");
        fs::rename(&path, &moved).expect("move opened output root");
        fs::create_dir_all(&path).expect("create attacker replacement root");

        let _lock = OutputLock::acquire(&root).expect("lock original root FD");
        atomic_write_json(&root, Path::new("state.json"), &serde_json::json!({"v": 1}))
            .expect("write state through root FD");
        let artifact =
            write_content_addressed_text(&root, Path::new("evidence"), "record", "anchored")
                .expect("write evidence through root FD");

        assert!(moved.join(OUTPUT_LOCK_NAME).is_file());
        assert!(moved.join("state.json").is_file());
        assert!(moved.join(&artifact.path).is_file());
        assert!(!path.join(OUTPUT_LOCK_NAME).exists());
        assert!(!path.join("state.json").exists());
        assert!(!path.join("evidence").exists());

        drop(_lock);
        drop(root);
        fs::remove_dir_all(path).expect("remove replacement root");
        fs::remove_dir_all(moved).expect("remove original root");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_and_final_entry_cannot_escape_output_root() {
        use std::os::unix::fs::symlink;

        let base = fixture_root("symlink");
        let root_path = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&outside).expect("create outside directory");
        fs::write(outside.join("sentinel"), b"outside").expect("write outside sentinel");
        let root = OutputRoot::open(&root_path).expect("open output root");
        symlink(&outside, root_path.join("linked")).expect("create hostile parent symlink");

        let parent_error =
            write_content_addressed_text(&root, Path::new("linked"), "record", "must-not-escape")
                .expect_err("symlinked parent must fail");
        assert!(parent_error.contains("no-follow") || parent_error.contains("Not a directory"));
        let outside_entries = fs::read_dir(&outside)
            .expect("read outside directory")
            .map(|entry| entry.expect("outside entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(outside_entries, vec![OsString::from("sentinel")]);

        let body = b"outside";
        let digest = sha256_hex(body);
        let file_name = format!("record-{digest}.txt");
        fs::create_dir_all(root_path.join("evidence")).expect("create evidence directory");
        symlink(
            outside.join("sentinel"),
            root_path.join("evidence").join(&file_name),
        )
        .expect("create hostile final symlink");
        let artifact = ArtifactRef {
            path: format!("evidence/{file_name}"),
            sha256: digest,
        };
        assert!(verify_artifact(&root, &artifact).is_err());
        assert!(
            write_content_addressed_text(&root, Path::new("evidence"), "record", "outside")
                .is_err()
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("read outside sentinel"),
            b"outside"
        );
        assert!(fs::symlink_metadata(root_path.join(&artifact.path))
            .expect("inspect governed evidence")
            .file_type()
            .is_symlink());

        symlink(
            outside.join("sentinel"),
            root_path.join("prediction-loop-state.json"),
        )
        .expect("create hostile state symlink");
        atomic_write_json(
            &root,
            Path::new("prediction-loop-state.json"),
            &serde_json::json!({"safe": true}),
        )
        .expect("state checkpoint replaces symlink entry without following it");
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("read preserved outside sentinel"),
            b"outside"
        );
        assert!(
            !fs::symlink_metadata(root_path.join("prediction-loop-state.json"))
                .expect("inspect governed state")
                .file_type()
                .is_symlink()
        );

        drop(root);
        fs::remove_dir_all(base).expect("remove symlink fixture");
    }

    #[cfg(unix)]
    #[test]
    fn replaced_parent_directory_fails_closed_instead_of_writing_outside() {
        use std::os::unix::fs::symlink;

        let base = fixture_root("parent-replacement");
        let root_path = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(root_path.join("evidence")).expect("create original parent");
        fs::create_dir_all(&outside).expect("create outside parent");
        let root = OutputRoot::open(&root_path).expect("open output root");
        fs::rename(
            root_path.join("evidence"),
            root_path.join("evidence-original"),
        )
        .expect("replace original evidence parent");
        symlink(&outside, root_path.join("evidence")).expect("redirect parent with symlink");

        assert!(write_content_addressed_text(
            &root,
            Path::new("evidence"),
            "record",
            "must-not-escape",
        )
        .is_err());
        assert!(fs::read_dir(&outside)
            .expect("read outside parent")
            .next()
            .is_none());

        drop(root);
        fs::remove_dir_all(base).expect("remove parent replacement fixture");
    }

    #[test]
    fn atomic_checkpoint_surfaces_directory_sync_failure_and_leaves_no_temporary_file() {
        let path = fixture_root("fsync");
        let root = OutputRoot::open(&path).expect("open output root");
        let state_relative = Path::new("prediction-loop-state.json");
        atomic_write_json(
            &root,
            state_relative,
            &serde_json::json!({"frontier": "initial"}),
        )
        .expect("write initial checkpoint");
        inject_next_directory_sync_failure();
        let error = atomic_write_json(
            &root,
            state_relative,
            &serde_json::json!({"frontier": "baseline"}),
        )
        .expect_err("directory fsync failure must propagate");
        assert!(error.contains("injected test failure"));
        let visible: serde_json::Value =
            read_json(&root, state_relative).expect("published checkpoint remains valid JSON");
        assert_eq!(visible, serde_json::json!({"frontier": "baseline"}));
        assert!(fs::read_dir(&path)
            .expect("read output root")
            .all(|entry| !entry
                .expect("output entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")));

        drop(root);
        fs::remove_dir_all(path).expect("remove fsync fixture");
    }

    #[test]
    fn content_addressed_retry_resyncs_an_existing_entry() {
        let path = fixture_root("evidence-resync");
        let root = OutputRoot::open(&path).expect("open output root");
        let artifact =
            write_content_addressed_text(&root, Path::new("evidence"), "record", "durable")
                .expect("write initial evidence");

        inject_next_directory_sync_failure();
        let error = write_content_addressed_text(&root, Path::new("evidence"), "record", "durable")
            .expect_err("existing evidence retry must propagate directory fsync failure");
        assert!(error.contains("injected test failure"));
        let retried =
            write_content_addressed_text(&root, Path::new("evidence"), "record", "durable")
                .expect("retry re-syncs existing evidence");
        assert_eq!(retried, artifact);

        drop(root);
        fs::remove_dir_all(path).expect("remove evidence resync fixture");
    }
}
