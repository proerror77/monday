use hft_research_worker_contracts::{
    execution::{Backend, Profile},
    task::{Artifact, AttemptContext, Lease, ResultReceipt, TaskKind, TaskSpec},
};

fn context() -> AttemptContext {
    let spec = TaskSpec {
        schema: 1,
        kind: TaskKind::Train,
        run_manifest_sha256: "a".repeat(64),
        view_manifest_sha256: "b".repeat(64),
        source_sha256: "c".repeat(64),
        image: format!("synthetic-fixture@sha256:{}", "d".repeat(64)),
        command: vec!["/usr/local/bin/synthetic-fixture".into()],
        profile: Profile {
            backend: Backend::KubernetesJob,
            cluster: "synthetic-fixture".into(),
            namespace: "research".into(),
            service_account: "worker".into(),
            architecture: "amd64".into(),
            cpu_millis: 1000,
            memory_mib: 128,
            scratch_mib: 64,
            gpu: 0,
            acceptance_sha256: "e".repeat(64),
            prepared_pvc: None,
            worker_secret: None,
        },
        timeout_ms: 1000,
        max_attempts: 2,
        output_prefix: "research/synthetic-fixture".into(),
        fit_identity_sha256: Some("f".repeat(64)),
        worker_configuration: None,
    };
    AttemptContext {
        lease: Lease {
            task_id: spec.id().unwrap(),
            attempt: 1,
            fence: 7,
            owner: "synthetic-controller".into(),
            expires_ms: 2000,
        },
        spec,
    }
}

#[test]
fn independent_receiver_rejects_changed_task_lease_fit_and_output_identity() {
    let original = context();
    let bytes = serde_json::to_vec(&original).unwrap();
    let received: AttemptContext = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&received).unwrap(), bytes);
    received.validate().unwrap();
    let receipt = ResultReceipt {
        task_id: received.lease.task_id.clone(),
        attempt: received.lease.attempt,
        fence: received.lease.fence,
        view_manifest_sha256: received.spec.view_manifest_sha256.clone(),
        source_sha256: received.spec.source_sha256.clone(),
        image: received.spec.image.clone(),
        fit_identity_sha256: received.spec.fit_identity_sha256.clone(),
        artifacts: vec![Artifact {
            key: format!("{}result.json", received.output_prefix()),
            sha256: "a".repeat(64),
            bytes: 1,
        }],
        checkpoint: None,
        prepared_view: None,
    };
    receipt.validate(&received.spec, &received.lease).unwrap();

    let mut foreign_task = received.clone();
    foreign_task.spec.command.push("--changed-input".into());
    assert!(foreign_task.validate().is_err());
    let mut unfenced = received.clone();
    unfenced.lease.fence = 0;
    assert!(unfenced.validate().is_err());
    let mut stale = receipt.clone();
    stale.fence -= 1;
    assert!(stale.validate(&received.spec, &received.lease).is_err());
    let mut wrong_fit = receipt.clone();
    wrong_fit.fit_identity_sha256 = Some("1".repeat(64));
    assert!(wrong_fit.validate(&received.spec, &received.lease).is_err());
    let mut foreign_output = receipt;
    foreign_output.artifacts[0].key = "research/other-attempt/result.json".into();
    assert!(foreign_output
        .validate(&received.spec, &received.lease)
        .is_err());
}
