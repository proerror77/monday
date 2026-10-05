//! Independent Kubernetes observation of the original admitted Attempt. A
//! missing resource, unknown ownership or running process yields no proof.
use crate::prediction_dispatch::{kubectl_binary, kubectl_json};
use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use hft_research_platform::{
    execution::ExecutionHandle,
    orchestrator::{Lease, TaskSpec},
};
use serde_json::Value;
use std::path::Path;

pub(super) struct VerifiedStoppedExecution {
    pub(super) handle: ExecutionHandle,
    pub(super) pod_uid: String,
    pub(super) observed_at: DateTime<Utc>,
    pub(super) job_sha256: String,
    pub(super) pod_sha256: String,
    pub(super) worker_exit_code: i64,
}

pub(super) fn read(
    context: &str,
    spec: &TaskSpec,
    lease: &Lease,
    handle: &ExecutionHandle,
) -> anyhow::Result<VerifiedStoppedExecution> {
    read_with(&kubectl_binary(), context, spec, lease, handle)
}

fn read_with(
    tool: &Path,
    context: &str,
    spec: &TaskSpec,
    lease: &Lease,
    handle: &ExecutionHandle,
) -> anyhow::Result<VerifiedStoppedExecution> {
    handle.validate(lease, spec)?;
    ensure!(
        context == spec.profile.cluster && spec.profile.gpu == 0,
        "terminal observer target differs from signed native execution"
    );
    let job = kubectl_json(
        tool,
        context,
        &handle.namespace,
        [
            "--request-timeout=30s",
            "get",
            "job",
            handle.name.as_str(),
            "-o",
            "json",
        ],
        "independently read original platform Job",
    )?;
    let selector = format!("job-name={}", handle.name);
    let pods = kubectl_json(
        tool,
        context,
        &handle.namespace,
        [
            "--request-timeout=30s",
            "get",
            "pods",
            "-l",
            selector.as_str(),
            "-o",
            "json",
        ],
        "independently read original platform owner Pod",
    )?;
    ensure!(
        pods["metadata"]["continue"]
            .as_str()
            .is_none_or(str::is_empty),
        "owner Pod list is incomplete"
    );
    let pods = pods["items"]
        .as_array()
        .context("platform owner Pod list is absent")?;
    ensure!(
        pods.len() == 1,
        "terminal observer requires one unambiguous original execution Pod"
    );
    verify(spec, lease, handle, &job, &pods[0], Utc::now())
}

