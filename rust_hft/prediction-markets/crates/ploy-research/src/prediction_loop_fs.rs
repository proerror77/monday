use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactRef {
    pub path: String,
    pub sha256: String,
}

pub(crate) struct OutputLock {
    file: File,
}

impl OutputLock {
    pub(crate) fn acquire(output_dir: &Path) -> Result<Self, String> {
        if fs::symlink_metadata(output_dir).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(format!(
                "prediction research output directory must not be a symlink: {}",
                output_dir.display()
            ));
        }
        create_dir_all_durable(output_dir, "output")?;
        let path = output_dir.join(".prediction-research-loop.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("open output lock {}: {error}", path.display()))?;
        file.try_lock_exclusive().map_err(|error| {
            format!(
                "prediction research output is already locked at {}: {error}",
                output_dir.display()
            )
        })?;
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

fn sync_directory(path: &Path, context: &str) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {context} directory {}: {error}", path.display()))
}

fn create_dir_all_durable(path: &Path, context: &str) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor
            .parent()
            .ok_or_else(|| format!("{context} directory has no existing ancestor"))?;
    }
    fs::create_dir_all(path)
        .map_err(|error| format!("create {context} directory {}: {error}", path.display()))?;
    for created in missing.iter().rev() {
        let parent = created
            .parent()
            .ok_or_else(|| format!("created {context} directory has no parent"))?;
        sync_directory(parent, context)?;
    }
    Ok(())
}

/// Remove only loop-owned crash leftovers whose filename ends in a parseable
/// UUID. The output lock must already be held, so no active writer can own one.
pub(crate) fn cleanup_stale_temporary_files(output_root: &Path) -> Result<usize, String> {
    fn visit(directory: &Path) -> Result<usize, String> {
        let mut removed = 0_usize;
        for entry in fs::read_dir(directory).map_err(|error| {
            format!(
                "read temporary-file directory {}: {error}",
                directory.display()
            )
        })? {
            let entry = entry.map_err(|error| format!("read temporary-file entry: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect temporary-file entry: {error}"))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "prediction output contains a symlink: {}",
                    entry.path().display()
                ));
            }
            if file_type.is_dir() {
                removed = removed.saturating_add(visit(&entry.path())?);
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let uuid_start = name.len().checked_sub(40);
            let owned_temporary = name.starts_with(".prediction-loop-tmp-")
                && name.ends_with(".tmp")
                && uuid_start.is_some_and(|start| {
                    start > 0
                        && name.as_bytes().get(start - 1) == Some(&b'-')
                        && uuid::Uuid::parse_str(&name[start..name.len() - 4]).is_ok()
                });
            if owned_temporary {
                fs::remove_file(entry.path()).map_err(|error| {
                    format!(
                        "remove stale temporary file {}: {error}",
                        entry.path().display()
                    )
                })?;
                removed = removed.saturating_add(1);
            }
        }
        if removed > 0 {
            File::open(directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    format!(
                        "sync temporary-file directory {}: {error}",
                        directory.display()
                    )
                })?;
        }
        Ok(removed)
    }

    visit(output_root)
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
    output_root: &Path,
    directory: &Path,
    prefix: &str,
    value: &T,
) -> Result<ArtifactRef, String> {
    let body = canonical_json_bytes(value)?;
    write_content_addressed(output_root, directory, prefix, "json", &body)
}

pub(crate) fn write_content_addressed_text(
    output_root: &Path,
    directory: &Path,
    prefix: &str,
    body: &str,
) -> Result<ArtifactRef, String> {
    write_content_addressed(output_root, directory, prefix, "txt", body.as_bytes())
}

fn write_content_addressed(
    output_root: &Path,
    directory: &Path,
    prefix: &str,
    extension: &str,
    body: &[u8],
) -> Result<ArtifactRef, String> {
    validate_artifact_write_directory(output_root, directory)?;
    create_dir_all_durable(directory, "evidence")?;
    reject_symlink_components(output_root, directory)?;
    let digest = sha256_hex(body);
    let path = directory.join(format!("{prefix}-{digest}.{extension}"));
    let temporary = directory.join(format!(
        ".prediction-loop-tmp-{prefix}-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                format!("create temporary evidence {}: {error}", temporary.display())
            })?;
        file.write_all(body).map_err(|error| {
            format!("write temporary evidence {}: {error}", temporary.display())
        })?;
        file.sync_all()
            .map_err(|error| format!("sync temporary evidence {}: {error}", temporary.display()))
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    let created = match fs::hard_link(&temporary, &path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(format!("publish evidence {}: {error}", path.display()));
        }
    };
    fs::remove_file(&temporary)
        .map_err(|error| format!("remove temporary evidence {}: {error}", temporary.display()))?;
    sync_directory(directory, "evidence")?;
    if !created {
        reject_symlink_components(output_root, &path)?;
        let existing = fs::read(&path)
            .map_err(|error| format!("read existing evidence {}: {error}", path.display()))?;
        if existing != body {
            return Err(format!("content-address collision at {}", path.display()));
        }
    }
    Ok(ArtifactRef {
        path: relative_path(output_root, &path)?,
        sha256: digest,
    })
}

