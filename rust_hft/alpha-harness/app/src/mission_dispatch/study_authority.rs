//! Small authority metadata operations over the existing domain signatures and
//! authenticated ledger. This tool never creates approvals, resets usage or dispatches work.
use crate::cli::print_json;
use alpha_domain::{
    campaign_control::{
        sign_campaign_root_grant, verify_campaign_root_grant, CampaignRootGrantV1,
        SignedCampaignRootGrantV1, VerifiedCampaignRootGrant,
    },
    campaign_study::{
        sign_campaign_study_grant, verify_campaign_study_grant, CampaignStudyGrantV1,
        SignedCampaignStudyGrantV1, VerifiedCampaignStudyGrant,
    },
};
use alpha_store::{campaign_ledger::CampaignLedgerEventV1, AlphaStore, ApprovalRecord};
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const ROOTS_SCHEMA: &str = "monday.campaign_study_roots.v1";
const REGISTRATION_SCHEMA: &str = "monday.campaign_study_registration.v1";

#[derive(Debug, Clone, clap::Args)]
pub struct RootSignArgs {
    #[arg(long)]
    pub grant: PathBuf,
    #[arg(long)]
    pub key_id: String,
    #[arg(long)]
    pub signing_key: PathBuf,
    #[arg(long)]
    pub trusted_keys: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
}
#[derive(Debug, Clone, clap::Args)]
pub struct StudySignArgs {
    #[arg(long)]
    pub grant: PathBuf,
    #[arg(long)]
    pub key_id: String,
    #[arg(long)]
    pub signing_key: PathBuf,
    #[arg(long)]
    pub trusted_keys: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
}
#[derive(Debug, Clone, clap::Args)]
pub struct StudyRegisterArgs {
    #[arg(long)]
    pub ledger: PathBuf,
    #[arg(long)]
    pub signed_study: PathBuf,
    #[arg(long)]
    pub trusted_keys: PathBuf,
    #[arg(long)]
    pub study_approval_id: String,
    /// JSON roots manifest; relative grant paths resolve beside this file.
    #[arg(long)]
    pub roots: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
}
#[derive(Debug, Clone, clap::Args)]
pub struct StudyInspectArgs {
    #[arg(long)]
    pub ledger: PathBuf,
    #[arg(long)]
    pub study_id: String,
    /// Create-once bounded summary, never a ledger or archive export.
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootBinding {
    signed_root_grant_path: PathBuf,
    approval_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootsManifest {
    schema_version: String,
    roots: Vec<RootBinding>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisteredRoot {
    grant_sha256: String,
    registration_receipt_sha256: String,
    approval_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrationReport {
    schema_version: String,
    study_id: String,
    study_grant_sha256: String,
    study_approval_id: String,
    registration_receipt_sha256: String,
    registration_sequence: u64,
    roots: BTreeMap<String, RegisteredRoot>,
    ledger_host: String,
    trial_accounting_changed: bool,
}

fn read<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let file = File::open(path).context("open authority metadata")?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_METADATA_BYTES {
        bail!("authority metadata is not a bounded regular file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!("authority metadata exceeds byte limit");
    }
    serde_json::from_slice(&bytes).context("invalid typed authority metadata")
}
fn retain<T: Serialize + DeserializeOwned + PartialEq>(
    path: &Path,
    value: &T,
) -> anyhow::Result<()> {
    crate::data_mission::ensure_output_path_is_not_symlink(path, "authority metadata output")?;
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!("authority output exceeds byte limit");
    }
    if path.exists() {
        let old: T = read(path)?;
        if old != *value {
            bail!("authority output already contains a different identity or ledger head");
        }
        return Ok(());
    }
    let parent = path.parent().context("authority output parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn trust(path: &Path) -> anyhow::Result<BTreeMap<String, VerifyingKey>> {
    let encoded: BTreeMap<String, String> = read(path)?;
    if encoded.is_empty() || encoded.len() > 64 {
        bail!("authority trust set is empty or oversized");
    }
    encoded
        .into_iter()
        .map(|(id, value)| {
            let bytes: [u8; 32] = hex::decode(value)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid public key length"))?;
            let key = VerifyingKey::from_bytes(&bytes)?;
            if key.is_weak() {
                bail!("weak authority public key");
            }
            Ok((id, key))
        })
        .collect()
}
fn signing_key(
    path: &Path,
    id: &str,
    trusted: &BTreeMap<String, VerifyingKey>,
) -> anyhow::Result<SigningKey> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() != 32 {
        bail!("authority signing key must be a private regular 32-byte file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            bail!("authority signing key must have mode 0600");
        }
    }
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            bail!("authority signing key identity changed during open");
        }
    }
    if opened.len() != 32 {
        bail!("authority signing key length changed");
    }
    let mut bytes = [0_u8; 32];
    file.read_exact(&mut bytes)?;
    let key = SigningKey::from_bytes(&bytes);
    if trusted.get(id) != Some(&key.verifying_key()) {
        bail!("signing key does not match the named trusted public key");
    }
    Ok(key)
}
fn fingerprint(key: &VerifyingKey) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn sign_root(args: RootSignArgs) -> anyhow::Result<()> {
    let grant: CampaignRootGrantV1 = read(&args.grant)?;
    let trusted = trust(&args.trusted_keys)?;
    let key = signing_key(&args.signing_key, &args.key_id, &trusted)?;
    let valid_from = grant.valid_from;
    let signed = sign_campaign_root_grant(grant, args.key_id, &key)?;
    // Signing may precede the future valid_from. Actual admission still checks
    // the current clock; this only verifies the typed signature and identity.
    verify_campaign_root_grant(&signed, &trusted, valid_from)?;
    retain(&args.output, &signed)?;
    print_json(
        &json!({"status":"root_signed","root_id":signed.grant.root_id,
        "grant_sha256":signed.content_sha256,"key_id":signed.key_id,
        "public_key_sha256":fingerprint(&key.verifying_key()),"output":args.output,"metadata_only":true}),
    )
}
pub fn sign_study(args: StudySignArgs) -> anyhow::Result<()> {
    let grant: CampaignStudyGrantV1 = read(&args.grant)?;
    let trusted = trust(&args.trusted_keys)?;
    let key = signing_key(&args.signing_key, &args.key_id, &trusted)?;
    let valid_from = grant.valid_from;
    let signed = sign_campaign_study_grant(grant, args.key_id, &key)?;
    verify_campaign_study_grant(&signed, &trusted, valid_from)?;
    retain(&args.output, &signed)?;
    print_json(
        &json!({"status":"study_signed","study_id":signed.grant.study_id,
        "grant_sha256":signed.content_sha256,"key_id":signed.key_id,
        "public_key_sha256":fingerprint(&key.verifying_key()),"output":args.output,"metadata_only":true}),
    )
}

fn require_ack() -> anyhow::Result<()> {
    crate::cli::require_cloud_data_host(std::env::consts::OS)?;
    if std::env::var("MONDAY_EXECUTION_HOST").as_deref() != Ok("ack") {
        bail!("Study ledger registration and inspection require the ACK execution boundary");
    }
    Ok(())
}
fn existing_ledger(path: &Path) -> anyhow::Result<()> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        bail!("Study operation requires an existing regular ledger");
    }
    if std::env::var_os("ALPHA_STORE_INTEGRITY_KEY_HEX").is_some() {
        bail!("Study operation requires the existing file-backed ledger integrity key");
    }
    // No create-on-missing key or database path is accepted by this tool.
    drop(AlphaStore::open_read_only(path)?);
    Ok(())
}

