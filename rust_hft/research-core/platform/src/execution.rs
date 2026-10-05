//! Kubernetes/ACS Jobs and AgentSandbox use the same persisted task identity.
//! No ECS/ACK provisioning, GitHub run, or CI receipt appears in this contract.
#[cfg(feature = "control")]
use anyhow::Context;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    identity,
    orchestrator::{Lease, TaskSpec},
    valid_digest,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    KubernetesJob,
    AcsJob,
    AgentSandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub backend: Backend,
    pub cluster: String,
    pub namespace: String,
    pub service_account: String,
    pub architecture: String,
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub scratch_mib: u32,
    pub gpu: u16,
    /// Profile acceptance binds scheduler, network, identity, cancellation,
    /// artifact access and (when used) the shared mount, on this exact target.
    pub acceptance_sha256: String,
    pub prepared_pvc: Option<String>,
    /// Operator-owned immutable secret containing scoped worker configuration.
    pub worker_secret: Option<String>,
}

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.cluster.is_empty()
                && dns_label(&self.namespace)
                && dns_label(&self.service_account),
            "invalid execution target"
        );
        ensure!(
            matches!(self.architecture.as_str(), "amd64" | "arm64"),
            "unsupported architecture"
        );
        ensure!(
            self.cpu_millis > 0
                && self.cpu_millis <= 256_000
                && self.memory_mib > 0
                && self.scratch_mib > 0,
            "invalid resources"
        );
        ensure!(self.gpu == 0, "no accepted GPU trainer/provider contract");
        ensure!(
            valid_digest(&self.acceptance_sha256),
            "missing provider acceptance identity"
        );
        if let Some(pvc) = &self.prepared_pvc {
            ensure!(dns_label(pvc), "invalid prepared PVC");
        }
        if let Some(secret) = &self.worker_secret {
            ensure!(dns_label(secret), "invalid worker secret");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    pub profile: Profile,
    pub ready: bool,
    pub process_tree_stop: bool,
    pub immutable_prepared_mount: bool,
    pub command_reattach: bool,
    pub artifact_readback: bool,
}

