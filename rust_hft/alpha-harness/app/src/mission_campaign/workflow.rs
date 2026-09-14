//! One declarative coordinator over existing preparation and Campaign stages.
//! The controller/ledger own execution truth; this module stores references to
//! their evidence, never replacement charges or invented training outcomes.
use super::{
    preparation::{self, FileRef, PreparationIndex},
    *,
};
use crate::cli::{CampaignPrepareArgs, CampaignWorkflowArgs};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::{
    collections::BTreeSet,
    process::{Command, Stdio},
};

const SCHEMA: &str = "monday.cex_campaign_workflow.v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    id: String,
    control: FileRef,
    signer: FileRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: String,
    preparation_plans: Vec<FileRef>,
    /// Absolute and retained with the plan. Recovery cannot renew it.
    deadline_at: DateTime<Utc>,
    context: String,
    namespace: String,
    members: Vec<Member>,
}

fn controller_path(base: &Path) -> PathBuf {
    #[cfg(test)]
    {
        base.join("controller.sh")
    }
    #[cfg(not(test))]
    {
        let _ = base;
        PathBuf::from("/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh")
    }
}

fn cycle_status(
    controller: &Path,
    cycle: &Path,
    output: &Path,
    index: &PreparationIndex,
    campaign_id: &str,
) -> anyhow::Result<serde_json::Value> {
    let process = Command::new(controller)
        .args(["status", "--work-dir"])
        .arg(cycle)
        .stdout(Stdio::from(File::create(output)?))
        .status()?;
    if !process.success() {
        bail!("retained Campaign status failed validation");
    }
    let status: serde_json::Value = serde_json::from_slice(&read(output)?)?;
    if status["generation"] != 0
        || status["source_revision"] != index.source_revision
        || status["image"] != index.image
        || status["campaign_inputs_sha256"] != index.campaign_inputs.sha256
        || status["campaign_id"]
            .as_str()
            .is_some_and(|id| id != campaign_id)
    {
        bail!("retained Campaign status belongs to another prepared member");
    }
    if matches!(
        status["checkpoint_status"].as_str(),
        Some("complete" | "terminal_failure")
    ) && status["campaign_id"] != campaign_id
    {
        bail!("terminal Campaign identity is missing");
    }
    Ok(status)
}

fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        bail!("workflow metadata exceeds its limit");
    }
    Ok(bytes)
}

fn retain(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    data_mission::ensure_output_path_is_not_symlink(path, "workflow evidence")?;
    if path.exists() {
        if read(path)? != bytes {
            bail!("workflow evidence differs; use its original plan and state");
        }
    } else {
        let mut temporary = data_mission::temporary_output_file(path, ".workflow-")?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(path).map_err(|e| e.error)?;
    }
    Ok(())
}

fn result_reference(path: &Path) -> anyhow::Result<serde_json::Value> {
    let value = read(path)?;
    Ok(json!({"path":path,"sha256":hex::encode(Sha256::digest(value))}))
}

