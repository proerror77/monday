use std::fs;
use std::path::Path;

fn workflow_contents(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn workflow_dispatch_input_count(content: &str) -> usize {
    let mut in_inputs = false;
    let mut base_indent = 0usize;
    let mut count = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if trimmed == "inputs:" {
            in_inputs = true;
            base_indent = indent;
            continue;
        }
        if in_inputs {
            if indent <= base_indent {
                break;
            }
            if indent == base_indent + 2 && trimmed.ends_with(':') {
                count += 1;
            }
        }
    }

    count
}

#[test]
fn ci_runs_dependency_vulnerability_audit() {
    let content = workflow_contents(".github/workflows/test.yml");
    let mut offenders = Vec::new();

    if !content.contains("name: Workflow lint") {
        offenders.push("test.yml: missing workflow lint job".to_string());
    }

    if !content.contains("actionlint/cmd/actionlint@v1.7.7") {
        offenders.push("test.yml: missing pinned actionlint installation".to_string());
    }

    if !content.contains("taiki-e/install-action@cargo-audit") {
        offenders.push("test.yml: missing cargo-audit installer step".to_string());
    }

    if !content.contains("cargo audit --ignore RUSTSEC-2023-0071") {
        offenders.push(
            "test.yml: missing cargo audit execution with the documented sqlx/rsa exception"
                .to_string(),
        );
    }

    for (needle, description) in [
        ("cargo fmt --all -- --check", "rustfmt gate"),
        (
            "npm run contracts:check --prefix ploy-frontend",
            "frontend contracts",
        ),
        ("npm run lint --prefix ploy-frontend", "frontend lint"),
        ("npm run build --prefix ploy-frontend", "frontend build"),
        (
            "npm audit --omit=dev --audit-level=moderate --prefix ploy-frontend",
            "frontend audit",
        ),
        ("-size +500k", "frontend chunk limit"),
        (
            "cargo build --locked \\\n            -p ploy-agent-sidecar",
            "Rust sidecar build",
        ),
        (
            "cargo test --locked \\\n            -p ploy-agent-sidecar",
            "Rust sidecar tests",
        ),
        (
            "cargo clippy --locked -p ploy-agent-sidecar --all-targets --no-deps -- -D warnings",
            "Rust sidecar clippy",
        ),
        (
            "node ploy-frontend/scripts/check-route-contract.mjs",
            "retired route scan",
        ),
        (
            "StrictHostKeyChecking[[:space:]=]+no|UserKnownHostsFile[[:space:]=]+/dev/null|allow[_-]?running",
            "insecure SSH scan",
        ),
    ] {
        if !content.contains(needle) {
            offenders.push(format!("test.yml: missing {description}"));
        }
    }

    for (needle, description) in [
        (
            "command -v rg >/dev/null 2>&1",
            "explicit rg availability guard",
        ),
        ("scan_status=$?", "rg exit-status capture"),
        ("case \"${scan_status}\" in", "rg exit-status dispatch"),
        ("forbidden ${label} match found", "rg match failure branch"),
        ("1)\n                return 0", "rg clean branch"),
        (
            "scanner error (rg exit ${scan_status})",
            "rg scanner-error branch",
        ),
        ("exit \"${scan_status}\"", "rg scanner-error propagation"),
        ("retired frontend route", "retired-route rg scan"),
    ] {
        if !content.contains(needle) {
            offenders.push(format!("test.yml: missing fail-closed {description}"));
        }
    }
    if content.contains("if rg -n") {
        offenders
            .push("test.yml: rg scans must not treat every nonzero status as clean".to_string());
    }

    for retired_ignore in [
        "RUSTSEC-2026-0049",
        "RUSTSEC-2026-0098",
        "RUSTSEC-2026-0099",
        "RUSTSEC-2026-0104",
    ] {
        if content.contains(retired_ignore) {
            offenders.push(format!(
                "test.yml: stale production-path ignore {retired_ignore}"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow dependency-audit guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn workflow_dispatch_inputs_stay_lintable() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow_dir = repo_root.join(".github/workflows");
    let mut offenders = Vec::new();

    for entry in fs::read_dir(&workflow_dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", workflow_dir.display()))
    {
        let path = entry.expect("workflow dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yml") {
            continue;
        }
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if !content.contains("workflow_dispatch:") {
            continue;
        }
        let input_count = workflow_dispatch_input_count(&content);
        if input_count > 10 {
            offenders.push(format!(
                "{}: workflow_dispatch has {input_count} inputs; move advanced knobs to options_json",
                path.strip_prefix(repo_root).unwrap_or(&path).display()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow_dispatch input-count guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_snapshot_preserves_empty_timestamp_args_over_ssh() {
    let workflow = workflow_contents(".github/workflows/research-snapshot.yml");
    let mut offenders = Vec::new();

    for needle in [
        "empty_arg=\"__ploy_empty__\"",
        "\"${SNAPSHOT_START_TS:-${empty_arg}}\"",
        "\"${SNAPSHOT_END_TS:-${empty_arg}}\"",
        "if [ \"${start_ts}\" = \"__ploy_empty__\" ]; then",
        "if [ \"${end_ts}\" = \"__ploy_empty__\" ]; then",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("research-snapshot.yml: missing `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "research snapshot SSH empty timestamp guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_snapshot_uses_sampled_snapshot_canonical_names_with_legacy_alias() {
    let workflow = workflow_contents(".github/workflows/research-snapshot.yml");
    let mut offenders = Vec::new();

    for needle in [
        "\"upload_sampled_snapshot\":true",
        "\"upload_\" + \"full_snapshot\": \"upload_sampled_snapshot\"",
        "options_json cannot include both {legacy_key} and {canonical_key}",
        "handle.write(f\"upload_sampled_snapshot={values['upload_sampled_snapshot']}\\n\")",
        "echo \"sampled_snapshot_embedded=${SNAPSHOT_UPLOAD_SAMPLED_SNAPSHOT}\"",
        "steps.snapshot_options.outputs.upload_sampled_snapshot == 'true'",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("research-snapshot.yml: missing `{needle}`"));
        }
    }

    for forbidden in [
        "full_snapshot_embedded=${SNAPSHOT_UPLOAD_",
        "steps.snapshot_options.outputs.upload_full_snapshot == 'true'",
    ] {
        if workflow.contains(forbidden) {
            offenders.push(format!(
                "research-snapshot.yml: still contains forbidden `{forbidden}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "research snapshot sampled naming guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn factor_evolve_daily_search_passes_snapshot_quote_age() {
    let workflow = workflow_contents(".github/workflows/factor-evolve-daily-research.yml");
    let mut offenders = Vec::new();

    for needle in [
        "\"evidence_stage\": \"factor_attribution\"",
        "\"trace_provenance\"",
        "\"candidate_replay_ref\": \"\"",
        "\"trace_manifest_uri\": \"\"",
        "max_quote_age_secs:",
        "MAX_QUOTE_AGE_SECS: ${{ github.event.inputs.max_quote_age_secs }}",
        "\"max_quote_age_secs\": max_quote_age_secs",
        "\"persist_research_trace\": True",
        "- Trace persistence: `required-for-search-mode`",
        "-f options_json=\"$(cat artifacts/factor-evolve-daily/hosted-options.json)\"",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!(
                "missing daily workflow quote-age handoff: {needle}"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "FactorEvolve daily search workflow guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn event_ml_rolling_evidence_has_hosted_artifact_lane() {
    let workflow = workflow_contents(".github/workflows/event-ml-rolling-evidence.yml");
    let runbook = workflow_contents("docs/runbooks/event-ml-automl-workflow.md");
    let mut offenders = Vec::new();

    for needle in [
        "source_dataset_run_id:",
        "actions: read",
        "Generate event ML rolling evidence from artifact on GitHub-hosted runner",
        "runs-on: ubuntu-latest",
        "github.event.inputs.source_dataset_run_id != ''",
        "scripts/download_github_artifact.py",
        "event-ml-rolling-datasets-${SOURCE_DATASET_RUN_ID}",
        "--runtime-score",
        "--replay-parity-ready",
        "create_handoff_issue",
        "create_config_pr",
        "model_artifact_path",
        "Reject missing event-root dataset artifact",
        "artifact-backed only",
        "direct DB export is no longer available",
        "Create config PR from ready Event ML handoff",
        "issues: write",
        "pull-requests: write",
        "Event ML handoff status is ${status}; no dry-run issue will be created.",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("event-ml-rolling-evidence.yml: missing `{needle}`"));
        }
    }

    for forbidden in [
        "Generate event ML rolling evidence from DB on ploy-ci-1",
        "runs-on: [self-hosted, ploy-ci-1]",
        "postgresql://postgres:postgres",
        "--db-url",
        "factor_research",
        "Skip config PR on legacy DB branch",
    ] {
        if workflow.contains(forbidden) {
            offenders.push(format!(
                "event-ml-rolling-evidence.yml: legacy DB path still contains `{forbidden}`"
            ));
        }
    }

    for needle in [
        "Prefer the hosted artifact path",
        "source_dataset_artifact_name",
        "runtime_score",
        "replay_parity_ready",
        "create_handoff_issue",
        "create_config_pr",
        "model_artifact_path",
        "workflow stays within GitHub's 10-input dispatch limit",
        "artifact-backed only",
        "source_dataset_run_id",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!("event-ml-automl-workflow.md: missing `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "event ML hosted artifact lane guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn ack_images_use_immutable_sha_tags() {
    let build = workflow_contents(".github/workflows/build-push-acr.yml");
    let deploy = workflow_contents(".github/workflows/deploy-ack.yml");
    let mut offenders = Vec::new();

    if !build.contains("default: false") {
        offenders.push("build-push-acr.yml: image pushes should default to false".to_string());
    }
    if build.contains(":latest") {
        offenders.push("build-push-acr.yml: must not build or push latest tags".to_string());
    }
    if !build.contains("ACR image pushes must use git_ref=main") {
        offenders.push("build-push-acr.yml: missing main-only push provenance gate".to_string());
    }
    if deploy.contains("default: \"latest\"") || deploy.contains("SHA or 'latest'") {
        offenders.push("deploy-ack.yml: must not accept latest as a deploy default".to_string());
    }
    if !deploy.contains("environment: ack") {
        offenders.push("deploy-ack.yml: missing protected ack environment".to_string());
    }
    if !deploy.contains("^[0-9a-f]{40}$") {
        offenders.push("deploy-ack.yml: missing full SHA image-tag validation".to_string());
    }

    assert!(
        offenders.is_empty(),
        "ACK immutable image guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_workflows_do_not_transfer_runtime_binaries_between_jobs() {
    let optimize = workflow_contents(".github/workflows/optimize.yml");
    let backtest = workflow_contents(".github/workflows/backtest.yml");
    let mut offenders = Vec::new();

    for forbidden in [
        "Upload optimize runner",
        "Download optimize runner",
        "optimize-runner-${{ github.sha }}",
    ] {
        if optimize.contains(forbidden) {
            offenders.push(format!(
                "optimize.yml: fragile binary artifact pattern still contains `{forbidden}`"
            ));
        }
    }
    for forbidden in [
        "Upload run_backtest binary",
        "Download run_backtest binary",
        "run_backtest-${{ github.sha }}",
    ] {
        if backtest.contains(forbidden) {
            offenders.push(format!(
                "backtest.yml: fragile binary artifact pattern still contains `{forbidden}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "research workflow binary handoff guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn replay_dryrun_parity_reports_strict_readiness() {
    let script = workflow_contents("scripts/replay_dryrun_parity.py");
    let workflow = workflow_contents(".github/workflows/replay-dryrun-parity.yml");
    let mut offenders = Vec::new();

    if !script.contains("strict_parity_ready") {
        offenders
            .push("replay_dryrun_parity.py: missing strict parity readiness field".to_string());
    }
    if !script.contains("missing_strict_parity_fields") {
        offenders.push("replay_dryrun_parity.py: missing strict parity caveat".to_string());
    }
    if !workflow.contains("Replay/dry-run parity evidence") {
        offenders.push("replay-dryrun-parity.yml: missing issue evidence comment".to_string());
    }

    assert!(
        offenders.is_empty(),
        "replay/dry-run parity guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn recorded_replay_parity_supports_auto_window() {
    let workflow = workflow_contents(".github/workflows/recorded-replay-parity.yml");
    let runbook = workflow_contents("docs/runbooks/strategy-research-cicd.md");
    let mut offenders = Vec::new();

    for needle in [
        "approval_environment:",
        "default: \"tango-1-1-build-only\"",
        "runner_source:",
        "Runner source: workflow_ref for exact-SHA promotion, deployed for diagnostics",
        "default: \"workflow_ref\"",
        "- \"deployed\"",
        "- \"workflow_ref\"",
        "Build replay runner from workflow ref",
        "if: ${{ github.event.inputs.runner_source == 'workflow_ref' }}",
        "--features new-ploy-runner/full",
        "-p new-ploy-runner",
        "target/${PLATFORM_TARGET}/release/new-ploy-runner",
        "tango-1-1:\"${REMOTE_DIR}/ploy-runner\"",
        "RUNNER_SOURCE",
        "runner_source must be deployed or workflow_ref",
        "runner_path=\"/opt/ploy/bin/ploy-runner\"",
        "runner_path=\"${remote_dir}/ploy-runner\"",
        "timeout 600 \"${runner_path}\" run",
        "Runner source",
        "skip_settlement_exits:",
        "Skip settlement exits for entry-only dry-run parity",
        "SKIP_SETTLEMENT_EXITS",
        "skip_settlement_exits must be true or false",
        "environment: ${{ inputs.approval_environment }}",
        "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
        "pm5d-threelayer-settlement-probability-btc-eth.ndjson",
        "TANGO_SSH_KEY",
        "ALIYUN_ECS_SSH_KEY",
        "No SSH key secret found for tango-1-1",
        "ssh-keygen -y -f ~/.ssh/tango_1_1_key",
        "default: \"auto\"",
        "resolve_recorded_replay_window.py",
        "--recording \"${recording_path}\"",
        "--dryrun-json \"${dryrun_report}\"",
        "StrictHostKeyChecking yes",
        "UserKnownHostsFile ~/.ssh/known_hosts",
        "TANGO_1_1_KNOWN_HOSTS",
        "resolved-window.json",
        "resolved-window.env",
        "lines.append(f\"skip_settlement_exits = {skip_settlement_exits}\")",
        "extract_official_settlement_evidence.py",
        "official-settlement-token-ids.json",
        "official-settlement-token-ids.tsv",
        "official-settlement-db-rows.json",
        "official-settlement-report.json",
        "CREATE TEMP TABLE replay_token_ids",
        "\\copy replay_token_ids(token_id)",
        "\\o ${official_db_rows_json}",
        "FROM pm_token_settlements s",
        "JOIN replay_token_ids t ON t.token_id = s.token_id",
        "--db-settlements-json \"${official_db_rows_json}\"",
        "RESOLVED_SINCE",
        "RESOLVED_UNTIL",
        "Requested window",
        "Resolved window",
        "Auto-window closed rows",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("recorded-replay-parity.yml: missing `{needle}`"));
        }
    }
    for needle in [
        "`runner_source=workflow_ref` is\nthe default",
        "`runner_source=deployed` only as a diagnostic",
        "does not\ndeploy artifacts, restart services, replace `/opt/ploy/bin/ploy-runner`, or\nenable live orders",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!(
                "strategy-research-cicd.md: missing recorded replay note `{needle}`"
            ));
        }
    }
    if workflow.contains("extract_dryrun_settlement_evidence.py") {
        offenders.push(
            "recorded-replay-parity.yml: dry-run settlement extraction must not feed official replay enrichment"
                .to_string(),
        );
    }
    if workflow.contains("FROM strategy_runtime_event_track_record") {
        offenders.push(
            "recorded-replay-parity.yml: official replay enrichment must not source settlement labels from track-record rows"
                .to_string(),
        );
    }
    if workflow.contains("-v token_ids_json=")
        || workflow.contains("jsonb_array_elements_text(:'token_ids_json'")
    {
        offenders.push(
            "recorded-replay-parity.yml: token ids must not be passed as one large psql argv value"
                .to_string(),
        );
    }

    for needle in [
        "defaults `since=auto` and `until=auto`",
        "records the resolved window",
        "timestamps remain supported",
        "`approval_environment` input therefore defaults to",
        "`tango-1-1-build-only`",
        "`TANGO_SSH_KEY` / `ALIYUN_ECS_SSH_KEY`",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!("strategy-research-cicd.md: missing `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "recorded replay parity auto-window guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn hosted_factor_walk_forward_uploads_alpha_chain_summary() {
    let workflow =
        workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "Summarize alpha search chain evidence",
        "scripts/summarize_alpha_search_chain.py",
        "artifacts/factor-walk-forward-v2-upload",
        "alpha-search-chain/summary.json",
        "alpha-search-chain/summary.md",
        "Upload report artifact",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    let summary_index = workflow
        .find("Summarize alpha search chain evidence")
        .unwrap_or(usize::MAX);
    let upload_index = workflow.find("Upload report artifact").unwrap_or(0);
    if summary_index > upload_index {
        offenders.push(
            "factor-walk-forward-v2-hosted-artifact.yml: summary must be generated before upload"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "hosted alpha chain summary artifact guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn factor_walk_forward_wires_alpha_prior_and_state_inputs() {
    let hosted = workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "alpha_search_llm_prior_json",
        "alpha_search_state_json",
        "require_deribit",
        "pm_book_sample_secs",
        "train_window_hours",
        "test_window_hours",
        "step_hours",
        "mcts-state.json",
        "--alpha-search-state-json",
        "--alpha-search-llm-prior-json",
        "--require-deribit",
        "--pm-book-sample-secs",
    ] {
        if !hosted.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "alpha-search prior/state workflow wiring guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn hosted_factor_walk_forward_splits_replay_parity_artifact_suffix() {
    let workflow =
        workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "replay_parity_run_id",
        "replay_parity_artifact_name",
        "split(\":\", 1)",
        "replay_parity_run_id must be <run-id>:<artifact-name>",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "hosted factor walk-forward replay parity artifact split guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn hosted_factor_walk_forward_has_candidate_replay_feedback_input() {
    let hosted = workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "candidate_strategy_replay_json",
        "candidate_strategy_replay_run_id",
        "candidate_strategy_replay_artifact_name",
        "full_depth_execution_surface_json",
        "full_depth_execution_surface_run_id",
        "full_depth_execution_surface_artifact_name",
        "candidate_strategy_replay_run_id must be <run-id>:<artifact-name>",
        "full_depth_execution_surface_run_id must be <run-id>:<artifact-name>",
        "Download candidate strategy replay artifact",
        "Download full-depth execution surface artifact",
        "runtime-candidate-replay-${WALK_CANDIDATE_STRATEGY_REPLAY_RUN_ID}",
        "full-depth-execution-surface-${WALK_FULL_DEPTH_EXECUTION_SURFACE_RUN_ID}",
        "--require candidate-strategy-replay.json",
        "--require full-depth-execution-surface.json",
        "artifacts/candidate-strategy-replay/candidate-strategy-replay.json",
        "artifacts/full-depth-execution-surface/full-depth-execution-surface.json",
        "--candidate-strategy-replay-json",
        "--full-depth-execution-surface-json",
        "--candidate-replay-json",
        "candidate-strategy-replay/candidate-strategy-replay.json",
    ] {
        if !hosted.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    let candidate_section = hosted
        .split("- name: Download candidate strategy replay artifact")
        .nth(1)
        .and_then(|tail| {
            tail.split("- name: Download full-depth execution surface artifact")
                .next()
        })
        .unwrap_or("");
    if candidate_section.contains("--strip-prefix") {
        offenders.push(
            "factor-walk-forward-v2-hosted-artifact.yml: candidate replay artifact must not use alpha-search strip-prefix"
                .to_string(),
        );
    }
    if !hosted.contains(
        "--strip-prefix \"factor-walk-forward-v2/alpha-search/${WALK_ALPHA_SEARCH_PLAN_TARGET}\"",
    ) || !hosted.contains("--require mcts-expansion-plan.json")
    {
        offenders.push(
            "factor-walk-forward-v2-hosted-artifact.yml: alpha search plan artifact contract was weakened"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "hosted factor walk-forward candidate replay feedback guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn factor_research_workflows_thread_pm_book_sample_cadence() {
    let hosted_walk =
        workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let hosted_review = workflow_contents(".github/workflows/factor-review-v2-hosted-artifact.yml");
    let sweep = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/run_factor_walk_forward_sweep.py"),
    )
    .expect("read sweep runner");
    let mut offenders = Vec::new();

    for (name, content) in [
        (
            "factor-walk-forward-v2-hosted-artifact.yml",
            hosted_walk.as_str(),
        ),
        (
            "factor-review-v2-hosted-artifact.yml",
            hosted_review.as_str(),
        ),
    ] {
        let needles: &[&str] = &["pm_book_sample_secs", "--pm-book-sample-secs"];
        for needle in needles {
            if !content.contains(needle) {
                offenders.push(format!("{name}: missing `{needle}`"));
            }
        }
    }

    for needle in [
        "\"WALK_PM_BOOK_SAMPLE_SECS\": \"pm_book_sample_secs\"",
        "manifest_key == \"pm_book_sample_secs\"",
        "manifest.get(\"lob_sample_secs\")",
    ] {
        if !hosted_walk.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    for needle in [
        "\"FACTOR_PM_BOOK_SAMPLE_SECS\": \"pm_book_sample_secs\"",
        "manifest_key == \"pm_book_sample_secs\"",
        "manifest.get(\"lob_sample_secs\")",
    ] {
        if !hosted_review.contains(needle) {
            offenders.push(format!(
                "factor-review-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    if !sweep.contains("\"pm_book_sample_secs\"") || !sweep.contains("--pm-book-sample-secs") {
        offenders.push(
            "run_factor_walk_forward_sweep.py: missing pm_book_sample_secs pass-through"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "PM book sample cadence workflow wiring guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn hosted_factor_walk_forward_dispatches_runtime_replay_requests() {
    let hosted = workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "dispatch_runtime_replay_requests",
        "Dispatch runtime replay requests",
        "runtime_replay_requests",
        "runtime-candidate-replay.yml",
        "Runtime replay request dispatch requires successful durable Research OS trace persistence",
        "decision.get(\"action\") != \"fix_runtime\"",
        "gh",
        "workflow",
        "run",
        "--ref",
        "GITHUB_REF_NAME",
        "deployment_id",
        "runtime_score",
        "options_json",
    ] {
        if !hosted.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "hosted runtime replay request dispatch guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn runtime_candidate_replay_allows_empty_entry_score_override() {
    let workflow = workflow_contents(".github/workflows/runtime-candidate-replay.yml");
    assert!(
        workflow.contains("min_entry_score_override=\"${7:-}\""),
        "runtime-candidate-replay.yml must tolerate an omitted three_layer_min_entry_score override"
    );
    for needle in [
        "--deployment-id",
        "--workflow-run-id",
        "--workflow-run-url",
        "--artifact-name",
        "--recording-path",
        "--config-path",
        "--runner-source",
        "--runner-git-sha",
        "--source-target",
        "--source-horizon",
    ] {
        assert!(
            workflow.contains(needle),
            "runtime-candidate-replay.yml must pass replay provenance arg `{needle}`"
        );
    }
}

#[test]
fn tango_deploy_removes_live_authority() {
    let workflow = workflow_contents(".github/workflows/deploy-tango-1-1.yml");
    let cloud_assist = workflow_contents("scripts/ci/deploy_tango_cloud_assist.py");
    let mut offenders = Vec::new();

    for needle in [
        "environment: ${{ inputs.deploy && 'tango-1-1' || 'tango-1-1-build-only' }}",
        "Verify research bundle has no live authority",
        "research host bundle contains a live deployment",
        "deploy_tango_cloud_assist.py --print-remote-script",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("deploy-tango-1-1.yml: missing `{needle}`"));
        }
    }

    for needle in [
        "require_research_host_has_no_live",
        "sed -i -E '/^[[:space:]]*(POLYMARKET_PRIVATE_KEY|PRIVATE_KEY)=/d'",
        "mode=Live",
        "systemctl stop ployd.service",
        "deployments pause",
        "deployments archive",
    ] {
        if !cloud_assist.contains(needle) {
            offenders.push(format!("deploy_tango_cloud_assist.py: missing `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "tango deploy live-authority removal guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn tango_deploy_pm_trade_postflight_uses_collector_health_not_fresh_trade_rows() {
    let workflow = workflow_contents(".github/workflows/deploy-tango-1-1.yml");
    let cloud_assist = workflow_contents("scripts/ci/deploy_tango_cloud_assist.py");
    let mut offenders = Vec::new();

    if !workflow.contains("deploy_tango_cloud_assist.py --print-remote-script") {
        offenders.push(
            "deploy-tango-1-1.yml: shared reviewed remote script is not executed".to_string(),
        );
    }

    for (name, content) in [("deploy_tango_cloud_assist.py", cloud_assist.as_str())] {
        if content.contains(
            "SELECT EXISTS (SELECT 1 FROM clob_trade_ticks WHERE received_at >= NOW() - INTERVAL '5 minutes')",
        ) {
            offenders.push(format!("{name}: still requires fresh clob_trade_ticks inserts"));
        }
        if content.contains("clob_trade_ticks is not receiving PM trade prints after deploy") {
            offenders.push(format!(
                "{name}: still emits stale PM trade freshness failure"
            ));
        }
        for needle in [
            "systemctl is-active --quiet ploy-pm-trade-collector.service",
            "require_service_guardrails ploy-pm-trade-collector.service",
            "check_recent_rows",
            "pm_market_catalog has no active crypto markets after market-discovery restart",
            "pm_market_metadata has no active crypto markets after market-discovery restart",
            "Continuing downstream service restarts; final postflight will fail if pm_market_catalog remains empty",
            "Continuing downstream service restarts; final postflight will fail if pm_market_metadata remains empty",
            "wait_for_recent_log",
            "journalctl -u",
            "Polymarket trade collector poll complete",
            "pm trade collector did not complete a healthy poll after deploy",
            "no partition of relation",
            "pm trade collector failed after deploy",
        ] {
            if !content.contains(needle) {
                offenders.push(format!("{name}: missing `{needle}`"));
            }
        }
    }

    for (name, content) in [("deploy_tango_cloud_assist.py", cloud_assist.as_str())] {
        let discovery_restart = content.find("systemctl restart ploy-market-discovery.service");
        let catalog_wait = content
            .find("pm_market_catalog has no active crypto markets after market-discovery restart");
        let metadata_wait = content
            .find("pm_market_metadata has no active crypto markets after market-discovery restart");
        let trade_restart = content.find("systemctl restart ploy-pm-trade-collector.service");
        let final_catalog_wait =
            content.rfind("pm_market_catalog has no active crypto markets after deploy");
        let final_metadata_wait =
            content.rfind("pm_market_metadata has no active crypto markets after deploy");
        match (
            discovery_restart,
            catalog_wait,
            metadata_wait,
            trade_restart,
            final_catalog_wait,
            final_metadata_wait,
        ) {
            (
                Some(discovery_restart),
                Some(catalog_wait),
                Some(metadata_wait),
                Some(trade_restart),
                Some(final_catalog_wait),
                Some(final_metadata_wait),
            ) => {
                if !(discovery_restart < catalog_wait && catalog_wait < trade_restart) {
                    offenders.push(format!(
                        "{name}: catalog readiness probe must run between market discovery and PM trade collector restart"
                    ));
                }
                if !(discovery_restart < metadata_wait && metadata_wait < trade_restart) {
                    offenders.push(format!(
                        "{name}: metadata readiness probe must run between market discovery and PM trade collector restart"
                    ));
                }
                if !(trade_restart < final_catalog_wait) {
                    offenders.push(format!(
                        "{name}: final catalog readiness gate must run after PM trade collector restart"
                    ));
                }
                if !(trade_restart < final_metadata_wait) {
                    offenders.push(format!(
                        "{name}: final metadata readiness gate must run after PM trade collector restart"
                    ));
                }
            }
            _ => offenders.push(format!(
                "{name}: missing market-discovery readiness ordering anchors"
            )),
        }
    }

    if !cloud_assist.contains(
        "\"pm trade collector did not complete a healthy poll after deploy\" \\\\\n  120 \\\\\n  5",
    ) {
        offenders.push(
            "deploy_tango_cloud_assist.py: PM trade collector healthy-poll wait is too short"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "tango deploy PM trade postflight guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_issue_workflows_apply_decision_labels() {
    let helper = workflow_contents(".github/scripts/research-issue-labels.js");
    let mut offenders = Vec::new();

    for needle in [
        "applyResearchIssueLabels",
        "labelsForDecision",
        "decision:pending",
        "decision:fix-data",
        "decision:fix-runtime",
        "evidence:factor-attribution",
        "evidence:diagnostic",
        "evidence:missing-metrics",
        "parity:blocked",
    ] {
        if !helper.contains(needle) {
            offenders.push(format!("research-issue-labels.js: missing `{needle}`"));
        }
    }

    for (workflow, evidence_label) in [
        (".github/workflows/backtest.yml", "evidence:backtest"),
        (
            ".github/workflows/replay-dryrun-parity.yml",
            "evidence:parity",
        ),
        (
            ".github/workflows/factor-review-v2-hosted-artifact.yml",
            "evidence:factor-review",
        ),
        (
            ".github/workflows/factor-walk-forward-v2-hosted-artifact.yml",
            "evidence:walk-forward",
        ),
        (".github/workflows/optimize.yml", "evidence:optimize"),
    ] {
        let content = workflow_contents(workflow);
        if !content.contains("research-issue-labels.js") {
            offenders.push(format!("{workflow}: missing shared research label helper"));
        }
        if !content.contains(evidence_label) {
            offenders.push(format!(
                "{workflow}: missing evidence label `{evidence_label}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "research issue label guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn factor_review_comments_are_factor_attribution_not_deployable_candidates() {
    let hosted = workflow_contents(".github/workflows/factor-review-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "const evidenceStage = \"factor_attribution\"",
        "- Evidence stage:",
        "continue-to-walk-forward",
        "continue-diagnostic-only",
        "\"evidence:factor-attribution\"",
    ] {
        if !hosted.contains(needle) {
            offenders.push(format!(
                "factor-review-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }
    for forbidden in [
        "candidate-for-oos-replay-gate",
        "no-deploy-factor-review-only",
    ] {
        if hosted.contains(forbidden) {
            offenders.push(format!(
                "factor-review-v2-hosted-artifact.yml: still contains legacy decision `{forbidden}`"
            ));
        }
    }
    for needle in [
        "evidence-stage.json",
        "\"kind\": \"research_evidence_stage\"",
        "\"promotion_ready\": false",
        "\"promotion_decision\": \"do_not_promote_from_factor_review\"",
        "\"allowed_next_stage\": \"walk_forward\"",
        "\"blocked_next_stages\": [\"dry_run_candidate\", \"live_candidate\"]",
    ] {
        if !hosted.contains(needle) {
            offenders.push(format!(
                "factor-review-v2-hosted-artifact.yml: missing artifact stage contract `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "factor review evidence-stage comment guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn strategy_research_runbook_stays_strategy_agnostic() {
    let runbook = workflow_contents("docs/runbooks/strategy-research-cicd.md");
    let research_template = workflow_contents(".github/ISSUE_TEMPLATE/strategy_research.yml");
    let implementation_template =
        workflow_contents(".github/ISSUE_TEMPLATE/strategy_implementation.yml");
    let backtest = workflow_contents(".github/workflows/backtest.yml");
    let optimize = workflow_contents(".github/workflows/optimize.yml");
    let mut offenders = Vec::new();

    for needle in [
        "Strategy-Agnostic Research and Runtime CI/CD Runbook",
        "Four-Layer Model",
        "Platform CI",
        "Research CI",
        "Runtime CD",
        "Promotion Gate",
        "PM5D is one current strategy profile, not the center of the CI/CD model",
        "strategy_family",
        "strategy_profile",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!("strategy-research-cicd.md: missing `{needle}`"));
        }
    }

    for forbidden in [
        "PM5D factor diagnostics",
        "PM5D walk-forward diagnostics",
        "PM5D backtest",
        "For PM5D, one event",
    ] {
        if runbook.contains(forbidden) {
            offenders.push(format!(
                "strategy-research-cicd.md: workflow contract is still PM5D-centered via `{forbidden}`"
            ));
        }
    }

    if research_template.contains("PM5D") {
        offenders
            .push("strategy_research.yml: placeholders should be strategy-agnostic".to_string());
    }
    if !research_template.contains("id: strategy_family")
        || !research_template.contains("id: strategy_profile")
    {
        offenders.push("strategy_research.yml: missing strategy family/profile fields".to_string());
    }
    if !implementation_template.contains("id: strategy_family")
        || !implementation_template.contains("id: strategy_profile")
    {
        offenders.push(
            "strategy_implementation.yml: missing strategy family/profile fields".to_string(),
        );
    }
    if !backtest.contains("name: Strategy Backtest") || backtest.contains("## PM5D Backtest") {
        offenders.push("backtest.yml: workflow naming should be strategy-agnostic".to_string());
    }
    if !optimize.contains("name: Optimize strategy params") {
        offenders.push("optimize.yml: workflow naming should be strategy-agnostic".to_string());
    }

    assert!(
        offenders.is_empty(),
        "strategy-agnostic CI/CD guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn auto_review_uses_bounded_workspace_checks() {
    let content = workflow_contents(".github/workflows/auto-review.yml");
    let mut offenders = Vec::new();

    if content.contains("cargo fmt --all -- --check") {
        offenders.push(
            "auto-review.yml: cargo fmt --all also formats local path dependencies such as vendor SDKs"
                .to_string(),
        );
    }
    if content.contains("cargo clippy --all-targets --features rl") {
        offenders.push(
            "auto-review.yml: rl is not a feature on the default workspace member set".to_string(),
        );
    }
    if !content.contains("git diff --name-only --diff-filter=ACMRT")
        || !content.contains("':!vendor/**'")
        || !content.contains("xargs rustfmt --check")
    {
        offenders.push(
            "auto-review.yml: formatting should target PR Rust file changes only and exclude vendor path dependencies"
                .to_string(),
        );
    }
    if content.contains("cargo clippy") || content.contains("Swatinem/rust-cache") {
        offenders.push(
            "auto-review.yml: advisory review should not duplicate heavy compile/test matrix work"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "auto-review workflow guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn test_matrix_owns_research_heavy_feature_contract() {
    let content = workflow_contents(".github/workflows/test.yml");
    let mut offenders = Vec::new();

    if !content
        .contains("cargo check --locked -p ploy-research --features db,polars-export,ml,rl,strategy-runtime --lib")
    {
        offenders.push(
            "test.yml: missing ploy-research heavy feature contract check".to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "research heavy CI guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn release_platform_workflow_is_build_only() {
    let content = workflow_contents(".github/workflows/release-platform.yml");
    let mut offenders = Vec::new();

    for forbidden in [
        "uses: appleboy/",
        "environment: production",
        "EC2_HOST",
        "deploy:",
    ] {
        if content.contains(forbidden) {
            offenders.push(format!(
                "release-platform.yml: build-only workflow contains `{forbidden}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "release-platform.yml build-only check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn host_deploy_workflows_require_main_provenance_and_pinned_ssh() {
    let tango = workflow_contents(".github/workflows/deploy-tango-1-1.yml");
    let trade = workflow_contents(".github/workflows/deploy-trade.yml");
    let mut offenders = Vec::new();

    for (name, content, environment, known_hosts_secret) in [
        (
            "deploy-tango-1-1.yml",
            &tango,
            "environment: tango-1-1",
            "TANGO_1_1_KNOWN_HOSTS",
        ),
        (
            "deploy-trade.yml",
            &trade,
            "'ploy-trade-1'",
            "PLOY_TRADE_1_KNOWN_HOSTS",
        ),
    ] {
        if !content.contains(environment) {
            offenders.push(format!("{name}: missing protected deployment environment"));
        }
        if !content.contains("Validate deploy provenance") {
            offenders.push(format!("{name}: missing deploy provenance validation step"));
        }
        if !content.contains("must dispatch the workflow from main")
            || !content.contains("must use git_ref=main")
            || !content.contains("does not match origin/main")
        {
            offenders.push(format!(
                "{name}: deployment is not hard-gated to main provenance"
            ));
        }
        if content.contains("StrictHostKeyChecking no")
            || content.contains("UserKnownHostsFile /dev/null")
        {
            offenders.push(format!("{name}: disables SSH host-key verification"));
        }
        if !content.contains("StrictHostKeyChecking yes")
            || !content.contains("UserKnownHostsFile ~/.ssh/known_hosts")
            || !content.contains(known_hosts_secret)
        {
            offenders.push(format!(
                "{name}: missing pinned known_hosts SSH verification"
            ));
        }
    }

    if !trade.contains("default: false") {
        offenders
            .push("deploy-trade.yml: live trade deploy should default deploy=false".to_string());
    }

    assert!(
        offenders.is_empty(),
        "host deploy workflow hardening check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_workflows_require_private_tango_db_endpoint() {
    let mut offenders = Vec::new();

    let backtest = workflow_contents(".github/workflows/backtest.yml");
    if !backtest.contains(
        "PLOY_RESEARCH_DATABASE_URL must target Tango-1-1 private VPC endpoint 172.16.0.204",
    ) {
        offenders.push("backtest.yml: missing private Tango DB endpoint guard".to_string());
    }
    if !backtest.contains("urlparse(os.environ[\"PLOY_RESEARCH_DATABASE_URL\"]).hostname") {
        offenders.push("backtest.yml: must parse the research DB URL host before use".to_string());
    }

    assert!(
        offenders.is_empty(),
        "research workflow private DB guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn optimize_workflow_builds_and_runs_in_one_job() {
    let content = workflow_contents(".github/workflows/optimize.yml");
    let mut offenders = Vec::new();

    if content.contains("download-artifact")
        || content.contains("optimize_backtest-${{ github.sha }}")
    {
        offenders.push(
            "optimize.yml: must not pass optimize_backtest through a binary artifact".to_string(),
        );
    }
    for forbidden in [
        "optimize_backtest",
        "allow_live_parquet_debug",
        "Sync Parquet data from Tango-1-1",
        "snapshot_run_id == ''",
        "ploy-strategy-bundles/parquet-feed",
    ] {
        if content.contains(forbidden) {
            offenders.push(format!(
                "optimize.yml: removed live/legacy optimizer path still contains `{forbidden}`"
            ));
        }
    }
    if content.contains("Swatinem/rust-cache") {
        offenders.push(
            "optimize.yml: must not use Swatinem/rust-cache after cache post-step hangs"
                .to_string(),
        );
    }
    if !content.contains("name: Build and run optimize on ploy-ci-1") {
        offenders.push("optimize.yml: must use the single build-and-run job".to_string());
    }
    if !content.contains("required: true")
        || !content.contains("--example three_layer_snapshot_optimize")
        || !content.contains("snapshot_run_id is required")
    {
        offenders.push(
            "optimize.yml: must require retained snapshots and run the snapshot optimizer"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "optimize workflow single-job guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn deployed_research_tools_do_not_ship_legacy_factor_research_binary() {
    let deploy = workflow_contents(".github/workflows/deploy-tango-1-1.yml");
    let deploy_helper = workflow_contents("scripts/ci/deploy_tango_cloud_assist.py");
    let acr = workflow_contents(".github/workflows/build-push-acr.yml");
    let dockerfile =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Dockerfile.research"))
            .expect("read Dockerfile.research");
    let cargo = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/ploy-research/Cargo.toml"),
    )
    .expect("read ploy-research Cargo.toml");
    let mut offenders = Vec::new();

    for (name, content) in [
        ("build-push-acr.yml", acr.as_str()),
        ("Dockerfile.research", dockerfile.as_str()),
        ("crates/ploy-research/Cargo.toml", cargo.as_str()),
    ] {
        if content.contains("factor_research") {
            offenders.push(format!(
                "{name}: legacy direct-DB factor research example still contains `factor_research`"
            ));
        }
    }
    for forbidden in ["--example factor_research", "examples/factor_research"] {
        if deploy.contains(forbidden) {
            offenders.push(format!(
                "deploy-tango-1-1.yml: legacy direct-DB factor research build still contains `{forbidden}`"
            ));
        }
    }
    for (name, content) in [
        ("build-push-acr.yml", acr.as_str()),
        ("Dockerfile.research", dockerfile.as_str()),
        ("crates/ploy-research/Cargo.toml", cargo.as_str()),
    ] {
        if content.contains("factor-research") {
            offenders.push(format!(
                "{name}: legacy factor-research binary packaging is still active"
            ));
        }
    }
    if !deploy_helper.contains("rm -f \"${{DEPLOY_ROOT}}/bin/factor-research\"") {
        offenders.push(
            "deploy_tango_cloud_assist.py: must remove stale factor-research binary on deploy"
                .to_string(),
        );
    }
    if !acr.contains("--example run_backtest") {
        offenders.push("build-push-acr.yml: should still build run_backtest".to_string());
    }
    if !dockerfile.contains("/opt/ploy/bin/run_backtest") {
        offenders.push("Dockerfile.research: should still package run_backtest".to_string());
    }

    assert!(
        offenders.is_empty(),
        "legacy factor research deploy guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn checked_in_platform_service_enforces_guardrails() {
    let content = workflow_contents("deployment/ployd.service");
    let required = [
        "Restart=always",
        "RestartSec=5",
        "StartLimitIntervalSec=300",
        "StartLimitBurst=5",
        "MemoryHigh=",
        "MemoryMax=",
        "OOMPolicy=kill",
        "EnvironmentFile=-/opt/ploy/.env",
        "ExecStart=/opt/ploy/bin/ployd",
    ];
    let mut offenders = Vec::new();

    if content.contains("StartLimitInterval=") {
        offenders.push(
            "deployment/ployd.service: still uses deprecated StartLimitInterval=".to_string(),
        );
    }

    for needle in required {
        if !content.contains(needle) {
            offenders.push(format!(
                "deployment/ployd.service: missing guardrail `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "checked-in ployd.service guardrail check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn checked_in_agent_sidecar_service_is_bounded_and_opt_in() {
    let content = workflow_contents("deployment/ploy-agent-sidecar.service");
    let required = [
        "Requires=ployd.service",
        "EnvironmentFile=/opt/ploy/.env",
        "ExecStart=/opt/ploy/bin/ploy-agent-sidecar",
        "Restart=on-failure",
        "MemoryHigh=384M",
        "MemoryMax=512M",
        "NoNewPrivileges=true",
        "ProtectSystem=full",
        "ProtectHome=true",
        "StateDirectoryMode=0700",
        "ReadWritePaths=/opt/ploy/run /var/lib/ploy-agent-sidecar",
    ];
    let mut offenders = Vec::new();
    for needle in required {
        if !content.contains(needle) {
            offenders.push(format!(
                "deployment/ploy-agent-sidecar.service: missing guardrail `{needle}`"
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "checked-in sidecar service guardrail check failed:\n{}",
        offenders.join("\n")
    );
}
