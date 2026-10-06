use ed25519_dalek::{Signer, SigningKey};
use hft_research_platform::{
    build::BuildArtifact,
    identity,
    orchestrator::Artifact,
    release::{
        BuildReleaseReceipt, BuildReleaseTrust, ReleaseProducer, SignedBuildRelease, SourceArchive,
    },
    sha256,
};

pub fn attest(
    mut artifact: BuildArtifact,
) -> (BuildArtifact, SignedBuildRelease, BuildReleaseTrust) {
    let source = SourceArchive {
        schema: 1,
        code_commit: artifact.build.code_commit.clone(),
        archive: Artifact {
            key: format!("research/sources/{}/source.tar", artifact.build.code_commit),
            sha256: sha256(b"source fixture"),
            bytes: 14,
        },
    };
    artifact.build.source_manifest_sha256 = identity(&source).unwrap();
    let build_id = artifact.build.id().unwrap();
    for executable in &mut artifact.executables {
        executable.blob.key = format!("research/builds/{build_id}/{}", executable.name);
    }
    let key = SigningKey::from_bytes(&[17; 32]);
    let mut signed = SignedBuildRelease {
        schema: 1,
        key_id: "fixture-release".into(),
        receipt: BuildReleaseReceipt {
            schema: 1,
            build_sha256: build_id,
            source,
            image: artifact.image.clone(),
            target: artifact.build.target.clone(),
            executables: artifact.executables.clone(),
            producer: ReleaseProducer {
                repository: "fixture/monday".into(),
                workflow_path: ".github/workflows/ploy-ci.yml".into(),
                source_sha: artifact.build.code_commit.clone(),
                run_id: 123,
                run_attempt: 2,
                job_id: 456,
            },
            publication_readback_sha256: sha256(b"publication fixture"),
        },
        signature_hex: String::new(),
    };
    signed.signature_hex = key.sign(&signed.signing_bytes().unwrap()).to_string();
    // Display encoding is uppercase; the wire contract requires lowercase.
    signed.signature_hex.make_ascii_lowercase();
    artifact.release_receipt_sha256 = identity(&signed).unwrap();
    let public = key
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let trust = BuildReleaseTrust {
        schema: 1,
        repository: "fixture/monday".into(),
        producer_workflow_path: ".github/workflows/ploy-ci.yml".into(),
        keys: [(signed.key_id.clone(), public)].into(),
    };
    (artifact, signed, trust)
}

#[cfg(feature = "publisher")]
// Shared fixtures expose corruption handles used only by the import regression.
#[allow(dead_code)]
pub mod published;
