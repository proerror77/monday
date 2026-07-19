//! monday-prediction-evaluator — executable PM5D factor walk-forward review
//!
//! The training window fits each factor's direction and selected-quantile
//! threshold. The following test window only applies that trained threshold and
//! scores executable PnL after PM CLOB fillability.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::prediction_loop::current_prediction_policy_snapshot_id;
use ploy_research::{
    autofactor_matrix_from_v2, build_factor_observations_v2_with_deribit_and_pm_books,
    build_factor_stability_report, build_full_depth_execution_matrix,
    build_prediction_research_feedback, build_settlement_probability_promotion_gate_report,
    build_settlement_probability_report_with_prior, format_autofactor_reports,
    format_factor_combo_v1_report, format_factor_stability_report,
    format_factor_walk_forward_v2_report, format_fillability_review_v1_report,
    format_full_depth_execution_matrix_report, format_liquidity_gate_v1_report,
    format_liquidity_gated_alpha_v1_report, format_meta_label_walk_forward_v1_report,
    format_repricing_ic_report, format_settlement_probability_promotion_gate_report,
    format_settlement_probability_report, format_settlement_probability_walk_forward_report,
    format_trade_formation_v1_report, liquidity_gate_v1_with_deribit_and_pm_books,
    liquidity_gated_alpha_v1_with_deribit_and_pm_books, load_research_snapshot,
    mine_domain_autofactors_from_v2_with_guidance, read_mcts_search_state,
    review_fillability_v1_with_deribit_and_pm_books, review_repricing_ic_with_deribit_and_pm_books,
    review_trade_formation_v1_with_deribit_and_pm_books, validate_prediction_research_prior,
    validate_snapshot_request_coverage, walk_forward_factor_combo_v1_with_deribit_and_pm_books,
    walk_forward_factors_v2_with_deribit_and_pm_books,
    walk_forward_meta_label_v1_with_deribit_and_pm_books,
    walk_forward_settlement_probability_report_with_prior,
    walk_forward_settlement_verdict_report_with_prior,
    write_alpha_search_artifacts_with_state_and_runtime_feedback, AlphaSearchRuntimeFeedback,
    AlphaZooSnapshot, AutoFactorOptions, AutoFactorV2Target, FactorComboV1Options,
    FactorObservation, FactorReviewOptions, FactorStabilityOptions, FactorWalkForwardOptions,
    FillabilityReviewOptions, FullDepthExecutionMatrixOptions, LiquidityGateV1Options,
    LiquidityGatedAlphaV1Options, LlmPriorSpec, MetaLabelWalkForwardOptions, RepricingIcOptions,
    ResearchSnapshotRequest, ReviewSide, SettlementProbabilityDataQualityMode,
    SettlementProbabilityPromotionGateOptions, SettlementProbabilityReportOptions,
    SettlementProbabilityTimeCohort, SettlementProbabilityWalkForwardOptions,
    TradeFormationReviewOptions,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn prediction_underlying_symbol(raw: &str) -> String {
    let normalized = raw.trim().to_ascii_uppercase().replace(['/', '-', '_'], "");
    for quote in ["USDT", "USDC", "USD"] {
        if let Some(base) = normalized.strip_suffix(quote) {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }
    normalized
}

fn validate_prediction_snapshot_contract_id(
    prior_snapshot_id: &str,
    snapshot_contract_hash: Option<&str>,
) -> Result<(), String> {
    let snapshot_contract_hash = snapshot_contract_hash
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "loaded snapshot is missing snapshot_contract_hash".to_string())?;
    if prior_snapshot_id != snapshot_contract_hash {
        return Err(format!(
            "prediction research prior data_snapshot_id {prior_snapshot_id} does not match loaded snapshot_contract_hash {snapshot_contract_hash}"
        ));
    }
    Ok(())
}

fn validate_expected_prediction_policy(
    expected: Option<&str>,
    governed_prior: Option<&LlmPriorSpec>,
) -> Result<(), String> {
    let expected = match (expected, governed_prior.is_some()) {
        (Some(expected), _) => expected,
        (None, true) => {
            return Err(
                "governed prediction evaluation requires --expected-search-policy-snapshot-id"
                    .to_string(),
            )
        }
        (None, false) => return Ok(()),
    };
    let current = current_prediction_policy_snapshot_id();
    if expected != current {
        return Err(format!(
            "expected prediction policy {expected} does not match current evaluator policy {current}"
        ));
    }
    if governed_prior
        .is_some_and(|prior| prior.search_policy_snapshot_id.as_deref() != Some(expected))
    {
        return Err("governed prior policy differs from current evaluator policy".to_string());
    }
    Ok(())
}

fn parse_time_cohort_boundary(
    raw: Option<&str>,
    required: bool,
) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = raw else {
        return if required {
            Err("governed prediction evaluation requires --time-cohort-boundary-ms".to_string())
        } else {
            Ok(None)
        };
    };
    let boundary_ms = raw
        .parse::<i64>()
        .map_err(|_| "--time-cohort-boundary-ms must be a positive integer".to_string())?;
    if boundary_ms <= 0 {
        return Err("--time-cohort-boundary-ms must be a positive integer".to_string());
    }
    let boundary = Utc
        .timestamp_millis_opt(boundary_ms)
        .single()
        .ok_or_else(|| "--time-cohort-boundary-ms is outside the UTC range".to_string())?;
    Ok(Some(boundary))
}

