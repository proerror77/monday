use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchBudget {
    pub max_candidates_per_day: usize,
    pub max_backtests_per_day: usize,
    pub max_llm_calls_per_day: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchManagerInput {
    #[serde(default = "default_evidence_stage")]
    pub evidence_stage: String,
    #[serde(default)]
    pub latest_runs: serde_json::Value,
    #[serde(default)]
    pub factor_registry_summary: serde_json::Value,
    #[serde(default)]
    pub rejected_factor_patterns: serde_json::Value,
    #[serde(default)]
    pub market_data_health: serde_json::Value,
    pub research_budget: ResearchBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchManagerPlan {
    pub theme: String,
    pub candidate_count: usize,
    pub search_depth: usize,
    pub priority: String,
    pub evidence_stage: String,
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocker_actions: Vec<ResearchBlockerAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchBlockerAction {
    pub blocker_family: String,
    pub action: String,
    pub reason: String,
}

fn default_evidence_stage() -> String {
    "factor_attribution".to_string()
}

pub fn validate_evidence_stage(stage: &str) -> Result<(), String> {
    match stage {
        "diagnostic" | "factor_attribution" | "executable_replay" | "walk_forward"
        | "runtime_parity" | "dry_run_candidate" | "live_candidate" => Ok(()),
        _ => Err(format!("unsupported evidence_stage: {stage}")),
    }
}

pub fn build_research_manager_plan(input: &ResearchManagerInput) -> ResearchManagerPlan {
    plan_next_research(input).unwrap_or_else(|_| {
        plan(
            "fix_workflow",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec!["fix_unsupported_evidence_stage"],
        )
    })
}

pub fn plan_next_research(input: &ResearchManagerInput) -> Result<ResearchManagerPlan, String> {
    validate_evidence_stage(&input.evidence_stage)?;

    if has_any_key_value(
        &input.market_data_health,
        &["missing", "stale", "critical_missing"],
        &[true.into(), "true".into(), "critical".into()],
    ) {
        return Ok(plan_with_blocker_actions(
            "fix_data",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "repair_or_exclude_missing_data_surface",
                "rerun_snapshot_data_audit",
            ],
            derive_blocker_actions(input),
        ));
    }

    let blocker_actions = derive_blocker_actions(input);
    let latest_decision = latest_run_decision_value(&input.latest_runs);
    let has_negative_runtime_economics = blocker_actions.iter().any(|item| {
        item.blocker_family == "strategy_economics"
            && item.action == "mutate_or_reject_negative_runtime_edge"
    });
    let has_blocked_runtime_replay_frontier =
        latest_runtime_replay_frontier_is_blocked(&input.factor_registry_summary);
    if has_ready_handoff(&input.latest_runs)
        && !has_negative_runtime_economics
        && !has_blocked_runtime_replay_frontier
    {
        return Ok(plan_with_blocker_actions(
            "ready_handoff",
            1,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "create_dry_run_handoff_issue",
                "open_config_pr_from_ready_handoff",
            ],
            Vec::new(),
        ));
    }

    if blocker_actions
        .iter()
        .any(|item| item.blocker_family.starts_with("data_"))
    {
        let mut actions = vec!["rerun_snapshot_data_audit"];
        if blocker_actions
            .iter()
            .any(|item| item.action == "repair_official_settlement_coverage")
        {
            actions.insert(0, "repair_official_settlement_coverage");
        }
        if blocker_actions
            .iter()
            .any(|item| item.action == "collect_full_depth_execution_surface")
        {
            actions.insert(0, "collect_full_depth_execution_surface");
        }
        return Ok(plan_with_blocker_actions(
            "fix_data",
            0,
            0,
            "high",
            &input.evidence_stage,
            actions,
            blocker_actions,
        ));
    }

    if !has_negative_runtime_economics
        && (contains_string(
            &latest_decision,
            &[
                "replay_parity_missing",
                "runtime_parity_missing",
                "candidate_strategy_replay_not_runtime_replay",
                "candidate_strategy_replay_missing",
                "missing_runtime_contract",
                "runtime_contract_unmapped_factor",
                "missing_runtime_strategy_mapping",
            ],
        ) || has_any_key_value(
            &latest_decision,
            &["replay_parity_ready", "runtime_parity_ready"],
            &[false.into(), "false".into()],
        ))
    {
        return Ok(plan_with_blocker_actions(
            "fix_runtime",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "run_recorded_replay_parity",
                "compare_runtime_scorer_contract",
            ],
            blocker_actions,
        ));
    }

    if has_unreplayed_unblocked_runtime_candidate(&input.factor_registry_summary) {
        return Ok(plan_with_blocker_actions(
            "candidate_to_runtime_replay",
            1,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "build_runtime_candidate_replay",
                "write_runtime_replay_trace_artifact",
            ],
            blocker_actions,
        ));
    }

    if blocker_actions.iter().any(|item| {
        matches!(
            item.blocker_family.as_str(),
            "search_power" | "execution_fillability" | "strategy_economics"
        )
    }) {
        return Ok(plan_with_blocker_actions(
            "revise_prior",
            input.research_budget.max_candidates_per_day.min(8),
            2,
            "high",
            &input.evidence_stage,
            vec![
                "generate_typed_llm_prior_json",
                "rerun_alpha_search_with_bounded_mutations",
            ],
            blocker_actions,
        ));
    }

    if has_unblocked_runtime_candidate(&input.factor_registry_summary) {
        return Ok(plan_with_blocker_actions(
            "candidate_to_runtime_replay",
            1,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "build_runtime_candidate_replay",
                "write_runtime_replay_trace_artifact",
            ],
            blocker_actions,
        ));
    }

    if contains_string(
        &latest_decision,
        &[
            "reward_stagnation",
            "empty_search",
            "stagnant",
            "revise_prior",
        ],
    ) || contains_string(
        &input.rejected_factor_patterns,
        &["stagnant", "repeated_rejection"],
    ) {
        return Ok(plan_with_blocker_actions(
            "revise_prior",
            input.research_budget.max_candidates_per_day.min(8),
            2,
            "medium",
            &input.evidence_stage,
            vec![
                "generate_typed_llm_prior_json",
                "rerun_alpha_search_with_bounded_mutations",
            ],
            blocker_actions,
        ));
    }

    let has_selected_nodes = contains_string(&input.latest_runs, &["selected_nodes"])
        || contains_string(&input.factor_registry_summary, &["candidate", "watchlist"]);
    let candidate_count = if has_selected_nodes {
        input.research_budget.max_candidates_per_day.min(12)
    } else {
        input.research_budget.max_candidates_per_day.min(4)
    };

    Ok(plan_with_blocker_actions(
        "continue_search",
        candidate_count,
        2,
        "normal",
        &input.evidence_stage,
        vec![
            "continue_hosted_alpha_search",
            "write_registry_preview_and_trace_artifacts",
        ],
        blocker_actions,
    ))
}

