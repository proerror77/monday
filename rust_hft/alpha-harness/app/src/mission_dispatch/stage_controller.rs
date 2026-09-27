//! Stage permissions within the existing Campaign controller. No worker ledger access.
use super::{admission, sequence_admission as studies, *};
use crate::{
    cli::{CampaignStageControllerArgs, InitStageAuthorityArgs},
    mission_campaign::market_encoder::{self, worker, MarketRequest},
};
use alpha_domain::{
    campaign_control::{
        verify_campaign_root_grant, CampaignAttemptReservationV1, SignedCampaignRootGrantV1,
    },
    campaign_stage::*,
};
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub(crate) fn init_authority(args: InitStageAuthorityArgs) -> anyhow::Result<()> {
    let key = if args.private_key.try_exists()? {
        read_key(&args.private_key)?
    } else {
        let mut bytes = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut out = options.open(&args.private_key)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        SigningKey::from_bytes(&bytes)
    };
    let authority = CampaignStageAuthorityV1 {
        schema_version: AUTHORITY_SCHEMA.into(),
        public_key_hex: hex::encode(key.verifying_key().as_bytes()),
        work_pvc_name: args.work_pvc_name,
        work_pvc_uid: args.work_pvc_uid,
    };
    authority.validate().map_err(anyhow::Error::msg)?;
    immutable_json(&args.public_out, &authority)?;
    print_json(
        &json!({"status":"stage_authority_prepared","public_authority":args.public_out,"private_key_printed":false}),
    )
}
fn read_key(path: &Path) -> anyhow::Result<SigningKey> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != 32 {
        bail!("stage signing key is not an owned private 32-byte file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("stage signing key must have mode 0600");
        }
    }
    let bytes: [u8; 32] = std::fs::read(path)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("stage private key length changed"))?;
    Ok(SigningKey::from_bytes(&bytes))
}
fn no_symlink(path: &Path) -> anyhow::Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir | Component::CurDir) {
            bail!("stage path is not normalized");
        }
        current.push(part);
        if let Ok(meta) = std::fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                bail!("stage path contains symlink");
            }
        }
    }
    Ok(())
}
fn immutable_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    no_symlink(path)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    if path.try_exists()? {
        if !path.is_file() || std::fs::read(path)? != bytes {
            bail!(
                "existing stage authority identity changed: {}",
                path.display()
            );
        }
        return Ok(());
    }
    let parent = path.parent().context("stage artifact has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
struct ControllerLock(PathBuf);
impl Drop for ControllerLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
struct Loaded {
    validated: studies::Validated,
    request: MarketRequest,
    control: admission::DispatchControl,
    manifest: Value,
    reservation: CampaignAttemptReservationV1,
    work: PathBuf,
    private_state: PathBuf,
    key: SigningKey,
}
fn load(args: &CampaignStageControllerArgs) -> anyhow::Result<Loaded> {
    validate_cluster_target(&args.context, &args.namespace)?;
    let validated = studies::validate(admission::read_json(&args.submission)?)?;
    let studies::StudyRequest::MarketEncoder(request) = &validated.submission.request else {
        bail!("stage controller accepts only the typed market study");
    };
    let request = (**request).clone();
    if request.build_source_revision != crate::cli::BUILD_SOURCE_REVISION {
        bail!("stage controller source differs from request");
    }
    let control = admission::read_control(&args.control)?;
    let manifest = studies::render(&validated, &control, &args.namespace)?;
    studies::inspection(&validated, &control, &manifest)?;
    let reservation: CampaignAttemptReservationV1 = serde_json::from_str(
        manifest["items"][0]["stringData"]["sequence-attempt.json"]
            .as_str()
            .context("missing reserved stage attempt")?,
    )?;
    no_symlink(&args.pvc_root)?;
    let root = args.pvc_root.canonicalize()?;
    if !root.is_dir() {
        bail!("stage controller requires an existing PVC mount");
    }
    let key_path = args.private_key.canonicalize()?;
    if key_path.starts_with(&root) {
        bail!("stage signing key must stay outside the worker PVC");
    }
    let key = read_key(&args.private_key)?;
    if key.verifying_key() != request.stage_authority.key().map_err(anyhow::Error::msg)? {
        bail!("controller key differs from signed stage authority");
    }
    let pvc = kubectl_json(
        &kubectl_binary(),
        &args.context,
        &args.namespace,
        [
            "--request-timeout=30s",
            "get",
            "pvc",
            request.stage_authority.work_pvc_name.as_str(),
            "-o",
            "json",
        ],
        "verify stage work PVC",
    )?;
    if pvc["metadata"]["uid"] != request.stage_authority.work_pvc_uid
        || pvc["status"]["phase"] != "Bound"
    {
        bail!("stage work PVC identity or state changed");
    }
    let work = root.join(studies::work_sub_path(&reservation)?);
    no_symlink(&work)?;
    let private_state = key_path
        .parent()
        .context("stage key parent")?
        .join("stage-controller-state")
        .join(reservation.operation_id()?);
    no_symlink(&private_state)?;
    Ok(Loaded {
        validated,
        request,
        control,
        manifest,
        reservation,
        work,
        private_state,
        key,
    })
}
pub(crate) fn run(args: CampaignStageControllerArgs) -> anyhow::Result<()> {
    let loaded = load(&args)?;
    if args.prepare_only {
        std::fs::create_dir_all(&loaded.work)?;
        std::fs::create_dir_all(&loaded.private_state)?;
        let binding = json!({"schema_version":"monday.stage_work_binding.v1","reservation":loaded.reservation,"work_pvc_uid":loaded.request.stage_authority.work_pvc_uid,"job_name":loaded.validated.job_name});
        immutable_json(&loaded.work.join("attempt-binding.json"), &binding)?;
        immutable_json(&loaded.private_state.join("attempt-binding.json"), &binding)?;
        for dir in ["requests", "permits"] {
            let path = loaded.work.join("stage-authority").join(dir);
            no_symlink(&path)?;
            std::fs::create_dir_all(path)?;
        }
        return print_json(
            &json!({"status":"prepared","work_sub_path":studies::work_sub_path(&loaded.reservation)?,"operation_id":loaded.reservation.operation_id()?,"controller_pod_label":{"research.monday/stage-controller":&loaded.validated.identity[..32]},"worker_cpu_millis":3000,"controller_suggested_cpu_millis":250,"accounting_changed":false}),
        );
    }
    let expected = json!({"schema_version":"monday.stage_work_binding.v1","reservation":loaded.reservation,"work_pvc_uid":loaded.request.stage_authority.work_pvc_uid,"job_name":loaded.validated.job_name});
    if !loaded.private_state.is_dir() || !loaded.work.is_dir() {
        bail!("stage controller requires --prepare-only before canonical submit");
    }
    immutable_json(
        &loaded.private_state.join("attempt-binding.json"),
        &expected,
    )?;
    immutable_json(&loaded.work.join("attempt-binding.json"), &expected)?;
    let lock = loaded.private_state.join("controller.lock");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .context("another stage controller exists; reconcile stale ownership before restarting")?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_all()?;
    let _lock = ControllerLock(lock);
    drop(loaded);
    let mut previous = "";
    let mut delay = 1_u64;
    loop {
        let state = iteration(&args)?;
        if args.once || state == "terminal" {
            return print_json(&json!({"status":state,"accounting_changed":false}));
        }
        delay = if state == previous && state != "stage_issued" {
            (delay * 2).min(5)
        } else {
            1
        };
        previous = state;
        std::thread::sleep(Duration::from_secs(delay));
    }
}
fn iteration(args: &CampaignStageControllerArgs) -> anyhow::Result<&'static str> {
    // Reopen the existing ledger and current key projection for every boundary.
    let loaded = load(args)?;
    let signed: SignedCampaignRootGrantV1 =
        admission::read_json(&loaded.control.signed_root_grant_path)?;
    let mut record = {
        let store = alpha_store::AlphaStore::open_read_only(&loaded.control.ledger_path)?;
        store.campaign_dispatch_record(
            &loaded.reservation.family_id,
            &loaded.reservation.operation_id()?,
        )?
    };
    if record.reservation != loaded.reservation
        || record.claim.target.manifest_sha256
            != alpha_domain::canonical_json_hash(&loaded.manifest)?
        || record.claim.target.context != args.context
        || record.claim.target.namespace != args.namespace
    {
        bail!("stage controller changed its registered claim");
    }
    if record.settlement.is_some() {
        if record.terminal_pod_uid.is_none() {
            bail!("settled stage attempt lacks terminal provenance");
        }
        return Ok("terminal");
    }
    let cancellation_path = loaded.private_state.join("cancellation.json");
    let intent_path = loaded.private_state.join("cancellation-intent.json");
    if record.cancellation.is_none()
        && !cancellation_path.try_exists()?
        && intent_path.try_exists()?
    {
        let intent: CancellationIntent = admission::read_json(&intent_path)?;
        let observed = kubectl_json(
            &kubectl_binary(),
            &args.context,
            &args.namespace,
            [
                "--request-timeout=30s",
                "get",
                "job",
                loaded.validated.job_name.as_str(),
                "-o",
                "json",
            ],
            "reconcile cancellation with lost response",
        )?;
        let cancellation = confirm_cancellation(
            &loaded.manifest["items"][1],
            &loaded.reservation,
            &intent,
            &observed,
        )?;
        immutable_json(&cancellation_path, &cancellation)?;
    }
    if record.cancellation.is_none() && cancellation_path.try_exists()? {
        let cancellation = admission::read_json(&cancellation_path)?;
        let mut store = alpha_store::AlphaStore::open(&loaded.control.ledger_path)?;
        store.record_campaign_dispatch_cancellation(&loaded.reservation, &cancellation)?;
        record = store.campaign_dispatch_record(
            &loaded.reservation.family_id,
            &loaded.reservation.operation_id()?,
        )?;
    }
    if let Some(cancellation) = &record.cancellation {
        if read_cancelled_terminal(
            &args.context,
            &args.namespace,
            &loaded.manifest["items"][1],
            cancellation,
        )?
        .is_some()
        {
            return Ok("terminal");
        }
        if Utc::now() >= cancellation.requested_at + chrono::TimeDelta::seconds(120) {
            bail!("cancellation was requested but failed terminal readback is still unconfirmed; retain the full charge for reconciliation");
        }
        return Ok("cancellation_requested");
    }
    let uid = record
        .claim
        .job_uid
        .as_deref()
        .context("stage controller requires a bound Job UID")?;
    let job = kubectl_json(
        &kubectl_binary(),
        &args.context,
        &args.namespace,
        [
            "--request-timeout=30s",
            "get",
            "job",
            loaded.validated.job_name.as_str(),
            "-o",
            "json",
        ],
        "read stage Job",
    )?;
    validate_job_readback(
        &job,
        &loaded.manifest["items"][1],
        &loaded.validated.job_name,
        &loaded.validated.request_sha256,
        false,
    )?;
    if job["metadata"]["uid"] != uid {
        bail!("stage Job UID changed");
    }
    let terminal = job["status"]["conditions"].as_array().is_some_and(|a| {
        a.iter()
            .any(|c| c["status"] == "True" && (c["type"] == "Complete" || c["type"] == "Failed"))
    });
    if terminal {
        return Ok("terminal");
    }
    let Some(start) = job["status"]["startTime"].as_str() else {
        if Utc::now() >= signed.grant.expires_at {
            bail!("stage controller reached original grant deadline before Job start");
        }
        return Ok("waiting_for_job");
    };
    let started: DateTime<Utc> = start.parse()?;
    let deadline = started
        + chrono::TimeDelta::seconds(i64::try_from(loaded.reservation.reserved_job_seconds)?);
    let selector = format!("job-name={}", loaded.validated.job_name);
    let pods = kubectl_json(
        &kubectl_binary(),
        &args.context,
        &args.namespace,
        [
            "--request-timeout=30s",
            "get",
            "pods",
            "-l",
            selector.as_str(),
            "-o",
            "json",
        ],
        "read stage worker Pod",
    )?;
    let pods = pods["items"].as_array().context("missing worker Pods")?;
    if pods.is_empty() {
        if Utc::now() >= deadline.min(signed.grant.expires_at) {
            bail!("stage controller reached original deadline without a worker Pod");
        }
        return Ok("waiting_for_pod");
    }
    if pods.len() != 1 {
        bail!("stage controller requires exactly one worker Pod");
    }
    let pod = &pods[0];
    let pod_uid = pod["metadata"]["uid"]
        .as_str()
        .context("worker Pod UID missing")?;
    if !pod["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|a| {
            a.iter()
                .any(|r| r["kind"] == "Job" && r["uid"] == uid && r["controller"] == true)
        })
        || pod["spec"]["containers"][0]["image"] != loaded.request.image
    {
        bail!("stage Pod does not belong to the exact claimed Job/image");
    }
    validate_running_pod(&loaded.manifest["items"][1], pod, uid)?;
    let active = (|| -> anyhow::Result<()> {
        let trusted = admission::read_trusted_keys(&loaded.control.trusted_keys_path)?;
        let verified = verify_campaign_root_grant(&signed, &trusted, Utc::now())?;
        let mut store = alpha_store::AlphaStore::open(&loaded.control.ledger_path)?;
        store.with_running_campaign_admission(
            &verified,
            &loaded.reservation,
            &record.claim.target,
            uid,
            started,
            Utc::now,
            |_, _| -> anyhow::Result<()> { Ok(()) },
        )
    })();
    if let Err(error) = active {
        let cancellation = cancel_bound_job(
            args,
            &loaded,
            &job,
            pod_uid,
            started,
            deadline,
            &format!("{error:#}"),
        )?;
        immutable_json(&cancellation_path, &cancellation)?;
        let mut store = alpha_store::AlphaStore::open(&loaded.control.ledger_path)?;
        store.record_campaign_dispatch_cancellation(&loaded.reservation, &cancellation)?;
        return Ok("cancellation_requested");
    }
    let job_identity = json!({"job_uid":uid,"pod_uid":pod_uid,"job_started_at":started,"job_deadline_at":deadline});
    immutable_json(&loaded.private_state.join("job.json"), &job_identity)?;
    let stages = worker::stages(&loaded.request)?;
    let results = loaded.work.join("market-encoder-results");
    let mut checked = worker::empty_result(
        &loaded.request,
        &loaded.validated.request_sha256,
        &signed.content_sha256,
        &loaded.reservation.content_hash()?,
    )?;
    for stage in stages {
        let name = worker::stage_name(stage.key);
        let receipt = results.join(format!("{name}.receipt.json"));
        if receipt.try_exists()? {
            no_symlink(&receipt)?;
            let receipt: worker::StageReceipt = market_encoder::read_json(&receipt)?;
            worker::verify_stage_record(&loaded.request, &results, &receipt, &mut checked)?;
            if checked.job_uid != uid || checked.job_deadline_at != Some(deadline) {
                bail!("stage history belongs to another Job or deadline");
            }
            continue;
        }
        let request_path = loaded
            .work
            .join("stage-authority/requests")
            .join(format!("{name}.json"));
        if !request_path.try_exists()? {
            return Ok("waiting_for_stage");
        }
        no_symlink(&request_path)?;
        let request: CampaignStageRequestV1 = admission::read_json(&request_path)?;
        request.validate().map_err(anyhow::Error::msg)?;
        if request.request_sha256 != loaded.validated.request_sha256
            || request.attempt_sha256 != loaded.reservation.content_hash()?
            || request.root_grant_sha256 != signed.content_sha256
            || request.job_name != loaded.validated.job_name
            || request.pod_uid != pod_uid
            || request.stage != stage.key
        {
            bail!("stage nonce request differs from current Job/attempt/stage");
        }
        let private = loaded.private_state.join(format!("{name}.permit.json"));
        let output = loaded
            .work
            .join("stage-authority/permits")
            .join(format!("{name}.json"));
        if private.try_exists()? {
            let existing: SignedCampaignStagePermitV1 = admission::read_json(&private)?;
            if existing.request != request
                || existing.job_uid != uid
                || existing.job_deadline_at != deadline
            {
                bail!("stage nonce or writer changed after issuance");
            }
            verify_stage_permit(
                &loaded.request.stage_authority,
                &request,
                &existing,
                existing.issued_at,
            )
            .map_err(anyhow::Error::msg)?;
            // An issued stage may already be fitting. Never renew its capability.
            immutable_json(&output, &existing)?;
            return Ok("stage_already_issued");
        }
        if results.join(format!("{name}.start.json")).try_exists()?
            || results.join(format!("{name}.weights")).try_exists()?
        {
            bail!("stage already started without recorded permission");
        }
        // Fresh trust and a short database transaction only after external reads.
        // The controller holds no live DuckDB handle while waiting or querying Kubernetes.
        let trusted = admission::read_trusted_keys(&loaded.control.trusted_keys_path)?;
        let verified = verify_campaign_root_grant(&signed, &trusted, Utc::now())?;
        let mut store = alpha_store::AlphaStore::open(&loaded.control.ledger_path)?;
        store.with_running_campaign_admission(
            &verified,
            &loaded.reservation,
            &record.claim.target,
            uid,
            started,
            Utc::now,
            |now, authority_deadline| -> anyhow::Result<()> {
                let permit = sign_stage_permit_bounded(
                    &loaded.request.stage_authority,
                    request.clone(),
                    uid.into(),
                    deadline,
                    authority_deadline,
                    now,
                    &loaded.key,
                )
                .map_err(anyhow::Error::msg)?;
                immutable_json(&private, &permit)?;
                immutable_json(&output, &permit)?;
                Ok(())
            },
        )?;
        crate::mission_runner::research_event(
            "alpha-harness",
            "campaign_stage_permission_issued",
            json!({"operation_id":loaded.reservation.operation_id()?,"job_uid":uid,"stage":request.stage,"nonce":request.nonce,"request_sha256":loaded.validated.request_sha256}),
        );
        return Ok("stage_issued");
    }
    Ok("waiting_for_terminal")
}