fn validate_time_cohort_range(
    boundary: DateTime<Utc>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(), String> {
    if boundary <= start || boundary >= end {
        return Err(format!(
            "time-cohort boundary {boundary} must be strictly inside evaluator range [{start}, {end})"
        ));
    }
    Ok(())
}

fn settlement_time_cohort_from_args(
    args: &[String],
    required: bool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    event_window_secs: Option<i64>,
) -> Result<Option<SettlementProbabilityTimeCohort>, String> {
    let Some(boundary) = parse_time_cohort_boundary(
        flag_value(args, "--time-cohort-boundary-ms").as_deref(),
        required,
    )?
    else {
        return Ok(None);
    };
    validate_time_cohort_range(boundary, start, end)?;
    let event_window_secs = event_window_secs
        .filter(|&value| !required || value == 300)
        .ok_or_else(|| {
            "prediction evaluator time cohort requires --event-window-secs (300 when governed)"
                .to_string()
        })?;
    SettlementProbabilityTimeCohort::new(boundary, event_window_secs).map(Some)
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn parse_data_quality_mode(raw: Option<String>) -> SettlementProbabilityDataQualityMode {
    match raw.as_deref().unwrap_or("strict_continuous") {
        "strict_continuous" | "strict-continuous" => {
            SettlementProbabilityDataQualityMode::StrictContinuous
        }
        "event_complete" | "event-complete" => SettlementProbabilityDataQualityMode::EventComplete,
        value => panic!("invalid --data-quality-mode: {value}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportSuite {
    Core,
    Full,
}

impl ReportSuite {
    fn parse(raw: Option<String>) -> Self {
        match raw.as_deref().unwrap_or("full") {
            "core" => Self::Core,
            "full" => Self::Full,
            value => panic!("invalid --report-suite: {value}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Full => "full",
        }
    }
}

fn replay_parity_evidence(path: &str) -> (bool, String) {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read replay parity JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse replay parity JSON {path} failed: {err}"));
    let runtime_ready = json
        .pointer("/runtime_evidence_comparison/strict_parity_ready")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let event_ready = json
        .pointer("/runtime_evidence_comparison/events/strict_parity_ready")
        .or_else(|| json.pointer("/event_comparison/strict_parity_ready"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let risk_flags = json
        .get("blocking_risk_flags")
        .or_else(|| json.get("risk_flags"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let advisory_flags = json
        .get("advisory_flags")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let decision = json
        .get("decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing>");
    let ready = runtime_ready && event_ready && risk_flags.is_empty() && decision == "continue";
    let evidence = format!(
        "replay_parity_json={} runtime_ready={} event_ready={} blocking_flags={} advisory_flags={} decision={}",
        path,
        runtime_ready,
        event_ready,
        if risk_flags.is_empty() {
            "<none>".to_string()
        } else {
            risk_flags.join("|")
        },
        if advisory_flags.is_empty() {
            "<none>".to_string()
        } else {
            advisory_flags.join("|")
        },
        decision,
    );
    (ready, evidence)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_time_cohort_boundary, replay_parity_evidence, settlement_time_cohort_from_args,
        validate_expected_prediction_policy, validate_prediction_snapshot_contract_id,
        validate_time_cohort_range,
    };
    use chrono::{TimeZone, Utc};
    use ploy_research::prediction_loop::current_prediction_policy_snapshot_id;
    use std::fs;

    fn write_parity_fixture(name: &str, payload: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "ploy-factor-walk-forward-{name}-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::write(&path, payload).expect("write parity fixture");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn governed_prediction_policy_must_match_the_current_evaluator() {
        let current = current_prediction_policy_snapshot_id();
        validate_expected_prediction_policy(Some(&current), None).expect("current baseline policy");
        assert!(
            validate_expected_prediction_policy(Some("sha256:stale"), None)
                .expect_err("stale controller policy must fail")
                .contains("does not match")
        );
    }

    #[test]
    fn governed_evaluator_rejects_missing_or_invalid_time_cohort_boundary() {
        let boundary_ms = 1_700_001_000_000_i64;
        let start = Utc
            .timestamp_millis_opt(boundary_ms - 1_000)
            .single()
            .unwrap();
        let end = Utc
            .timestamp_millis_opt(boundary_ms + 1_000)
            .single()
            .unwrap();
        assert!(
            settlement_time_cohort_from_args(&[], true, start, end, Some(300))
                .expect_err("governed baseline must carry a boundary")
                .contains("requires --time-cohort-boundary-ms")
        );
        for raw in ["0", "-1", "not-a-timestamp"] {
            let args = vec!["--time-cohort-boundary-ms".to_string(), raw.to_string()];
            assert!(settlement_time_cohort_from_args(&args, true, start, end, Some(300)).is_err());
        }

        let boundary = parse_time_cohort_boundary(Some(&boundary_ms.to_string()), true)
            .expect("valid governed boundary")
            .expect("present boundary");
        assert_eq!(boundary.timestamp_millis(), boundary_ms);
        validate_time_cohort_range(boundary, start, end).expect("interior boundary");
        assert!(validate_time_cohort_range(start, start, end).is_err());
        assert!(validate_time_cohort_range(end, start, end).is_err());
        let args = vec!["--time-cohort-boundary-ms".into(), boundary_ms.to_string()];
        assert!(settlement_time_cohort_from_args(&args, true, start, end, Some(900)).is_err());
    }

    #[test]
    fn replay_parity_requires_runtime_and_event_strict_readiness() {
        let path = write_parity_fixture(
            "event-not-ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {"strict_parity_ready": true},
              "event_comparison": {"strict_parity_ready": false}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(!ready);
        assert!(evidence.contains("runtime_ready=true"));
        assert!(evidence.contains("event_ready=false"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_parity_accepts_runtime_and_event_strict_readiness() {
        let path = write_parity_fixture(
            "ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {"strict_parity_ready": true},
              "event_comparison": {"strict_parity_ready": true}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(ready, "{evidence}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_parity_prefers_runtime_event_comparison_over_legacy_event_summary() {
        let path = write_parity_fixture(
            "runtime-event-ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {
                "strict_parity_ready": true,
                "events": {"strict_parity_ready": true}
              },
              "event_comparison": {"strict_parity_ready": false}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(ready, "{evidence}");
        assert!(evidence.contains("runtime_ready=true"));
        assert!(evidence.contains("event_ready=true"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn prediction_prior_matches_snapshot_contract_hash_not_content_hash() {
        let contract_hash = format!("sha256:{}", "a".repeat(64));
        let content_hash = "b".repeat(64);

        assert!(
            validate_prediction_snapshot_contract_id(&contract_hash, Some(&contract_hash)).is_ok()
        );
        assert!(
            validate_prediction_snapshot_contract_id(&content_hash, Some(&contract_hash)).is_err()
        );
        assert!(validate_prediction_snapshot_contract_id(&contract_hash, None).is_err());
    }
}

fn alpha_search_plan_factor_names(path: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha search plan JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha search plan JSON {path} failed: {err}"));
    json.get("selected_nodes")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("factor_name"))
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn read_llm_prior(path: &str) -> LlmPriorSpec {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha search LLM prior JSON {path} failed: {err}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha search LLM prior JSON {path} failed: {err}"))
}

fn read_alpha_zoo_snapshot(path: &str) -> AlphaZooSnapshot {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha zoo snapshot JSON {path} failed: {err}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha zoo snapshot JSON {path} failed: {err}"))
}

fn runtime_feedback_from_candidate_replay(path: &str) -> Option<AlphaSearchRuntimeFeedback> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read candidate strategy replay JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse candidate strategy replay JSON {path} failed: {err}"));
    let runtime_score = json
        .get("runtime_score")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let base_factor = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score.as_str())
        .to_string();
    if base_factor.is_empty() {
        return None;
    }
    let diagnostics = json
        .get("strategy_diagnostics")
        .and_then(serde_json::Value::as_object);
    let counterfactual = json
        .get("score_counterfactual")
        .and_then(serde_json::Value::as_object);
    let metric = |key: &str| -> usize {
        diagnostics
            .and_then(|values| values.get(key))
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(0)
    };
    let configured_threshold = counterfactual
        .and_then(|values| values.get("configured_entry_threshold"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("0.25");
    let direct_passes = counterfactual
        .and_then(|values| values.get("direct_pass_counts"))
        .and_then(|counts| counts.get(configured_threshold))
        .or_else(|| {
            counterfactual
                .and_then(|values| values.get("direct_pass_counts"))
                .and_then(|counts| counts.get("0.25"))
        })
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or_else(|| metric("settlement_autofactor_predictive_score_ge_025"));
    Some(AlphaSearchRuntimeFeedback {
        version: None,
        target: None,
        side: None,
        runtime_score,
        base_factor,
        entry_signals: metric("entry_signals"),
        direct_passes_at_configured_threshold: direct_passes,
        formula_evaluations: metric("settlement_autofactor_formula_evaluations").max(
            counterfactual
                .and_then(|values| values.get("formula_evaluations"))
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(0),
        ),
        depth_fillable: metric("settlement_autofactor_depth_fillable").max(
            counterfactual
                .and_then(|values| values.get("depth_fillable"))
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(0),
        ),
        executable_edge_pass_min_edge: metric(
            "settlement_autofactor_executable_edge_pass_min_edge",
        ),
    })
}

fn filter_autofactor_reports(
    reports: Vec<ploy_research::AutoFactorReport>,
    factor_name_filter: Option<&str>,
) -> Vec<ploy_research::AutoFactorReport> {
    let Some(filter) = factor_name_filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
    else {
        return reports;
    };
    reports
        .into_iter()
        .filter(|report| report.name.contains(filter))
        .collect()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if flag_value(&args, "--db-url").is_some() {
        eprintln!("ERROR: direct DB factor walk-forward has been removed; pass --snapshot-dir");
        std::process::exit(2);
    }
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });
    let symbols: Vec<String> = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let pm_book_sample_secs: i32 = flag_value(&args, "--pm-book-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(lob_sample_secs);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let event_window_secs = flag_value(&args, "--event-window-secs").map(|raw| {
        raw.parse::<i64>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or_else(|| panic!("invalid --event-window-secs: {raw}"))
    });
    let review = FactorReviewOptions {
        stake_usd: flag_value(&args, "--stake-usd")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(15.0),
        min_observations: flag_value(&args, "--min-observations")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        top_quantile: flag_value(&args, "--top-quantile")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0.2),
        max_quote_age_secs,
    };
    let options = FactorWalkForwardOptions {
        review,
        train_window_days: flag_value(&args, "--train-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(2),
        test_window_days: flag_value(&args, "--test-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        step_days: flag_value(&args, "--step-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        train_window_hours: flag_value(&args, "--train-window-hours")
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| raw.parse().ok()),
        test_window_hours: flag_value(&args, "--test-window-hours")
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| raw.parse().ok()),
        step_hours: flag_value(&args, "--step-hours")
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| raw.parse().ok()),
        top_n: flag_value(&args, "--top-n")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        factor_name_filter: flag_value(&args, "--factor-name-filter"),
    };
    let report_suite = ReportSuite::parse(flag_value(&args, "--report-suite"));

    eprintln!(
        "monday-prediction-evaluator: {} -> {} for {:?}, stake_usd={:.2}, train_window={}, test_window={}, step={}, event_window_secs={}, observation_sample_secs={}, pm_book_sample_secs={}, factor_name_filter={}, report_suite={}",
        start,
        end,
        symbols,
        options.review.stake_usd,
        options.train_window_label(),
        options.test_window_label(),
        options.step_label(),
        event_window_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<all>".to_string()),
        observation_sample_secs,
        pm_book_sample_secs,
        options.factor_name_filter.as_deref().unwrap_or("<none>"),
        report_suite.as_str()
    );

    let snapshot_dir = flag_value(&args, "--snapshot-dir");
    let replay_parity_json = flag_value(&args, "--replay-parity-json");
    let candidate_strategy_replay_json = flag_value(&args, "--candidate-strategy-replay-json");
    let alpha_search_output_dir = flag_value(&args, "--alpha-search-output-dir");
    let alpha_search_plan_json = flag_value(&args, "--alpha-search-plan-json");
    let alpha_search_llm_prior_json = flag_value(&args, "--alpha-search-llm-prior-json");
    let expected_prediction_policy = flag_value(&args, "--expected-search-policy-snapshot-id");
    let settlement_time_cohort = settlement_time_cohort_from_args(
        &args,
        expected_prediction_policy.is_some(),
        start,
        end,
        event_window_secs,
    )
    .unwrap_or_else(|reason| panic!("prediction evaluator time cohort mismatch: {reason}"));
    let llm_prior = alpha_search_llm_prior_json.as_deref().map(read_llm_prior);
    let governed_prediction_prior = llm_prior
        .as_ref()
        .filter(|prior| prior.mission_id.is_some() || !prior.probability_blends.is_empty());
    validate_expected_prediction_policy(
        expected_prediction_policy.as_deref(),
        governed_prediction_prior,
    )
    .unwrap_or_else(|reason| panic!("prediction evaluator policy mismatch: {reason}"));
    if let Some(prior) = governed_prediction_prior {
        validate_prediction_research_prior(prior)
            .unwrap_or_else(|reason| panic!("prediction research prior is not governed: {reason}"));
        if prior.horizon.as_deref() == Some("5m") && event_window_secs != Some(300) {
            panic!(
                "governed 5m prediction research requires --event-window-secs 300, got {}",
                event_window_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<missing>".to_string())
            );
        }
    }
    if let Some(path) = alpha_search_llm_prior_json.as_deref() {
        eprintln!("alpha search typed LLM prior loaded from {path}");
    }
    let alpha_search_state_json = flag_value(&args, "--alpha-search-state-json");
    let alpha_zoo_snapshot_json = flag_value(&args, "--alpha-zoo-snapshot-json");
    let data_quality_mode = parse_data_quality_mode(flag_value(&args, "--data-quality-mode"));
    let require_deribit = flag_present(&args, "--require-deribit");
    let min_event_complete_events = flag_value(&args, "--min-event-complete-events")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(20);
    let min_event_complete_rows = flag_value(&args, "--min-event-complete-rows")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(40);
    let min_promotion_entry_fill_rate = flag_value(&args, "--min-promotion-entry-fill-rate")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(|| {
            SettlementProbabilityPromotionGateOptions::default().min_entry_fill_rate
        });
    let snapshot_dir = snapshot_dir.unwrap_or_else(|| {
        eprintln!("ERROR: --snapshot-dir is required for monday-prediction-evaluator");
        std::process::exit(2);
    });
    let snapshot_provenance: String;
    let snapshot_hash: String;
    let snapshot_contract_hash: Option<String>;
    let snapshot_data_audit_status: Option<String>;
    let include_deribit: bool;
    let (observations, deribit_snapshots, all_pm_book_snapshots): (
        Vec<FactorObservation>,
        Vec<_>,
        Vec<_>,
    ) = {
        let started = std::time::Instant::now();
        let snapshot =
            load_research_snapshot(&snapshot_dir).expect("load research snapshot failed");
        let snapshot_symbol_set: HashSet<&str> = snapshot
            .manifest
            .symbols
            .iter()
            .map(String::as_str)
            .collect();
        let missing_symbols: Vec<&str> = symbols
            .iter()
            .map(String::as_str)
            .filter(|symbol| !snapshot_symbol_set.contains(symbol))
            .collect();
        if !missing_symbols.is_empty() {
            eprintln!(
                "ERROR: requested symbols {:?} are not present in snapshot symbols {:?}",
                missing_symbols, snapshot.manifest.symbols
            );
            std::process::exit(2);
        }
        let validation_symbols = snapshot.manifest.symbols.clone();
        validate_snapshot_request_coverage(
            &snapshot.manifest,
            ResearchSnapshotRequest {
                symbols: &validation_symbols,
                start,
                end,
                lob_sample_secs,
                pm_book_sample_secs,
                observation_sample_secs,
                max_quote_age_secs,
                stake_usd: options.review.stake_usd,
                require_official_settlement: true,
            },
        )
        .expect("snapshot does not cover requested walk-forward inputs");
        snapshot_hash = snapshot.manifest.snapshot_hash.clone().unwrap_or_default();
        snapshot_contract_hash = snapshot.manifest.snapshot_contract_hash.clone();
        eprintln!(
            "snapshot: schema={} hash={} generated_at={} observations={} deribit={} pm_books={} load_ms={}",
            snapshot.manifest.schema_version,
            if snapshot_hash.is_empty() { "<missing>" } else { &snapshot_hash },
            snapshot.manifest.generated_at,
            snapshot.observations.len(),
            snapshot.deribit_snapshots.len(),
            snapshot.pm_book_snapshots.len(),
            started.elapsed().as_millis()
        );
        snapshot_data_audit_status = snapshot.manifest.data_audit_status.clone();
        include_deribit = snapshot.manifest.include_deribit;
        snapshot_provenance = format!(
            "# Snapshot\nsnapshot_schema={}\nsnapshot_hash={}\nsnapshot_contract_hash={}\nsnapshot_generated_at={}\nsnapshot_optimizer_data_dir={}\nsnapshot_data_requirements={}\nsnapshot_data_audit_status={}\nsnapshot_data_audit_report={}\nsnapshot_include_deribit={}\n",
            snapshot.manifest.schema_version,
            if snapshot_hash.is_empty() { "<missing>" } else { &snapshot_hash },
            snapshot_contract_hash.as_deref().unwrap_or("<missing>"),
            snapshot.manifest.generated_at,
            snapshot
                .manifest
                .optimizer_data_dir
                .as_deref()
                .unwrap_or("<missing>"),
            if snapshot.manifest.data_requirements.is_empty() {
                "<unspecified>".to_string()
            } else {
                snapshot.manifest.data_requirements.join(",")
            },
            snapshot
                .manifest
                .data_audit_status
                .as_deref()
                .unwrap_or("<not-recorded>"),
            snapshot
                .manifest
                .data_audit_report
                .as_deref()
                .unwrap_or("<not-recorded>"),
            snapshot.manifest.include_deribit
        );
        let requested_symbol_set: HashSet<&str> = symbols.iter().map(String::as_str).collect();
        let mut observations: Vec<FactorObservation> = snapshot
            .observations
            .into_iter()
            .filter(|row| requested_symbol_set.contains(row.symbol.as_str()))
            .filter(|row| {
                event_window_secs.is_none_or(|expected| row.event_window_secs == expected)
            })
            .collect();
        let event_ids: HashSet<String> = observations
            .iter()
            .map(|row| row.event_id.clone())
            .collect();
        let deribit_snapshots: Vec<_> = snapshot
            .deribit_snapshots
            .into_iter()
            .filter(|row| requested_symbol_set.contains(row.symbol.as_str()))
            .collect();
        let pm_book_snapshots: Vec<_> = snapshot
            .pm_book_snapshots
            .into_iter()
            .filter(|row| event_ids.contains(&row.event_id))
            .collect();
        observations.shrink_to_fit();
        eprintln!(
            "snapshot filtered: requested_symbols={:?} event_window_secs={} observations={} deribit={} pm_books={}",
            symbols,
            event_window_secs
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<all>".to_string()),
            observations.len(),
            deribit_snapshots.len(),
            pm_book_snapshots.len()
        );
        (observations, deribit_snapshots, pm_book_snapshots)
    };

    if let Some(prior) = governed_prediction_prior {
        let prior_snapshot_id = prior
            .data_snapshot_id
            .as_deref()
            .expect("validated prediction snapshot id");
        validate_prediction_snapshot_contract_id(
            prior_snapshot_id,
            snapshot_contract_hash.as_deref(),
        )
        .unwrap_or_else(|reason| panic!("{reason}"));
        let prior_symbol = prediction_underlying_symbol(&prior.symbols[0]);
        let requested_underlyings = symbols
            .iter()
            .map(|symbol| prediction_underlying_symbol(symbol))
            .collect::<HashSet<_>>();
        if requested_underlyings.len() != 1 || !requested_underlyings.contains(&prior_symbol) {
            panic!(
                "prediction research prior symbol {} does not match requested symbols {:?}",
                prior.symbols[0], symbols
            );
        }
        if observations.is_empty() {
            panic!(
                "governed 5m prediction research found no 300s observations; legacy snapshots default event_window_secs to 0 and must be rebuilt"
            );
        }
        if observations.iter().any(|row| row.event_window_secs != 300) {
            panic!("governed 5m prediction evaluator received a non-300s observation");
        }
    }

    if observations.is_empty() {
        eprintln!("no observations — check date range, symbols, quote coverage, and settlements");
        return;
    }

    let report = walk_forward_factors_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        options.clone(),
    );
    println!("{snapshot_provenance}");
    println!("{}", format_factor_walk_forward_v2_report(&report));
    let execution_matrix = build_full_depth_execution_matrix(
        &observations,
        &all_pm_book_snapshots,
        FullDepthExecutionMatrixOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            max_quote_age_secs,
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_full_depth_execution_matrix_report(&execution_matrix, options.top_n)
    );
    let conservative_execution_matrix = build_full_depth_execution_matrix(
        &observations,
        &all_pm_book_snapshots,
        FullDepthExecutionMatrixOptions {
            visible_depth_haircut: 0.5,
            max_levels: Some(3),
            min_bucket_observations: options.review.min_observations.max(20),
            max_quote_age_secs,
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_full_depth_execution_matrix_report(&conservative_execution_matrix, options.top_n)
    );
    if report_suite == ReportSuite::Full {
        let repricing_ic_report = review_repricing_ic_with_deribit_and_pm_books(
            &observations,
            &deribit_snapshots,
            &all_pm_book_snapshots,
            RepricingIcOptions {
                review: options.review.clone(),
                ..Default::default()
            },
        );
        println!(
            "{}",
            format_repricing_ic_report(&repricing_ic_report, options.top_n)
        );
    }
    let autofactor_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        &options.review,
    );
    let settlement_probability_report = build_settlement_probability_report_with_prior(
        &autofactor_rows,
        llm_prior.as_ref(),
        SettlementProbabilityReportOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_settlement_probability_report(&settlement_probability_report)
    );
    let settlement_walk_forward_options = SettlementProbabilityWalkForwardOptions {
        walk_forward: options.clone(),
        probability: SettlementProbabilityReportOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            ..Default::default()
        },
        time_cohort: settlement_time_cohort,
        ..Default::default()
    };
    let settlement_probability_walk_forward_report =
        walk_forward_settlement_probability_report_with_prior(
            &autofactor_rows,
            start,
            end,
            llm_prior.as_ref(),
            settlement_walk_forward_options.clone(),
        );
    let settlement_verdict_walk_forward_report = walk_forward_settlement_verdict_report_with_prior(
        &autofactor_rows,
        start,
        end,
        llm_prior.as_ref(),
        settlement_walk_forward_options,
    );
    println!(
        "{}",
        format_settlement_probability_walk_forward_report(
            &settlement_probability_walk_forward_report
        )
    );
    let event_complete_rows = autofactor_rows
        .iter()
        .filter(|row| {
            row.label_full_depth_entry_fillable
                && row.label_conservative_entry_fillable
                && row.label_settlement_win.is_some()
                && row.label_full_depth_executable_pnl_15u.is_some()
                && row.label_conservative_executable_pnl_15u.is_some()
                && (!include_deribit || row.deribit_mark_iv.is_finite())
        })
        .collect::<Vec<_>>();
    let event_complete_events = event_complete_rows
        .iter()
        .map(|row| row.event_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    let replay_parity_status = replay_parity_json.as_deref().map(replay_parity_evidence);
    let promotion_gate_report = build_settlement_probability_promotion_gate_report(
        &settlement_probability_report,
        &settlement_probability_walk_forward_report,
        &execution_matrix,
        &conservative_execution_matrix,
        SettlementProbabilityPromotionGateOptions {
            stake_usd: options.review.stake_usd,
            min_entry_fill_rate: min_promotion_entry_fill_rate,
            require_deribit,
            include_deribit,
            data_audit_status: snapshot_data_audit_status,
            data_quality_mode,
            event_complete_events,
            event_complete_rows: event_complete_rows.len(),
            min_event_complete_events,
            min_event_complete_rows,
            global_full_depth_entry_fill_rate: Some(report.health.full_depth_entry_fill_rate()),
            replay_parity_ready: replay_parity_status
                .as_ref()
                .map(|(ready, _)| *ready)
                .unwrap_or(false),
            replay_parity_evidence: replay_parity_status.map(|(_, evidence)| evidence),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_settlement_probability_promotion_gate_report(&promotion_gate_report)
    );
    if let (Some(output_root), Some(prior)) =
        (alpha_search_output_dir.as_deref(), llm_prior.as_ref())
    {
        if let Some(feedback) = build_prediction_research_feedback(
            prior,
            &settlement_verdict_walk_forward_report,
            promotion_gate_report.options.min_positive_window_ratio,
        ) {
            let output_dir = std::path::Path::new(output_root)
                .join(AutoFactorV2Target::FullDepthSettlementExecutablePnl.as_str());
            std::fs::create_dir_all(&output_dir).unwrap_or_else(|err| {
                panic!(
                    "create prediction research feedback directory {} failed: {err}",
                    output_dir.display()
                )
            });
            let json = serde_json::to_string_pretty(&feedback)
                .expect("serialize prediction research feedback");
            let bytes = format!("{json}\n");
            let content_hash = format!("{:x}", Sha256::digest(bytes.as_bytes()));
            let output_path =
                output_dir.join(format!("prediction-research-feedback-{content_hash}.json"));
            if output_path.exists() {
                let existing = std::fs::read(&output_path).unwrap_or_else(|err| {
                    panic!(
                        "read existing prediction research feedback {} failed: {err}",
                        output_path.display()
                    )
                });
                assert_eq!(
                    existing,
                    bytes.as_bytes(),
                    "content-addressed prediction feedback collision at {}",
                    output_path.display()
                );
            } else {
                std::fs::write(&output_path, bytes).unwrap_or_else(|err| {
                    panic!(
                        "write prediction research feedback {} failed: {err}",
                        output_path.display()
                    )
                });
            }
            eprintln!(
                "prediction research feedback written mission_id={} candidates={} path={}",
                feedback.mission_id,
                feedback.candidates.len(),
                output_path.display()
            );
        }
    }
    let autofactor_options = AutoFactorOptions {
        min_observations: options.review.min_observations.max(50),
        min_window_observations: options.review.min_observations.max(20),
        min_top_bucket_full_depth_entry_fill_rate: min_promotion_entry_fill_rate,
        ..Default::default()
    };
    let alpha_search_input_names = alpha_search_output_dir.as_ref().and_then(|_| {
        autofactor_matrix_from_v2(&autofactor_rows)
            .map(|matrix| matrix.input_names().into_iter().collect::<Vec<_>>())
            .map_err(|err| {
                eprintln!("alpha search matrix build failed: {err}");
                err
            })
            .ok()
    });
    let alpha_search_plan_names = alpha_search_plan_json
        .as_deref()
        .map(alpha_search_plan_factor_names)
        .unwrap_or_default();
    let mcts_state = alpha_search_state_json.as_deref().map(|path| {
        read_mcts_search_state(path)
            .unwrap_or_else(|err| panic!("read alpha search MCTS state JSON {path} failed: {err}"))
    });
    if let Some(path) = alpha_search_state_json.as_deref() {
        eprintln!("alpha search cumulative MCTS state loaded from {path}");
    }
    let alpha_zoo = alpha_zoo_snapshot_json
        .as_deref()
        .map(read_alpha_zoo_snapshot);
    if let Some(path) = alpha_zoo_snapshot_json.as_deref() {
        eprintln!("alpha zoo snapshot loaded from {path}");
    }
    let runtime_feedback = candidate_strategy_replay_json
        .as_deref()
        .and_then(runtime_feedback_from_candidate_replay);
    if let Some(feedback) = runtime_feedback.as_ref() {
        eprintln!(
            "alpha search runtime feedback loaded: runtime_score={} entry_signals={} direct_passes={} executable_edge_pass_min_edge={} formula_evaluations={}",
            feedback.runtime_score,
            feedback.entry_signals,
            feedback.direct_passes_at_configured_threshold,
            feedback.executable_edge_pass_min_edge,
            feedback.formula_evaluations
        );
    }
    if let Some(path) = alpha_search_plan_json.as_deref() {
        eprintln!(
            "alpha search MCTS plan loaded: {} selected nodes from {}",
            alpha_search_plan_names.len(),
            path
        );
    }
    for target in [
        AutoFactorV2Target::FullDepthRepricePnl10s(ReviewSide::Up),
        AutoFactorV2Target::FullDepthRepricePnl10s(ReviewSide::Down),
        AutoFactorV2Target::FullDepthRepricePnl30s(ReviewSide::Up),
        AutoFactorV2Target::FullDepthRepricePnl30s(ReviewSide::Down),
        AutoFactorV2Target::FullDepthSettlementExecutablePnl,
        AutoFactorV2Target::TradeableFullDepthSettlementPnl,
    ] {
        let target_name = target.as_str();
        let side = target.review_side();
        let plan_names = side
            .is_none()
            .then_some(alpha_search_plan_names.as_slice())
            .unwrap_or_default();
        let lane_llm_prior = side.is_none().then_some(llm_prior.as_ref()).flatten();
        match mine_domain_autofactors_from_v2_with_guidance(
            &autofactor_rows,
            target,
            &autofactor_options,
            plan_names,
            lane_llm_prior,
        ) {
            Ok(reports) => {
                let reports =
                    filter_autofactor_reports(reports, options.factor_name_filter.as_deref());
                let side_suffix = side
                    .map(|side| format!(" side={}", side.as_str()))
                    .unwrap_or_default();
                println!("# AutoFactor target={target_name}{side_suffix}");
                println!("{}", format_autofactor_reports(&reports, options.top_n));
                if let (Some(output_dir), Some(input_names)) = (
                    alpha_search_output_dir.as_deref(),
                    alpha_search_input_names.as_ref(),
                ) {
                    if side.is_none() {
                        match write_alpha_search_artifacts_with_state_and_runtime_feedback(
                            output_dir,
                            target_name,
                            input_names,
                            &reports,
                            &autofactor_options,
                            mcts_state.as_ref(),
                            runtime_feedback.as_ref(),
                            llm_prior.as_ref(),
                            alpha_zoo.as_ref(),
                        ) {
                            Ok(summary) => eprintln!(
                                "alpha search artifacts written target={} candidates={} rejected={} best={} dir={}",
                                summary.target,
                                summary.candidate_count,
                                summary.rejected_count,
                                summary.best_candidate.as_deref().unwrap_or("<none>"),
                                summary.output_dir
                            ),
                            Err(err) => eprintln!(
                                "alpha search artifact write failed for {}: {err}",
                                target_name
                            ),
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("autofactor seed report failed for {}: {err}", target_name);
            }
        }
    }
    if report_suite == ReportSuite::Core {
        return;
    }
    let fillability_report = review_fillability_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        FillabilityReviewOptions {
            review: options.review.clone(),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_fillability_review_v1_report(&fillability_report, options.top_n)
    );
    let liquidity_gate_report = liquidity_gate_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        LiquidityGateV1Options {
            review: options.review.clone(),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_liquidity_gate_v1_report(&liquidity_gate_report)
    );
    let gated_alpha_report = liquidity_gated_alpha_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        LiquidityGatedAlphaV1Options {
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            walk_forward: options.clone(),
        },
    );
    println!(
        "{}",
        format_liquidity_gated_alpha_v1_report(&gated_alpha_report, options.top_n)
    );
    let formation_report = review_trade_formation_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        TradeFormationReviewOptions {
            review: options.review.clone(),
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            top_n: options.top_n,
            ..Default::default()
        },
    );
    println!("{}", format_trade_formation_v1_report(&formation_report));
    let meta_label_report = walk_forward_meta_label_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        MetaLabelWalkForwardOptions {
            review: options.review.clone(),
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            train_window_days: options.train_window_days,
            test_window_days: options.test_window_days,
            step_days: options.step_days,
            top_n: options.top_n,
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_meta_label_walk_forward_v1_report(&meta_label_report)
    );
    let stability_report =
        build_factor_stability_report(&report, FactorStabilityOptions::default());
    println!(
        "{}",
        format_factor_stability_report(&stability_report, options.top_n)
    );
    let combo_report = walk_forward_factor_combo_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        FactorComboV1Options {
            walk_forward: options,
            ..Default::default()
        },
    );
    println!("{}", format_factor_combo_v1_report(&combo_report));
}