fn plan(
    theme: &str,
    candidate_count: usize,
    search_depth: usize,
    priority: &str,
    evidence_stage: &str,
    actions: Vec<&str>,
) -> ResearchManagerPlan {
    ResearchManagerPlan {
        theme: theme.to_string(),
        candidate_count,
        search_depth,
        priority: priority.to_string(),
        evidence_stage: evidence_stage.to_string(),
        actions: actions.into_iter().map(str::to_string).collect(),
        blocker_actions: Vec::new(),
    }
}

fn plan_with_blocker_actions(
    theme: &str,
    candidate_count: usize,
    search_depth: usize,
    priority: &str,
    evidence_stage: &str,
    actions: Vec<&str>,
    blocker_actions: Vec<ResearchBlockerAction>,
) -> ResearchManagerPlan {
    ResearchManagerPlan {
        blocker_actions,
        ..plan(
            theme,
            candidate_count,
            search_depth,
            priority,
            evidence_stage,
            actions,
        )
    }
}

fn derive_blocker_actions(input: &ResearchManagerInput) -> Vec<ResearchBlockerAction> {
    let mut actions = Vec::new();
    let latest_value = latest_run_decision_value(&input.latest_runs);
    let latest_runtime_replay = latest_recent_runtime_replay(&input.factor_registry_summary);
    let latest = latest_value.to_string().to_lowercase();
    let runtime_replay = latest_runtime_replay
        .as_ref()
        .map(|value| value.to_string().to_lowercase())
        .unwrap_or_default();
    let market_data = market_data_blocker_text(&input.market_data_health);
    let frontier_runtime_replay_roi = latest_runtime_replay
        .as_ref()
        .and_then(runtime_market_update_replay_roi)
        .or_else(|| runtime_market_update_replay_roi(&latest_value));
    let negative_runtime_replay = frontier_runtime_replay_roi
        .map(|roi| roi < 0.0)
        .unwrap_or(false);
    let runtime_or_promotion_blockers = if runtime_replay.is_empty() {
        latest.clone()
    } else {
        runtime_replay.clone()
    };
    let data_blocker_text = if negative_runtime_replay {
        latest.as_str()
    } else {
        runtime_or_promotion_blockers.as_str()
    };
    let frontier_proves_full_depth = has_any_key_value(
        &latest_value,
        &[
            "full_depth_entry",
            "full_depth_execution_surface",
            "full_fidelity",
        ],
        &[true.into(), "true".into()],
    );
    let market_covers_full_depth_execution = market_data_has_full_depth_execution_surface(
        &input.market_data_health,
        "clob_orderbook_snapshots",
    );
    let market_covers_official_settlement =
        market_data_has_valid_settlement_surface(&input.market_data_health, "pm_token_settlements");

    if contains_any_text(
        data_blocker_text,
        &[
            "official_settlement_missing",
            "missing_official_settlement",
            "settlement_labels_missing",
            "official settlement",
        ],
    ) {
        actions.push(blocker_action(
            "data_settlement",
            "repair_official_settlement_coverage",
            "Runtime replay traded events are missing official settlement labels.",
        ));
    }
    if contains_any_text(
        data_blocker_text,
        &[
            "sampled_snapshot_required_for_execution_surface",
            "required_execution_surface_is_sampled_snapshot",
            "full_depth_missing",
            "full-depth missing",
        ],
    ) && !market_covers_full_depth_execution
    {
        actions.push(blocker_action(
            "data_execution_surface",
            "collect_full_depth_execution_surface",
            "Promotion requires full-depth executable evidence instead of sampled snapshots.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &["trade_count_too_small", "min_trade_count", "too_few_trades"],
    ) {
        actions.push(blocker_action(
            "search_power",
            "increase_distinct_event_coverage_or_reduce_selectivity",
            "Runtime replay did not produce enough distinct event-level trades.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "fillability",
            "fill_rate_too_low",
            "entry_fill_rate_too_low",
            "capacity_too_low",
        ],
    ) {
        actions.push(blocker_action(
            "execution_fillability",
            "prefer_high_fillability_depth_filters",
            "Candidate selection must avoid thin depth or low-fillability entry surfaces.",
        ));
    }
    let has_data_action = actions
        .iter()
        .any(|item| item.blocker_family.starts_with("data_"));
    let positive_runtime_replay = frontier_runtime_replay_roi
        .map(|roi| roi > 0.0)
        .unwrap_or(false);
    let economics_tokens = if positive_runtime_replay {
        &["walk_forward_oos"][..]
    } else {
        &[
            "roi_too_low",
            "candidate_strategy_replay_roi_too_low",
            "total_pnl_nonpositive",
            "walk_forward_oos",
            "negative_runtime_edge",
        ][..]
    };
    if (contains_any_text(&runtime_or_promotion_blockers, economics_tokens)
        || negative_runtime_replay)
        && !(positive_runtime_replay && has_data_action)
    {
        actions.push(blocker_action(
            "strategy_economics",
            "mutate_or_reject_negative_runtime_edge",
            "Latest replay or walk-forward evidence failed economic/OOS gates.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "missing_runtime_contract",
            "runtime_contract_unmapped_factor",
            "missing_runtime_strategy_mapping",
        ],
    ) {
        actions.push(blocker_action(
            "runtime_contract",
            "repair_runtime_contract_mapping",
            "Factor needs a typed unblocked runtime contract before replay or promotion.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "candidate_strategy_replay_not_runtime_replay",
            "requires_runtime_replay_not_top_bucket_aggregate",
            "candidate_strategy_replay_identity_basis_mismatch",
        ],
    ) {
        actions.push(blocker_action(
            "runtime_replay",
            "build_runtime_market_update_replay",
            "Top-bucket aggregate evidence must be replaced by ordered runtime MarketUpdate replay.",
        ));
    }

    if !market_covers_full_depth_execution
        && !frontier_proves_full_depth
        && contains_any_text(
            &market_data,
            &[
                "required_execution_surface_is_sampled_snapshot",
                "sampled_snapshot_required_for_execution_surface",
                "full_depth_missing",
                "full-depth missing",
            ],
        )
    {
        actions.push(blocker_action(
            "promotion_data_execution_surface",
            "collect_full_depth_execution_surface",
            "Research snapshot data health reports sampled execution surface; promotion needs full-depth evidence.",
        ));
    }
    if !market_covers_official_settlement
        && contains_any_text(
            &market_data,
            &[
                "required_execution_surface_not_materialized",
                "pm_token_settlements",
                "official_settlement_missing",
            ],
        )
    {
        actions.push(blocker_action(
            "promotion_data_settlement",
            "repair_official_settlement_coverage",
            "Research snapshot data health reports missing or non-materialized official settlement labels.",
        ));
    }

    dedupe_blocker_actions(actions)
}

fn latest_run_decision_value(latest_runs: &serde_json::Value) -> serde_json::Value {
    let frontier = latest_runs
        .get("runs")
        .and_then(serde_json::Value::as_array)
        .and_then(|runs| runs.first())
        .unwrap_or(latest_runs);
    strip_non_frontier_blocker_fields(frontier)
}

fn strip_non_frontier_blocker_fields(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut stripped = serde_json::Map::new();
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "evaluated_factors" | "entries" | "factors" | "recent_factors" | "patterns"
                ) {
                    continue;
                }
                stripped.insert(key.clone(), strip_non_frontier_blocker_fields(item));
            }
            serde_json::Value::Object(stripped)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(strip_non_frontier_blocker_fields)
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn blocker_action(blocker_family: &str, action: &str, reason: &str) -> ResearchBlockerAction {
    ResearchBlockerAction {
        blocker_family: blocker_family.to_string(),
        action: action.to_string(),
        reason: reason.to_string(),
    }
}

fn contains_any_text(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn market_data_blocker_text(value: &serde_json::Value) -> String {
    let mut fragments = Vec::new();
    collect_market_data_blocker_values(value, &mut fragments);
    if fragments.is_empty()
        && has_any_key_value(
            value,
            &["missing_blocks_promotion", "critical_missing"],
            &[true.into(), "true".into(), "critical".into()],
        )
    {
        fragments.push(value.clone());
    }
    fragments
        .into_iter()
        .map(|item| item.to_string().to_lowercase())
        .collect::<Vec<_>>()
        .join("\n")
}

fn market_data_has_full_depth_execution_surface(
    value: &serde_json::Value,
    surface_name: &str,
) -> bool {
    value
        .get("execution_surfaces")
        .and_then(|item| item.get("surfaces"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("surface").and_then(serde_json::Value::as_str) == Some(surface_name)
                    && item
                        .get("full_fidelity")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    && !item
                        .get("incomplete")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true)
                    && item
                        .get("row_count")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                        > 0
            })
        })
}

fn market_data_has_valid_settlement_surface(value: &serde_json::Value, surface_name: &str) -> bool {
    value
        .get("settlement_surfaces")
        .and_then(|item| item.get("surfaces"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("surface").and_then(serde_json::Value::as_str) == Some(surface_name)
                    && item
                        .get("valid")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    && item
                        .get("settlement_token_count")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                        > 0
                    && json_array_empty(item.get("blockers"))
            })
        })
}

