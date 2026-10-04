//! Bounded research artifact transport shared by CEX and Prediction workers.
//! This crate owns no data acquisition, model, market task, or execution adapter.
use anyhow::{bail, Context};
use reqwest::{
    blocking::Client,
    header::{HeaderValue, CONTENT_TYPE},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MAX_RESULT_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;

pub fn ensure_real_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory for output safety")?
            .join(path)
    };
    ensure_real_directory_at(&normalize_platform_root_alias(&absolute_path)?, label)
}

pub fn temporary_output_file(path: &Path, prefix: &str) -> anyhow::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_real_directory(parent, "temporary output parent")?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(parent)
        .with_context(|| format!("create private temporary output in {}", parent.display()))
}

pub fn ensure_output_path_is_not_symlink(path: &Path, label: &str) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{label} path cannot be a symbolic link: {}", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("inspect {label} path {}", path.display()))
        }
    }
}

pub fn persist_output_file(
    file: tempfile::NamedTempFile,
    path: &Path,
    label: &str,
) -> anyhow::Result<()> {
    ensure_output_path_is_not_symlink(path, label)?;
    file.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("atomically publish {label} to {}", path.display()))?;
    Ok(())
}

fn normalize_platform_root_alias(path: &Path) -> anyhow::Result<PathBuf> {
    let mut normalized = PathBuf::new();
    let mut resolved_root_component = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => normalized.push(".."),
            Component::Normal(component) if !resolved_root_component => {
                let root_component = normalized.join(component);
                normalized = if is_platform_root_alias(component) {
                    match std::fs::symlink_metadata(&root_component) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            std::fs::canonicalize(&root_component).with_context(|| {
                                format!(
                                    "resolve platform root alias for output safety: {}",
                                    root_component.display()
                                )
                            })?
                        }
                        Ok(_) | Err(_) => root_component,
                    }
                } else {
                    root_component
                };
                resolved_root_component = true;
            }
            Component::Normal(component) => {
                normalized.push(component);
                resolved_root_component = true;
            }
        }
    }
    Ok(normalized)
}

fn is_platform_root_alias(component: &std::ffi::OsStr) -> bool {
    // On macOS these are symlinks into /private. They are the only root-level
    // aliases we normalize; every other symlink must fail the directory walk.
    #[cfg(target_os = "macos")]
    {
        component == std::ffi::OsStr::new("tmp") || component == std::ffi::OsStr::new("var")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = component;
        false
    }
}

fn ensure_real_directory_at(path: &Path, label: &str) -> anyhow::Result<()> {
    // Root-level platform aliases (for example macOS /var) are normalized
    // above. Every remaining component is application-controlled and must not
    // resolve through a symlink.
    if let Some(parent) = path.parent().filter(|parent| *parent != path) {
        ensure_real_directory_at(parent, label)?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create {label} directory {}", path.display()))
                }
            }
            std::fs::symlink_metadata(path)
                .with_context(|| format!("inspect {label} directory {}", path.display()))?
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect {label} directory {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "{label} directory cannot be a symbolic link: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!("{label} path must be a directory: {}", path.display());
    }
    Ok(())
}

pub fn normalized_sha256(label: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA256 is invalid");
    }
    Ok(value)
}

pub fn fetch_to_file(
    client: &Client,
    source: &str,
    destination: &Path,
    max_bytes: u64,
) -> anyhow::Result<(u64, String)> {
    let mut reader: Box<dyn Read> =
        if source.starts_with("http://") || source.starts_with("https://") {
            let response = client
                .get(source)
                .send()
                .map_err(reqwest::Error::without_url)?
                .error_for_status()
                .map_err(reqwest::Error::without_url)?;
            if response
                .content_length()
                .is_some_and(|length| length > max_bytes)
            {
                bail!("source exceeds the allowed size");
            }
            Box::new(response)
        } else {
            let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
            let file = File::open(path)
                .with_context(|| format!("failed to open local source {}", path.display()))?;
            if file.metadata()?.len() > max_bytes {
                bail!("source exceeds the allowed size");
            }
            Box::new(file)
        };
    let mut temporary = temporary_output_file(destination, ".monday-fetch-")?;
    let bytes = std::io::copy(
        &mut reader.by_ref().take(max_bytes + 1),
        temporary.as_file_mut(),
    )?;
    temporary.as_file().sync_all()?;
    if bytes > max_bytes {
        bail!("source exceeds the allowed size");
    }
    persist_output_file(temporary, destination, "fetched source")?;
    Ok((bytes, sha256_file(destination)?))
}