struct Loaded {
    study: VerifiedCampaignStudyGrant,
    roots: Vec<(VerifiedCampaignRootGrant, String)>,
}
fn load(args: &StudyRegisterArgs, now: DateTime<Utc>) -> anyhow::Result<Loaded> {
    let trusted = trust(&args.trusted_keys)?;
    let signed: SignedCampaignStudyGrantV1 = read(&args.signed_study)?;
    let study = verify_campaign_study_grant(&signed, &trusted, now)?;
    let manifest: RootsManifest = read(&args.roots)?;
    if manifest.schema_version != ROOTS_SCHEMA
        || manifest.roots.len() != study.grant().members.len()
    {
        bail!("root bindings do not cover the exact signed Study members");
    }
    let base = args.roots.parent().context("root bindings parent")?;
    let mut families = BTreeSet::new();
    let mut roots = Vec::new();
    for binding in manifest.roots {
        let signed: SignedCampaignRootGrantV1 = read(&base.join(binding.signed_root_grant_path))?;
        let verified = verify_campaign_root_grant(&signed, &trusted, now)?;
        let member = study
            .member(&signed.grant.family.family_id)
            .context("unlisted Study root family")?;
        if !member.matches_root(&signed.grant, &signed.content_sha256)
            || !families.insert(signed.grant.family.family_id.clone())
            || binding.approval_id.is_empty()
        {
            bail!("root binding changes a signed member or repeats its family");
        }
        roots.push((verified, binding.approval_id));
    }
    roots.sort_by(|a, b| {
        a.0.grant()
            .family
            .family_id
            .cmp(&b.0.grant().family.family_id)
    });
    Ok(Loaded { study, roots })
}