impl Acceptance {
    pub fn admit(&self, spec: &TaskSpec) -> Result<()> {
        spec.validate()?;
        ensure!(
            self.profile == spec.profile
                && self.ready
                && self.process_tree_stop
                && self.artifact_readback,
            "backend capability has not been accepted"
        );
        ensure!(
            self.profile.prepared_pvc.is_none() || self.immutable_prepared_mount,
            "shared storage compatibility unverified"
        );
        ensure!(
            self.profile.backend != Backend::AgentSandbox || self.command_reattach,
            "Sandbox command reconnect has not been verified"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHandle {
    pub backend: Backend,
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    pub uid: String,
    pub attempt: u32,
    pub fence: i64,
    pub task_id: String,
    pub request_sha256: String,
}

/// Public resource identity only. Credential bytes never enter the task ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttemptIdentityRef {
    pub secret_name: String,
    pub secret_uid: String,
    pub scope_sha256: String,
    pub native_evidence_sha256: String,
    pub data_sha256: String,
    pub attempt: u32,
    pub fence: i64,
    pub deadline_ms: i64,
    pub launch_lease: Lease,
}
impl AttemptIdentityRef {
    pub fn validate(&self, spec: &TaskSpec, lease: &Lease) -> Result<()> {
        ensure!(
            spec.kind == crate::orchestrator::TaskKind::CexCampaign
                && self.secret_name == format!("{}-identity", resource_name(lease))
                && !self.secret_uid.is_empty()
                && self.secret_uid.len() <= 128
                && [
                    &self.scope_sha256,
                    &self.native_evidence_sha256,
                    &self.data_sha256
                ]
                .into_iter()
                .all(|v| valid_digest(v))
                && self.attempt == lease.attempt
                && self.fence == lease.fence
                && self.deadline_ms > 0,
            "late identity changed Attempt scope"
        );
        ensure!(
            self.launch_lease.task_id == lease.task_id
                && self.launch_lease.attempt == lease.attempt
                && self.launch_lease.fence == lease.fence
                && self.launch_lease.owner == lease.owner
                && self.launch_lease.expires_ms > 0,
            "late identity changed launch context"
        );
        Ok(())
    }
}

impl ExecutionHandle {
    pub fn validate(&self, lease: &Lease, spec: &TaskSpec) -> Result<()> {
        ensure!(
            self.backend == spec.profile.backend
                && self.cluster == spec.profile.cluster
                && self.namespace == spec.profile.namespace
                && !self.uid.is_empty(),
            "execution target changed"
        );
        ensure!(
            self.attempt == lease.attempt
                && self.fence == lease.fence
                && self.task_id == lease.task_id
                && self.request_sha256 == identity(spec)?
                && self.name == resource_name(lease),
            "execution identity changed"
        );
        Ok(())
    }
}

pub fn resource_name(lease: &Lease) -> String {
    format!(
        "research-{}-{}",
        lease.task_id.chars().take(24).collect::<String>(),
        lease.attempt
    )
}

fn task_label(task_id: &str) -> String {
    task_id.chars().take(40).collect()
}

pub fn render(spec: &TaskSpec, lease: &Lease, acceptance: &Acceptance) -> Result<Value> {
    acceptance.admit(spec)?;
    let context = crate::orchestrator::AttemptContext {
        spec: spec.clone(),
        lease: lease.clone(),
    };
    context.validate()?;
    let context_json = serde_json::to_string(&context)?;
    ensure!(
        context_json.len() <= 64 * 1024,
        "Attempt context exceeds worker environment bound"
    );
    ensure!(
        valid_digest(&lease.task_id)
            && lease.task_id == spec.id()?
            && lease.attempt > 0
            && lease.fence > 0,
        "invalid launch lease"
    );
    let labels = json!({"monday.io/task": task_label(&lease.task_id), "monday.io/attempt": lease.attempt.to_string(), "monday.io/fence": lease.fence.to_string()});
    let annotations = json!({"monday.io/task-sha256": lease.task_id, "monday.io/request-sha256": spec.id()?, "monday.io/view-manifest-sha256": spec.view_manifest_sha256, "monday.io/acceptance-sha256": spec.profile.acceptance_sha256});
    let mut pod = json!({
        "metadata": {"labels": labels, "annotations": annotations},
        "spec": {
            "restartPolicy": "Never", "serviceAccountName": spec.profile.service_account,
            "automountServiceAccountToken": false,
            "nodeSelector": {"kubernetes.io/arch": spec.profile.architecture},
            "terminationGracePeriodSeconds": 30,
            "activeDeadlineSeconds": (spec.timeout_ms + 999) / 1000,
            "securityContext": {"runAsNonRoot": true, "runAsUser": 1000, "fsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
            "containers": [{
                "name": "worker", "image": spec.image, "command": spec.command,
                "env": [
                    {"name": "MONDAY_ATTEMPT_CONTEXT", "value": context_json},
                    {"name": "MONDAY_TASK_ID", "value": lease.task_id},
                    {"name": "MONDAY_RUN_MANIFEST", "value": spec.run_manifest_sha256},
                    {"name": "MONDAY_ATTEMPT", "value": lease.attempt.to_string()},
                    {"name": "MONDAY_FENCE", "value": lease.fence.to_string()},
                    {"name": "MONDAY_VIEW_MANIFEST", "value": spec.view_manifest_sha256},
                    {"name": "MONDAY_OUTPUT_PREFIX", "value": format!("{}/{}/{}/", spec.output_prefix, lease.task_id, lease.attempt)}
                ],
                "resources": {"requests": {"cpu": format!("{}m", spec.profile.cpu_millis), "memory": format!("{}Mi", spec.profile.memory_mib)}, "limits": {"cpu": format!("{}m", spec.profile.cpu_millis), "memory": format!("{}Mi", spec.profile.memory_mib)}},
                "securityContext": {"allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true, "capabilities": {"drop": ["ALL"]}},
                "volumeMounts": [{"name": "scratch", "mountPath": "/work"}]
            }],
            "volumes": [{"name": "scratch", "emptyDir": {"sizeLimit": format!("{}Mi", spec.profile.scratch_mib)}}]
        }
    });
    if spec.kind == crate::orchestrator::TaskKind::CexCampaign {
        ensure!(
            spec.profile.gpu == 0 && spec.profile.backend != Backend::AgentSandbox,
            "native CEX Campaign requires admitted CPU Job compute"
        );
        pod["spec"]["nodeSelector"]["workload"] = json!("backtest");
        ensure!(
            spec.profile.scratch_mib > 8,
            "private configuration exceeds reserved scratch budget"
        );
        pod["spec"]["volumes"][0]["emptyDir"]["sizeLimit"] =
            json!(format!("{}Mi", spec.profile.scratch_mib - 8));
        pod["spec"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"private-state","emptyDir":{"sizeLimit":"8Mi"}}));
        // Native scientific libraries use temporary files. Both mount points
        // share one bounded volume, not two independent scratch allocations.
        pod["spec"]["containers"][0]["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"scratch", "mountPath":"/tmp"}));
        let mounts = pod["spec"]["containers"][0]["volumeMounts"]
            .as_array_mut()
            .unwrap();
        mounts.push(json!({"name":"private-state","mountPath":"/config","subPath":"config","readOnly":true}));
        mounts.push(json!({"name":"private-state","mountPath":"/identity","subPath":"identity","readOnly":true}));
    }
    if let Some(pvc) = &spec.profile.prepared_pvc {
        pod["spec"]["volumes"].as_array_mut().unwrap().push(
            json!({"name":"prepared", "persistentVolumeClaim":{"claimName":pvc,"readOnly":true}}),
        );
        pod["spec"]["containers"][0]["volumeMounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"prepared","mountPath":"/prepared","readOnly":true}));
    }
    if let Some(secret) = &spec.profile.worker_secret {
        if spec.kind == crate::orchestrator::TaskKind::CexCampaign {
            pod["spec"]["volumes"].as_array_mut().unwrap().push(json!({"name":"configuration-inputs","secret":{"secretName":secret,"defaultMode":288}}));
            pod["spec"]["initContainers"] = json!([{
                "name":"stage-configuration", "image":spec.image, "command":[spec.command[0],"--stage-configuration"],
                "env":[{"name":"MONDAY_ATTEMPT_CONTEXT","value":context_json}],
                "resources":pod["spec"]["containers"][0]["resources"],
                "securityContext":pod["spec"]["containers"][0]["securityContext"],
                "volumeMounts":[{"name":"configuration-inputs","mountPath":"/configuration-inputs","readOnly":true},{"name":"private-state","mountPath":"/private-state"}]
            }]);
            // The controlled launcher supplies a separate exact per-Attempt
            // identity mount. No caller credential or static Secret can do so.
        } else {
            pod["spec"]["volumes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"identity","secret":{"secretName":secret,"defaultMode":288}}));
            pod["spec"]["containers"][0]["volumeMounts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"identity","mountPath":"/config","readOnly":true}));
        }
    }
    match spec.profile.backend {
        Backend::KubernetesJob | Backend::AcsJob => {
            if spec.profile.backend == Backend::AcsJob {
                pod["metadata"]["labels"]["alibabacloud.com/compute-class"] =
                    json!("general-purpose");
            }
            Ok(
                json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":resource_name(lease),"namespace":spec.profile.namespace,"labels":labels,"annotations":annotations},"spec":{"backoffLimit":0,"activeDeadlineSeconds":(spec.timeout_ms + 999) / 1000,"template":pod}}),
            )
        }
        Backend::AgentSandbox => {
            // This is a CRD lifecycle adapter, not an invented Rust E2B SDK.
            // Native provider command/event APIs remain behind acceptance.
            pod["metadata"]["labels"]["alibabacloud.com/compute-class"] = json!("agent-sandbox");
            Ok(
                json!({"apiVersion":"agents.kruise.io/v1alpha1","kind":"Sandbox","metadata":{"name":resource_name(lease),"namespace":spec.profile.namespace,"labels":labels,"annotations":annotations},"spec":{"runtimes":[{"name":"agent-runtime"}],"template":pod}}),
            )
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    Pending,
    Running,
    Succeeded,
    Failed,
    Gone,
}

/// Explicit target and token are supplied by the operator, never discovered
/// through a CI environment. Constructor performs no network operation.
#[cfg(feature = "control")]
pub struct Kubernetes {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    token: String,
    cluster: String,
}

#[cfg(feature = "control")]
impl Kubernetes {
    /// Read-only configuration check on the exact accepted Kubernetes target.
    /// A name, digest supplied by a caller, or immutable flag alone is insufficient.
    pub async fn verify_worker_configuration(&self, spec: &TaskSpec) -> Result<Option<String>> {
        let mut native_trust_sha256 = None;
        if let Some(expected) = &spec.worker_configuration {
            let path = format!(
                "/api/v1/namespaces/{}/secrets/{}",
                spec.profile.namespace, expected.secret_name
            );
            let value = self
                .read(&path)
                .await?
                .context("immutable worker configuration missing")?;
            ensure!(
                value["kind"] == "Secret"
                    && value["type"] == "Opaque"
                    && value["immutable"] == true
                    && value["metadata"]["namespace"] == spec.profile.namespace
                    && value["metadata"]["name"] == expected.secret_name
                    && value["metadata"]["uid"] == expected.secret_uid,
                "worker configuration resource changed"
            );
            let data: std::collections::BTreeMap<String, String> =
                serde_json::from_value(value["data"].clone())?;
            ensure!(
                crate::orchestrator::worker_configuration_reference(
                    &spec.profile.namespace,
                    &expected.secret_name,
                    &expected.secret_uid,
                    &data
                )? == *expected,
                "worker configuration contents changed"
            );
            if spec.kind == crate::orchestrator::TaskKind::CexCampaign {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD.decode(
                    data.get("native-trust.json")
                        .context("Campaign configuration lacks native trust")?,
                )?;
                let trust: crate::admission::NativeAdmissionTrust = serde_json::from_slice(&bytes)?;
                native_trust_sha256 = Some(identity(&trust)?);
            }
        }
        Ok(native_trust_sha256)
    }
    pub fn new(endpoint: &str, token: String, cluster: String, ca_pem: &[u8]) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint)?;
        ensure!(
            endpoint.scheme() == "https"
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.host_str().is_some(),
            "invalid Kubernetes endpoint"
        );
        ensure!(
            !token.is_empty() && !cluster.is_empty(),
            "missing target authentication"
        );
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .no_proxy()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(ca_pem)?)
            .build()?;
        Ok(Self {
            client,
            endpoint,
            token,
            cluster,
        })
    }

    fn path(backend: Backend, namespace: &str) -> String {
        match backend {
            Backend::KubernetesJob | Backend::AcsJob => {
                format!("/apis/batch/v1/namespaces/{namespace}/jobs")
            }
            Backend::AgentSandbox => {
                format!("/apis/agents.kruise.io/v1alpha1/namespaces/{namespace}/sandboxes")
            }
        }
    }

    async fn read(&self, path: &str) -> Result<Option<Value>> {
        let response = self
            .client
            .get(self.endpoint.join(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let mut response = response.error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 2 * 1024 * 1024,
                "Kubernetes response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    pub async fn launch(
        &self,
        spec: &TaskSpec,
        lease: &Lease,
        acceptance: &Acceptance,
        checkpoint: Option<&crate::orchestrator::Checkpoint>,
        remaining_ms: i64,
        identity: Option<&crate::artifact_identity::IssuedAttemptIdentity>,
    ) -> Result<(ExecutionHandle, Option<AttemptIdentityRef>)> {
        ensure!(
            self.cluster == spec.profile.cluster,
            "target cluster mismatch"
        );
        ensure!(
            remaining_ms > 0 && remaining_ms <= spec.timeout_ms,
            "invalid remaining deadline"
        );
        let native_trust = self.verify_worker_configuration(spec).await?;
        let mut attempt_identity = if spec.kind == crate::orchestrator::TaskKind::CexCampaign {
            let issued = identity.context(
                "native Campaign launch requires controlled per-Attempt identity issuance",
            )?;
            issued.matches_context(&crate::orchestrator::AttemptContext {
                spec: spec.clone(),
                lease: lease.clone(),
            })?;
            ensure!(
                native_trust.as_deref() == Some(issued.native_trust_sha256()),
                "static native trust differs from verified PG admission"
            );
            Some(self.prepare_attempt_identity(spec, lease, issued).await?)
        } else {
            ensure!(identity.is_none(), "unexpected late Campaign identity");
            None
        };
        let mut resource = render(spec, lease, acceptance)?;
        if let Some(reference) = &attempt_identity {
            install_attempt_identity(&mut resource, reference)?;
        }
        resource["spec"]["template"]["spec"]["activeDeadlineSeconds"] =
            json!((remaining_ms + 999) / 1000);
        if spec.profile.backend != Backend::AgentSandbox {
            resource["spec"]["activeDeadlineSeconds"] = json!((remaining_ms + 999) / 1000);
        }
        if let Some(checkpoint) = checkpoint {
            ensure!(
                checkpoint.task_id == lease.task_id && checkpoint.attempt < lease.attempt,
                "invalid recovery checkpoint"
            );
            let env = &mut resource["spec"]["template"]["spec"]["containers"][0]["env"];
            env.as_array_mut().context("worker environment missing")?.push(json!({"name":"MONDAY_RECOVERY_CHECKPOINT","value":serde_json::to_string(checkpoint)?}));
        }
        let collection = Self::path(spec.profile.backend, &spec.profile.namespace);
        let path = format!("{collection}/{}", resource_name(lease));
        // Recovery uses the same name. A POST timeout is resolved by readback,
        // never by minting another resource or resetting the original timeout.
        let existing = self.read(&path).await?;
        let object = if let Some(existing) = existing {
            existing
        } else {
            if let Some(issued) = identity {
                let remaining = identity_remaining_ms(issued, lease)?.min(remaining_ms);
                resource["spec"]["activeDeadlineSeconds"] = json!((remaining + 999) / 1000);
                resource["spec"]["template"]["spec"]["activeDeadlineSeconds"] =
                    json!((remaining + 999) / 1000);
            }
            let response = self
                .client
                .post(self.endpoint.join(&collection)?)
                .bearer_auth(&self.token)
                .json(&resource)
                .send()
                .await;
            match response {
                Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::CONFLICT => {
                }
                Ok(r) => {
                    r.error_for_status()?;
                }
                Err(_) => {}
            }
            self.read(&path)
                .await?
                .context("launch outcome unresolved; reconcile same resource")?
        };
        if let Some(reference) = &mut attempt_identity {
            let worker = object["spec"]["template"]["spec"]["containers"]
                .as_array()
                .and_then(|cs| cs.iter().find(|c| c["name"] == "worker"))
                .context("worker missing")?;
            let context = read_launch_context(worker)?;
            ensure!(
                context.spec == *spec
                    && context.lease.task_id == lease.task_id
                    && context.lease.attempt == lease.attempt
                    && context.lease.fence == lease.fence
                    && context.lease.owner == lease.owner,
                "recovered Job changed launch context"
            );
            reference.launch_lease = context.lease;
            reference.validate(spec, lease)?;
            let mut original = render(spec, &reference.launch_lease, acceptance)?;
            install_attempt_identity(&mut original, reference)?;
            for pointer in [
                "/metadata/annotations",
                "/spec/template/metadata/annotations",
            ] {
                ensure!(
                    object
                        .pointer(pointer)
                        .is_some_and(|annotations| annotations["monday.io/identity-secret-uid"]
                            == reference.secret_uid
                            && annotations["monday.io/identity-scope-sha256"]
                                == reference.scope_sha256),
                    "Job late identity drift"
                );
            }
            ensure!(
                object["spec"]["template"]["spec"]["volumes"]
                    == original["spec"]["template"]["spec"]["volumes"],
                "Job late mount drift"
            );
            let actual = object["spec"]["template"]["spec"]["initContainers"]
                .as_array()
                .context("Job initializer missing")?;
            ensure!(actual.len() == 1, "unexpected Job initializer");
            let expected = &original["spec"]["template"]["spec"]["initContainers"][0];
            for field in [
                "name",
                "image",
                "command",
                "env",
                "securityContext",
                "volumeMounts",
            ] {
                ensure!(actual[0][field] == expected[field], "Job initializer drift");
            }
            verify_cpu_resources(worker, &spec.profile)?;
            verify_cpu_resources(&actual[0], &spec.profile)?;
            let pod_spec = &object["spec"]["template"]["spec"];
            ensure!(
                pod_spec["serviceAccountName"] == spec.profile.service_account
                    && pod_spec["automountServiceAccountToken"] == false
                    && pod_spec["nodeSelector"]["kubernetes.io/arch"] == spec.profile.architecture
                    && pod_spec["nodeSelector"]["workload"] == "backtest"
                    && object["spec"]["backoffLimit"] == 0,
                "Job admission resources changed"
            );
            for value in [
                &object["spec"]["activeDeadlineSeconds"],
                &pod_spec["activeDeadlineSeconds"],
            ] {
                ensure!(
                    value
                        .as_i64()
                        .is_some_and(|n| n > 0 && n <= (spec.timeout_ms + 999) / 1000),
                    "Job deadline drift"
                );
            }
        }
        Ok((self.handle(&object, spec, lease)?, attempt_identity))
    }

    async fn prepare_attempt_identity(
        &self,
        spec: &TaskSpec,
        lease: &Lease,
        issued: &crate::artifact_identity::IssuedAttemptIdentity,
    ) -> Result<AttemptIdentityRef> {
        use base64::Engine;
        let name = format!("{}-identity", resource_name(lease));
        let data: std::collections::BTreeMap<String, String> = issued
            .late_files()
            .iter()
            .map(|(name, bytes)| {
                (
                    name.clone(),
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                )
            })
            .collect();
        let collection = format!("/api/v1/namespaces/{}/secrets", spec.profile.namespace);
        let path = format!("{collection}/{name}");
        let resource = json!({"apiVersion":"v1","kind":"Secret","type":"Opaque","immutable":true,"metadata":{"name":name,"namespace":spec.profile.namespace,"labels":{"monday.io/task":task_label(&lease.task_id),"monday.io/attempt":lease.attempt.to_string(),"monday.io/fence":lease.fence.to_string()},"annotations":{"monday.io/identity-scope-sha256":issued.scope_id(),"monday.io/native-evidence-sha256":issued.native_evidence_sha256(),"monday.io/identity-deadline-ms":issued.deadline_ms().to_string()}},"data":data});
        if self.read(&path).await?.is_none() {
            identity_remaining_ms(issued, lease)?;
            match self
                .client
                .post(self.endpoint.join(&collection)?)
                .bearer_auth(&self.token)
                .json(&resource)
                .send()
                .await
            {
                Ok(response)
                    if response.status().is_success()
                        || response.status() == reqwest::StatusCode::CONFLICT => {}
                Ok(response) => {
                    response.error_for_status()?;
                }
                Err(_) => {} // Private journal and deterministic name arm recovery.
            }
        }
        let object = self
            .read(&path)
            .await?
            .context("late identity outcome unresolved; reconcile same Secret")?;
        identity_reference(&object, spec, lease, issued)
    }

    /// Call only after the original process tree stopped. UID preconditions
    /// protect another writer's resource; absence is read back before cleanup.
    pub async fn cleanup_attempt_identity(
        &self,
        spec: &TaskSpec,
        lease: &Lease,
        known: Option<&AttemptIdentityRef>,
        issued: Option<&crate::artifact_identity::IssuedAttemptIdentity>,
    ) -> Result<bool> {
        let path = format!(
            "/api/v1/namespaces/{}/secrets/{}-identity",
            spec.profile.namespace,
            resource_name(lease)
        );
        let Some(object) = self.read(&path).await? else {
            return Ok(true);
        };
        let mut reference = if let Some(issued) = issued {
            identity_reference(&object, spec, lease, issued)?
        } else {
            known
                .context("orphan late identity lacks owned journal or ledger reference")?
                .clone()
        };
        if let Some(known) = known {
            reference.launch_lease = known.launch_lease.clone();
        }
        reference.validate(spec, lease)?;
        if let Some(known) = known {
            ensure!(known == &reference, "late identity cleanup UID drift");
        }
        verify_identity_reference(&object, spec, &reference)?;
        self.client.delete(self.endpoint.join(&path)?).bearer_auth(&self.token).json(&json!({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":reference.secret_uid}})).send().await?.error_for_status()?;
        Ok(self.read(&path).await?.is_none())
    }

    fn handle(&self, object: &Value, spec: &TaskSpec, lease: &Lease) -> Result<ExecutionHandle> {
        ensure!(
            object["metadata"]["annotations"]["monday.io/request-sha256"] == spec.id()?
                && object["metadata"]["annotations"]["monday.io/task-sha256"] == lease.task_id
                && object["metadata"]["labels"]["monday.io/task"] == task_label(&lease.task_id)
                && object["metadata"]["labels"]["monday.io/attempt"]
                    .as_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    == Some(lease.attempt)
                && object["metadata"]["labels"]["monday.io/fence"]
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    == Some(lease.fence),
            "resource collision or stale resource"
        );
        let worker = object["spec"]["template"]["spec"]["containers"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["name"] == "worker"))
            .context("worker missing from resource readback")?;
        ensure!(
            worker["image"] == spec.image
                && worker["command"] == serde_json::to_value(&spec.command)?,
            "admitted worker image/command drift"
        );
        let handle = ExecutionHandle {
            backend: spec.profile.backend,
            cluster: self.cluster.clone(),
            namespace: spec.profile.namespace.clone(),
            name: resource_name(lease),
            uid: object["metadata"]["uid"]
                .as_str()
                .context("resource UID absent")?
                .into(),
            attempt: lease.attempt,
            fence: lease.fence,
            task_id: lease.task_id.clone(),
            request_sha256: spec.id()?,
        };
        handle.validate(lease, spec)?;
        Ok(handle)
    }

    pub async fn observe(&self, handle: &ExecutionHandle) -> Result<Observation> {
        ensure!(
            handle.cluster == self.cluster
                && dns_label(&handle.namespace)
                && dns_label(&handle.name),
            "invalid handle target"
        );
        let path = format!(
            "{}/{}",
            Self::path(handle.backend, &handle.namespace),
            handle.name
        );
        let Some(object) = self.read(&path).await? else {
            return Ok(Observation::Gone);
        };
        ensure!(
            object["metadata"]["uid"] == handle.uid,
            "resource UID drift"
        );
        if handle.backend == Backend::AgentSandbox {
            // Readiness of the sandbox is not scientific result completion.
            return Ok(
                if object["status"]["conditions"].as_array().is_some_and(|cs| {
                    cs.iter()
                        .any(|c| c["type"] == "Ready" && c["status"] == "True")
                }) {
                    Observation::Running
                } else {
                    Observation::Pending
                },
            );
        }
        let conditions = object["status"]["conditions"].as_array();
        if conditions.is_some_and(|cs| {
            cs.iter()
                .any(|c| c["type"] == "Failed" && c["status"] == "True")
        }) {
            return Ok(Observation::Failed);
        }
        if conditions.is_some_and(|cs| {
            cs.iter()
                .any(|c| c["type"] == "Complete" && c["status"] == "True")
        }) {
            return Ok(Observation::Succeeded);
        }
        Ok(if object["status"]["active"].as_u64().unwrap_or(0) > 0 {
            Observation::Running
        } else {
            Observation::Pending
        })
    }

    /// Retain naturally finished Jobs and Pods for independent terminal audit.
    /// Other attempts use UID-bound deletion and an empty Pod readback.
    pub async fn stop(
        &self,
        spec: &TaskSpec,
        lease: &Lease,
        known: Option<&ExecutionHandle>,
    ) -> Result<bool> {
        ensure!(
            self.cluster == spec.profile.cluster,
            "target cluster mismatch"
        );
        let path = format!(
            "{}/{}",
            Self::path(spec.profile.backend, &spec.profile.namespace),
            resource_name(lease)
        );
        if let Some(object) = self.read(&path).await? {
            let handle = self.handle(&object, spec, lease)?;
            if let Some(known) = known {
                ensure!(known == &handle, "deletion UID drift");
            }
            if spec.profile.backend != Backend::AgentSandbox && job_has_terminal_condition(&object)
            {
                return retained_job_stopped(
                    &object,
                    &self.owned_pods(spec, lease).await?,
                    &handle,
                );
            }
            self.client.delete(self.endpoint.join(&path)?).bearer_auth(&self.token).json(&json!({"apiVersion":"v1","kind":"DeleteOptions","propagationPolicy":"Foreground","preconditions":{"uid":handle.uid}})).send().await?.error_for_status()?;
            return Ok(false);
        }
        let list = self.owned_pods(spec, lease).await?;
        let items = list["items"].as_array().context("Pod list missing items")?;
        Ok(items.is_empty())
    }

    async fn owned_pods(&self, spec: &TaskSpec, lease: &Lease) -> Result<Value> {
        let selector = format!(
            "monday.io/task={},monday.io/attempt={},monday.io/fence={}",
            task_label(&lease.task_id),
            lease.attempt,
            lease.fence
        );
        let mut url = self.endpoint.join(&format!(
            "/api/v1/namespaces/{}/pods",
            spec.profile.namespace
        ))?;
        url.query_pairs_mut()
            .append_pair("labelSelector", &selector);
        let list = self
            .read(url.as_str())
            .await?
            .context("Pod list unavailable")?;
        ensure!(
            list["metadata"]["continue"]
                .as_str()
                .is_none_or(str::is_empty),
            "incomplete Pod list"
        );
        list["items"].as_array().context("Pod list missing items")?;
        Ok(list)
    }
}

#[cfg(feature = "control")]
fn read_launch_context(worker: &Value) -> Result<crate::orchestrator::AttemptContext> {
    let env = worker["env"]
        .as_array()
        .context("launch environment missing")?;
    let values: Vec<_> = env
        .iter()
        .filter(|v| v["name"] == "MONDAY_ATTEMPT_CONTEXT")
        .collect();
    ensure!(values.len() == 1, "ambiguous launch context");
    let value = values[0]["value"]
        .as_str()
        .context("launch context missing")?;
    ensure!(value.len() <= 64 * 1024, "launch context exceeds bound");
    let context: crate::orchestrator::AttemptContext = serde_json::from_str(value)?;
    context.validate()?;
    Ok(context)
}

#[cfg(feature = "control")]
fn verify_cpu_resources(container: &Value, profile: &Profile) -> Result<()> {
    for section in ["requests", "limits"] {
        let map = container["resources"][section]
            .as_object()
            .context("worker resource contract missing")?;
        ensure!(
            map.len() == 2 && map.contains_key("cpu") && map.contains_key("memory"),
            "unadmitted worker resource"
        );
        let cpu = map["cpu"].as_str().context("CPU quantity missing")?;
        let millis = if let Some(value) = cpu.strip_suffix('m') {
            value.parse::<u64>()?
        } else {
            let (whole, fraction) = cpu.split_once('.').unwrap_or((cpu, ""));
            ensure!(
                fraction.len() <= 3 && fraction.bytes().all(|c| c.is_ascii_digit()),
                "invalid CPU quantity"
            );
            let whole = whole
                .parse::<u64>()?
                .checked_mul(1000)
                .context("CPU quantity overflow")?;
            let sub = if fraction.is_empty() {
                0
            } else {
                fraction
                    .parse::<u64>()?
                    .checked_mul(10_u64.pow(u32::try_from(3 - fraction.len())?))
                    .context("CPU quantity overflow")?
            };
            whole.checked_add(sub).context("CPU quantity overflow")?
        };
        let memory = map["memory"].as_str().context("memory quantity missing")?;
        let bytes = if let Some(value) = memory.strip_suffix("Mi") {
            value
                .parse::<u64>()?
                .checked_mul(1024 * 1024)
                .context("memory overflow")?
        } else if let Some(value) = memory.strip_suffix("Gi") {
            value
                .parse::<u64>()?
                .checked_mul(1024 * 1024 * 1024)
                .context("memory overflow")?
        } else {
            memory.parse::<u64>()?
        };
        ensure!(
            millis == u64::from(profile.cpu_millis)
                && bytes == u64::from(profile.memory_mib) * 1024 * 1024,
            "worker resources exceed signed profile"
        );
    }
    Ok(())
}

#[cfg(feature = "control")]
fn identity_remaining_ms(
    issued: &crate::artifact_identity::IssuedAttemptIdentity,
    lease: &Lease,
) -> Result<i64> {
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    ensure!(
        lease.expires_ms > now,
        "Attempt lease expired before provider mutation"
    );
    let remaining = issued
        .deadline_ms()
        .checked_sub(now)
        .context("Attempt identity deadline overflow")?;
    ensure!(
        remaining > 0,
        "Attempt identity expired before provider mutation"
    );
    Ok(remaining)
}

#[cfg(feature = "control")]
fn identity_reference(
    object: &Value,
    spec: &TaskSpec,
    lease: &Lease,
    issued: &crate::artifact_identity::IssuedAttemptIdentity,
) -> Result<AttemptIdentityRef> {
    use base64::Engine;
    let data: std::collections::BTreeMap<String, String> = issued
        .late_files()
        .iter()
        .map(|(name, bytes)| {
            (
                name.clone(),
                base64::engine::general_purpose::STANDARD.encode(bytes),
            )
        })
        .collect();
    ensure!(
        object["data"] == serde_json::to_value(&data)?,
        "late identity bytes differ from owned journal"
    );
    let uid = object["metadata"]["uid"]
        .as_str()
        .context("late identity UID missing")?
        .to_owned();
    let name = format!("{}-identity", resource_name(lease));
    let reference = AttemptIdentityRef {
        secret_name: name.clone(),
        secret_uid: uid.clone(),
        scope_sha256: issued.scope_id().into(),
        native_evidence_sha256: issued.native_evidence_sha256().into(),
        data_sha256: identity(&(
            "monday.attempt_identity_secret_contents.v1",
            &spec.profile.namespace,
            &name,
            &uid,
            &data,
        ))?,
        attempt: lease.attempt,
        fence: lease.fence,
        deadline_ms: issued.deadline_ms(),
        launch_lease: lease.clone(),
    };
    reference.validate(spec, lease)?;
    verify_identity_reference(object, spec, &reference)?;
    Ok(reference)
}

#[cfg(feature = "control")]
fn verify_identity_reference(
    object: &Value,
    spec: &TaskSpec,
    reference: &AttemptIdentityRef,
) -> Result<()> {
    ensure!(
        object["kind"] == "Secret"
            && object["type"] == "Opaque"
            && object["immutable"] == true
            && object["metadata"]["name"] == reference.secret_name
            && object["metadata"]["namespace"] == spec.profile.namespace
            && object["metadata"]["uid"] == reference.secret_uid
            && object["metadata"]["labels"]["monday.io/task"] == task_label(&spec.id()?)
            && object["metadata"]["labels"]["monday.io/attempt"]
                .as_str()
                .and_then(|v| v.parse::<u32>().ok())
                == Some(reference.attempt)
            && object["metadata"]["labels"]["monday.io/fence"]
                .as_str()
                .and_then(|v| v.parse::<i64>().ok())
                == Some(reference.fence)
            && object["metadata"]["annotations"]["monday.io/identity-scope-sha256"]
                == reference.scope_sha256
            && object["metadata"]["annotations"]["monday.io/native-evidence-sha256"]
                == reference.native_evidence_sha256
            && object["metadata"]["annotations"]["monday.io/identity-deadline-ms"]
                .as_str()
                .and_then(|v| v.parse::<i64>().ok())
                == Some(reference.deadline_ms),
        "late identity metadata drift"
    );
    let data: std::collections::BTreeMap<String, String> =
        serde_json::from_value(object["data"].clone())?;
    ensure!(
        identity(&(
            "monday.attempt_identity_secret_contents.v1",
            &spec.profile.namespace,
            &reference.secret_name,
            &reference.secret_uid,
            &data
        ))? == reference.data_sha256,
        "late identity contents drift"
    );
    Ok(())
}

#[cfg(feature = "control")]
fn install_attempt_identity(resource: &mut Value, reference: &AttemptIdentityRef) -> Result<()> {
    for pointer in [
        "/metadata/annotations",
        "/spec/template/metadata/annotations",
    ] {
        let annotations = resource
            .pointer_mut(pointer)
            .context("Job annotations missing")?;
        annotations["monday.io/identity-secret-uid"] = json!(reference.secret_uid);
        annotations["monday.io/identity-scope-sha256"] = json!(reference.scope_sha256);
    }
    resource["spec"]["template"]["spec"]["volumes"].as_array_mut().context("Job volumes missing")?.push(json!({"name":"identity-inputs","secret":{"secretName":reference.secret_name,"defaultMode":288}}));
    resource["spec"]["template"]["spec"]["initContainers"][0]["volumeMounts"]
        .as_array_mut()
        .context("Job initialization missing")?
        .push(json!({"name":"identity-inputs","mountPath":"/identity-inputs","readOnly":true}));
    Ok(())
}

#[cfg(feature = "control")]
fn job_has_terminal_condition(job: &Value) -> bool {
    job["status"]["conditions"]
        .as_array()
        .is_some_and(|conditions| {
            conditions.iter().any(|c| {
                matches!(c["type"].as_str(), Some("Complete" | "Failed")) && c["status"] == "True"
            })
        })
}

#[cfg(feature = "control")]
fn retained_job_stopped(job: &Value, pods: &Value, handle: &ExecutionHandle) -> Result<bool> {
    if !job_has_terminal_condition(job)
        || !job["metadata"]["deletionTimestamp"].is_null()
        || job["status"]["active"].as_u64().unwrap_or(0) != 0
    {
        return Ok(false);
    }
    let conditions = job["status"]["conditions"]
        .as_array()
        .context("terminal conditions missing")?;
    let complete = conditions
        .iter()
        .filter(|c| c["type"] == "Complete" && c["status"] == "True")
        .count();
    let failed = conditions
        .iter()
        .filter(|c| c["type"] == "Failed" && c["status"] == "True")
        .count();
    let expected_phase = match (complete, failed) {
        (1, 0)
            if job["status"]["succeeded"].as_u64() == Some(1)
                && job["status"]["failed"].as_u64().unwrap_or(0) == 0 =>
        {
            "Succeeded"
        }
        (0, 1)
            if job["status"]["failed"].as_u64() == Some(1)
                && job["status"]["succeeded"].as_u64().unwrap_or(0) == 0 =>
        {
            "Failed"
        }
        _ => return Ok(false),
    };
    ensure!(
        pods["metadata"]["continue"]
            .as_str()
            .is_none_or(str::is_empty),
        "incomplete Pod list"
    );
    let items = pods["items"].as_array().context("Pod list missing items")?;
    if items.len() != 1 {
        return Ok(false);
    }
    let pod = &items[0];
    let owners = pod["metadata"]["ownerReferences"]
        .as_array()
        .context("Pod owner missing")?;
    if !owners.iter().any(|owner| {
        owner["controller"] == true
            && owner["kind"] == "Job"
            && owner["uid"] == handle.uid
            && owner["name"] == handle.name
    }) || pod["metadata"]["namespace"] != handle.namespace
        || pod["metadata"]["uid"].as_str().is_none_or(str::is_empty)
        || !pod["metadata"]["deletionTimestamp"].is_null()
        || !["labels", "annotations"].iter().all(|field| {
            job["spec"]["template"]["metadata"][field]
                .as_object()
                .is_some_and(|expected| {
                    expected
                        .iter()
                        .all(|(key, value)| pod["metadata"][field][key] == *value)
                })
        })
        || pod["status"]["phase"] != expected_phase
        || pod["spec"]["ephemeralContainers"]
            .as_array()
            .is_some_and(|cs| !cs.is_empty())
    {
        return Ok(false);
    }
    for (declared, status) in [
        ("containers", "containerStatuses"),
        ("initContainers", "initContainerStatuses"),
    ] {
        let expected = job["spec"]["template"]["spec"][declared].as_array();
        let actual_spec = pod["spec"][declared].as_array();
        if expected != actual_spec {
            return Ok(false);
        }
        let statuses = pod["status"][status].as_array();
        let expected = expected.map(Vec::as_slice).unwrap_or_default();
        let statuses = statuses.map(Vec::as_slice).unwrap_or_default();
        if statuses.len() != expected.len() || (declared == "containers" && expected.is_empty()) {
            return Ok(false);
        }
        let mut seen = std::collections::BTreeSet::new();
        for container in statuses {
            let name = container["name"]
                .as_str()
                .context("container status lacks name")?;
            if !seen.insert(name)
                || !expected.iter().any(|c| c["name"] == name)
                || !container["state"]["terminated"].is_object()
                || !container["state"]["running"].is_null()
                || !container["state"]["waiting"].is_null()
                || container["restartCount"].as_u64() != Some(0)
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(all(test, feature = "control"))]
mod stop_tests {
    use super::*;
    #[test]
    fn late_secret_identity_rejects_uid_and_byte_substitution() {
        let spec = TaskSpec {
            schema: 1,
            kind: crate::orchestrator::TaskKind::CexCampaign,
            run_manifest_sha256: "a".repeat(64),
            view_manifest_sha256: "b".repeat(64),
            source_sha256: "c".repeat(64),
            image: format!("fixture@sha256:{}", "d".repeat(64)),
            command: vec!["/app/worker".into()],
            profile: Profile {
                backend: Backend::KubernetesJob,
                cluster: "fixture".into(),
                namespace: "research".into(),
                service_account: "worker".into(),
                architecture: "amd64".into(),
                cpu_millis: 1000,
                memory_mib: 128,
                scratch_mib: 32,
                gpu: 0,
                acceptance_sha256: "e".repeat(64),
                prepared_pvc: None,
                worker_secret: Some("configuration".into()),
            },
            timeout_ms: 10_000,
            max_attempts: 1,
            output_prefix: "research/results".into(),
            fit_identity_sha256: None,
            worker_configuration: Some(crate::orchestrator::WorkerConfigurationRef {
                schema: "monday.worker_configuration.v1".into(),
                secret_name: "configuration".into(),
                secret_uid: "static-uid".into(),
                configuration_sha256: "f".repeat(64),
            }),
        };
        let mut task = crate::orchestrator::Task::new(spec.clone()).unwrap();
        let lease = task.claim("owner", 1000, 1000).unwrap();
        let name = format!("{}-identity", resource_name(&lease));
        let data: std::collections::BTreeMap<String, String> = [
            ("artifact.token".into(), "Zml4dHVyZS10b2tlbg==".into()),
            ("native-admission.json".into(), "e30=".into()),
        ]
        .into();
        let reference = AttemptIdentityRef {
            secret_name: name.clone(),
            secret_uid: "original".into(),
            scope_sha256: "a".repeat(64),
            native_evidence_sha256: "b".repeat(64),
            data_sha256: identity(&(
                "monday.attempt_identity_secret_contents.v1",
                &spec.profile.namespace,
                &name,
                "original",
                &data,
            ))
            .unwrap(),
            attempt: lease.attempt,
            fence: lease.fence,
            deadline_ms: 11000,
            launch_lease: lease.clone(),
        };
        let object = json!({"kind":"Secret","type":"Opaque","immutable":true,"metadata":{"name":name,"namespace":"research","uid":"original","labels":{"monday.io/task":task_label(&task.id),"monday.io/attempt":"1","monday.io/fence":"1"},"annotations":{"monday.io/identity-scope-sha256":reference.scope_sha256,"monday.io/native-evidence-sha256":reference.native_evidence_sha256,"monday.io/identity-deadline-ms":"11000"}},"data":data});
        verify_identity_reference(&object, &spec, &reference).unwrap();
        let mut changed = object.clone();
        changed["metadata"]["uid"] = json!("foreign");
        assert!(verify_identity_reference(&changed, &spec, &reference).is_err());
        let mut changed = object.clone();
        changed["data"]["artifact.token"] = json!("Y2hhbmdlZA==");
        assert!(verify_identity_reference(&changed, &spec, &reference).is_err());
        let mut changed = object.clone();
        changed["immutable"] = json!(false);
        assert!(verify_identity_reference(&changed, &spec, &reference).is_err());
        let mut changed = object;
        changed["metadata"]["labels"]["monday.io/fence"] = json!("2");
        assert!(verify_identity_reference(&changed, &spec, &reference).is_err());
        let context = crate::orchestrator::AttemptContext { spec, lease };
        let value = serde_json::to_string(&context).unwrap();
        let mut worker = json!({"env":[{"name":"MONDAY_ATTEMPT_CONTEXT","value":value}]});
        assert_eq!(read_launch_context(&worker).unwrap(), context);
        let duplicate = worker["env"][0].clone();
        worker["env"].as_array_mut().unwrap().push(duplicate);
        assert!(read_launch_context(&worker).is_err());
    }
    fn fixture() -> (Value, Value, ExecutionHandle) {
        let metadata = json!({"labels":{"monday.io/task":"task","monday.io/attempt":"1","monday.io/fence":"2"},"annotations":{"monday.io/request-sha256":"request"}});
        let spec = json!({"containers":[{"name":"worker","image":"fixture@sha256:abc"}],"initContainers":[{"name":"configuration","image":"fixture@sha256:abc"}]});
        let job = json!({"metadata":{"uid":"original","name":"research-task-1"},"spec":{"template":{"metadata":metadata,"spec":spec}},"status":{"active":0,"succeeded":1,"conditions":[{"type":"Complete","status":"True"}]}});
        let mut pod_metadata = metadata;
        pod_metadata["namespace"] = json!("research");
        pod_metadata["uid"] = json!("pod-original");
        pod_metadata["ownerReferences"] =
            json!([{"controller":true,"kind":"Job","uid":"original","name":"research-task-1"}]);
        let pods = json!({"items":[{"metadata":pod_metadata,"spec":spec,"status":{"phase":"Succeeded","containerStatuses":[{"name":"worker","restartCount":0,"state":{"terminated":{"exitCode":0}}}],"initContainerStatuses":[{"name":"configuration","restartCount":0,"state":{"terminated":{"exitCode":0}}}]}}]});
        let handle = ExecutionHandle {
            backend: Backend::KubernetesJob,
            cluster: "fixture".into(),
            namespace: "research".into(),
            name: "research-task-1".into(),
            uid: "original".into(),
            attempt: 1,
            fence: 2,
            task_id: "task".into(),
            request_sha256: "request".into(),
        };
        (job, pods, handle)
    }

    #[test]
    fn retained_terminal_requires_every_original_process_stopped() {
        let (job, pods, handle) = fixture();
        assert!(retained_job_stopped(&job, &pods, &handle).unwrap());
        for bad in [
            {
                let mut p = pods.clone();
                p["items"][0]["status"]["initContainerStatuses"][0]["state"] =
                    json!({"running":{}});
                p
            },
            {
                let mut p = pods.clone();
                p["items"][0]["status"]["containerStatuses"] = json!([]);
                p
            },
            {
                let mut p = pods.clone();
                p["items"][0]["metadata"]["ownerReferences"][0]["uid"] = json!("foreign");
                p
            },
            {
                let mut p = pods.clone();
                p["items"][0]["status"]["containerStatuses"][0]["restartCount"] = json!(1);
                p
            },
            {
                let mut p = pods.clone();
                let duplicate = p["items"][0].clone();
                p["items"].as_array_mut().unwrap().push(duplicate);
                p
            },
            json!({"items":[]}),
        ] {
            assert!(!retained_job_stopped(&job, &bad, &handle).unwrap());
        }
        let mut partial = pods.clone();
        partial["metadata"]["continue"] = json!("more");
        assert!(retained_job_stopped(&job, &partial, &handle).is_err());
        let mut deleting = job.clone();
        deleting["metadata"]["deletionTimestamp"] = json!("2026-10-05T00:00:00Z");
        assert!(!retained_job_stopped(&deleting, &pods, &handle).unwrap());
        let mut active = job;
        active["status"]["active"] = json!(1);
        assert!(!retained_job_stopped(&active, &pods, &handle).unwrap());
    }
}
