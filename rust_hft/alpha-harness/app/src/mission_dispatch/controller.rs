//! Readback deployment packaging. Authority remains in native dispatch/settlement;
//! rendering neither creates cluster resources nor approves a research attempt.

use super::{
    admission, load_submission, render_manifest, validate_submission, ValidatedSubmission,
};
use crate::{
    cli::{print_json, CampaignControllerHandoffArgs, CampaignControllerPrepareArgs},
    data_mission::temporary_output_file,
    prediction_dispatch::{validate_cluster_target, validate_dns_label},
};
use alpha_domain::campaign_control::SignedCampaignRootGrantV1;
use anyhow::{bail, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MOUNT: &str = "/campaign-root";
const AUTHORITY: &str = "/authority";
const KUBECONFIG: &str = "/tmp/monday-campaign-kubeconfig.json";
const MAX_AUTHORITY_BYTES: u64 = 700_000;

pub(crate) fn render(args: CampaignControllerHandoffArgs) -> anyhow::Result<()> {
    let validated = validate_submission(load_submission(&args.submission)?)?;
    let manifest = render_value(&args, &validated)?;
    let manifest_sha256 = write_new_private_json(&args.output, &manifest)?;
    print_json(&json!({
        "status":"rendered", "campaign_id":validated.submission.request.campaign_id,
        "request_sha256":validated.request_sha256, "output":args.output,
        "manifest_sha256":manifest_sha256,
        "creates_cluster_resources":false,
    }))
}

fn mount_path(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let physical = path
        .canonicalize()
        .context("resolve controller volume input")?;
    let relative = physical
        .strip_prefix(root)
        .context("controller input escapes block-volume root")?;
    Ok(Path::new(MOUNT).join(relative))
}

fn read_authority(path: &Path) -> anyhow::Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_AUTHORITY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AUTHORITY_BYTES {
        bail!("controller authority exceeds Secret budget");
    }
    String::from_utf8(bytes).context("controller authority must be UTF-8 JSON")
}