fn validate_artifact_write_directory(output_root: &Path, directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(output_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "evidence root must not be a symlink: {}",
                output_root.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "inspect evidence root {}: {error}",
                output_root.display()
            ));
        }
    }
    let relative = directory
        .strip_prefix(output_root)
        .map_err(|_| format!("artifact path {} escapes output root", directory.display()))?;
    let mut current = output_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!("unsafe artifact path {}", directory.display()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "evidence path contains symlink component {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect evidence path {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("state path has no parent: {}", path.display()))?;
    create_dir_all_durable(parent, "state")?;
    let body = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize loop state: {error}"))?;
    let temporary = parent.join(format!(
        ".prediction-loop-tmp-state-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("create temporary state {}: {error}", temporary.display()))?;
        file.write_all(&body)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|error| format!("write temporary state {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("sync temporary state {}: {error}", temporary.display()))?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| format!("replace loop state {}: {error}", path.display()))?;
    sync_directory(parent, "state")
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let body = fs::read(path).map_err(|error| format!("read JSON {}: {error}", path.display()))?;
    serde_json::from_slice(&body).map_err(|error| format!("parse JSON {}: {error}", path.display()))
}

pub(crate) fn artifact_path(output_root: &Path, artifact: &ArtifactRef) -> Result<PathBuf, String> {
    let relative = Path::new(&artifact.path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("unsafe evidence path {}", artifact.path));
    }
    Ok(output_root.join(relative))
}

pub(crate) fn verify_artifact(
    output_root: &Path,
    artifact: &ArtifactRef,
) -> Result<PathBuf, String> {
    let path = artifact_path(output_root, artifact)?;
    reject_symlink_components(output_root, &path)?;
    let body = fs::read(&path)
        .map_err(|error| format!("read referenced evidence {}: {error}", path.display()))?;
    let digest = sha256_hex(&body);
    if digest != artifact.sha256 {
        return Err(format!("evidence hash mismatch for {}", path.display()));
    }
    let file_name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !file_name.ends_with(&artifact.sha256) {
        return Err(format!(
            "evidence filename is not content-addressed: {}",
            path.display()
        ));
    }
    Ok(path)
}

/// Read a verified content-addressed artifact without allowing an unbounded
/// allocation from a caller-controlled file.
pub(crate) fn read_verified_artifact_bounded(
    output_root: &Path,
    artifact: &ArtifactRef,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let path = artifact_path(output_root, artifact)?;
    reject_symlink_components(output_root, &path)?;
    let file = File::open(&path)
        .map_err(|error| format!("open referenced evidence {}: {error}", path.display()))?;
    let mut body = Vec::with_capacity(max_bytes.saturating_add(1).min(64 * 1024));
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|error| format!("read referenced evidence {}: {error}", path.display()))?;
    if body.len() > max_bytes {
        return Err(format!(
            "referenced evidence exceeds {max_bytes} bytes: {}",
            path.display()
        ));
    }
    let digest = sha256_hex(&body);
    if digest != artifact.sha256 {
        return Err(format!("evidence hash mismatch for {}", path.display()));
    }
    let file_name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !file_name.ends_with(&artifact.sha256) {
        return Err(format!(
            "evidence filename is not content-addressed: {}",
            path.display()
        ));
    }
    Ok(body)
}

fn reject_symlink_components(output_root: &Path, path: &Path) -> Result<(), String> {
    let root_metadata = fs::symlink_metadata(output_root)
        .map_err(|error| format!("inspect evidence root {}: {error}", output_root.display()))?;
    if root_metadata.file_type().is_symlink() {
        return Err(format!(
            "evidence root must not be a symlink: {}",
            output_root.display()
        ));
    }
    let relative = path
        .strip_prefix(output_root)
        .map_err(|_| format!("artifact path {} escapes output root", path.display()))?;
    let mut current = output_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!("unsafe artifact path {}", path.display()));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("inspect evidence path {}: {error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "evidence path contains symlink component {}",
                current.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map_err(|_| {
            format!(
                "path {} escapes output root {}",
                path.display(),
                root.display()
            )
        })
        .map(|relative| relative.to_string_lossy().into_owned())
}

pub(crate) fn next_attempt_dir(parent: &Path) -> Result<PathBuf, String> {
    create_dir_all_durable(parent, "attempt parent")?;
    for index in 1..=10_000_u32 {
        let path = parent.join(format!("attempt-{index:03}"));
        match fs::create_dir(&path) {
            Ok(()) => {
                sync_directory(parent, "attempt parent")?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "create attempt directory {}: {error}",
                    path.display()
                ))
            }
        }
    }
    Err(format!(
        "attempt directory limit exceeded under {}",
        parent.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_addressed_writer_rejects_parent_directory_escape_before_creation() {
        let root = tempfile::tempdir().unwrap();
        let output_root = root.path().join("output");
        let escaped_directory = output_root.join("inside/../../escaped");

        assert!(write_content_addressed_json(
            &output_root,
            &escaped_directory,
            "record",
            &serde_json::json!({"a": 1}),
        )
        .expect_err("a writer directory must not escape its output root")
        .contains("unsafe artifact path"));
        assert!(!escaped_directory.exists());
    }

    #[test]
    fn content_addressed_artifact_rejects_tampering_and_escape() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-loop-fs-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        let artifact = write_content_addressed_json(
            &root,
            &root.join("evidence"),
            "record",
            &serde_json::json!({"b": 2, "a": 1}),
        )
        .expect("write artifact");
        verify_artifact(&root, &artifact).expect("verify artifact");
        let duplicate = write_content_addressed_json(
            &root,
            &root.join("evidence"),
            "record",
            &serde_json::json!({"a": 1, "b": 2}),
        )
        .expect("reuse identical artifact");
        assert_eq!(duplicate, artifact);
        assert!(fs::read_dir(root.join("evidence"))
            .expect("read evidence directory")
            .all(|entry| !entry
                .expect("evidence entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));

        let path = artifact_path(&root, &artifact).expect("artifact path");
        fs::write(&path, b"tampered").expect("tamper artifact");
        assert!(verify_artifact(&root, &artifact)
            .expect_err("tampering must fail")
            .contains("hash mismatch"));
        assert!(write_content_addressed_json(
            &root,
            &root.join("evidence"),
            "record",
            &serde_json::json!({"a": 1, "b": 2}),
        )
        .expect_err("existing artifact must not be replaced")
        .contains("content-address collision"));
        assert_eq!(
            fs::read(&path).expect("read preserved artifact"),
            b"tampered"
        );
        assert!(fs::read_dir(root.join("evidence"))
            .expect("read evidence directory")
            .all(|entry| !entry
                .expect("evidence entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));

        let escaped = ArtifactRef {
            path: "../escape.json".to_string(),
            sha256: "0".repeat(64),
        };
        assert!(artifact_path(&root, &escaped).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_removes_only_uuid_bound_loop_temporaries() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-loop-cleanup-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).expect("create nested output");
        let stale = nested.join(format!(
            ".prediction-loop-tmp-evidence-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let deceptive = nested.join(format!(
            ".evidence-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let unrelated = nested.join(".user.tmp");
        fs::write(&stale, b"complete but unpublished").expect("write stale temp");
        fs::write(&deceptive, b"not loop-owned").expect("write deceptive temp");
        fs::write(&unrelated, b"user file").expect("write unrelated temp");

        assert_eq!(cleanup_stale_temporary_files(&root).expect("cleanup"), 1);
        assert!(!stale.exists());
        assert!(deceptive.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(root).expect("remove cleanup fixture");
    }

    #[cfg(unix)]
    #[test]
    fn artifact_verification_rejects_symlink_components() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-loop-symlink-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let outside = root.with_extension("outside");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&outside).expect("create outside");
        let body = b"outside evidence";
        let digest = sha256_hex(body);
        let filename = format!("record-{digest}.txt");
        fs::write(outside.join(&filename), body).expect("write outside evidence");
        symlink(&outside, root.join("linked")).expect("create directory symlink");
        let artifact = ArtifactRef {
            path: format!("linked/{filename}"),
            sha256: digest,
        };

        assert!(verify_artifact(&root, &artifact)
            .expect_err("symlinked evidence must fail closed")
            .contains("symlink"));
        fs::remove_dir_all(root).expect("remove root");
        fs::remove_dir_all(outside).expect("remove outside");
    }
}
