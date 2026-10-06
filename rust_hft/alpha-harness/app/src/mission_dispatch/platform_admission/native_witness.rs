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
    let expected = check_public_role(key_id, trust, authority_public_keys, release_public_keys)?;
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
    ensure!(
        key.verifying_key().to_bytes() == expected,
        "native witness differs from configured public trust"
    );
    Ok(key)
}

/// A restored signature must preserve the signer role without loading a key.
pub(in crate::mission_dispatch) fn check_public_role(
    key_id: &str,
    trust: &NativeAdmissionTrust,
    authority_public_keys: &[[u8; 32]],
    release_public_keys: &[[u8; 32]],
) -> anyhow::Result<[u8; 32]> {
    ensure!(
        trust.schema == "monday.native_reservation_trust.v1"
            && (1..=32).contains(&trust.native_reservation_keys.len())
            && !key_id.is_empty()
            && key_id.len() <= 128,
        "invalid native witness public trust"
    );
    let encoded = trust
        .native_reservation_keys
        .get(key_id)
        .context("native witness key is absent from trust")?;
    let public: [u8; 32] = hex::decode(encoded)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid native witness public key length"))?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&public)?;
    ensure!(
        !key.is_weak() && *encoded == hex::encode(public),
        "native witness public key is weak or noncanonical"
    );
    ensure!(
        !authority_public_keys.contains(&public) && !release_public_keys.contains(&public),
        "native witness must be distinct from scientific authority and release signers"
    );
    Ok(public)
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

    #[test]
    fn retained_valid_signature_cannot_reuse_scientific_or_release_role() {
        use hft_research_platform::revocation::{sign_revocation, NativeRequestRevocation};
        let key = SigningKey::from_bytes(&[42; 32]);
        let public = key.verifying_key().to_bytes();
        let trust = NativeAdmissionTrust {
            schema: "monday.native_reservation_trust.v1".into(),
            native_reservation_keys: [("host-witness".into(), hex::encode(public))].into(),
        };
        let signed = sign_revocation(
            NativeRequestRevocation {
                schema: "monday.native_request_revocation.v1".into(),
                tenant: "native-fixture".into(),
                request_sha256: "a".repeat(64),
                operation_sha256: "b".repeat(64),
                family_id: "native-family".into(),
                root_grant_sha256: "c".repeat(64),
                reason_receipt_sha256: "d".repeat(64),
                effective_ms: 2000,
                issued_ms: 1000,
            },
            "host-witness".into(),
            &key,
        )
        .unwrap();
        // The retained signature is cryptographically valid. Role separation
        // still rejects it without reading or requiring any private key file.
        trust.verify_revocation(&signed).unwrap();
        assert!(check_public_role(&signed.key_id, &trust, &[public], &[]).is_err());
        assert!(check_public_role(&signed.key_id, &trust, &[], &[public]).is_err());
        assert_eq!(
            check_public_role(&signed.key_id, &trust, &[], &[]).unwrap(),
            public
        );
        let mut weak = trust;
        let mut identity = [0; 32];
        identity[0] = 1;
        weak.native_reservation_keys
            .insert(signed.key_id.clone(), hex::encode(identity));
        assert!(check_public_role(&signed.key_id, &weak, &[], &[]).is_err());
    }
}
