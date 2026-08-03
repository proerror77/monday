use crate::{
    cli::{print_json, BaselineEvidenceArgs},
    data_mission,
    governance::selected_walk_forward_evidence,
};
use alpha_domain::{canonical_json_hash, MissionStatus, TrainingValidationEvidenceV1};
use alpha_store::AlphaStore;
use anyhow::{bail, Context};

pub fn produce(args: BaselineEvidenceArgs) -> anyhow::Result<()> {
    let store = AlphaStore::open(&args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    if lineage.mission.status != MissionStatus::Completed {
        bail!(
            "baseline evidence requires a completed legacy mission; observed {:?}",
            lineage.mission.status
        );
    }
    let Some((candidate, evaluation)) = selected_walk_forward_evidence(&store, &args.mission_id)?
    else {
        bail!("baseline evidence requires exactly one passing walk-forward candidate");
    };
    let candidate_hash = canonical_json_hash(&candidate.artifact)?;
    if candidate_hash != candidate.content_hash {
        bail!("stored candidate content hash does not match its artifact");
    }
    let evaluation_record_hash = canonical_json_hash(&evaluation.record)?;
    if evaluation_record_hash != evaluation.content_hash {
        bail!("stored evaluation content hash does not match its record");
    }
    let dataset_revision = store
        .get_registry_revision(lineage.mission.dataset_manifest_id.as_str())
        .context("legacy mission dataset manifest is not registered")?;
    if dataset_revision.registry_kind != "dataset" {
        bail!("legacy mission dataset identity is not a dataset registry revision");
    }
    let evaluation_payload = serde_json::from_value(evaluation.record.payload.clone())
        .context("selected walk-forward evaluation is not a CandidateEvaluation")?;
    let source_mission_sha256 = canonical_json_hash(&lineage.mission)?;
    let dataset_manifest_sha256 = canonical_json_hash(&dataset_revision.payload)?;
    let lineage_hash = canonical_json_hash(&lineage)?;
    let evidence = TrainingValidationEvidenceV1::new(
        lineage.mission.mission_id.clone(),
        source_mission_sha256,
        format!("search-lineage-{lineage_hash}"),
        lineage.mission.dataset_manifest_id.as_str().to_string(),
        dataset_manifest_sha256,
        candidate.candidate_id,
        evaluation.record.evaluation_id,
        evaluation_record_hash,
        candidate.artifact,
        evaluation_payload,
    )?;
    evidence.validate()?;
    data_mission::write_json_create_once(&args.output, &evidence)?;
    print_json(&serde_json::json!({
        "schema_version": evidence.schema_version,
        "evidence_id": evidence.evidence_id,
        "artifact_sha256": evidence.artifact_sha256,
        "evidence_reference": evidence.evidence_reference(),
        "source_mission_id": evidence.source_mission_id,
        "source_mission_sha256": evidence.source_mission_sha256,
        "source_search_lineage_id": evidence.source_search_lineage_id,
        "dataset_manifest_id": evidence.dataset_manifest_id,
        "dataset_manifest_sha256": evidence.dataset_manifest_sha256,
        "output": args.output,
    }))
}