pub fn create_bundle<'a>(
    work_dir: &Path,
    bundle: &Path,
    roots: impl IntoIterator<Item = &'a PathBuf>,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    for root in roots {
        collect_files(root, &mut files)?;
    }
    files.sort();
    let temporary = temporary_output_file(bundle, ".monday-bundle-")?;
    let mut archive = ZipWriter::new(temporary.reopen()?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    for path in files {
        let name = path
            .strip_prefix(work_dir)
            .with_context(|| format!("bundle path escapes work directory: {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        archive.start_file(name, options)?;
        std::io::copy(&mut File::open(path)?, &mut archive)?;
    }
    let file = archive.finish()?;
    file.sync_all()?;
    drop(file);
    persist_output_file(temporary, bundle, "bundle")?;
    Ok(())
}

pub fn publish_result(client: &Client, destination: &str, bundle: &Path) -> anyhow::Result<()> {
    checked_result_bundle_bytes(bundle)?;
    publish_immutable_file(client, destination, bundle, "application/zip")
}

pub fn publish_immutable_file(
    client: &Client,
    destination: &str,
    source: &Path,
    content_type: &'static str,
) -> anyhow::Result<()> {
    if destination.starts_with("http://") || destination.starts_with("https://") {
        client
            .put(destination)
            .header(CONTENT_TYPE, HeaderValue::from_static(content_type))
            .header("x-oss-forbid-overwrite", "true")
            .body(File::open(source)?)
            .send()
            .map_err(reqwest::Error::without_url)?
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        return Ok(());
    }
    let path = Path::new(destination.strip_prefix("file://").unwrap_or(destination));
    let mut output = temporary_output_file(path, ".monday-result-")?;
    std::io::copy(&mut File::open(source)?, output.as_file_mut())?;
    output.as_file().sync_all()?;
    match output.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("result destination already exists: {}", path.display())
        }
        Err(error) => Err(error.error)
            .with_context(|| format!("atomically publish result to {}", path.display())),
    }
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(hex::encode(digest.finalize()))
}

pub fn configured_binary(environment: &str, installed_path: &Path) -> anyhow::Result<PathBuf> {
    let path = binary_path(
        std::env::var_os(environment)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        installed_path,
    )?;
    if !path.is_file() {
        bail!(
            "configured research binary does not exist: {}",
            path.display()
        );
    }
    Ok(path)
}

fn binary_path(configured: Option<PathBuf>, installed_path: &Path) -> anyhow::Result<PathBuf> {
    let path = configured.unwrap_or_else(|| installed_path.to_path_buf());
    if !path.is_absolute() {
        bail!("research binary path must be absolute: {}", path.display());
    }
    Ok(path)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if metadata.file_type().is_symlink() {
            bail!("bundle input cannot contain symbolic links");
        }
        if metadata.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if metadata.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

pub fn checked_result_bundle_bytes(bundle: &Path) -> anyhow::Result<u64> {
    let bundle_bytes = bundle.metadata()?.len();
    if bundle_bytes > MAX_RESULT_BUNDLE_BYTES {
        bail!(
            "result bundle exceeds the allowed size: {} bytes > {} bytes",
            bundle_bytes,
            MAX_RESULT_BUNDLE_BYTES
        );
    }
    Ok(bundle_bytes)
}

struct BoundedWriter<W> {
    inner: W,
    remaining: u64,
    max_bytes: u64,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let allowed = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        if allowed == 0 && !buffer.is_empty() {
            return Err(std::io::Error::other(format!(
                "serialized JSON exceeds maximum {} bytes",
                self.max_bytes
            )));
        }
        let written = self.inner.write(&buffer[..allowed])?;
        self.remaining = self
            .remaining
            .saturating_sub(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    write_json_atomic_bounded(path, value, u64::MAX)
}

pub fn write_json_atomic_bounded(
    path: &Path,
    value: &impl serde::Serialize,
    max_bytes: u64,
) -> anyhow::Result<()> {
    let mut temporary = temporary_output_file(path, ".monday-json-")?;
    serde_json::to_writer_pretty(
        BoundedWriter {
            inner: temporary.as_file_mut(),
            remaining: max_bytes,
            max_bytes,
        },
        value,
    )?;
    temporary.as_file().sync_all()?;
    persist_output_file(temporary, path, "JSON evidence")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_default_uses_the_installed_product_path() {
        let installed = Path::new("/usr/local/bin/monday-prediction-research");
        assert_eq!(binary_path(None, installed).unwrap(), installed);
    }

    #[test]
    fn binary_override_must_be_an_explicit_absolute_path() {
        let installed = Path::new("/usr/local/bin/monday-prediction-research");
        let root = tempfile::tempdir().unwrap();
        let configured = root.path().join("research-fixture");
        assert_eq!(
            binary_path(Some(configured.clone()), installed).unwrap(),
            configured
        );
        assert!(
            binary_path(Some(PathBuf::from("relative-fixture")), installed)
                .unwrap_err()
                .to_string()
                .contains("must be absolute")
        );
    }

    #[test]
    fn platform_root_aliases_are_explicitly_whitelisted() {
        assert!(!is_platform_root_alias(std::ffi::OsStr::new("evil")));

        #[cfg(target_os = "macos")]
        {
            assert!(is_platform_root_alias(std::ffi::OsStr::new("tmp")));
            assert!(is_platform_root_alias(std::ffi::OsStr::new("var")));
        }
    }
}
