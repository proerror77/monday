use super::*;
use hft_research_platform::{
    execution::{render, resource_name, Acceptance, Backend, Profile},
    orchestrator::{TaskKind, WorkerConfigurationRef},
};
use serde_json::json;

fn fixture() -> (
    TaskSpec,
    Lease,
    ExecutionHandle,
    Value,
    Value,
    DateTime<Utc>,
) {
    let digest = |c: char| c.to_string().repeat(64);
    let spec = TaskSpec {
        schema: 1,
        kind: TaskKind::CexCampaign,
        run_manifest_sha256: digest('a'),
        view_manifest_sha256: digest('b'),
        source_sha256: digest('c'),
        image: format!("registry/worker@sha256:{}", digest('d')),
        command: vec![
            "monday-cex-worker".into(),
            "mission".into(),
            "campaign-execute".into(),
            "--request".into(),
            "/config/campaign.json".into(),
        ],
        profile: Profile {
            backend: Backend::KubernetesJob,
            cluster: "fixture-cluster".into(),
            namespace: "monday-research".into(),
            service_account: "research-worker".into(),
            architecture: "amd64".into(),
            cpu_millis: 2000,
            memory_mib: 2048,
            scratch_mib: 1024,
            gpu: 0,
            acceptance_sha256: digest('e'),
            prepared_pvc: None,
            worker_secret: Some("campaign-config".into()),
        },
        timeout_ms: 60_000,
        max_attempts: 1,
        output_prefix: "research/campaign-results".into(),
        fit_identity_sha256: None,
        worker_configuration: Some(WorkerConfigurationRef {
            schema: "monday.worker_configuration.v1".into(),
            secret_name: "campaign-config".into(),
            secret_uid: "fixture-secret-uid".into(),
            configuration_sha256: digest('f'),
        }),
    };
    let observed: DateTime<Utc> = "2026-10-05T04:00:30Z".parse().unwrap();
    let lease = Lease {
        task_id: spec.id().unwrap(),
        attempt: 1,
        fence: 7,
        owner: "fixture-host".into(),
        expires_ms: observed.timestamp_millis() - 1,
    };
    let handle = ExecutionHandle {
        backend: spec.profile.backend,
        cluster: spec.profile.cluster.clone(),
        namespace: spec.profile.namespace.clone(),
        name: resource_name(&lease),
        uid: "original-job-uid".into(),
        attempt: lease.attempt,
        fence: lease.fence,
        task_id: lease.task_id.clone(),
        request_sha256: spec.id().unwrap(),
    };
    let mut job = render(
        &spec,
        &lease,
        &Acceptance {
            profile: spec.profile.clone(),
            ready: true,
            process_tree_stop: true,
            immutable_prepared_mount: true,
            command_reattach: false,
            artifact_readback: true,
        },
    )
    .unwrap();
    job["metadata"]["uid"] = json!(handle.uid);
    job["status"] = json!({"active":0,"succeeded":1,"startTime":"2026-10-05T04:00:00Z","conditions":[{"type":"Complete","status":"True"}]});
    let status = |name: &str, started: &str, finished: &str| json!({"name":name,"restartCount":0,"imageID":format!("containerd://{}",spec.image),"state":{"terminated":{"exitCode":0,"startedAt":started,"finishedAt":finished}}});
    let mut pod = job["spec"]["template"].clone();
    pod["apiVersion"] = json!("v1");
    pod["kind"] = json!("Pod");
    pod["metadata"]["uid"] = json!("original-pod-uid");
    pod["metadata"]["namespace"] = json!(handle.namespace);
    pod["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"Job","name":handle.name,"uid":handle.uid,"controller":true}]);
    pod["status"] = json!({"phase":"Succeeded","containerStatuses":[status("worker","2026-10-05T04:00:02Z","2026-10-05T04:00:20Z")],"initContainerStatuses":[status("stage-configuration","2026-10-05T04:00:00Z","2026-10-05T04:00:01Z")]});
    (spec, lease, handle, job, pod, observed)
}

#[test]
fn original_job_owner_and_all_processes_must_be_stopped() {
    let (spec, lease, handle, job, pod, observed) = fixture();
    let proof = verify(&spec, &lease, &handle, &job, &pod, observed).unwrap();
    assert_eq!(proof.handle, handle);
    assert_eq!(proof.pod_uid, "original-pod-uid");
    assert_eq!(proof.observed_at, observed);
    assert_eq!(
        proof.job_sha256,
        alpha_domain::canonical_json_hash(&job).unwrap()
    );
    assert_eq!(
        proof.pod_sha256,
        alpha_domain::canonical_json_hash(&pod).unwrap()
    );
    assert_eq!(proof.worker_exit_code, 0);
    assert_eq!(proof.job, job);
    assert_eq!(proof.pod, pod);
    // Historical lease expiry does not renew execution or erase stop evidence.
    assert!(lease.expires_ms < observed.timestamp_millis());
    assert!(read("foreign-cluster", &spec, &lease, &handle).is_err());
    let mut normalized_job = job.clone();
    let mut normalized_pod = pod.clone();
    for object in [&mut normalized_job["spec"]["template"], &mut normalized_pod] {
        for section in ["requests", "limits"] {
            object["spec"]["containers"][0]["resources"][section]["cpu"] = json!("2");
            object["spec"]["containers"][0]["resources"][section]["memory"] = json!("2Gi");
        }
    }
    verify(
        &spec,
        &lease,
        &handle,
        &normalized_job,
        &normalized_pod,
        observed,
    )
    .unwrap();

    for (pointer, value) in [
        (
            "/metadata/ownerReferences/0/uid",
            json!("recreated-job-uid"),
        ),
        ("/metadata/labels/monday.io~1fence", json!("8")),
        (
            "/metadata/annotations/monday.io~1request-sha256",
            json!("f".repeat(64)),
        ),
        ("/status/phase", json!("Running")),
        ("/status/containerStatuses/0/restartCount", json!(1)),
        (
            "/status/containerStatuses/0/imageID",
            json!(format!("registry/worker@sha256:{}", "a".repeat(64))),
        ),
        (
            "/status/containerStatuses/0/state/terminated/finishedAt",
            json!("2026-10-05T04:01:00Z"),
        ),
        (
            "/status/initContainerStatuses/0/name",
            json!("foreign-initializer"),
        ),
        (
            "/status/initContainerStatuses/0/state",
            json!({"running":{"startedAt":"2026-10-05T04:00:00Z"}}),
        ),
        ("/status/initContainerStatuses/0/restartCount", json!(1)),
        ("/spec/containers/0/resources/limits/cpu", json!("4000m")),
    ] {
        let mut changed = pod.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            verify(&spec, &lease, &handle, &job, &changed, observed).is_err(),
            "accepted changed Pod {pointer}"
        );
    }
    for (pointer, value) in [
        ("/metadata/uid", json!("recreated-job-uid")),
        ("/spec/backoffLimit", json!(1)),
        ("/spec/activeDeadlineSeconds", json!(61)),
        ("/status/active", json!(1)),
    ] {
        let mut changed = job.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            verify(&spec, &lease, &handle, &changed, &pod, observed).is_err(),
            "accepted changed Job {pointer}"
        );
    }
    let mut failed = pod.clone();
    let mut failed_job = job.clone();
    failed_job["status"]["conditions"] = json!([{"type":"Failed","status":"True"}]);
    failed_job["status"]["succeeded"] = json!(0);
    failed_job["status"]["failed"] = json!(1);
    failed["status"]["phase"] = json!("Failed");
    failed["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"] = json!(42);
    assert_eq!(
        verify(&spec, &lease, &handle, &failed_job, &failed, observed)
            .unwrap()
            .worker_exit_code,
        42
    );
}

#[test]
fn provider_readback_rejects_missing_or_ambiguous_owner_pods() {
    use std::os::unix::fs::PermissionsExt;
    let (spec, lease, handle, job, pod, _) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let tool = directory.path().join("kubectl");
    std::fs::write(&tool, "#!/bin/sh\ncase \"$*\" in\n*\" get job \"*) cat \"$(dirname \"$0\")/job.json\";;\n*\" get pods \"*) cat \"$(dirname \"$0\")/pods.json\";;\n*) exit 2;;\nesac\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        directory.path().join("job.json"),
        serde_json::to_vec(&job).unwrap(),
    )
    .unwrap();
    for pods in [vec![], vec![pod.clone(), pod.clone()]] {
        std::fs::write(
            directory.path().join("pods.json"),
            serde_json::to_vec(&json!({"items":pods})).unwrap(),
        )
        .unwrap();
        assert!(read_with(&tool, &spec.profile.cluster, &spec, &lease, &handle).is_err());
    }
    std::fs::write(
        directory.path().join("pods.json"),
        serde_json::to_vec(&json!({"items":[pod]})).unwrap(),
    )
    .unwrap();
    assert!(read_with(&tool, &spec.profile.cluster, &spec, &lease, &handle).is_ok());
}