// Cheap complete preflight avoids partial registrations for wrong metadata.
// The native store repeats the authoritative checks under its approval guards.
struct ApprovalScope<'a> {
    class: &'a str,
    subject: &'a str,
    key_id: &'a str,
    payload: Value,
    valid_from: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}
fn approval_matches(
    approval: &ApprovalRecord,
    scope: ApprovalScope<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    approval.validate()?;
    if approval.approval_class != scope.class
        || approval.subject_id != scope.subject
        || approval.signer_id.as_deref() != Some(scope.key_id)
        || !approval.is_active_at(now)
        || approval.valid_from.is_none_or(|t| t > scope.valid_from)
        || approval.expires_at.is_none_or(|t| t < scope.expires_at)
        || scope
            .payload
            .as_object()
            .context("approval scope payload")?
            .iter()
            .any(|(k, v)| approval.payload.get(k) != Some(v))
    {
        bail!("existing approval does not authorize this exact grant");
    }
    Ok(())
}

fn preflight(
    store: &AlphaStore,
    args: &StudyRegisterArgs,
    loaded: &Loaded,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let study = &loaded.study;
    let g = study.grant();
    approval_matches(
        &store.get_approval(&args.study_approval_id)?,
        ApprovalScope {
            class: "campaign_study",
            subject: &g.study_id,
            key_id: &study.signed_grant().key_id,
            payload: json!({"grant_sha256":study.content_sha256(),"study_id":g.study_id}),
            valid_from: g.valid_from,
            expires_at: g.expires_at,
        },
        now,
    )?;
    if store
        .campaign_study_grant(&g.study_id)?
        .is_some_and(|old| old != *study.signed_grant())
    {
        bail!("Study is already registered with different authority");
    }
    for (root, approval_id) in &loaded.roots {
        let r = root.grant();
        approval_matches(
            &store.get_approval(approval_id)?,
            ApprovalScope {
                class: "campaign_root",
                subject: &r.root_id,
                key_id: &root.signed_grant().key_id,
                payload: json!({"grant_sha256":root.content_sha256(),"family_id":r.family.family_id}),
                valid_from: r.valid_from,
                expires_at: r.expires_at,
            },
            now,
        )?;
        if store
            .campaign_study_id_for_family(&r.family.family_id)?
            .is_some_and(|id| id != g.study_id)
        {
            bail!("root family already belongs to another cumulative Study");
        }
        for receipt in store.campaign_family_receipts(&r.family.family_id)? {
            if let CampaignLedgerEventV1::RootRegistered {
                signed, approval, ..
            } = receipt.receipt.event
            {
                if signed.content_sha256 == root.content_sha256()
                    && (*signed != *root.signed_grant() || approval.approval_id != *approval_id)
                {
                    bail!("existing root registration differs from this authority or approval");
                }
            }
        }
    }
    if args.output.exists() {
        let old: RegistrationReport = read(&args.output)?;
        if old.study_id != g.study_id
            || old.study_grant_sha256 != study.content_sha256()
            || old.study_approval_id != args.study_approval_id
            || old.roots.len() != loaded.roots.len()
            || loaded.roots.iter().any(|(root, approval)| {
                old.roots
                    .get(&root.grant().family.family_id)
                    .is_none_or(|r| {
                        r.grant_sha256 != root.content_sha256() || r.approval_id != *approval
                    })
            })
        {
            bail!("registration output belongs to another immutable request");
        }
    }
    Ok(())
}
#[cfg(test)]
fn register_in_store(
    store: &mut AlphaStore,
    args: &StudyRegisterArgs,
    now: DateTime<Utc>,
) -> anyhow::Result<RegistrationReport> {
    register_with_clock(store, args, || now)
}
fn register_with_clock(
    store: &mut AlphaStore,
    args: &StudyRegisterArgs,
    clock: impl Fn() -> DateTime<Utc>,
) -> anyhow::Result<RegistrationReport> {
    let loaded = load(args, clock())?;
    preflight(store, args, &loaded, clock())?;
    let mut roots = BTreeMap::new();
    for (verified, approval_id) in loaded.roots {
        let receipt = store.register_campaign_root(&verified, &approval_id, clock())?;
        roots.insert(
            verified.grant().family.family_id.clone(),
            RegisteredRoot {
                grant_sha256: verified.content_sha256().into(),
                registration_receipt_sha256: receipt.content_sha256,
                approval_id,
            },
        );
    }
    let receipt = store.register_campaign_study(&loaded.study, &args.study_approval_id, clock())?;
    let report = RegistrationReport {
        schema_version: REGISTRATION_SCHEMA.into(),
        study_id: loaded.study.grant().study_id.clone(),
        study_grant_sha256: loaded.study.content_sha256().into(),
        study_approval_id: args.study_approval_id.clone(),
        registration_receipt_sha256: receipt.content_sha256,
        registration_sequence: receipt.receipt.sequence,
        roots,
        ledger_host: "ack".into(),
        trial_accounting_changed: false,
    };
    retain(&args.output, &report)?;
    Ok(report)
}
pub fn register(args: StudyRegisterArgs) -> anyhow::Result<()> {
    require_ack()?;
    existing_ledger(&args.ledger)?;
    let mut store = AlphaStore::open(&args.ledger)?;
    let report = register_with_clock(&mut store, &args, Utc::now)?;
    let usage = store.campaign_study_usage(&report.study_id)?;
    print_json(
        &json!({"status":"study_registered","registration":report,"current_usage":usage,"output":args.output}),
    )
}
fn inspection(store: &AlphaStore, id: &str) -> anyhow::Result<Value> {
    let signed = store
        .campaign_study_grant(id)?
        .context("Study is not registered")?;
    let receipts = store.campaign_study_receipts(id)?;
    let head = receipts
        .last()
        .context("registered Study lacks its receipt head")?;
    let mut receipt_objects = receipts
        .iter()
        .map(|r| Ok(json!({"key":r.object_key(),"sha256":r.object_sha256()?})))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut members = Vec::new();
    for member in &signed.grant.members {
        let receipts = store.campaign_family_receipts(&member.family_id)?;
        let head = receipts
            .last()
            .context("registered family lacks its receipt head")?;
        for receipt in &receipts {
            receipt_objects
                .push(json!({"key":receipt.object_key(),"sha256":receipt.object_sha256()?}));
        }
        members.push(json!({"family_id":member.family_id,"root_grant_sha256":member.root_grant_sha256,
            "head_sequence":head.receipt.sequence,"head_sha256":head.content_sha256,"usage":store.campaign_family_usage(&member.family_id)?}));
    }
    Ok(
        json!({"schema_version":"monday.campaign_study_inspection.v1","study_id":id,
        "study_grant_sha256":signed.content_sha256,"budget":signed.grant.budget,
        "head_sequence":head.receipt.sequence,"head_sha256":head.content_sha256,
        "usage":store.campaign_study_usage(id)?,"members":members,"receipt_objects":receipt_objects,"ledger_host":"ack",
        "historical_readback":true,"accounting_changed":false}),
    )
}
pub fn inspect(args: StudyInspectArgs) -> anyhow::Result<()> {
    require_ack()?;
    existing_ledger(&args.ledger)?;
    let store = AlphaStore::open_read_only(&args.ledger)?;
    let report = inspection(&store, &args.study_id)?;
    retain(&args.output, &report)?;
    print_json(&report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{campaign_control::*, campaign_study::*};
    use chrono::TimeDelta;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        root: tempfile::TempDir,
        args: StudyRegisterArgs,
        signed_root: SignedCampaignRootGrantV1,
        signed_study: SignedCampaignStudyGrantV1,
        now: DateTime<Utc>,
        key: SigningKey,
    }
    fn save<T: Serialize>(path: &Path, value: &T) {
        std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    fn approval(
        class: &str,
        id: &str,
        subject: &str,
        payload: Value,
        from: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> ApprovalRecord {
        ApprovalRecord {
            approval_id: id.into(),
            approval_class: class.into(),
            subject_id: subject.into(),
            payload,
            signer_id: Some("operator".into()),
            valid_from: Some(from),
            expires_at: Some(until),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: from,
        }
    }
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[47; 32]);
        let root_grant = CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: "operator-root".into(),
            family: CampaignFamilyPolicyV1 {
                family_id: "operator-family".into(),
                definition_sha256: "1".repeat(64),
                max_trials: 60,
            },
            execution_scope: CampaignExecutionScope::PreHoldout,
            execution: CampaignExecutionBindingV1 {
                campaign_inputs_sha256: "2".repeat(64),
                evaluation_protocol_sha256: "3".repeat(64),
                evaluation_views: CampaignEvaluationViewsV1 {
                    search_view_sha256: "a".repeat(64),
                    selection_view_sha256: "b".repeat(64),
                    selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
                },
                source_revision: "c".repeat(40),
                runner_image: format!("registry/runner@sha256:{}", "d".repeat(64)),
                controller_image: format!("registry/controller@sha256:{}", "e".repeat(64)),
                job_cpu_millis: 3000,
                job_memory_mib: 12288,
                accelerator: Default::default(),
            },
            allowed_policy_revision_ids: BTreeSet::from([format!(
                "cex-search-policy-{}",
                "f".repeat(64)
            )]),
            max_follow_ups: 0,
            budget: CampaignRootBudgetV1 {
                max_trials: 60,
                max_job_attempts: 4,
                max_job_seconds: 3600,
                max_llm_tokens: 0,
            },
            valid_from: now - TimeDelta::minutes(1),
            expires_at: now + TimeDelta::hours(2),
        };
        let signed_root =
            sign_campaign_root_grant(root_grant.clone(), "operator".into(), &key).unwrap();
        let study = CampaignStudyGrantV1 {
            schema_version: STUDY_GRANT_SCHEMA.into(),
            study_id: "original-operator-study".into(),
            members: vec![CampaignStudyMemberV1 {
                family_id: root_grant.family.family_id.clone(),
                root_grant_sha256: signed_root.content_sha256.clone(),
                family_definition_sha256: root_grant.family.definition_sha256.clone(),
                family_max_trials: root_grant.family.max_trials,
                execution_scope: root_grant.execution_scope.clone(),
                execution: root_grant.execution.clone(),
                label_horizon_sha256: "9".repeat(64),
            }],
            budget: CampaignStudyBudgetV1 {
                max_trials: 60,
                max_job_attempts: 4,
                max_job_seconds: 3600,
                max_llm_tokens: 0,
            },
            valid_from: root_grant.valid_from,
            expires_at: root_grant.expires_at,
        };
        let signed_study =
            sign_campaign_study_grant(study.clone(), "operator".into(), &key).unwrap();
        let args = StudyRegisterArgs {
            ledger: root.path().join("ledger.duckdb"),
            signed_study: root.path().join("study.json"),
            trusted_keys: root.path().join("trust.json"),
            study_approval_id: "study-approval".into(),
            roots: root.path().join("roots.json"),
            output: root.path().join("registration.json"),
        };
        save(&args.signed_study, &signed_study);
        save(&root.path().join("root.json"), &signed_root);
        save(&root.path().join("unsigned-root.json"), &root_grant);
        save(&root.path().join("unsigned-study.json"), &study);
        save(
            &args.trusted_keys,
            &json!({"operator":hex::encode(key.verifying_key().as_bytes())}),
        );
        save(
            &args.roots,
            &RootsManifest {
                schema_version: ROOTS_SCHEMA.into(),
                roots: vec![RootBinding {
                    signed_root_grant_path: "root.json".into(),
                    approval_id: "root-approval".into(),
                }],
            },
        );
        let key_path = root.path().join("issuer.key");
        std::fs::write(&key_path, key.as_bytes()).unwrap();
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut store = AlphaStore::open(&args.ledger).unwrap();
        store.record_approval(&approval("campaign_root","root-approval",&root_grant.root_id,
            json!({"grant_sha256":signed_root.content_sha256,"family_id":root_grant.family.family_id}),root_grant.valid_from,root_grant.expires_at)).unwrap();
        store
            .record_approval(&approval(
                "campaign_study",
                "study-approval",
                &study.study_id,
                json!({"grant_sha256":signed_study.content_sha256,"study_id":study.study_id}),
                study.valid_from,
                study.expires_at,
            ))
            .unwrap();
        drop(store);
        Fixture {
            root,
            args,
            signed_root,
            signed_study,
            now,
            key,
        }
    }
    fn root_sign_args(f: &Fixture) -> RootSignArgs {
        RootSignArgs {
            grant: f.root.path().join("unsigned-root.json"),
            key_id: "operator".into(),
            signing_key: f.root.path().join("issuer.key"),
            trusted_keys: f.args.trusted_keys.clone(),
            output: f.root.path().join("signed-root-output.json"),
        }
    }
    #[test]
    fn study_authority_signatures_are_native_pinned_and_create_once() {
        let f = fixture();
        let args = root_sign_args(&f);
        sign_root(args.clone()).unwrap();
        sign_root(args.clone()).unwrap();
        assert_eq!(
            read::<SignedCampaignRootGrantV1>(&args.output).unwrap(),
            f.signed_root
        );
        let study_args = StudySignArgs {
            grant: f.root.path().join("unsigned-study.json"),
            key_id: "operator".into(),
            signing_key: args.signing_key,
            trusted_keys: args.trusted_keys,
            output: f.root.path().join("signed-study-output.json"),
        };
        sign_study(study_args.clone()).unwrap();
        sign_study(study_args.clone()).unwrap();
        assert_eq!(
            read::<SignedCampaignStudyGrantV1>(&study_args.output).unwrap(),
            f.signed_study
        );
        let mut changed = f.signed_root.grant.clone();
        changed.budget.max_trials = 59;
        save(&args.grant, &changed);
        assert!(sign_root(root_sign_args(&f)).is_err());
        assert_eq!(
            read::<SignedCampaignRootGrantV1>(&args.output).unwrap(),
            f.signed_root
        );
    }
    #[test]
    fn study_authority_rejects_wrong_or_exposed_signing_key() {
        let f = fixture();
        let args = root_sign_args(&f);
        std::fs::write(&args.signing_key, [48; 32]).unwrap();
        assert!(sign_root(args.clone()).is_err());
        assert!(!args.output.exists());
        std::fs::write(&args.signing_key, f.key.as_bytes()).unwrap();
        std::fs::set_permissions(&args.signing_key, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(sign_root(args.clone()).is_err());
        std::fs::set_permissions(&args.signing_key, std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let alias = f.root.path().join("key-alias");
        std::os::unix::fs::symlink(&args.signing_key, &alias).unwrap();
        assert!(sign_root(RootSignArgs {
            signing_key: alias,
            ..args
        })
        .is_err());
    }
    #[test]
    fn study_authority_register_is_idempotent_and_preserves_historical_consumption() {
        let f = fixture();
        let mut store = AlphaStore::open(&f.args.ledger).unwrap();
        let trusted = trust(&f.args.trusted_keys).unwrap();
        let verified = verify_campaign_root_grant(&f.signed_root, &trusted, f.now).unwrap();
        store
            .register_campaign_root(&verified, "root-approval", f.now)
            .unwrap();
        let reservation = CampaignAttemptReservationV1 {
            schema_version: ATTEMPT_SCHEMA.into(),
            root_grant_sha256: f.signed_root.content_sha256.clone(),
            family_id: f.signed_root.grant.family.family_id.clone(),
            campaign_id: "cex-campaign-before-study".into(),
            generation: 0,
            parent_result_sha256: None,
            policy_revision_id: f
                .signed_root
                .grant
                .allowed_policy_revision_ids
                .first()
                .unwrap()
                .clone(),
            request_sha256: "8".repeat(64),
            attempt_ordinal: 0,
            declared_trials: 5,
            reserved_job_seconds: 10,
            reserved_llm_tokens: 0,
            execution: f.signed_root.grant.execution.clone(),
        };
        store
            .reserve_campaign_attempt(&verified, &reservation, f.now)
            .unwrap();
        store
            .settle_campaign_attempt(
                &reservation.family_id,
                &CampaignAttemptSettlementV1 {
                    operation_id: reservation.operation_id().unwrap(),
                    reservation_sha256: reservation.content_hash().unwrap(),
                    evidence_sha256: "7".repeat(64),
                    outcome: CampaignAttemptOutcomeV1::Failed,
                    consumed_trials: Some(5),
                },
                f.now,
            )
            .unwrap();
        let first = register_in_store(&mut store, &f.args, f.now).unwrap();
        let head = store
            .campaign_study_receipts(&f.signed_study.grant.study_id)
            .unwrap();
        let usage = store
            .campaign_study_usage(&f.signed_study.grant.study_id)
            .unwrap();
        assert_eq!(usage.consumed_trials, 5);
        let second = register_in_store(&mut store, &f.args, f.now + TimeDelta::seconds(1)).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            store
                .campaign_study_receipts(&f.signed_study.grant.study_id)
                .unwrap(),
            head
        );
        assert_eq!(
            store
                .campaign_study_usage(&f.signed_study.grant.study_id)
                .unwrap(),
            usage
        );
        let summary = inspection(&store, &f.signed_study.grant.study_id).unwrap();
        assert_eq!(summary["usage"]["consumed_trials"], 5);
        assert!(summary.get("receipts").is_none());
    }
    #[test]
    fn study_authority_rejects_wrong_approval_member_and_untrusted_grant_before_registration() {
        let f = fixture();
        let mut store = AlphaStore::open(&f.args.ledger).unwrap();
        let mut args = f.args.clone();
        args.study_approval_id = "root-approval".into();
        assert!(register_in_store(&mut store, &args, f.now).is_err());
        assert!(store
            .campaign_family_receipts(&f.signed_root.grant.family.family_id)
            .unwrap()
            .is_empty());
        let mut changed = f.signed_study.grant.clone();
        changed.members[0].label_horizon_sha256 = "6".repeat(64);
        changed.members[0].root_grant_sha256 = "5".repeat(64);
        save(
            &f.args.signed_study,
            &sign_campaign_study_grant(changed, "operator".into(), &f.key).unwrap(),
        );
        assert!(register_in_store(&mut store, &f.args, f.now).is_err());
        let wrong_key = SigningKey::from_bytes(&[49; 32]);
        save(
            &f.args.signed_study,
            &sign_campaign_study_grant(f.signed_study.grant.clone(), "operator".into(), &wrong_key)
                .unwrap(),
        );
        assert!(register_in_store(&mut store, &f.args, f.now).is_err());
        assert!(store
            .campaign_family_receipts(&f.signed_root.grant.family.family_id)
            .unwrap()
            .is_empty());
        assert!(!f.args.output.exists());
    }
    #[test]
    fn study_authority_reregistration_cannot_replace_budget_or_restore_revoked_approval() {
        let f = fixture();
        let mut store = AlphaStore::open(&f.args.ledger).unwrap();
        register_in_store(&mut store, &f.args, f.now).unwrap();
        let old = store
            .campaign_study_receipts(&f.signed_study.grant.study_id)
            .unwrap();
        let mut grant = f.signed_study.grant.clone();
        grant.budget.max_trials = 59;
        let altered = sign_campaign_study_grant(grant, "operator".into(), &f.key).unwrap();
        save(&f.args.signed_study, &altered);
        store
            .record_approval(&approval(
                "campaign_study",
                "alternate-approval",
                &altered.grant.study_id,
                json!({"grant_sha256":altered.content_sha256,"study_id":altered.grant.study_id}),
                altered.grant.valid_from,
                altered.grant.expires_at,
            ))
            .unwrap();
        let mut args = f.args.clone();
        args.study_approval_id = "alternate-approval".into();
        args.output = f.root.path().join("new-registration.json");
        assert!(register_in_store(&mut store, &args, f.now).is_err());
        assert_eq!(
            store
                .campaign_study_receipts(&f.signed_study.grant.study_id)
                .unwrap(),
            old
        );
        save(&f.args.signed_study, &f.signed_study);
        store
            .revoke_approval("study-approval", "operator", "revoked for test", f.now)
            .unwrap();
        assert!(register_in_store(&mut store, &f.args, f.now + TimeDelta::seconds(1)).is_err());
        assert!(inspection(&store, &f.signed_study.grant.study_id).is_ok());
    }
    #[test]
    fn study_authority_rechecks_expiry_after_metadata_loading() {
        let f = fixture();
        let mut store = AlphaStore::open(&f.args.ledger).unwrap();
        let calls = std::cell::Cell::new(0);
        let result = register_with_clock(&mut store, &f.args, || {
            let call = calls.get();
            calls.set(call + 1);
            if call < 2 {
                f.now
            } else {
                f.signed_root.grant.expires_at + TimeDelta::seconds(1)
            }
        });
        assert!(result.is_err());
        assert!(store
            .campaign_family_receipts(&f.signed_root.grant.family.family_id)
            .unwrap()
            .is_empty());
        assert!(!f.args.output.exists());
    }
}
