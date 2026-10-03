use hft_research_platform::{
    build::{BuildArtifact, BuildSpec, BuiltExecutable},
    execution::{Backend, Profile},
    orchestrator::{Artifact, State, Task, TaskKind, TaskSpec},
    research::Run,
    sha256,
};
fn hash(c: char) -> String {
    c.to_string().repeat(64)
}
fn artifact() -> BuildArtifact {
    let build = BuildSpec {
        schema: 1,
        code_commit: "a".repeat(40),
        source_manifest_sha256: hash('a'),
        cargo_lock_sha256: hash('b'),
        toolchain_manifest_sha256: hash('c'),
        target: "x86_64-unknown-linux-gnu".into(),
        packages: vec!["science".into()],
        binaries: vec!["science".into()],
        features: vec![],
        default_features: false,
        profile: "research".into(),
        profile_manifest_sha256: hash('d'),
        rustflags_sha256: hash('e'),
        native_environment_sha256: hash('f'),
        builder_image: format!("builder@sha256:{}", hash('a')),
    };
    let build_id = build.id().unwrap();
    BuildArtifact {
        schema: 1,
        build,
        image: format!("science@sha256:{}", hash('b')),
        executables: vec![BuiltExecutable {
            name: "science".into(),
            blob: Artifact {
                key: format!("research/builds/{build_id}/science"),
                sha256: sha256(b"executable fixture"),
                bytes: 18,
            },
        }],
        release_receipt_sha256: hash('c'),
    }
}
fn run(artifact: &BuildArtifact) -> Run {
    Run {
        schema: 1,
        experiment_sha256: hash('a'),
        build_artifact_sha256: artifact.id().unwrap(),
        configuration_sha256: hash('b'),
        command: vec![
            "/usr/local/bin/science".into(),
            "--config".into(),
            "/work/config.json".into(),
        ],
        code_commit: artifact.build.code_commit.clone(),
        source_manifest_sha256: artifact.build.source_manifest_sha256.clone(),
        image: artifact.image.clone(),
        kind: TaskKind::Train,
        data_manifest_sha256: hash('d'),
        seed: 7,
        evaluator_sha256: hash('e'),
        evaluation_protocol_sha256: hash('f'),
        fit_identity_sha256: None,
    }
}
#[test]
fn parameter_experiments_and_retry_reuse_one_verified_build() {
    let artifact = artifact();
    let first = run(&artifact);
    let mut second = first.clone();
    second.seed = 8;
    second.configuration_sha256 = hash('c');
    second.data_manifest_sha256 = hash('e');
    assert_ne!(first.id().unwrap(), second.id().unwrap());
    assert_eq!(first.build_artifact_sha256, second.build_artifact_sha256);
    first.admit_build(&artifact).unwrap();
    second.admit_build(&artifact).unwrap();
    artifact
        .verify_bytes(|_| Ok(b"executable fixture".to_vec()))
        .unwrap();
    let spec = TaskSpec {
        schema: 1,
        kind: first.kind,
        run_manifest_sha256: first.id().unwrap(),
        view_manifest_sha256: first.data_manifest_sha256.clone(),
        source_sha256: first.source_manifest_sha256.clone(),
        image: first.image.clone(),
        command: first.command.clone(),
        profile: Profile {
            backend: Backend::KubernetesJob,
            cluster: "fixture".into(),
            namespace: "research".into(),
            service_account: "worker".into(),
            architecture: "amd64".into(),
            cpu_millis: 1000,
            memory_mib: 128,
            scratch_mib: 64,
            gpu: 0,
            acceptance_sha256: hash('a'),
            prepared_pvc: None,
            worker_secret: None,
        },
        timeout_ms: 10000,
        max_attempts: 2,
        output_prefix: "research/results".into(),
        fit_identity_sha256: None,
    };
    first.admit(&spec).unwrap();
    let mut task = Task::new(spec).unwrap();
    let lease = task.claim("owner", 100, 1000).unwrap();
    task.stop(State::Failed, true).unwrap();
    task.stopped(lease.attempt, lease.fence).unwrap();
    let retry = task.claim("owner", 200, 1000).unwrap();
    assert_eq!(retry.attempt, 2);
    assert_eq!(task.spec.run_manifest_sha256, first.id().unwrap());
    first.admit(&task.spec).unwrap();
    assert_eq!(task.deadline_ms, Some(10100));
}
#[test]
fn compiler_inputs_invalidate_build_and_cache_never_proves_executable_bytes() {
    let artifact = artifact();
    let base = artifact.build.id().unwrap();
    let variants = [
        {
            let mut b = artifact.build.clone();
            b.code_commit = "b".repeat(40);
            b
        },
        {
            let mut b = artifact.build.clone();
            b.toolchain_manifest_sha256 = hash('a');
            b
        },
        {
            let mut b = artifact.build.clone();
            b.features = vec!["science/other".into()];
            b
        },
        {
            let mut b = artifact.build.clone();
            b.cargo_lock_sha256 = hash('c');
            b
        },
        {
            let mut b = artifact.build.clone();
            b.native_environment_sha256 = hash('e');
            b
        },
        {
            let mut b = artifact.build.clone();
            b.profile = "release".into();
            b
        },
    ];
    for changed in variants {
        assert_ne!(base, changed.id().unwrap());
    }
    assert!(artifact
        .verify_bytes(|_| anyhow::bail!("missing executable"))
        .is_err());
    assert!(artifact
        .verify_bytes(|_| Ok(b"tampered cache bytes".to_vec()))
        .is_err());
    let mut changed = artifact.clone();
    changed.executables.clear();
    assert!(changed.id().is_err());
    assert!(artifact
        .admits_command(&["cargo".into(), "build".into(), "--workspace".into()])
        .is_err());
    let args = artifact.build.cargo_arguments().unwrap();
    assert!(!args
        .iter()
        .any(|a| a == "--workspace" || a == "--all-features"));
    let mut changed_run = run(&artifact);
    changed_run.image = format!("other@sha256:{}", hash('f'));
    assert!(changed_run.admit_build(&artifact).is_err());
}
