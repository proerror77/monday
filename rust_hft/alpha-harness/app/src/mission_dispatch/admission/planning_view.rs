//! The original signed search view and its verified normalized projection.
//! This module cannot qualify original high-frequency raw books or create grants.
use super::*;
use crate::mission_campaign::{prepared_inputs, CampaignRequest};
use alpha_domain::representation::{PlanningViewV1, PlanningVisibilityV1};
use alpha_store::campaign_ledger::VerifiedCampaignPlanningPermission;

/// Process-local authority. Only the guarded original source path constructs it.
pub(crate) struct VerifiedPlanningView<'a> {
    permission: VerifiedCampaignPlanningPermission<'a>,
    trusted_keys: &'a Path,
    view: PlanningViewV1,
    prepared: prepared_inputs::VerifiedNativeCampaignPreparedInputs,
}

impl VerifiedPlanningView<'_> {
    pub(crate) fn view(&self) -> &PlanningViewV1 {
        &self.view
    }
    pub(crate) fn prepared(&self) -> &prepared_inputs::VerifiedNativeCampaignPreparedInputs {
        &self.prepared
    }
    pub(crate) fn recheck(&self) -> anyhow::Result<()> {
        check_current_authority(&self.permission, self.trusted_keys)
    }

    fn inventory(&self) -> anyhow::Result<Value> {
        self.recheck()?;
        let input = self.prepared().prepared();
        Ok(json!({
            "planning_view": self.view(),
            "data_stage": "verified_original_normalized_search_projection",
            "collection_sha256": input.id(),
            "development_rows_sha256": input.development_rows_sha256(),
            "source_receipt_sha256": input.source_receipt_sha256(),
            "producer": input.source_build(),
            "feature_names": input.manifest().features.manifest.spec.feature_names,
            "rows": input.rows().len(),
            "raw_planning_available": false,
            "status": "no_executable_comparison",
            "materializations": [], "arms": [], "hypothesis": null,
            "requested_resources": null,
            "limitation": "The original normalized projection does not qualify high-frequency raw books or regenerate OFI. Existing columns require their own bound native Campaign.",
        }))
    }
}

pub(crate) fn inspect_inventory(control: &Path, request: &Path) -> anyhow::Result<Value> {
    with_authorized_prepared_view(control, request, |scope| scope.inventory())
}

/// Internal composition point for reusing already materialized columns. The
/// callback never receives a constructor for raw-book or signed authority.
pub(crate) fn with_authorized_prepared_view<T>(
    control_path: &Path,
    request_path: &Path,
    action: impl FnOnce(&VerifiedPlanningView<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let control = read_control(control_path)?;
    let signed: SignedCampaignRootGrantV1 = read_json(&control.signed_root_grant_path)?;
    let root = verify(&signed, &control.trusted_keys_path)?;
    let store = AlphaStore::open_read_only(&control.ledger_path)?;
    let permission = store.inspect_campaign_planning_permission(&root)?;
    permission.recheck()?;
    let request: CampaignRequest = read_json(request_path)?;
    crate::mission_campaign::validate_request(&request)?;
    let receipt_path = control
        .campaign_inputs_path
        .as_ref()
        .context("planning requires the original immutable DataReady receipt")?;
    let receipt = read_bounded(receipt_path, MAX_CONTROL_BYTES)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let directory = tempfile::tempdir().context("planning normalized source readback")?;
    let scope = authorize_projection(
        permission,
        &control.trusted_keys_path,
        &request,
        &receipt,
        |sha, guard| {
            prepared_inputs::acquire_planning_prepared(
                &request,
                sha,
                &client,
                directory.path(),
                guard,
            )
        },
    )?;
    scope.recheck()?;
    let result = action(&scope)?;
    scope.recheck()?;
    Ok(result)
}

fn authorize_projection<'a>(
    permission: VerifiedCampaignPlanningPermission<'a>,
    trusted_keys: &'a Path,
    request: &CampaignRequest,
    receipt: &[u8],
    acquire: impl FnOnce(
        &str,
        &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<prepared_inputs::VerifiedNativeCampaignPreparedInputs>,
) -> anyhow::Result<VerifiedPlanningView<'a>> {
    let root = permission.root();
    let guard = || check_current_authority(&permission, trusted_keys);
    guard()?;
    let metadata = prepared_inputs::inspect_planning_ready_metadata(
        receipt,
        &root.grant().execution.campaign_inputs_sha256,
        request,
    )?;
    let view = project_signed_search_view(root, request, metadata.materialization())?;
    if let Some(member) = permission.study_member()? {
        let (protocol, _) =
            super::evaluation_views_for_materialization(request, metadata.materialization())?;
        let horizon = match request.research_plan.label_horizon.as_ref() {
            Some(horizon) => horizon.clone(),
            None => alpha_domain::campaign_horizon::CampaignLabelHorizonV1::new(
                protocol.labels.horizon_buckets,
                protocol.labels.observation_frequency_millis,
                protocol.walk_forward.purge_rows,
                protocol.walk_forward.embargo_rows,
            )
            .map_err(anyhow::Error::msg)?,
        };
        if member.label_horizon_sha256 != horizon.content_hash().map_err(anyhow::Error::msg)? {
            bail!("planning target horizon differs from the original signed Study member");
        }
    }
    let request_sha256 =
        hft_cex_research_input::sha256(&crate::mission_campaign::serialize_request(request)?);
    // This local request hash checks source/request consistency under signed
    // DataReady/view scope. It is not a reservation, native witness, or Run admission.
    let prepared = acquire(&request_sha256, &guard)?;
    guard()?;
    if prepared.campaign_inputs_sha256() != root.grant().execution.campaign_inputs_sha256
        || prepared.source_revision() != root.grant().execution.source_revision
        || prepared.evaluation_protocol_sha256()
            != root.grant().execution.evaluation_protocol_sha256
        || prepared.request_sha256() != request_sha256
        || prepared.finalized_request() != request
    {
        bail!("verified planning projection differs from the signed original source scope");
    }
    prepared_inputs::validate_planning_projection_metadata(
        prepared.prepared().manifest(),
        request,
    )?;
    Ok(VerifiedPlanningView {
        permission,
        trusted_keys,
        view,
        prepared,
    })
}