pub(super) fn render_value(
    args: &CampaignControllerHandoffArgs,
    validated: &ValidatedSubmission,
) -> anyhow::Result<Value> {
    validate_cluster_target(&args.context, &args.namespace)?;
    for (label, name) in [
        ("PVC", &args.pvc),
        ("service account", &args.service_account),
        ("trusted keys ConfigMap", &args.trusted_keys_configmap),
        ("Campaign Pod", &args.campaign_pod),
    ] {
        validate_dns_label(label, name)?;
    }
    let root = args
        .volume_root
        .canonicalize()
        .context("resolve block-volume root")?;
    if !root.is_dir() || !args.work_dir.is_dir() {
        bail!("existing controller volume and work directories are required");
    }
    let work_dir = mount_path(&root, &args.work_dir)?;
    let state: Value = serde_json::from_str(&read_authority(
        &args.work_dir.join("controller-inputs.json"),
    )?)?;
    let request = &validated.submission.request;
    if state["context"] != args.context
        || state["namespace"] != args.namespace
        || state["source_revision"] != request.build_source_revision
        || state["image"] != validated.submission.image
        || state["campaign_inputs_sha256"] != request.campaign_inputs_sha256
    {
        bail!("controller checkpoint inputs differ from the submitted execution binding");
    }
    let generation = args
        .work_dir
        .join(format!("generation-{}", request.research_plan.generation));
    for marker in ["finalized", "dispatched"] {
        if !generation.join(marker).is_file() {
            bail!("controller handoff requires a finalized and dispatched generation");
        }
    }
    let checkpoint = validate_submission(load_submission(&generation.join("submission.json"))?)?;
    if checkpoint.request_sha256 != validated.request_sha256
        || checkpoint.submission.image != validated.submission.image
    {
        bail!("controller checkpoint submission differs from the requested handoff");
    }
    let finalized: Value =
        serde_json::from_str(&read_authority(&generation.join("finalize-report.json"))?)?;
    if finalized["request_sha256"] != validated.request_sha256
        || finalized["job_name"] != validated.job_name
    {
        bail!("controller checkpoint differs from the finalized request");
    }
    let mut control = admission::read_control(&args.control)?;
    if !control.ledger_path.is_file() {
        bail!("controller handoff requires an existing ledger file");
    }
    let worker_manifest = render_manifest(validated, &args.namespace)?;
    let inspection = admission::inspect_binding(
        validated,
        &worker_manifest,
        &control.materialization_path,
        &control.controller_image,
        control.attempt_ordinal,
    )?;
    let signed_root_bytes = read_authority(&control.signed_root_grant_path)?;
    let signed: SignedCampaignRootGrantV1 = serde_json::from_str(&signed_root_bytes)?;
    if signed.grant.execution != inspection.execution {
        bail!("controller handoff differs from the root execution binding");
    }
    let trusted_keys = read_authority(&control.trusted_keys_path)?;
    let _: std::collections::BTreeMap<String, String> = serde_json::from_str(&trusted_keys)?;
    control.ledger_path = mount_path(&root, &control.ledger_path)?;
    control.materialization_path = mount_path(&root, &control.materialization_path)?;
    control.signed_root_grant_path = Path::new(AUTHORITY).join("root-grant.json");
    control.trusted_keys_path = Path::new("/trusted-keys").join("root-public-keys.json");
    let control_bytes = serde_json::to_string(&control)?;
    if control_bytes.len() + trusted_keys.len() + signed_root_bytes.len()
        > MAX_AUTHORITY_BYTES as usize
    {
        bail!("controller authority exceeds Secret budget");
    }
    let name = format!("campaign-cycle-{}", &validated.request_sha256[..16]);
    let secret = format!("{name}-authority");
    let role = format!("{name}-readback");
    let labels = json!({"app.kubernetes.io/name":"monday-campaign-cycle-controller", "research.monday/owner":name});
    let metadata = |name: &str| json!({"name":name,"namespace":args.namespace,"labels":labels});
    let mounts = json!([
        {"name":"campaign-root","mountPath":MOUNT},
        {"name":"authority","mountPath":AUTHORITY,"readOnly":true},
        {"name":"trusted-keys","mountPath":"/trusted-keys","readOnly":true},
        {"name":"tmp","mountPath":"/tmp"}
    ]);
    let security = json!({"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}});
    Ok(json!({"apiVersion":"v1","kind":"List","items":[
        {"apiVersion":"v1","kind":"Secret","metadata":metadata(&secret),"immutable":true,"type":"Opaque",
         "stringData":{"control.json":control_bytes,"root-grant.json":signed_root_bytes}},
        {"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role","metadata":metadata(&role),"rules":[
            {"apiGroups":["batch"],"resources":["jobs"],"resourceNames":[validated.job_name],"verbs":["get","watch"]},
            {"apiGroups":[""],"resources":["pods"],"resourceNames":[args.campaign_pod],"verbs":["get"]},
            {"apiGroups":[""],"resources":["pods"],"verbs":["list"]}
        ]},
        {"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":metadata(&role),
         "subjects":[{"kind":"ServiceAccount","name":args.service_account,"namespace":args.namespace}],
         "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":role}},
        {"apiVersion":"batch/v1","kind":"Job","metadata":metadata(&name),"spec":{
            "backoffLimit":0,"activeDeadlineSeconds":28_800,"ttlSecondsAfterFinished":86_400,
            "template":{"metadata":{"labels":labels},"spec":{
                "restartPolicy":"Never","serviceAccountName":args.service_account,"automountServiceAccountToken":true,
                "imagePullSecrets":[{"name":"monday-acr"}],"nodeSelector":{"kubernetes.io/arch":"amd64","workload":"backtest"},
                "securityContext":{"runAsNonRoot":true,"runAsUser":1000,"runAsGroup":1000,"fsGroup":1000,
                                   "fsGroupChangePolicy":"OnRootMismatch","seccompProfile":{"type":"RuntimeDefault"}},
                "initContainers":[{"name":"prepare-controller","image":control.controller_image,
                    "command":["/usr/local/bin/alpha-harness","mission","dispatch","prepare-controller",
                               "--control","/authority/control.json","--context",args.context,"--namespace",args.namespace,
                               "--kubeconfig-out",KUBECONFIG],"volumeMounts":mounts,"securityContext":security,
                    "resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"256Mi"}}}],
                "containers":[{"name":"campaign-cycle-controller","image":control.controller_image,"imagePullPolicy":"IfNotPresent",
                    "args":["ack-readback","--work-dir",work_dir,"--campaign-pod-name",args.campaign_pod,
                            "--alpha-harness","/usr/local/bin/alpha-harness","--aliyun","aliyun","--kubectl","kubectl"],
                    "env":[{"name":"MONDAY_CAMPAIGN_CONTROL","value":"/authority/control.json"},{"name":"KUBECONFIG","value":KUBECONFIG}],
                    "volumeMounts":mounts,"securityContext":security,
                    "resources":{"requests":{"cpu":"500m","memory":"1Gi"},"limits":{"cpu":"2","memory":"4Gi"}}}],
                "volumes":[{"name":"campaign-root","persistentVolumeClaim":{"claimName":args.pvc}},
                           {"name":"authority","secret":{"secretName":secret,"defaultMode":288}},
                           {"name":"trusted-keys","configMap":{"name":args.trusted_keys_configmap}},
                           {"name":"tmp","emptyDir":{}}]
            }}
        }}
    ]}))
}

fn write_new_private_json(path: &Path, value: &Value) -> anyhow::Result<String> {
    let mut file = temporary_output_file(path, ".campaign-controller-")?;
    let bytes = serde_json::to_vec_pretty(value)?;
    file.as_file_mut().write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|error| error.error)
        .context("publish new private controller artifact")?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

#[cfg(unix)]
pub(super) fn restrict_integrity_key(ledger: &Path, expected_uid: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut key = ledger.as_os_str().to_os_string();
    key.push(".integrity-key");
    let key = PathBuf::from(key);
    let before = std::fs::symlink_metadata(&key)?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.uid() != expected_uid
        || before.len() != 32
    {
        bail!("controller integrity key must be an owned regular 32-byte file");
    }
    let file = std::fs::File::open(&key)?;
    let opened = file.metadata()?;
    if opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.uid() != expected_uid
        || opened.len() != 32
    {
        bail!("controller integrity key identity changed during preparation");
    }
    // Set permissions on the verified file handle, never on a re-resolved path.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn prepare(args: CampaignControllerPrepareArgs) -> anyhow::Result<()> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let control = admission::read_control(&args.control)?;
    if !control.ledger_path.is_file() {
        bail!("controller preparation requires an existing ledger file");
    }
    if !control.ledger_path.starts_with(MOUNT) || args.kubeconfig_out != Path::new(KUBECONFIG) {
        bail!("controller preparation requires its declared volume and temporary kubeconfig");
    }
    #[cfg(unix)]
    restrict_integrity_key(&control.ledger_path, 1000)?;
    #[cfg(not(unix))]
    bail!("controller preparation requires Unix file ownership");
    let host: std::net::IpAddr = std::env::var("KUBERNETES_SERVICE_HOST")?.parse()?;
    let port: u16 = std::env::var("KUBERNETES_SERVICE_PORT_HTTPS")?.parse()?;
    let server = format!("https://{}", std::net::SocketAddr::new(host, port));
    let config = json!({"apiVersion":"v1","kind":"Config",
        "clusters":[{"name":"research","cluster":{"server":server,"certificate-authority":"/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"}}],
        "users":[{"name":"controller","user":{"tokenFile":"/var/run/secrets/kubernetes.io/serviceaccount/token"}}],
        "contexts":[{"name":args.context,"context":{"cluster":"research","user":"controller","namespace":args.namespace}}],
        "current-context":args.context});
    write_new_private_json(&args.kubeconfig_out, &config)?;
    print_json(&json!({"status":"prepared","private_key_mode":"0600","token_materialized":false}))
}
