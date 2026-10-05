//! The host loads one distinct witness key only after source/data/build gates.
//! This module never runs Cargo or serializes private signing material.
use anyhow::{ensure, Context};
use ed25519_dalek::SigningKey;
use hft_research_platform::admission::NativeAdmissionTrust;
use rustix::fs::{open, openat, Mode, OFlags};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

pub(in crate::mission_dispatch) fn load(
    path: &Path,
    key_id: &str,
    trust: &NativeAdmissionTrust,
    authority_public_keys: &[[u8; 32]],
    release_public_keys: &[[u8; 32]],
) -> anyhow::Result<SigningKey> {
    ensure!(
        trust.schema == "monday.native_reservation_trust.v1"
            && (1..=32).contains(&trust.native_reservation_keys.len())
            && !key_id.is_empty()
            && key_id.len() <= 128,
        "invalid native witness public trust"
    );
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "native witness path must be absolute and canonical"
    );
    let parent = path.parent().context("native witness parent is missing")?;
    let parent_fd = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let parent_file = File::from(parent_fd);
    let parent_metadata = parent_file.metadata()?;
    ensure!(
        parent_metadata.is_dir() && parent_metadata.permissions().mode() & 0o777 == 0o700,
        "native witness parent must be private mode 0700"
    );
    let fd = openat(
        &parent_file,
        path.file_name()
            .context("native witness filename is missing")?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let mut file = File::from(fd);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() == 32
            && metadata.permissions().mode() & 0o777 == 0o600
            && metadata.uid() == parent_metadata.uid(),
        "native witness must be a private regular 32-byte file"
    );
    let mut bytes = [0; 32];
    file.read_exact(&mut bytes)?;
    let key = SigningKey::from_bytes(&bytes);
    bytes.fill(0);
    let mut trailing = [0; 1];
    ensure!(
        file.read(&mut trailing)? == 0,
        "native witness changed length during read"
    );
    let public = key.verifying_key().to_bytes();
    ensure!(
        !key.verifying_key().is_weak()
            && trust.native_reservation_keys.get(key_id) == Some(&hex::encode(public)),
        "native witness differs from configured public trust"
    );
    ensure!(
        !authority_public_keys.contains(&public) && !release_public_keys.contains(&public),
        "native witness must be distinct from scientific authority and release signers"
    );
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn witness_rejects_fifo_symlink_permissions_and_authority_key_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let key = SigningKey::from_bytes(&[42; 32]);
        let trust = NativeAdmissionTrust {
            schema: "monday.native_reservation_trust.v1".into(),
            native_reservation_keys: [(
                "host-witness".into(),
                hex::encode(key.verifying_key().as_bytes()),
            )]
            .into(),
        };
        let path = root.join("witness.key");
        std::fs::write(&path, [42; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load(&path, "host-witness", &trust, &[], &[])
                .unwrap()
                .verifying_key(),
            key.verifying_key()
        );
        assert!(load(
            &path,
            "host-witness",
            &trust,
            &[key.verifying_key().to_bytes()],
            &[]
        )
        .is_err());
        assert!(load(
            &path,
            "host-witness",
            &trust,
            &[],
            &[key.verifying_key().to_bytes()]
        )
        .is_err());
        assert!(load(&path, "another-key", &trust, &[], &[]).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path, "host-witness", &trust, &[], &[]).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("link.key");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(load(&link, "host-witness", &trust, &[], &[]).is_err());
        let fifo = root.join("fifo.key");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(load(&fifo, "host-witness", &trust, &[], &[]).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load(&path, "host-witness", &trust, &[], &[]).is_err());
    }
}