#[cfg(all(test, feature = "scientific"))]
mod tests {
    use super::*;
    use alpha_domain::campaign_control::{
        sign_campaign_root_grant, CampaignExecutionScope, CampaignFamilyPolicyV1,
        CampaignRootBudgetV1, CampaignRootGrantV1, ROOT_GRANT_SCHEMA,
    };
    use chrono::TimeDelta;
    use ed25519_dalek::SigningKey;
    use std::{cell::Cell, collections::BTreeSet};

    fn authority(
        fixture: &crate::mission_campaign::tests::NativePreparedFixture,
        wrong_view: bool,
    ) -> (
        AlphaStore,
        VerifiedCampaignRootGrant,
        tempfile::TempDir,
        PathBuf,
    ) {
        let request = &fixture.request;
        let materialization = fixture.inputs.render_inputs().materialization();
        let (protocol, mut views) =
            super::super::evaluation_views_for_materialization(request, materialization).unwrap();
        if wrong_view {
            std::mem::swap(
                &mut views.search_view_sha256,
                &mut views.selection_view_sha256,
            );
        }
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[71; 32]);
        let signed = sign_campaign_root_grant(
            CampaignRootGrantV1 {
                schema_version: ROOT_GRANT_SCHEMA.into(),
                root_id: "planning-root".into(),
                family: CampaignFamilyPolicyV1 {
                    family_id: "planning-family".into(),
                    definition_sha256: "a".repeat(64),
                    max_trials: 1000,
                },
                execution_scope: CampaignExecutionScope::PreHoldout,
                execution: CampaignExecutionBindingV1 {
                    campaign_inputs_sha256: request.campaign_inputs_sha256.clone(),
                    evaluation_protocol_sha256: protocol.content_hash().unwrap(),
                    evaluation_views: views,
                    source_revision: request.build_source_revision.clone(),
                    runner_image: format!("registry/research@sha256:{}", request.image_identity),
                    controller_image: format!("registry/controller@sha256:{}", "b".repeat(64)),
                    job_cpu_millis: 2000,
                    job_memory_mib: 4096,
                    accelerator: alpha_domain::research_accelerator::ResearchAcceleratorV1::Cpu,
                },
                allowed_policy_revision_ids: BTreeSet::from([request
                    .research_plan
                    .search_policy_revision
                    .revision_id
                    .clone()]),
                max_follow_ups: 1,
                budget: CampaignRootBudgetV1 {
                    max_trials: 1000,
                    max_job_attempts: 10,
                    max_job_seconds: 100000,
                    max_llm_tokens: 0,
                },
                valid_from: now,
                expires_at: now + TimeDelta::hours(1),
            },
            "planning-root-key".into(),
            &key,
        )
        .unwrap();
        let root = verify_campaign_root_grant(
            &signed,
            &BTreeMap::from([("planning-root-key".into(), key.verifying_key())]),
            now,
        )
        .unwrap();
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.record_approval(&alpha_store::ApprovalRecord {
            approval_id: "planning-approval".into(), approval_class: "campaign_root".into(),
            subject_id: root.grant().root_id.clone(),
            payload: json!({"grant_sha256":root.content_sha256(),"family_id":root.grant().family.family_id}),
            signer_id: Some(signed.key_id.clone()), valid_from: Some(root.grant().valid_from),
            expires_at: Some(root.grant().expires_at), revoked_at: None, revoked_by: None,
            revocation_reason: None, created_at: now,
        }).unwrap();
        store
            .register_campaign_root(&root, "planning-approval", now)
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let keys = directory.path().join("current-trust.json");
        std::fs::write(
            &keys,
            serde_json::to_vec(&BTreeMap::from([(
                signed.key_id,
                hex::encode(key.verifying_key().as_bytes()),
            )]))
            .unwrap(),
        )
        .unwrap();
        (store, root, directory, keys)
    }

    #[test]
    fn signed_scope_reuses_original_validation_projection_without_raw_or_run_authority() {
        let fixture = crate::mission_campaign::tests::native_prepared_fixture_for_tests();
        let (store, root, _directory, keys) = authority(&fixture, false);
        let before = store
            .campaign_family_snapshot(&root.grant().family.family_id)
            .unwrap();
        let receipt = std::fs::read(fixture.augmented_receipt_path()).unwrap();
        assert_eq!(
            fixture
                .inputs
                .prepared()
                .manifest()
                .features
                .manifest
                .spec
                .split,
            hft_cex_research_input::data::Split::Validation
        );
        let request = fixture.request.clone();
        let scope = authorize_projection(
            store.inspect_campaign_planning_permission(&root).unwrap(),
            &keys,
            &request,
            &receipt,
            |expected, guard| {
                guard()?;
                assert_eq!(fixture.inputs.request_sha256(), expected);
                Ok(fixture.inputs)
            },
        )
        .unwrap();
        let report = scope.inventory().unwrap();
        assert_eq!(
            report["data_stage"],
            "verified_original_normalized_search_projection"
        );
        assert_eq!(report["raw_planning_available"], false);
        assert_eq!(report["status"], "no_executable_comparison");
        assert_eq!(report["arms"], json!([]));
        assert_eq!(scope.view().visibility, PlanningVisibilityV1::Development);
        assert_eq!(
            store
                .campaign_family_snapshot(&root.grant().family.family_id)
                .unwrap(),
            before
        );
        std::fs::write(&keys, b"{}").unwrap();
        assert!(scope.inventory().is_err());
    }

    #[test]
    fn wrong_signed_partition_or_damaged_ready_is_rejected_before_projection_fetch() {
        for wrong_view in [true, false] {
            let fixture = crate::mission_campaign::tests::native_prepared_fixture_for_tests();
            let (store, root, _directory, keys) = authority(&fixture, wrong_view);
            let before = store
                .campaign_family_snapshot(&root.grant().family.family_id)
                .unwrap();
            let mut receipt = std::fs::read(fixture.augmented_receipt_path()).unwrap();
            if !wrong_view {
                receipt.push(b' ');
            }
            let calls = Cell::new(0);
            let request = fixture.request.clone();
            assert!(authorize_projection(
                store.inspect_campaign_planning_permission(&root).unwrap(),
                &keys,
                &request,
                &receipt,
                |_, _| {
                    calls.set(calls.get() + 1);
                    Ok(fixture.inputs)
                }
            )
            .is_err());
            assert_eq!(calls.get(), 0);
            assert_eq!(
                store
                    .campaign_family_snapshot(&root.grant().family.family_id)
                    .unwrap(),
                before
            );
        }
    }

    #[test]
    fn distinct_study_key_removal_denies_planning_without_a_reservation() {
        use alpha_domain::campaign_study::{
            sign_campaign_study_grant, verify_campaign_study_grant, CampaignStudyBudgetV1,
            CampaignStudyGrantV1, CampaignStudyMemberV1, STUDY_GRANT_SCHEMA,
        };
        let fixture = crate::mission_campaign::tests::native_prepared_fixture_for_tests();
        let (mut store, root, _directory, keys) = authority(&fixture, false);
        let key = SigningKey::from_bytes(&[72; 32]);
        let signed = sign_campaign_study_grant(
            CampaignStudyGrantV1 {
                schema_version: STUDY_GRANT_SCHEMA.into(),
                study_id: "planning-study".into(),
                members: vec![CampaignStudyMemberV1 {
                    family_id: root.grant().family.family_id.clone(),
                    root_grant_sha256: root.content_sha256().into(),
                    family_definition_sha256: root.grant().family.definition_sha256.clone(),
                    family_max_trials: root.grant().family.max_trials,
                    execution_scope: root.grant().execution_scope.clone(),
                    execution: root.grant().execution.clone(),
                    label_horizon_sha256: "c".repeat(64),
                }],
                budget: CampaignStudyBudgetV1 {
                    max_trials: 1000,
                    max_job_attempts: 10,
                    max_job_seconds: 100000,
                    max_llm_tokens: 0,
                },
                valid_from: root.grant().valid_from,
                expires_at: root.grant().expires_at,
            },
            "planning-study-key".into(),
            &key,
        )
        .unwrap();
        let study = verify_campaign_study_grant(
            &signed,
            &BTreeMap::from([(signed.key_id.clone(), key.verifying_key())]),
            Utc::now(),
        )
        .unwrap();
        store.record_approval(&alpha_store::ApprovalRecord {
            approval_id: "planning-study-approval".into(), approval_class: "campaign_study".into(),
            subject_id: study.grant().study_id.clone(),
            payload: json!({"grant_sha256":study.content_sha256(),"study_id":study.grant().study_id}),
            signer_id: Some(signed.key_id.clone()), valid_from: Some(study.grant().valid_from),
            expires_at: Some(study.grant().expires_at), revoked_at: None, revoked_by: None,
            revocation_reason: None, created_at: study.grant().valid_from,
        }).unwrap();
        store
            .register_campaign_study(&study, "planning-study-approval", Utc::now())
            .unwrap();
        let root_keys = read_trusted_keys(&keys).unwrap();
        let mut both = root_keys
            .iter()
            .map(|(id, value)| (id.clone(), hex::encode(value.as_bytes())))
            .collect::<BTreeMap<_, _>>();
        both.insert(signed.key_id, hex::encode(key.verifying_key().as_bytes()));
        std::fs::write(&keys, serde_json::to_vec(&both).unwrap()).unwrap();
        let before = store
            .campaign_study_snapshot(&study.grant().study_id)
            .unwrap();
        let permission = store.inspect_campaign_planning_permission(&root).unwrap();
        check_current_authority(&permission, &keys).unwrap();
        both.remove("planning-study-key");
        std::fs::write(&keys, serde_json::to_vec(&both).unwrap()).unwrap();
        assert!(check_current_authority(&permission, &keys).is_err());
        assert_eq!(
            store
                .campaign_study_snapshot(&study.grant().study_id)
                .unwrap(),
            before
        );
    }
}