pub fn run(args: CampaignWorkflowArgs) -> anyhow::Result<()> {
    #[cfg(not(test))]
    if std::env::consts::OS != "linux"
        || std::env::var("MONDAY_EXECUTION_HOST").as_deref() != Ok("ack")
    {
        bail!("Campaign workflow execution and readback belong in ACK");
    }
    let path = std::fs::canonicalize(&args.plan)?;
    let base = path.parent().context("workflow plan parent")?;
    let bytes = read(&path)?;
    let plan: Plan = serde_json::from_slice(&bytes)?;
    let mut ids = BTreeSet::new();
    if plan.schema_version != SCHEMA
        || plan.members.is_empty()
        || plan.members.len() > 32
        || plan.preparation_plans.is_empty()
        || plan.preparation_plans.len() > 32
    {
        bail!("invalid workflow schema or bounded member count");
    }
    crate::prediction_dispatch::validate_cluster_target(&plan.context, &plan.namespace)?;
    for member in &plan.members {
        validate_dns_label("workflow member", &member.id)?;
        if !ids.insert(&member.id) {
            bail!("duplicate workflow member");
        }
    }
    data_mission::ensure_real_directory(&args.work_dir, "workflow state")?;
    let root = std::fs::canonicalize(&args.work_dir)?;
    let lock_path = root.join(".workflow.lock");
    data_mission::ensure_output_path_is_not_symlink(&lock_path, "workflow lock")?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock()
        .context("this workflow has an active coordinator")?;
    if Utc::now() >= plan.deadline_at && !root.join("workflow-plan.json").exists() {
        bail!("workflow deadline expired before preparation or dispatch");
    }
    retain(&root.join("workflow-plan.json"), &bytes)?;
    let controller = controller_path(base);
    if !controller.is_file() || controller.is_symlink() {
        bail!("the bundled canonical Campaign controller is unavailable");
    }
    let mut total_trials = 0usize;
    let mut comparison_bound = 0usize;
    let mut prepared_ids = BTreeSet::new();
    for reference in &plan.preparation_plans {
        let (trials, bound, members) = preparation::workflow_plan_bound(reference, base)?;
        total_trials = total_trials
            .checked_add(trials)
            .context("workflow comparison budget overflow")?;
        comparison_bound = comparison_bound.max(bound);
        for member in members {
            if !prepared_ids.insert(member) {
                bail!("duplicate workflow member across input groups");
            }
        }
    }
    if prepared_ids.iter().collect::<BTreeSet<_>>() != ids {
        bail!("workflow members differ from its input groups");
    }
    comparison_bound = comparison_bound.max(total_trials);
    let mut groups = Vec::new();
    for reference in &plan.preparation_plans {
        let prepared = preparation::prepare_report(
            CampaignPrepareArgs {
                ledger: args.ledger.clone(),
                plan: base.join(&reference.path),
                output_root: root.join("preparations"),
            },
            Some(&reference.sha256),
            preparation::PreparationPurpose::ExecuteWorkflow {
                comparison_trials: comparison_bound,
            },
        )?;
        let index_ref = FileRef {
            path: PathBuf::from(
                prepared["preparation"]
                    .as_str()
                    .context("preparation path")?,
            ),
            sha256: prepared["sha256"]
                .as_str()
                .context("preparation hash")?
                .into(),
        };
        let index: PreparationIndex = serde_json::from_slice(&preparation::verified_bytes(
            &index_ref,
            &root,
            16 * 1024 * 1024,
        )?)?;
        groups.push((
            index,
            index_ref
                .path
                .parent()
                .context("preparation parent")?
                .to_owned(),
        ));
    }
    let group_refs = groups
        .iter()
        .map(|(index, path)| (index, path.as_path()))
        .collect::<Vec<_>>();
    let result = execute(
        &plan,
        base,
        &root,
        &lock,
        &group_refs,
        &hex::encode(Sha256::digest(bytes)),
    )?;
    print_json(&result)?;
    if result["state"] != "complete" {
        bail!(
            "Campaign workflow requires attention; retained status: {}",
            root.join("workflow-status.json").display()
        );
    }
    Ok(())
}

fn execute(
    plan: &Plan,
    base: &Path,
    root: &Path,
    lock: &File,
    groups: &[(&PreparationIndex, &Path)],
    plan_sha: &str,
) -> anyhow::Result<serde_json::Value> {
    match execute_inner(plan, base, root, lock, groups, plan_sha) {
        Ok(report) => Ok(report),
        Err(error) => {
            data_mission::write_json_atomic(
                &root.join("workflow-status.json"),
                &json!({
                    "schema_version":"monday.cex_campaign_workflow_report.v1", "plan_sha256":plan_sha,
                    "state":"needs_attention", "failure":error.to_string(),
                    "deadline_at":plan.deadline_at, "declared_members":plan.members.len(),
                    "accounting_source":"authenticated_campaign_ledger", "native_cycle_state":root.join("cycles")
                }),
            )?;
            Err(error)
        }
    }
}