fn collect_market_data_blocker_values(
    value: &serde_json::Value,
    fragments: &mut Vec<serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "surface_blockers" | "promotion_blockers" | "data_repair_blockers"
                ) {
                    fragments.push(item.clone());
                } else {
                    collect_market_data_blocker_values(item, fragments);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_market_data_blocker_values(item, fragments);
            }
        }
        _ => {}
    }
}

fn dedupe_blocker_actions(actions: Vec<ResearchBlockerAction>) -> Vec<ResearchBlockerAction> {
    let mut deduped = Vec::new();
    for action in actions {
        if !deduped
            .iter()
            .any(|item: &ResearchBlockerAction| item.action == action.action)
        {
            deduped.push(action);
        }
    }
    deduped
}

fn contains_string(value: &serde_json::Value, needles: &[&str]) -> bool {
    let raw = value.to_string();
    needles.iter().any(|needle| raw.contains(needle))
}

fn has_any_key_value(
    value: &serde_json::Value,
    keys: &[&str],
    expected_values: &[serde_json::Value],
) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, item)| {
            (keys.contains(&key.as_str()) && expected_values.contains(item))
                || has_any_key_value(item, keys, expected_values)
        }),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| has_any_key_value(item, keys, expected_values)),
        _ => false,
    }
}

fn latest_recent_runtime_replay(value: &serde_json::Value) -> Option<serde_json::Value> {
    value
        .get("recent_candidate_replays")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| {
                    item.get("basis").and_then(serde_json::Value::as_str)
                        == Some("runtime_market_update_replay")
                })
                .cloned()
        })
}