fn validate_running_pod(expected_job: &Value, pod: &Value, job_uid: &str) -> anyhow::Result<()> {
    if !pod["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|a| {
            a.iter()
                .any(|r| r["kind"] == "Job" && r["uid"] == job_uid && r["controller"] == true)
        })
    {
        bail!("worker Pod owner changed");
    }
    let mut observed = expected_job.clone();
    observed["spec"]["template"]["spec"] = pod["spec"].clone();
    // Node assignment is made by Kubernetes, not by the worker's execution contract.
    observed["spec"]["template"]["spec"]["nodeName"] =
        expected_job["spec"]["template"]["spec"]["nodeName"].clone();
    if job_execution_projection(&observed) != job_execution_projection(expected_job) {
        bail!("worker Pod differs from claimed executable, inputs or resources");
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancellationIntent {
    schema_version: String,
    /// Empty response fields are deliberately invalid as completed ledger evidence.
    planned: alpha_store::campaign_ledger::CampaignDispatchCancellationV1,
}
fn cancel_bound_job(
    args: &CampaignStageControllerArgs,
    loaded: &Loaded,
    job: &Value,
    pod_uid: &str,
    started: DateTime<Utc>,
    deadline: DateTime<Utc>,
    reason: &str,
) -> anyhow::Result<alpha_store::campaign_ledger::CampaignDispatchCancellationV1> {
    let uid = job["metadata"]["uid"]
        .as_str()
        .context("cancellation Job UID missing")?;
    let version = job["metadata"]["resourceVersion"]
        .as_str()
        .context("cancellation Job resourceVersion missing")?;
    let patch = cancellation_patch(uid, version);
    let intent = CancellationIntent {
        schema_version: "monday.campaign_cancellation_intent.v1".into(),
        planned: alpha_store::campaign_ledger::CampaignDispatchCancellationV1 {
            operation_id: loaded.reservation.operation_id()?,
            job_uid: uid.into(),
            pod_uid: pod_uid.into(),
            original_resource_version: version.into(),
            patched_resource_version: String::new(),
            reason: reason.chars().take(2048).collect(),
            requested_at: Utc::now(),
            job_started_at: started,
            original_deadline_at: deadline,
            patch_sha256: alpha_domain::canonical_json_hash(&patch)?,
            patch_result_sha256: String::new(),
        },
    };
    immutable_json(
        &loaded.private_state.join("cancellation-intent.json"),
        &intent,
    )?;
    let text = serde_json::to_string(&patch)?;
    let result = kubectl_with_input(
        &kubectl_binary(),
        &args.context,
        &args.namespace,
        [
            "--request-timeout=30s",
            "patch",
            "job",
            loaded.validated.job_name.as_str(),
            "--type=json",
            "--patch",
            text.as_str(),
            "-o",
            "json",
        ],
        &[],
    )?;
    let patched: Value = serde_json::from_slice(&ensure_kubectl_success(
        result,
        "stop exact revoked Campaign Job; durable intent retained for independent reconciliation",
    )?)?;
    confirm_cancellation(
        &loaded.manifest["items"][1],
        &loaded.reservation,
        &intent,
        &patched,
    )
}
fn confirm_cancellation(
    expected: &Value,
    reservation: &CampaignAttemptReservationV1,
    intent: &CancellationIntent,
    observed: &Value,
) -> anyhow::Result<alpha_store::campaign_ledger::CampaignDispatchCancellationV1> {
    let planned = &intent.planned;
    if intent.schema_version != "monday.campaign_cancellation_intent.v1"
        || planned.operation_id != reservation.operation_id()?
        || !planned.patched_resource_version.is_empty()
        || !planned.patch_result_sha256.is_empty()
        || alpha_domain::canonical_json_hash(&cancellation_patch(
            &planned.job_uid,
            &planned.original_resource_version,
        ))? != planned.patch_sha256
    {
        bail!("cancellation intent changed its charged attempt or patch");
    }
    let mut shortened = expected.clone();
    shortened["spec"]["activeDeadlineSeconds"] = json!(1);
    validate_job_readback(
        observed,
        &shortened,
        expected["metadata"]["name"]
            .as_str()
            .context("cancellation Job name")?,
        &reservation.request_sha256,
        false,
    )
    .context("cancellation effect is unconfirmed; do not repeat or infer a successful patch")?;
    let started: DateTime<Utc> = observed["status"]["startTime"]
        .as_str()
        .context("cancellation start time missing")?
        .parse()?;
    if observed["metadata"]["uid"] != planned.job_uid
        || started != planned.job_started_at
        || planned.original_deadline_at
            != started
                + chrono::TimeDelta::seconds(i64::try_from(reservation.reserved_job_seconds)?)
    {
        bail!("cancellation readback changed its original Job identity or deadline");
    }
    let version = observed["metadata"]["resourceVersion"]
        .as_str()
        .context("cancellation resourceVersion missing")?;
    if version == planned.original_resource_version {
        bail!("cancellation mutation is not independently visible");
    }
    let mut completed = planned.clone();
    completed.patched_resource_version = version.into();
    completed.patch_result_sha256 = alpha_domain::canonical_json_hash(observed)?;
    Ok(completed)
}
pub(super) fn cancellation_patch(uid: &str, version: &str) -> Value {
    json!([
        {"op":"test","path":"/metadata/uid","value":uid},
        {"op":"test","path":"/metadata/resourceVersion","value":version},
        {"op":"replace","path":"/spec/activeDeadlineSeconds","value":1}
    ])
}
pub(super) fn read_cancelled_terminal(
    context: &str,
    namespace: &str,
    expected: &Value,
    cancel: &alpha_store::campaign_ledger::CampaignDispatchCancellationV1,
) -> anyhow::Result<Option<Value>> {
    let name = expected["metadata"]["name"]
        .as_str()
        .context("cancelled Job name")?;
    let job = kubectl_json(
        &kubectl_binary(),
        context,
        namespace,
        ["--request-timeout=30s", "get", "job", name, "-o", "json"],
        "read cancelled Campaign Job",
    )?;
    let selector = format!("job-name={name}");
    let pods = kubectl_json(
        &kubectl_binary(),
        context,
        namespace,
        [
            "--request-timeout=30s",
            "get",
            "pods",
            "-l",
            selector.as_str(),
            "-o",
            "json",
        ],
        "read cancelled Campaign Pod",
    )?;
    let pods = pods["items"]
        .as_array()
        .context("cancelled Pod list missing")?;
    if pods.len() != 1 {
        bail!("cancelled Campaign must retain exactly one original execution Pod");
    }
    validate_cancelled_terminal(expected, &job, &pods[0], cancel)
}
fn validate_cancelled_terminal(
    expected: &Value,
    job: &Value,
    pod: &Value,
    cancel: &alpha_store::campaign_ledger::CampaignDispatchCancellationV1,
) -> anyhow::Result<Option<Value>> {
    let mut shortened = expected.clone();
    shortened["spec"]["activeDeadlineSeconds"] = json!(1);
    let name = expected["metadata"]["name"]
        .as_str()
        .context("cancelled Job name")?;
    let hash = expected["metadata"]["annotations"]["research.monday/request-sha256"]
        .as_str()
        .context("cancelled request hash")?;
    validate_job_readback(job, &shortened, name, hash, false)?;
    let observed_start: DateTime<Utc> = job["status"]["startTime"]
        .as_str()
        .context("cancelled Job lost its original start time")?
        .parse()?;
    let original_seconds = expected["spec"]["activeDeadlineSeconds"]
        .as_u64()
        .context("missing original Job deadline")?;
    if observed_start != cancel.job_started_at
        || observed_start + chrono::TimeDelta::seconds(i64::try_from(original_seconds)?)
            != cancel.original_deadline_at
        || job["metadata"]["uid"] != cancel.job_uid
        || pod["metadata"]["uid"] != cancel.pod_uid
        || alpha_domain::canonical_json_hash(&cancellation_patch(
            &cancel.job_uid,
            &cancel.original_resource_version,
        ))? != cancel.patch_sha256
    {
        bail!("cancelled Job differs from authenticated cancellation evidence");
    }
    validate_running_pod(expected, pod, &cancel.job_uid)?;
    let failed = job["status"]["conditions"].as_array().is_some_and(|a| {
        a.iter().any(|c| {
            c["type"] == "Failed" && c["status"] == "True" && c["reason"] == "DeadlineExceeded"
        })
    });
    if !failed
        || job["status"]["active"].as_u64().unwrap_or(0) != 0
        || pod["status"]["phase"] != "Failed"
    {
        return Ok(None);
    }
    if job["status"]["succeeded"].as_u64().unwrap_or(0) > 0 {
        bail!("cancellation cannot promote a successful result");
    }
    Ok(Some(
        json!({"schema_version":"monday.campaign_cancelled_terminal.v1","cancellation":cancel,
        "job_uid":cancel.job_uid,"pod_uid":cancel.pod_uid,"request_sha256":hash,"outcome":"failed",
        "job_failed_reason":"DeadlineExceeded","pod_phase":"Failed","sealed_holdout_opened":false,"deployment_authority":false}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_controller_cancelled_terminal_accepts_only_attested_deadline_change() {
        let request = crate::mission_campaign::market_encoder::request_tests::request();
        let name = "alpha-campaign-cancellation-test";
        let manifest = render_campaign_manifest(
            DispatchManifestInput {
                attempt_id: "cancel-test",
                campaign_id: &request.campaign_id,
                job_name: name,
                secret_name: "cancel-inputs",
                image: &request.image,
                image_digest: &request.image_identity,
                request_sha256: &"a".repeat(64),
                request_json: "{}",
                submission_identity_sha256: &"b".repeat(64),
                active_deadline_seconds: 1200,
                trusted_keys_json: None,
                args: vec!["mission".into()],
            },
            "monday-research",
        )
        .unwrap();
        let expected = &manifest["items"][1];
        let mut job = expected.clone();
        job["metadata"]["uid"] = json!("job-uid");
        job["metadata"]["resourceVersion"] = json!("after");
        job["spec"]["suspend"] = json!(false);
        job["spec"]["activeDeadlineSeconds"] = json!(1);
        job["status"] = json!({"active":0,"failed":1,"conditions":[{"type":"Failed","status":"True","reason":"DeadlineExceeded"}]});
        let mut pod = json!({"metadata":{"uid":"pod-uid","ownerReferences":[{"kind":"Job","uid":"job-uid","controller":true}]},"spec":expected["spec"]["template"]["spec"],"status":{"phase":"Failed"}});
        pod["spec"]["nodeName"] = json!("research-worker");
        let now = Utc::now();
        job["status"]["startTime"] = json!(now);
        let reservation = CampaignAttemptReservationV1 {
            schema_version: alpha_domain::campaign_control::ATTEMPT_SCHEMA.into(),
            root_grant_sha256: "c".repeat(64),
            family_id: request.plan.study_id.clone(),
            campaign_id: request.campaign_id.clone(),
            execution: studies::execution_binding(
                &request,
                &format!("registry/controller@sha256:{}", "f".repeat(64)),
            )
            .unwrap(),
            generation: 0,
            parent_result_sha256: None,
            policy_revision_id: request.policy_id().unwrap(),
            request_sha256: "a".repeat(64),
            attempt_ordinal: 0,
            declared_trials: 22,
            reserved_job_seconds: 1200,
            reserved_llm_tokens: 0,
        };
        let cancel = alpha_store::campaign_ledger::CampaignDispatchCancellationV1 {
            operation_id: reservation.operation_id().unwrap(),
            job_uid: "job-uid".into(),
            pod_uid: "pod-uid".into(),
            original_resource_version: "before".into(),
            patched_resource_version: "after".into(),
            reason: "study revoked after P".into(),
            requested_at: now,
            job_started_at: now,
            original_deadline_at: now + chrono::TimeDelta::seconds(1200),
            patch_sha256: alpha_domain::canonical_json_hash(&cancellation_patch(
                "job-uid", "before",
            ))
            .unwrap(),
            patch_result_sha256: "a".repeat(64),
        };
        assert!(validate_cancelled_terminal(expected, &job, &pod, &cancel)
            .unwrap()
            .is_some());
        let mut planned = cancel.clone();
        planned.patched_resource_version.clear();
        planned.patch_result_sha256.clear();
        let intent = CancellationIntent {
            schema_version: "monday.campaign_cancellation_intent.v1".into(),
            planned,
        };
        // An API response lost after applying the patch is recovered by independent GET.
        let recovered = confirm_cancellation(expected, &reservation, &intent, &job).unwrap();
        assert_eq!(recovered.job_uid, cancel.job_uid);
        assert_eq!(recovered.original_resource_version, "before");
        let mut unapplied = job.clone();
        unapplied["spec"]["activeDeadlineSeconds"] = json!(1200);
        assert!(confirm_cancellation(expected, &reservation, &intent, &unapplied).is_err());
        let mut replacement = job.clone();
        replacement["metadata"]["uid"] = json!("different-job");
        assert!(confirm_cancellation(expected, &reservation, &intent, &replacement).is_err());
        let mut waiting = job.clone();
        waiting["status"]["conditions"] = json!([]);
        assert!(
            validate_cancelled_terminal(expected, &waiting, &pod, &cancel)
                .unwrap()
                .is_none()
        );
        let mut drift = job.clone();
        drift["spec"]["activeDeadlineSeconds"] = json!(2);
        assert!(validate_cancelled_terminal(expected, &drift, &pod, &cancel).is_err());
        let mut drift = pod.clone();
        drift["metadata"]["uid"] = json!("another-pod");
        assert!(validate_cancelled_terminal(expected, &job, &drift, &cancel).is_err());
        let mut drift = pod;
        drift["spec"]["containers"][0]["args"] = json!(["different-execution"]);
        assert!(validate_cancelled_terminal(expected, &job, &drift, &cancel).is_err());
        let patch = cancellation_patch("job-uid", "before");
        assert_eq!(
            patch[0],
            json!({"op":"test","path":"/metadata/uid","value":"job-uid"})
        );
        assert_eq!(
            patch[1],
            json!({"op":"test","path":"/metadata/resourceVersion","value":"before"})
        );
    }
    #[test]
    fn stage_controller_authority_is_private_reusable_and_refuses_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let args = InitStageAuthorityArgs {
            private_key: root.join("private.key"),
            public_out: root.join("public.json"),
            work_pvc_name: "market-work".into(),
            work_pvc_uid: "task-pvc-uid".into(),
        };
        init_authority(args.clone()).unwrap();
        let before = std::fs::read(&args.public_out).unwrap();
        init_authority(args.clone()).unwrap();
        assert_eq!(std::fs::read(&args.public_out).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::{fs::symlink, fs::PermissionsExt};
            assert_eq!(
                std::fs::metadata(&args.private_key)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            symlink(&root, root.join("redirect")).unwrap();
            assert!(
                immutable_json(&root.join("redirect/permit.json"), &json!({"bad":"write"}))
                    .is_err()
            );
        }
    }
}