fn verify(
    spec: &TaskSpec,
    lease: &Lease,
    handle: &ExecutionHandle,
    job: &Value,
    pod: &Value,
    observed_at: DateTime<Utc>,
) -> anyhow::Result<VerifiedStoppedExecution> {
    spec.validate()?;
    handle.validate(lease, spec)?;
    ensure!(
        spec.kind == hft_research_platform::orchestrator::TaskKind::CexCampaign
            && spec.max_attempts == 1,
        "terminal observer requires exact native Campaign attempt"
    );
    let attempt_label = lease.attempt.to_string();
    let fence_label = lease.fence.to_string();
    let annotation = &job["metadata"]["annotations"];
    ensure!(
        job["apiVersion"] == "batch/v1"
            && job["kind"] == "Job"
            && job["metadata"]["name"] == handle.name
            && job["metadata"]["namespace"] == handle.namespace
            && job["metadata"]["uid"] == handle.uid
            && job["metadata"]["deletionTimestamp"].is_null()
            && annotation["monday.io/task-sha256"] == lease.task_id
            && annotation["monday.io/request-sha256"] == spec.id()?
            && annotation["monday.io/view-manifest-sha256"] == spec.view_manifest_sha256
            && job["metadata"]["labels"]["monday.io/attempt"] == attempt_label.as_str()
            && job["metadata"]["labels"]["monday.io/fence"] == fence_label.as_str(),
        "terminal Job identity or fence differs from original platform event"
    );
    let expected = &job["spec"]["template"]["spec"];
    let deadline = job["spec"]["activeDeadlineSeconds"]
        .as_i64()
        .context("terminal Job deadline is absent")?;
    ensure!(
        expected["serviceAccountName"] == spec.profile.service_account
            && expected["automountServiceAccountToken"] == false
            && expected["nodeSelector"]["kubernetes.io/arch"] == spec.profile.architecture
            && expected["nodeSelector"]["workload"] == "backtest"
            && expected["restartPolicy"] == "Never"
            && job["spec"]["backoffLimit"] == 0
            && absent_or_count(&job["spec"]["parallelism"], 1)
            && absent_or_count(&job["spec"]["completions"], 1)
            && deadline > 0
            && deadline <= (spec.timeout_ms + 999) / 1000,
        "terminal Job execution target drifted"
    );
    let job_containers = expected["containers"]
        .as_array()
        .context("terminal Job executor is absent")?;
    let containers = pod["spec"]["containers"]
        .as_array()
        .context("terminal Pod executors are absent")?;
    let statuses = pod["status"]["containerStatuses"]
        .as_array()
        .context("terminal Pod process status is absent")?;
    ensure!(
        job_containers.len() == 1 && containers.len() == 1 && statuses.len() == 1,
        "terminal observer rejects extra or ambiguous executors"
    );
    let worker = &job_containers[0];
    ensure!(
        worker["name"] == "worker"
            && worker["image"] == spec.image
            && worker["command"] == serde_json::to_value(&spec.command)?
            && worker["resources"]["limits"]["cpu"] == format!("{}m", spec.profile.cpu_millis)
            && worker["resources"]["requests"]["cpu"] == format!("{}m", spec.profile.cpu_millis)
            && worker["resources"]["limits"]["memory"] == format!("{}Mi", spec.profile.memory_mib)
            && worker["resources"]["requests"]["memory"]
                == format!("{}Mi", spec.profile.memory_mib),
        "terminal Job changed admitted command, image or resources"
    );
    let contexts = worker["env"]
        .as_array()
        .context("terminal Job context is absent")?
        .iter()
        .filter(|v| v["name"] == "MONDAY_ATTEMPT_CONTEXT")
        .collect::<Vec<_>>();
    ensure!(
        contexts.len() == 1,
        "terminal Job has ambiguous platform context"
    );
    let context: hft_research_platform::orchestrator::AttemptContext = serde_json::from_str(
        contexts[0]["value"]
            .as_str()
            .context("terminal context is not text")?,
    )?;
    context.validate()?;
    ensure!(
        context.spec == *spec && context.lease == *lease,
        "terminal Job context changed the signed attempt"
    );
    let owners = pod["metadata"]["ownerReferences"]
        .as_array()
        .context("terminal Pod ownership is absent")?;
    let controllers: Vec<_> = owners
        .iter()
        .filter(|owner| owner["controller"] == true)
        .collect();
    ensure!(
        controllers.len() == 1
            && controllers[0]["apiVersion"] == "batch/v1"
            && controllers[0]["kind"] == "Job"
            && controllers[0]["name"] == handle.name
            && controllers[0]["uid"] == handle.uid,
        "terminal Pod belongs to another Job UID"
    );
    let pod_uid = pod["metadata"]["uid"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .context("terminal Pod UID is absent")?;
    let conditions = job["status"]["conditions"]
        .as_array()
        .context("natural terminal Job condition is absent")?;
    let complete = conditions
        .iter()
        .any(|c| c["type"] == "Complete" && c["status"] == "True");
    let failed = conditions
        .iter()
        .any(|c| c["type"] == "Failed" && c["status"] == "True");
    ensure!(
        pod["apiVersion"] == "v1"
            && pod["kind"] == "Pod"
            && pod["metadata"]["namespace"] == handle.namespace
            && pod["metadata"]["deletionTimestamp"].is_null()
            && pod["metadata"]["labels"]["monday.io/attempt"] == attempt_label.as_str()
            && pod["metadata"]["labels"]["monday.io/fence"] == fence_label.as_str()
            && pod["spec"]["automountServiceAccountToken"] == false
            && matches!(
                pod["status"]["phase"].as_str(),
                Some("Succeeded" | "Failed")
            )
            && complete != failed
            && ((pod["status"]["phase"] == "Succeeded"
                && complete
                && job["status"]["succeeded"] == 1
                && absent_or_count(&job["status"]["failed"], 0))
                || (pod["status"]["phase"] == "Failed"
                    && failed
                    && job["status"]["failed"] == 1
                    && absent_or_count(&job["status"]["succeeded"], 0)))
            && absent_or_count(&job["status"]["active"], 0),
        "platform execution is not independently stopped"
    );
    for (name, value) in [
        ("monday.io/task-sha256", lease.task_id.as_str()),
        ("monday.io/request-sha256", handle.request_sha256.as_str()),
        (
            "monday.io/view-manifest-sha256",
            spec.view_manifest_sha256.as_str(),
        ),
        (
            "monday.io/acceptance-sha256",
            spec.profile.acceptance_sha256.as_str(),
        ),
    ] {
        ensure!(
            pod["metadata"]["annotations"][name] == value
                && job["spec"]["template"]["metadata"]["annotations"][name] == value,
            "terminal process annotation changed {name}"
        );
    }
    for field in [
        "serviceAccountName",
        "nodeSelector",
        "restartPolicy",
        "securityContext",
        "volumes",
        "activeDeadlineSeconds",
    ] {
        ensure!(
            pod["spec"][field] == expected[field],
            "terminal Pod changed admitted process field {field}"
        );
    }
    for field in [
        "name",
        "image",
        "command",
        "args",
        "resources",
        "env",
        "envFrom",
        "securityContext",
        "volumeMounts",
    ] {
        ensure!(
            containers[0][field] == worker[field],
            "terminal Pod changed admitted executor field {field}"
        );
    }
    ensure!(
        pod["spec"]["initContainers"] == expected["initContainers"]
            && pod["spec"]["ephemeralContainers"].is_null(),
        "terminal Pod introduced an unrecorded process tree"
    );
    let init = expected["initContainers"].as_array();
    let init_status = pod["status"]["initContainerStatuses"].as_array();
    ensure!(
        init_status.map_or(0, Vec::len) == init.map_or(0, Vec::len),
        "terminal initializer processes have not stopped"
    );
    if let (Some(init), Some(statuses)) = (init, init_status) {
        let mut names = std::collections::BTreeSet::new();
        for initializer in init {
            ensure!(
                initializer["image"] == spec.image,
                "initializer image differs from signed release"
            );
            let name = initializer["name"]
                .as_str()
                .context("initializer name is absent")?;
            ensure!(names.insert(name), "ambiguous initializer process names");
            let matches: Vec<_> = statuses.iter().filter(|s| s["name"] == name).collect();
            ensure!(matches.len() == 1, "initializer process identity differs");
            stopped_process(matches[0], initializer, job, observed_at)?;
        }
    }
    let status = &statuses[0];
    let terminated = &status["state"]["terminated"];
    ensure!(
        status["name"] == "worker"
            && status["restartCount"].as_u64() == Some(0)
            && terminated.is_object()
            && status["state"]["running"].is_null()
            && status["state"]["waiting"].is_null(),
        "terminal worker process is not stopped exactly once"
    );
    stopped_process(status, worker, job, observed_at)?;
    let exit = terminated["exitCode"]
        .as_i64()
        .context("terminal worker exit code is absent")?;
    ensure!(
        pod["status"]["phase"] != "Succeeded" || exit == 0,
        "successful Pod has a failed worker process"
    );
    Ok(VerifiedStoppedExecution {
        handle: handle.clone(),
        pod_uid: pod_uid.into(),
        observed_at,
        job_sha256: alpha_domain::canonical_json_hash(job)?,
        pod_sha256: alpha_domain::canonical_json_hash(pod)?,
        worker_exit_code: exit,
    })
}

fn absent_or_count(value: &Value, expected: u64) -> bool {
    value.is_null() || value.as_u64() == Some(expected)
}

fn stopped_process(
    status: &Value,
    container: &Value,
    job: &Value,
    observed_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let terminated = &status["state"]["terminated"];
    ensure!(
        status["restartCount"].as_u64() == Some(0)
            && terminated.is_object()
            && status["state"]["running"].is_null()
            && status["state"]["waiting"].is_null(),
        "process is not independently stopped exactly once"
    );
    let image_id = status["imageID"]
        .as_str()
        .context("actual process imageID is absent")?;
    let expected_image = container["image"]
        .as_str()
        .context("admitted process image is absent")?;
    ensure!(
        image_id.rsplit_once("sha256:").map(|(_, h)| h)
            == expected_image.rsplit_once("sha256:").map(|(_, h)| h),
        "actual process image differs from native release"
    );
    let parse = |value: &Value| -> anyhow::Result<DateTime<Utc>> {
        Ok(value
            .as_str()
            .context("process timestamp is absent")?
            .parse()?)
    };
    let started = parse(&terminated["startedAt"])?;
    let finished = parse(&terminated["finishedAt"])?;
    ensure!(
        parse(&job["status"]["startTime"])? <= started
            && started <= finished
            && finished <= observed_at,
        "process time facts are inconsistent"
    );
    ensure!(
        terminated["exitCode"].as_i64().is_some(),
        "process exit code is absent"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