fn runtime_market_update_replay_roi(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Object(map) => {
            let basis = map
                .get("basis")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if basis == "runtime_market_update_replay" {
                if let Some(roi) = map
                    .get("metrics")
                    .and_then(|metrics| metrics.get("roi"))
                    .and_then(serde_json::Value::as_f64)
                {
                    return Some(roi);
                }
            }
            map.values().find_map(runtime_market_update_replay_roi)
        }
        serde_json::Value::Array(items) => items.iter().find_map(runtime_market_update_replay_roi),
        _ => None,
    }
}

fn latest_runtime_replay_frontier_is_blocked(value: &serde_json::Value) -> bool {
    latest_recent_runtime_replay(value)
        .as_ref()
        .is_some_and(runtime_replay_is_blocked)
}

fn runtime_replay_is_blocked(value: &serde_json::Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };

    let basis = map
        .get("basis")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if basis != "runtime_market_update_replay" {
        return false;
    }

    if map
        .get("promotion_ready")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        return true;
    }

    if map
        .get("promotion_decision")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|decision| {
            matches!(
                decision,
                "blocked" | "reject" | "rejected" | "revise" | "do_not_promote"
            )
        })
    {
        return true;
    }

    map.get("blocking_risk_flags")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

fn has_unblocked_runtime_candidate(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            unblocked_runtime_candidate_score(map).is_some()
                || map.values().any(has_unblocked_runtime_candidate)
        }
        serde_json::Value::Array(items) => items.iter().any(has_unblocked_runtime_candidate),
        _ => false,
    }
}

fn has_unreplayed_unblocked_runtime_candidate(value: &serde_json::Value) -> bool {
    let replayed_scores = replayed_runtime_scores(value);
    has_unreplayed_unblocked_runtime_candidate_inner(value, &replayed_scores)
}

fn has_unreplayed_unblocked_runtime_candidate_inner(
    value: &serde_json::Value,
    replayed_scores: &HashSet<String>,
) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            unblocked_runtime_candidate_score(map)
                .is_some_and(|score| !replayed_scores.contains(score))
                || map.values().any(|item| {
                    has_unreplayed_unblocked_runtime_candidate_inner(item, replayed_scores)
                })
        }
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| has_unreplayed_unblocked_runtime_candidate_inner(item, replayed_scores)),
        _ => false,
    }
}

fn unblocked_runtime_candidate_score(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<&str> {
    let status = map
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let candidate_status = status.is_empty() || matches!(status, "candidate" | "watchlist");
    if !candidate_status || !json_array_empty(map.get("blockers")) {
        return None;
    }

    let contract = map
        .get("runtime_contract")
        .and_then(serde_json::Value::as_object)?;
    let version_ok = contract.get("version").and_then(serde_json::Value::as_str)
        == Some("autofactor_runtime_contract_v1");
    if !version_ok
        || !contract
            .get("strategy_profile")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|profile| !profile.is_empty())
        || !json_array_empty(contract.get("blockers"))
    {
        return None;
    }

    contract
        .get("runtime_score")
        .and_then(serde_json::Value::as_str)
        .filter(|score| !score.is_empty())
}

fn replayed_runtime_scores(value: &serde_json::Value) -> HashSet<String> {
    let mut scores = HashSet::new();
    extend_replayed_runtime_scores(value, "recent_candidate_replays", &mut scores);
    extend_replayed_runtime_scores(value, "ready_candidate_replays", &mut scores);
    scores
}

fn extend_replayed_runtime_scores(
    value: &serde_json::Value,
    key: &str,
    scores: &mut HashSet<String>,
) {
    if let Some(items) = value.get(key).and_then(serde_json::Value::as_array) {
        scores.extend(
            items
                .iter()
                .filter(|item| runtime_replay_basis(item) == Some("runtime_market_update_replay"))
                .filter_map(runtime_replay_score)
                .map(str::to_string),
        );
    }
}

fn runtime_replay_basis(value: &serde_json::Value) -> Option<&str> {
    value
        .get("basis")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .get("artifact_json")
                .and_then(|artifact| artifact.get("basis"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .get("artifact_json")
                .and_then(|artifact| artifact.get("identity"))
                .and_then(|identity| identity.get("basis"))
                .and_then(serde_json::Value::as_str)
        })
}

fn runtime_replay_score(value: &serde_json::Value) -> Option<&str> {
    value
        .get("runtime_score")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .get("artifact_json")
                .and_then(|artifact| artifact.get("runtime_score"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .get("artifact_json")
                .and_then(|artifact| artifact.get("identity"))
                .and_then(|identity| identity.get("runtime_score"))
                .and_then(serde_json::Value::as_str)
        })
}

fn has_ready_handoff(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let status = map
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let recommended_action = map
                .get("recommended_action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if status == "ready" && recommended_action == "create_dry_run_handoff" {
                return true;
            }

            let decision = map
                .get("decision")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let replay_ready = map
                .get("candidate_strategy_replay")
                .and_then(|item| item.get("ready"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if decision == "qualified" && replay_ready {
                return true;
            }

            map.values().any(has_ready_handoff)
        }
        serde_json::Value::Array(items) => items.iter().any(has_ready_handoff),
        _ => false,
    }
}