fn execute_inner(
    plan: &Plan,
    base: &Path,
    root: &Path,
    lock: &File,
    groups: &[(&PreparationIndex, &Path)],
    plan_sha: &str,
) -> anyhow::Result<serde_json::Value> {
    let controller = controller_path(base);
    let mut ids = BTreeSet::new();
    for (index, _) in groups {
        for member in &index.members {
            if !ids.insert(member.id.as_str()) {
                bail!("duplicate prepared member across input groups");
            }
        }
    }
    if ids != plan.members.iter().map(|m| m.id.as_str()).collect() {
        bail!("prepared input groups do not match workflow members");
    }
    let mut outcomes = Vec::new();
    let mut state = "complete";
    for member in &plan.members {
        let (index, index_root) = groups
            .iter()
            .find(|(index, _)| index.members.iter().any(|m| m.id == member.id))
            .context("prepared workflow input group missing")?;
        let prepared = index
            .members
            .iter()
            .find(|m| m.id == member.id)
            .context("prepared member")?;
        let cycle = root.join("cycles").join(&member.id);
        data_mission::ensure_real_directory(&cycle, "workflow cycle")?;
        let output = cycle.join("workflow-controller-output.json");
        data_mission::ensure_output_path_is_not_symlink(&output, "workflow controller output")?;
        let mut complete = false;
        let mut recover_summary = false;
        if cycle.join("controller-inputs.json").exists() {
            let status = cycle_status(&controller, &cycle, &output, index, &prepared.campaign_id)?;
            match status["checkpoint_status"].as_str() {
                Some("complete") => {
                    complete = cycle.join("cycle-result.json").exists();
                    recover_summary = !complete;
                }
                Some("terminal_failure") => {
                    outcomes.push(json!({"id":member.id,"state":"failed","evidence":result_reference(&cycle.join("generation-0/terminal-failure"))?}));
                    state = "failed";
                    break;
                }
                Some("incomplete") | Some("needs_approval") => (),
                _ => bail!("unsupported retained Campaign status"),
            }
        }
        if !complete {
            preparation::verified_bytes(&member.control, base, MAX_REQUEST_BYTES)?;
            let mut command = Command::new(&controller);
            if recover_summary || Utc::now() >= plan.deadline_at {
                if !cycle.join("generation-0/dispatched").exists() {
                    outcomes.push(
                        json!({"id":member.id,"state":"deadline_exhausted","dispatched":false}),
                    );
                    state = "deadline_exhausted";
                    break;
                }
                // Historical readback is allowed after expiry. This mode cannot
                // sign or dispatch, and the Job retains its native deadline.
                command.args(["ack-readback", "--discover-campaign-pod"]);
                command
                    .env_remove("MONDAY_CAMPAIGN_DEADLINE_AT")
                    .env_remove("MONDAY_CAMPAIGN_DEADLINE_GUARDED");
            } else {
                preparation::verified_bytes(&member.signer, base, MAX_REQUEST_BYTES)?;
                command
                    .args(["start", "--run-to-terminal", "--max-follow-ups", "0"])
                    .arg("--campaign-inputs")
                    .arg(index_root.join(&index.campaign_inputs.path))
                    .arg("--input-root")
                    .arg(&index.input_root)
                    .arg("--source-revision")
                    .arg(&index.source_revision)
                    .arg("--image")
                    .arg(&index.image)
                    .arg("--campaign-root")
                    .arg(&index.campaign_root)
                    .arg("--initial-research-plan")
                    .arg(index_root.join(&prepared.research_plan.path))
                    .arg("--prepared-freeze")
                    .arg(index_root.join(&prepared.freeze.path))
                    .arg("--prepared-freeze-sha256")
                    .arg(&prepared.freeze.sha256)
                    .arg("--preparation-ledger")
                    .arg(&index.ledger)
                    .arg("--signer")
                    .arg(base.join(&member.signer.path))
                    .arg("--context")
                    .arg(&plan.context)
                    .arg("--namespace")
                    .arg(&plan.namespace)
                    .env("MONDAY_CAMPAIGN_DEADLINE_AT", plan.deadline_at.to_rfc3339())
                    .env_remove("MONDAY_CAMPAIGN_DEADLINE_GUARDED");
                for seed in &index.seeds {
                    command.arg("--seed").arg(seed);
                }
            }
            command
                .arg("--work-dir")
                .arg(&cycle)
                .arg("--control")
                .arg(base.join(&member.control.path))
                .arg("--alpha-harness")
                .arg(std::env::current_exe()?);
            // Inherit the locked open file description into the controller so
            // parent interruption cannot admit a second live coordinator.
            let status = command
                .stdin(Stdio::from(lock.try_clone()?))
                .stdout(Stdio::from(File::create(&output)?))
                .stderr(Stdio::inherit())
                .status()?;
            if !status.success() {
                let failure = cycle.join("generation-0/terminal-failure");
                state = if failure.exists() {
                    let checked =
                        cycle_status(&controller, &cycle, &output, index, &prepared.campaign_id)?;
                    if checked["checkpoint_status"] != "terminal_failure" {
                        bail!("terminal failure is not validated by the controller");
                    }
                    "failed"
                } else {
                    "needs_attention"
                };
                outcomes.push(
                    json!({"id":member.id,"state":state,"exit_code":status.code(),
                    "evidence":if failure.exists() {Some(result_reference(&failure)?)} else {None},
                    "cycle_state":cycle}),
                );
                break;
            }
        }
        let result = cycle.join("cycle-result.json");
        let checked = cycle_status(&controller, &cycle, &output, index, &prepared.campaign_id)?;
        if checked["checkpoint_status"] != "complete" {
            bail!("workflow member lacks validated terminal Campaign evidence");
        }
        outcomes.push(json!({"id":member.id,"state":"complete","reused":complete,"evidence":result_reference(&result)?}));
    }
    let report = json!({"schema_version":"monday.cex_campaign_workflow_report.v1","plan_sha256":plan_sha,
        "state":state,"deadline_at":plan.deadline_at,"members":outcomes,"declared_members":plan.members.len(),
        "automatic_follow_ups":false,"accounting_source":"authenticated_campaign_ledger","report_reuses_native_metrics":true});
    data_mission::write_json_atomic(&root.join("workflow-status.json"), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: tempfile::TempDir,
        plan: Plan,
        index: PreparationIndex,
        lock: File,
    }

    impl Fixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("controller.sh");
            std::fs::write(&script, r#"#!/usr/bin/env bash
set -euo pipefail
mode="$1"; shift
base="$(dirname "$0")"
arguments="$*"
while (($#)); do
  if [[ "$1" == --work-dir ]]; then dir="$2"; shift 2; else shift; fi
done
id="${dir##*/}"
if [[ "$mode" == status ]]; then
  if [[ -e "$dir/generation-0/terminal-failure" ]]; then
    state=terminal_failure
  elif [[ -e "$dir/generation-0/generation-complete" ]]; then
    state=complete
  else state=incomplete; fi
  header="$base/status-header.json"
  if [[ -f "$base/status-header-$id.json" ]]; then header="$base/status-header-$id.json"; fi
  jq -c --arg state "$state" --arg id "$id" '. + {checkpoint_status:$state,generation:0,campaign_id:$id}' "$header"
  exit 0
fi
printf '%s %s\n' "$mode" "$arguments" >>"$base/calls-$id"
mkdir -p "$dir/generation-0"
printf '{}' >"$dir/controller-inputs.json"
if [[ "$mode" == start ]]; then
  [[ "$arguments" == *"--max-follow-ups 0"* && "$arguments" == *"--run-to-terminal"* ]]
  touch "$dir/generation-0/dispatched"
else
  [[ "$mode" == ack-readback && "$arguments" == *"--discover-campaign-pod"* && -e "$dir/generation-0/dispatched" ]]
  [[ -z "${MONDAY_CAMPAIGN_DEADLINE_AT:-}" ]]
fi
if [[ -e "$base/fail-$id" ]]; then
  printf '{"reason":"job_failed"}' >"$dir/generation-0/terminal-failure"
  exit 70
fi
if [[ -e "$base/interrupt-$id" ]]; then rm "$base/interrupt-$id"; exit 75; fi
printf '{"termination_reason":"campaign_no_candidate"}' >"$dir/cycle-result.json"
touch "$dir/generation-0/generation-complete"
"#).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let control = root.path().join("control.json");
            std::fs::write(&control, b"{}").unwrap();
            let reference = |path: &Path| FileRef {
                path: path.into(),
                sha256: hex::encode(Sha256::digest(std::fs::read(path).unwrap())),
            };
            let ids = ["first", "second"];
            let plan = Plan {
                schema_version: SCHEMA.into(),
                preparation_plans: vec![reference(&control)],
                deadline_at: Utc::now() + chrono::TimeDelta::minutes(5),
                context: "context".into(),
                namespace: "monday-research".into(),
                members: ids
                    .iter()
                    .map(|id| Member {
                        id: (*id).into(),
                        control: reference(&control),
                        signer: reference(&script),
                    })
                    .collect(),
            };
            let index = PreparationIndex {
                ledger: control.clone(),
                schema_version: "monday.cex_campaign_preparation.v1".into(),
                plan_sha256: "a".repeat(64),
                source_revision: BUILD_SOURCE_REVISION.into(),
                image: "image".into(),
                campaign_root: "https://example.invalid".into(),
                campaign_inputs: reference(&control),
                input_root: root.path().join("absent-data"),
                prepared_inputs: reference(&control),
                seeds: vec!["7".into(), u64::MAX.to_string()],
                members: ids
                    .iter()
                    .map(|id| preparation::PreparedMember {
                        id: (*id).into(),
                        research_plan: reference(&control),
                        freeze: reference(&control),
                        campaign_id: (*id).into(),
                        declared_trials: 1,
                    })
                    .collect(),
            };
            let lock = File::create(root.path().join("lock")).unwrap();
            std::fs::write(root.path().join("status-header.json"), serde_json::to_vec(&json!({"source_revision":&index.source_revision,"image":&index.image,"campaign_inputs_sha256":&index.campaign_inputs.sha256})).unwrap()).unwrap();
            lock.try_lock().unwrap();
            Self {
                root,
                plan,
                index,
                lock,
            }
        }
        fn run(&self) -> serde_json::Value {
            execute(
                &self.plan,
                self.root.path(),
                self.root.path(),
                &self.lock,
                &[(&self.index, self.root.path())],
                &"b".repeat(64),
            )
            .unwrap()
        }
        fn calls(&self, id: &str) -> usize {
            std::fs::read_to_string(self.root.path().join(format!("calls-{id}")))
                .unwrap_or_default()
                .lines()
                .count()
        }
    }

    #[test]
    fn workflow_runs_distinct_input_groups_without_restarting_completed_members() {
        let fixture = Fixture::new();
        let mut first: PreparationIndex =
            serde_json::from_value(serde_json::to_value(&fixture.index).unwrap()).unwrap();
        let mut second: PreparationIndex =
            serde_json::from_value(serde_json::to_value(&fixture.index).unwrap()).unwrap();
        first.members.truncate(1);
        second.members.remove(0);
        second.campaign_inputs.sha256 = "f".repeat(64);
        std::fs::write(fixture.root.path().join("status-header-second.json"),serde_json::to_vec(&json!({
            "source_revision":&second.source_revision,"image":&second.image,"campaign_inputs_sha256":&second.campaign_inputs.sha256
        })).unwrap()).unwrap();
        let groups = [
            (&first, fixture.root.path()),
            (&second, fixture.root.path()),
        ];
        let result = execute(
            &fixture.plan,
            fixture.root.path(),
            fixture.root.path(),
            &fixture.lock,
            &groups,
            &"b".repeat(64),
        )
        .unwrap();
        assert_eq!(result["state"], "complete");
        let again = execute(
            &fixture.plan,
            fixture.root.path(),
            fixture.root.path(),
            &fixture.lock,
            &groups,
            &"b".repeat(64),
        )
        .unwrap();
        assert!(again["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["reused"] == true));
        for id in ["first", "second"] {
            assert_eq!(
                std::fs::read_to_string(fixture.root.path().join(format!("calls-{id}")))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
        let duplicate = [
            (&first, fixture.root.path()),
            (&fixture.index, fixture.root.path()),
        ];
        assert!(execute(
            &fixture.plan,
            fixture.root.path(),
            fixture.root.path(),
            &fixture.lock,
            &duplicate,
            &"b".repeat(64)
        )
        .is_err());
    }

    #[test]
    fn completed_generation_rebuilds_its_missing_summary_without_dispatching_again() {
        let fixture = Fixture::new();
        assert_eq!(fixture.run()["state"], "complete");
        std::fs::remove_file(fixture.root.path().join("cycles/first/cycle-result.json")).unwrap();
        assert_eq!(fixture.run()["state"], "complete");
        let calls = std::fs::read_to_string(fixture.root.path().join("calls-first")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.starts_with("start "))
                .count(),
            1
        );
        assert!(calls.lines().last().unwrap().starts_with("ack-readback "));
    }

    #[test]
    fn a_completed_checkpoint_for_another_source_is_not_adopted() {
        let fixture = Fixture::new();
        assert_eq!(fixture.run()["state"], "complete");
        std::fs::write(
            fixture.root.path().join("status-header.json"),
            b"{\"source_revision\":\"foreign\"}",
        )
        .unwrap();
        assert!(execute(
            &fixture.plan,
            fixture.root.path(),
            fixture.root.path(),
            &fixture.lock,
            &[(&fixture.index, fixture.root.path())],
            &"b".repeat(64)
        )
        .is_err());
        assert_eq!((fixture.calls("first"), fixture.calls("second")), (1, 1));
        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.root.path().join("workflow-status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["state"], "needs_attention");
    }

    #[test]
    fn workflow_resumes_only_incomplete_members_and_preserves_native_seed_strings() {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.path().join("interrupt-second"), b"").unwrap();
        assert_eq!(fixture.run()["state"], "needs_attention");
        assert_eq!((fixture.calls("first"), fixture.calls("second")), (1, 1));
        assert_eq!(fixture.run()["state"], "complete");
        assert_eq!((fixture.calls("first"), fixture.calls("second")), (1, 2));
        assert_eq!(fixture.run()["state"], "complete");
        assert_eq!((fixture.calls("first"), fixture.calls("second")), (1, 2));
        let calls = std::fs::read_to_string(fixture.root.path().join("calls-first")).unwrap();
        assert!(calls.contains("18446744073709551615"));
    }

    #[test]
    fn terminal_failure_halts_the_matrix_and_does_not_restart_a_failed_member() {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.path().join("fail-first"), b"").unwrap();
        assert_eq!(fixture.run()["state"], "failed");
        assert_eq!(fixture.run()["state"], "failed");
        assert_eq!((fixture.calls("first"), fixture.calls("second")), (1, 0));
    }

    #[test]
    fn expired_workflow_can_only_read_back_an_already_dispatched_job() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.root.path().join("interrupt-first"), b"").unwrap();
        assert_eq!(fixture.run()["state"], "needs_attention");
        fixture.plan.deadline_at = Utc::now() - chrono::TimeDelta::seconds(1);
        let report = fixture.run();
        assert_eq!(report["state"], "deadline_exhausted");
        assert_eq!(report["members"][0]["state"], "complete");
        let calls = std::fs::read_to_string(fixture.root.path().join("calls-first")).unwrap();
        assert!(calls.lines().last().unwrap().starts_with("ack-readback "));
        assert_eq!(fixture.calls("second"), 0);
    }

    #[test]
    fn retained_plan_conflicts_and_symlinked_output_fail_before_execution() {
        let fixture = Fixture::new();
        let path = fixture.root.path().join("workflow-plan.json");
        retain(&path, b"old").unwrap();
        assert!(retain(&path, b"new").is_err());
        let cycle = fixture.root.path().join("cycles/first");
        std::fs::create_dir_all(&cycle).unwrap();
        std::os::unix::fs::symlink(&path, cycle.join("workflow-controller-output.json")).unwrap();
        assert!(execute(
            &fixture.plan,
            fixture.root.path(),
            fixture.root.path(),
            &fixture.lock,
            &[(&fixture.index, fixture.root.path())],
            &"b".repeat(64)
        )
        .is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"old");
        assert_eq!(fixture.calls("first"), 0);
    }
}
