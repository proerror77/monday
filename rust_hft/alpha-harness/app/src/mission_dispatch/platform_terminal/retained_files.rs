//! Bind every retained observation and native artifact before the durable audit.
//! Only the authenticated ledger may select a package for publication retry.
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    files: BTreeMap<String, Identity>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
    sha256: String,
    bytes: u64,
}

pub(super) fn retain(root: &Path) -> anyhow::Result<String> {
    let manifest = Manifest {
        schema: "monday.native_terminal_observation_files.v1".into(),
        files: inventory(root)?,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    super::super::platform_admission::retain(&root.join("retained-manifest.json"), &bytes)?;
    Ok(hft_research_platform::sha256(&bytes))
}

pub(super) fn verify(root: &Path, expected: &str) -> anyhow::Result<()> {
    let bytes = super::super::platform_admission::file_bytes(
        &root.join("retained-manifest.json"),
        1024 * 1024,
        true,
    )?;
    ensure!(
        hft_research_platform::sha256(&bytes) == expected,
        "retained terminal manifest changed"
    );
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.schema == "monday.native_terminal_observation_files.v1"
            && manifest.files == inventory(root)?,
        "retained observation or native artifact coverage changed"
    );
    Ok(())
}

pub(super) fn objects(root: &Path) -> anyhow::Result<Vec<(String, String, u64)>> {
    let mut files = inventory(root)?;
    for name in ["terminal-audit.json", "retained-manifest.json"] {
        add(root, name, &mut files)?;
    }
    Ok(files
        .into_iter()
        .map(|(name, file)| (name, file.sha256, file.bytes))
        .collect())
}

fn inventory(root: &Path) -> anyhow::Result<BTreeMap<String, Identity>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non UTF-8 terminal file"))?;
        if name == "terminal-audit.json" || name == "retained-manifest.json" {
            continue;
        }
        if name == "round-readback" {
            ensure!(
                entry.file_type()?.is_dir(),
                "native round cache is not a regular directory"
            );
            for round in std::fs::read_dir(entry.path())? {
                let round = round?;
                let filename = round
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("non UTF-8 round file"))?;
                add(root, &format!("round-readback/{filename}"), &mut files)?;
            }
        } else {
            add(root, &name, &mut files)?;
        }
    }
    ensure!(
        !files.is_empty() && files.len() <= 520,
        "unbounded retained terminal coverage"
    );
    Ok(files)
}

fn add(root: &Path, name: &str, files: &mut BTreeMap<String, Identity>) -> anyhow::Result<()> {
    use rustix::fs::{open, Mode, OFlags};
    ensure!(
        !name.starts_with('/')
            && !name.contains("..")
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b)),
        "unsafe retained file path"
    );
    let path = root.join(name);
    ensure!(
        path.parent()
            .context("retained file parent absent")?
            .canonicalize()?
            == path.parent().unwrap(),
        "retained file parent changed"
    );
    let mut file = File::from(open(
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= 512 * 1024 * 1024,
        "retained file is absent, nonregular or unbounded"
    );
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .context("retained byte count overflow")?;
        ensure!(
            bytes <= metadata.len(),
            "retained file grew during readback"
        );
        digest.update(&buffer[..n]);
    }
    ensure!(
        bytes == metadata.len()
            && files
                .insert(
                    name.into(),
                    Identity {
                        sha256: hex::encode(digest.finalize()),
                        bytes
                    }
                )
                .is_none(),
        "retained file changed or repeated"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_retained_observation_bytes_and_coverage_are_content_bound() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::write(root.join("platform-snapshot.json"), b"actual fixture bytes")?;
        let digest = super::retain(&root)?;
        super::verify(&root, &digest)?;
        std::fs::write(root.join("platform-snapshot.json"), b"changed bytes")?;
        assert!(super::verify(&root, &digest).is_err());
        std::fs::write(root.join("platform-snapshot.json"), b"actual fixture bytes")?;
        std::fs::write(root.join("extra.json"), b"new unbound bytes")?;
        assert!(super::verify(&root, &digest).is_err());
        std::fs::remove_file(root.join("extra.json"))?;
        std::fs::remove_file(root.join("platform-snapshot.json"))?;
        assert!(super::verify(&root, &digest).is_err());
        Ok(())
    }
}