fn json_array_empty(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Array(items)) => items.is_empty(),
        Some(serde_json::Value::Null) | None => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        evidence_stage: &str,
        latest_runs: serde_json::Value,
        market_data_health: serde_json::Value,
    ) -> ResearchManagerInput {
        ResearchManagerInput {
            evidence_stage: evidence_stage.to_string(),
            latest_runs,
            factor_registry_summary: serde_json::json!({"selected_nodes": [{"id": "n1"}]}),
            rejected_factor_patterns: serde_json::json!({}),
            market_data_health,
            research_budget: ResearchBudget {
                max_candidates_per_day: 20,
                max_backtests_per_day: 4,
                max_llm_calls_per_day: 1,
            },
        }
    }

    #[test]
    fn planner_fails_closed_on_unknown_evidence_stage() {
        let err = plan_next_research(&input(
            "unknown_stage",
            serde_json::json!({}),
            serde_json::json!({}),
        ))
        .expect_err("stage");
        assert!(err.contains("unsupported evidence_stage"));
    }

    #[test]
    fn planner_prioritizes_data_repairs() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({}),
            serde_json::json!({"critical_missing": true, "surface_blockers": [{"name": "clob", "blocker_type": "data_repair"}]}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_data");
        assert_eq!(plan.candidate_count, 0);
    }

    #[test]
    fn planner_does_not_rerun_snapshot_for_promotion_only_blockers() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({}),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {"surface": "clob_orderbook_snapshots", "reason": "required_execution_surface_is_sampled_snapshot"}
                ]
            }),
        ))
        .expect("plan");
        assert_ne!(plan.theme, "fix_data");
        assert!(plan.candidate_count > 0);
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "promotion_data_execution_surface"
                && item.action == "collect_full_depth_execution_surface"
        }));
    }

    #[test]
    fn planner_routes_runtime_contract_blockers_to_runtime_repair() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({"blockers": ["missing_runtime_contract:auto_settlement_edge"]}),
            serde_json::json!({"missing_blocks_promotion": true, "critical_missing": false}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_runtime");
    }

    #[test]
    fn planner_prioritizes_runtime_parity_repairs() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({"handoff": {"replay_parity_ready": false}}),
            serde_json::json!({}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_runtime");
    }

    #[test]
    fn planner_revises_prior_after_stagnation() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({"stop_reason": "reward_stagnation"}),
            serde_json::json!({}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
    }

    #[test]
    fn planner_routes_unblocked_runtime_candidate_to_candidate_replay() {
        let mut input = input(
            "walk_forward",
            serde_json::json!({}),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {"surface": "clob_orderbook_snapshots", "reason": "required_execution_surface_is_sampled_snapshot"}
                ]
            }),
        );
        input.factor_registry_summary = serde_json::json!({
            "recent_factors": [{
                "factor_name": "auto_settlement_conservative_settlement_edge",
                "status": "candidate",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "blockers": [],
                "runtime_contract": {
                    "version": "autofactor_runtime_contract_v1",
                    "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
                    "strategy_profile": "settlement_probability",
                    "blockers": []
                }
            }]
        });

        let plan = plan_next_research(&input).expect("plan");
        assert_eq!(plan.theme, "candidate_to_runtime_replay");
        assert!(plan
            .actions
            .contains(&"build_runtime_candidate_replay".to_string()));
    }

    #[test]
    fn planner_routes_ready_handoff_ahead_of_stale_negative_edge_blockers() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({
                "source": "experiment_trace",
                "runs": [{
                    "run_id": "26367562792",
                    "artifacts": [
                        {
                            "event_type": "autofactor_strategy_handoff",
                            "output_json": {
                                "status": "ready",
                                "recommended_action": "create_dry_run_handoff",
                                "strategies": [{
                                    "name": "mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
                                    "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
                                }]
                            }
                        },
                        {
                            "event_type": "autofactor_promotion",
                            "output_json": {
                                "decision": "qualified",
                                "candidate_strategy_replay": {
                                    "ready": true,
                                    "basis": "runtime_market_update_replay",
                                    "blockers": [],
                                    "metrics": {
                                        "roi": 0.11653800000105065,
                                        "trade_count": 50,
                                        "settlement_event_count": 50
                                    }
                                },
                                "input_prior": {
                                    "runtime_avoid_factors": [{
                                        "reason": "negative_runtime_edge"
                                    }]
                                }
                            }
                        }
                    ]
                }]
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "ready_handoff");
        assert_eq!(plan.candidate_count, 1);
        assert_eq!(plan.search_depth, 0);
        assert!(plan
            .actions
            .contains(&"create_dry_run_handoff_issue".to_string()));
        assert!(plan
            .actions
            .contains(&"open_config_pr_from_ready_handoff".to_string()));
        assert!(plan.blocker_actions.is_empty());
    }

    #[test]
    fn planner_uses_durable_ready_handoff_frontier_summary() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({
                "source": "experiment_trace",
                "evidence_stage": "factor_attribution",
                "runs": [{
                    "run_id": "newest-factor-preview",
                    "artifacts": [{
                        "event_type": "factor_registry_preview",
                        "output_json": {
                            "factors": [{
                                "factor_name": "blocked_unselected_factor",
                                "blockers": ["missing_runtime_contract"]
                            }]
                        }
                    }]
                }],
                "ready_handoffs": [{
                    "run_id": "26377165132",
                    "event_type": "strategy_handoff",
                    "output_json": {
                        "kind": "autofactor_strategy_handoff",
                        "status": "ready",
                        "recommended_action": "create_dry_run_handoff",
                        "strategies": [{
                            "runtime_score": "autofactor_formula:ready_factor"
                        }],
                        "candidate_strategy_replay": {
                            "ready": true,
                            "basis": "runtime_market_update_replay"
                        }
                    }
                }]
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "ready_handoff");
        assert_eq!(plan.candidate_count, 1);
        assert!(plan.blocker_actions.is_empty());
    }

    #[test]
    fn planner_routes_newer_negative_runtime_replay_ahead_of_stale_ready_handoff() {
        let mut input = input(
            "walk_forward",
            serde_json::json!({
                "source": "experiment_trace",
                "evidence_stage": "walk_forward",
                "runs": [{
                    "run_id": "26530018058",
                    "artifacts": [{
                        "event_type": "strategy_handoff",
                        "output_json": {
                            "status": "blocked",
                            "recommended_action": "do_not_promote"
                        }
                    }]
                }],
                "ready_handoffs": [{
                    "run_id": "26377165132",
                    "created_at": "2026-05-24T17:16:29Z",
                    "event_type": "strategy_handoff",
                    "output_json": {
                        "kind": "autofactor_strategy_handoff",
                        "status": "ready",
                        "recommended_action": "create_dry_run_handoff",
                        "strategies": [{
                            "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
                        }],
                        "candidate_strategy_replay": {
                            "ready": true,
                            "basis": "runtime_market_update_replay"
                        }
                    }
                }]
            }),
            serde_json::json!({}),
        );
        input.factor_registry_summary = serde_json::json!({
            "recent_candidate_replays": [{
                "candidate_replay_id": "candidate_replay:negative",
                "run_id": "26530018058",
                "workflow_run_id": "26528933436",
                "basis": "runtime_market_update_replay",
                "promotion_ready": false,
                "promotion_decision": "blocked",
                "strategy_profile": "settlement_probability",
                "runtime_score": "autofactor_formula:auto_settlement_model_full_depth_settlement_edge_spread_adjusted",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "metrics": {
                    "roi": -0.013906620311657932,
                    "total_pnl": -30.246899177856,
                    "trade_count": 145,
                    "unique_event_count": 145,
                    "entry_fill_rate": 1.0
                },
                "blocking_risk_flags": [
                    "official_settlement_missing:142<145",
                    "roi_too_low:-0.013907<0.000000"
                ],
                "created_at": "2026-05-27T18:17:00Z"
            }],
            "ready_candidate_replays": [{
                "candidate_replay_id": "candidate_replay:old-ready",
                "run_id": "26377165132",
                "workflow_run_id": "26367311478",
                "basis": "runtime_market_update_replay",
                "promotion_ready": true,
                "promotion_decision": "promote_to_runtime",
                "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
                "metrics": {
                    "roi": 0.11653800000105063,
                    "trade_count": 50
                },
                "blocking_risk_flags": [],
                "created_at": "2026-05-24T17:16:29Z"
            }]
        });

        let plan = plan_next_research(&input).expect("plan");

        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(!plan
            .actions
            .contains(&"create_dry_run_handoff_issue".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                && item.action == "mutate_or_reject_negative_runtime_edge"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
    }

    #[test]
    fn planner_routes_newer_blocked_runtime_replay_ahead_of_stale_ready_handoff() {
        let mut input = input(
            "walk_forward",
            serde_json::json!({
                "source": "experiment_trace",
                "evidence_stage": "walk_forward",
                "runs": [{
                    "run_id": "26548811085",
                    "artifacts": [{
                        "event_type": "autofactor_promotion",
                        "output_json": {
                            "decision": "blocked",
                            "blockers": [
                                "missing_runtime_contract",
                                "runtime_contract_unmapped_factor"
                            ],
                            "candidate_strategy_replay": {
                                "basis": "factor_walk_forward_top_bucket_aggregate",
                                "blockers": [
                                    "candidate_strategy_replay_not_runtime_replay",
                                    "candidate_strategy_replay_identity_basis_mismatch"
                                ]
                            }
                        }
                    }]
                }],
                "ready_handoffs": [{
                    "run_id": "26377165132",
                    "created_at": "2026-05-24T17:16:29Z",
                    "event_type": "strategy_handoff",
                    "output_json": {
                        "kind": "autofactor_strategy_handoff",
                        "status": "ready",
                        "recommended_action": "create_dry_run_handoff",
                        "strategies": [{
                            "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
                        }],
                        "candidate_strategy_replay": {
                            "ready": true,
                            "basis": "runtime_market_update_replay"
                        }
                    }
                }]
            }),
            serde_json::json!({}),
        );
        input.factor_registry_summary = serde_json::json!({
            "recent_candidate_replays": [{
                "candidate_replay_id": "candidate_replay:blocked-positive",
                "run_id": "26550036258",
                "workflow_run_id": "26550036258",
                "basis": "runtime_market_update_replay",
                "promotion_ready": false,
                "promotion_decision": "blocked",
                "strategy_profile": "settlement_probability",
                "runtime_score": "autofactor_formula:llm_mut_spread_adjusted_external_move_select_entry_price_quality_ge_075_runtime_pass_through_add_capacity_gate",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "metrics": {
                    "roi": 0.23777533981084,
                    "total_pnl": 71.332601943252,
                    "trade_count": 20,
                    "unique_event_count": 20,
                    "entry_fill_rate": 1.0,
                    "settlement_event_count": 19
                },
                "blocking_risk_flags": [
                    "trade_count_too_small:20<50",
                    "official_settlement_missing:19<20"
                ],
                "created_at": "2026-05-28T02:05:53Z"
            }],
            "ready_candidate_replays": [{
                "candidate_replay_id": "candidate_replay:old-ready",
                "run_id": "26377165132",
                "workflow_run_id": "26367311478",
                "basis": "runtime_market_update_replay",
                "promotion_ready": true,
                "promotion_decision": "promote_to_runtime",
                "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
                "metrics": {
                    "roi": 0.11653800000105063,
                    "trade_count": 50
                },
                "blocking_risk_flags": [],
                "created_at": "2026-05-24T17:16:29Z"
            }]
        });

        let plan = plan_next_research(&input).expect("plan");

        assert_eq!(plan.theme, "fix_data");
        assert!(!plan
            .actions
            .contains(&"create_dry_run_handoff_issue".to_string()));
        assert!(!plan
            .actions
            .contains(&"open_config_pr_from_ready_handoff".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "search_power"
                && item.action == "increase_distinct_event_coverage_or_reduce_selectivity"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                || item.action == "mutate_or_reject_negative_runtime_edge"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "runtime_contract"
                && item.action == "repair_runtime_contract_mapping"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "runtime_replay"
                && item.action == "build_runtime_market_update_replay"
        }));
    }

    #[test]
    fn planner_does_not_route_unselected_preview_factor_blockers_to_runtime_repair() {
        let mut input = input(
            "factor_attribution",
            serde_json::json!({
                "source": "experiment_trace",
                "runs": [{
                    "run_id": "newest-factor-preview",
                    "artifacts": [{
                        "event_type": "factor_registry_preview",
                        "output_json": {
                            "factors": [{
                                "factor_name": "blocked_unselected_factor",
                                "blockers": ["missing_runtime_contract"]
                            }]
                        }
                    }]
                }]
            }),
            serde_json::json!({}),
        );
        input.factor_registry_summary = serde_json::json!({
            "runtime_ready_candidates": [{
                "factor_name": "ready_factor",
                "status": "candidate",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "blockers": [],
                "runtime_contract": {
                    "version": "autofactor_runtime_contract_v1",
                    "runtime_score": "autofactor_formula:ready_factor",
                    "strategy_profile": "settlement_probability",
                    "blockers": []
                }
            }]
        });

        let plan = plan_next_research(&input).expect("plan");

        assert_eq!(plan.theme, "candidate_to_runtime_replay");
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "runtime_contract"
                && item.action == "repair_runtime_contract_mapping"
        }));
    }

    #[test]
    fn planner_turns_runtime_replay_trade_count_blocker_into_typed_prior_action() {
        let plan = plan_next_research(&input(
            "executable_replay",
            serde_json::json!({
                "candidate_strategy_replay": {
                    "blocking_risk_flags": ["trade_count_too_small:29<50"]
                }
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "search_power"
                && item.action == "increase_distinct_event_coverage_or_reduce_selectivity"
        }));
    }

    #[test]
    fn planner_routes_latest_negative_runtime_economics_to_revise_prior() {
        let mut input = input(
            "walk_forward",
            serde_json::json!({
                "latest_run": {
                    "promotion_gate": {
                        "blocked_gates": [
                            "walk_forward_oos: no non-naive model has non-empty OOS windows"
                        ]
                    },
                    "candidate_strategy_replay": {
                        "blocking_risk_flags": [
                            "roi_too_low:-0.079091<0.000000",
                            "candidate_strategy_replay_roi_too_low:-0.079091<0.000000"
                        ]
                    }
                }
            }),
            serde_json::json!({}),
        );
        input.rejected_factor_patterns = serde_json::json!({
            "patterns": [{
                "blockers_json": [
                    "official_settlement_missing:48<51",
                    "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
                    "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                ],
                "count": 8
            }]
        });

        let plan = plan_next_research(&input).expect("plan");

        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                && item.action == "mutate_or_reject_negative_runtime_edge"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.action == "repair_official_settlement_coverage"
                || item.action == "collect_full_depth_execution_surface"
                || item.action == "build_runtime_market_update_replay"
        }));
    }

    #[test]
    fn planner_routes_fresh_runtime_ready_candidate_ahead_of_old_negative_replay() {
        let mut input = input("walk_forward", serde_json::json!({}), serde_json::json!({}));
        input.factor_registry_summary = serde_json::json!({
            "recent_candidate_replays": [{
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:old_negative",
                "promotion_ready": false,
                "promotion_decision": "blocked",
                "metrics": {
                    "roi": -1.0,
                    "total_pnl": -15.0,
                    "trade_count": 1,
                    "unique_event_count": 1,
                    "entry_fill_rate": 1.0
                },
                "blocking_risk_flags": [
                    "trade_count_too_small:1<50",
                    "roi_too_low:-1.000000<0.000000"
                ]
            }],
            "runtime_ready_candidates": [
                {
                    "factor_name": "old_negative",
                    "status": "candidate",
                    "target": "full_depth_settlement_executable_pnl",
                    "horizon": "5m",
                    "blockers": [],
                    "runtime_contract": {
                        "version": "autofactor_runtime_contract_v1",
                        "runtime_score": "autofactor_formula:old_negative",
                        "strategy_profile": "settlement_probability",
                        "blockers": []
                    }
                },
                {
                    "factor_name": "fresh_candidate",
                    "status": "candidate",
                    "target": "full_depth_settlement_executable_pnl",
                    "horizon": "5m",
                    "blockers": [],
                    "runtime_contract": {
                        "version": "autofactor_runtime_contract_v1",
                        "runtime_score": "autofactor_formula:fresh_candidate",
                        "strategy_profile": "settlement_probability",
                        "blockers": []
                    }
                }
            ]
        });

        let plan = plan_next_research(&input).expect("plan");

        assert_eq!(plan.theme, "candidate_to_runtime_replay");
        assert!(plan
            .actions
            .contains(&"build_runtime_candidate_replay".to_string()));
        assert!(!plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                && item.action == "mutate_or_reject_negative_runtime_edge"
        }));
    }

    #[test]
    fn planner_uses_frontier_run_decision_not_historical_or_unselected_factor_blockers() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({
                "source": "experiment_trace",
                "runs": [
                    {
                        "run_id": "newest",
                        "artifacts": [{
                            "event_type": "autofactor_promotion",
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "blockers": [
                                        "roi_too_low:-0.079091<0.000000"
                                    ],
                                    "decision_contract": {
                                        "official_settlement": true,
                                        "full_depth_entry": true,
                                        "one_decision_per_event": true
                                    }
                                },
                                "promotion_gate": {
                                    "blocked_gates": ["walk_forward_oos"]
                                },
                                "evaluated_factors": [{
                                    "blockers": [
                                        "official_settlement_missing:48<51",
                                        "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
                                        "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                                        "missing_runtime_contract"
                                    ]
                                }]
                            }
                        }]
                    },
                    {
                        "run_id": "older",
                        "artifacts": [{
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "blockers": [
                                        "official_settlement_missing:48<51",
                                        "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
                                        "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                                    ]
                                }
                            }
                        }]
                    }
                ]
            }),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {
                        "surface": "clob_orderbook_snapshots",
                        "reason": "required_execution_surface_is_sampled_snapshot"
                    },
                    {
                        "surface": "pm_token_settlements",
                        "reason": "required_execution_surface_not_materialized"
                    }
                ]
            }),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                && item.action == "mutate_or_reject_negative_runtime_edge"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family.starts_with("data_")
                || item.blocker_family == "runtime_replay"
                || item.blocker_family == "runtime_contract"
        }));
    }

    #[test]
    fn planner_turns_runtime_replay_data_blockers_into_data_actions() {
        let plan = plan_next_research(&input(
            "executable_replay",
            serde_json::json!({
                "candidate_strategy_replay": {
                    "blocking_risk_flags": [
                        "official_settlement_missing:25<29",
                        "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots"
                    ]
                }
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "fix_data");
        assert!(plan
            .actions
            .contains(&"repair_official_settlement_coverage".to_string()));
        assert!(plan
            .actions
            .contains(&"collect_full_depth_execution_surface".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_execution_surface"
                && item.action == "collect_full_depth_execution_surface"
        }));
    }

    #[test]
    fn planner_does_not_treat_positive_runtime_replay_with_data_blockers_as_negative_edge() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({
                "source": "experiment_trace",
                "runs": [{
                    "run_id": "26365232589",
                    "artifacts": [{
                        "event_type": "autofactor_promotion",
                        "output_json": {
                            "candidate_strategy_replay": {
                                "basis": "runtime_market_update_replay",
                                "blocking_risk_flags": [
                                    "trade_count_too_small:45<50",
                                    "official_settlement_missing:42<45"
                                ],
                                "decision_contract": {
                                    "official_settlement": false,
                                    "full_depth_entry": true,
                                    "one_decision_per_event": true
                                },
                                "metrics": {
                                    "roi": 0.11810712860376293,
                                    "total_pnl": 79.72231180753998,
                                    "trade_count": 45,
                                    "unique_event_count": 45
                                },
                                "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
                            },
                            "promotion_gate": {
                                "blocked_gates": ["walk_forward_oos"]
                            },
                            "input_prior": {
                                "runtime_avoid_factors": [{
                                    "reason": "negative_runtime_edge"
                                }]
                            }
                        }
                    }]
                }]
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "fix_data");
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "search_power"
                && item.action == "increase_distinct_event_coverage_or_reduce_selectivity"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                || item.action == "mutate_or_reject_negative_runtime_edge"
        }));
    }

    #[test]
    fn planner_surfaces_snapshot_settlement_promotion_blocker_without_forcing_data_repair() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({}),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {
                        "surface": "pm_token_settlements",
                        "reason": "required_execution_surface_not_materialized"
                    }
                ]
            }),
        ))
        .expect("plan");

        assert_ne!(plan.theme, "fix_data");
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "promotion_data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
    }

    #[test]
    fn planner_revises_prior_when_stale_full_depth_blocker_is_already_materialized() {
        let mut input = input(
            "walk_forward",
            serde_json::json!({
                "source": "experiment_trace",
                "runs": [{
                    "run_id": "26542589633",
                    "artifacts": [{
                        "event_type": "autofactor_promotion",
                        "output_json": {
                            "candidate_strategy_replay": {
                                "basis": "factor_walk_forward_top_bucket_aggregate",
                                "blockers": [
                                    "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
                                    "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                                ],
                                "decision_contract": {
                                    "official_settlement": true,
                                    "full_depth_entry": true,
                                    "one_decision_per_event": true
                                }
                            },
                            "promotion_gate": {
                                "blocked_gates": ["walk_forward_oos"]
                            }
                        }
                    }]
                }]
            }),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "execution_surfaces": {
                    "surfaces": [{
                        "surface": "clob_orderbook_snapshots",
                        "full_fidelity": true,
                        "incomplete": false,
                        "row_count": 1443746
                    }]
                },
                "settlement_surfaces": {
                    "surfaces": []
                },
                "promotion_blockers": [
                    {
                        "surface": "pm_token_settlements",
                        "reason": "required_execution_surface_not_materialized"
                    }
                ]
            }),
        );
        input.factor_registry_summary = serde_json::json!({
            "recent_candidate_replays": [{
                "candidate_replay_id": "candidate_replay:negative",
                "run_id": "26542589633",
                "workflow_run_id": "26528933436",
                "basis": "runtime_market_update_replay",
                "promotion_ready": false,
                "promotion_decision": "blocked",
                "strategy_profile": "settlement_probability",
                "runtime_score": "autofactor_formula:auto_settlement_model_full_depth_settlement_edge_spread_adjusted",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "metrics": {
                    "roi": -0.013906620311657932,
                    "total_pnl": -30.246899177856,
                    "trade_count": 145,
                    "unique_event_count": 145,
                    "entry_fill_rate": 1.0
                },
                "blocking_risk_flags": [
                    "official_settlement_missing:142<145",
                    "roi_too_low:-0.013907<0.000000"
                ]
            }]
        });

        let plan = plan_next_research(&input).expect("plan");

        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(!plan
            .actions
            .contains(&"collect_full_depth_execution_surface".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "promotion_data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "strategy_economics"
                && item.action == "mutate_or_reject_negative_runtime_edge"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_execution_surface"
                && item.action == "collect_full_depth_execution_surface"
        }));
    }

    #[test]
    fn planner_ignores_materialized_settlement_surface_metadata_without_blocker() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({
                "candidate_strategy_replay": {
                    "blocking_risk_flags": ["missing_runtime_contract"]
                }
            }),
            serde_json::json!({
                "missing_blocks_promotion": false,
                "critical_missing": false,
                "surface_blockers": [],
                "promotion_blockers": [],
                "data_repair_blockers": [],
                "source_surfaces": [
                    {
                        "name": "pm_token_settlements",
                        "row_count": null,
                        "role": "settlement_labels"
                    }
                ],
                "settlement_surfaces": {
                    "source": "official_settlement_coverage_checks",
                    "surfaces": [
                        {
                            "surface": "pm_token_settlements",
                            "valid": true,
                            "blockers": [],
                            "settlement_token_count": 2194
                        }
                    ]
                }
            }),
        ))
        .expect("plan");

        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "runtime_contract"
                && item.action == "repair_runtime_contract_mapping"
        }));
        assert!(!plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "promotion_data_settlement"
                || item.action == "repair_official_settlement_coverage"
        }));
    }
}