fn check_current_authority(
    permission: &VerifiedCampaignPlanningPermission<'_>,
    trusted_keys: &Path,
) -> anyhow::Result<()> {
    let root = permission.root();
    let current = read_trusted_keys(trusted_keys)?;
    if current.get(&root.signed_grant().key_id) != Some(root.verifying_key()) {
        bail!("planning signer is no longer in the current operator trust set");
    }
    permission.recheck()?;
    if let Some((id, key)) = permission.study_signer()? {
        if current.get(&id) != Some(&key) {
            bail!("planning Study signer is no longer in the current operator trust set");
        }
    }
    Ok(())
}

fn project_signed_search_view(
    root: &VerifiedCampaignRootGrant,
    request: &CampaignRequest,
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<PlanningViewV1> {
    let execution = &root.grant().execution;
    if request.campaign_inputs_sha256 != execution.campaign_inputs_sha256
        || request.build_source_revision != execution.source_revision
        || request.image_identity != image_digest(&execution.runner_image)?
    {
        bail!("planning request differs from the original signed data/source/image");
    }
    validate_render_materialization_scope_for_horizon(
        materialization,
        request.research_plan.label_horizon.as_ref(),
        request
            .research_plan
            .development_precheck
            .as_ref()
            .and_then(|receipt| receipt.protocol.calendar.as_ref()),
    )?;
    validate_materialization(
        materialization,
        &request.feature_sha256,
        &approved_validation(materialization)?,
    )?;
    let (protocol, views) = super::evaluation_views_for_materialization(request, materialization)?;
    if protocol.content_hash()? != execution.evaluation_protocol_sha256
        || views != execution.evaluation_views
        || views.search_view_sha256 == views.selection_view_sha256
    {
        bail!("actual planning search partition differs from signed authority or shares withheld data");
    }
    if !root
        .grant()
        .allowed_policy_revision_ids
        .contains(&request.research_plan.search_policy_revision.revision_id)
    {
        bail!("planning policy revision is outside the original signed allowlist");
    }
    Ok(PlanningViewV1 {
        view: alpha_domain::CexResearchContentRefV1 {
            id: "campaign-search-and-learning-v2".into(),
            content_sha256: views.search_view_sha256,
        },
        family_id: root.grant().family.family_id.clone(),
        visibility: PlanningVisibilityV1::Development,
        permission: alpha_domain::CexResearchContentRefV1 {
            id: root.grant().root_id.clone(),
            content_sha256: root.content_sha256().into(),
        },
    })
}
