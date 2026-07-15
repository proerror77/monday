pub mod alpha_search;
pub mod attribution;
pub mod autofactor;
pub mod backtest;
pub mod backtesting;
pub mod dataset;
#[cfg(feature = "db")]
pub mod deribit;
pub mod event_ml;
pub mod factors;
pub mod factors_new;
pub mod factors_v2;
#[cfg(any(feature = "ml", feature = "rl"))]
pub mod model;
pub mod orderbook;
pub mod prediction_llm;
pub mod prediction_loop;
mod prediction_loop_fs;
pub mod replay;
pub mod research_os;
pub mod research_snapshot;
pub mod signal;

pub use backtesting::{run_backtest, BacktestReport};
pub use dataset::{
    assign_chronological_event_splits, build_canonical_event_chronology, build_event_root_dataset,
    standard_event_root_dataset_artifacts, ChronologyAnchor, ChronologyOrdering, DatasetArtifacts,
    DatasetBuildError, DatasetBuildManifest, DatasetBuildStats, DatasetLabelContract,
    DatasetSkipCounts, DatasetSourceWindow, DatasetSplit, DatasetSplitArtifactPaths,
    DatasetSplitAssignment, DatasetSplitCounts, DatasetSplitDerivedArtifacts, DatasetSplitPolicy,
    EventChronologyBuild, EventChronologyKey, EventIndexEntry, EventMetadataChronologyInput,
    EventRootDatasetBuild, EventRootDatasetBuildRequest, SplitBuildError, CANONICAL_REGIME_VERSION,
    DATASET_MANIFEST_VERSION, REPRICING_LABEL_30S, SETTLEMENT_LABEL,
};
#[cfg(feature = "polars-export")]
pub use dataset::{
    event_index_to_frame, event_summaries_to_frame, export_event_root_dataset_parquet,
    split_assignments_to_frame, DatasetExportError,
};
#[cfg(feature = "db")]
pub use deribit::{
    load_deribit_feature_snapshots, load_deribit_feature_snapshots_with_timings,
    DeribitFeatureLoadResult,
};
pub use event_ml::{
    build_event_ml_strategy_handoff, build_walk_forward_report, canonical_event_ml_architecture,
    event_ml_architecture_markdown, event_ml_strategy_handoff_markdown, gate_matrix,
    walk_forward_report_markdown, ArchitectureArtifact, EventMlArchitecture,
    EventMlStrategyCandidate, EventMlStrategyHandoff, EventMlStrategyHandoffConfig,
    EventMlStrategyHandoffStatus, EventMlStrategyPromotionGate, LaneReadiness, LaneStatus,
    LearningLane, LearningLaneId, PhaseId, ReadinessGate, WalkForwardAggregate, WalkForwardConfig,
    WalkForwardGate, WalkForwardGateStatus, WalkForwardMetric, WalkForwardReadiness,
    WalkForwardReport, WalkForwardWindow, WorkflowPhase, EVENT_ML_ARCHITECTURE_VERSION,
    EVENT_ML_STRATEGY_HANDOFF_VERSION, WALK_FORWARD_REPORT_VERSION,
};
pub use factors::{
    aggregate_factor_metrics, build_event_summaries, build_factor_observations,
    build_factor_observations_with_lob, build_factor_observations_with_lob_sampled,
    build_factor_observations_with_lob_sampled_and_oracle_evidence, factor_metrics,
    normalized_underlying_symbol, AggregatedFactorMetric, ChainlinkOracleAuditFailure,
    ChainlinkOracleBoundaryEvidence, ChainlinkOracleFailureReason, ChainlinkOracleSettlementAudit,
    ChainlinkOracleSettlementEvidence, EventFactorSummary, FactorMetric, FactorObservation,
    FactorObservationBuild, ResearchLobSnapshot, ResearchPmBookLevel, ResearchPmBookSnapshot,
    GOVERNED_CHAINLINK_BOUNDARY_MAX_AGE_SECS, GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION,
};
#[cfg(feature = "polars-export")]
pub use factors::{export_observations_parquet, observations_to_frame};
#[cfg(feature = "db")]
pub use factors::{
    load_research_lob_snapshots, load_research_lob_snapshots_sampled,
    load_research_pm_book_snapshots_sampled,
};
pub use replay::replay_fills;
#[cfg(feature = "db")]
pub use research_snapshot::{build_research_snapshot_from_database, ResearchSnapshotBuildOptions};
pub use research_snapshot::{
    load_research_snapshot, validate_governed_chainlink_5m_settlement_audit,
    validate_governed_chainlink_5m_settlement_evidence, validate_snapshot_request,
    validate_snapshot_request_coverage, write_research_snapshot, ResearchSnapshot,
    ResearchSnapshotArtifacts, ResearchSnapshotManifest, ResearchSnapshotPhaseTiming,
    ResearchSnapshotPmBookSource, ResearchSnapshotRequest, ResearchSnapshotRowCounts,
    RESEARCH_SNAPSHOT_SCHEMA_VERSION,
};

pub const CRATE_MARKER: &str = "ploy-research";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}

// New layered pipeline exports.
//
// Keep `Regime` as a single root-level export from operator contracts. The
// factor registry uses the same type internally, but does not re-export its own
// `factors_new::Regime` alias.
pub use alpha_search::{
    read_mcts_search_state, root_gene, write_alpha_search_artifacts,
    write_alpha_search_artifacts_with_state,
    write_alpha_search_artifacts_with_state_and_runtime_feedback, AlphaSearchArtifactError,
    AlphaSearchArtifactSummary, AlphaSearchRuntimeFeedback, AlphaZooEntry, AlphaZooSnapshot,
    MctsSearchStateArtifact, MctsSearchStateNode,
};
pub use attribution::{factor_pnl, regime_pnl, AttributionReport, RegimePnl};
pub use autofactor::{
    autofactor_labels_from_v2, autofactor_matrix_from_v2, autofactor_runtime_contract_catalog,
    autofactor_target_contract, autofactor_target_horizon, autofactor_windows_from_v2,
    domain_seed_candidates, evaluate_named_factor, format_autofactor_reports, mine_autofactors,
    mine_domain_autofactors_from_v2, mine_domain_autofactors_from_v2_with_guidance,
    mine_domain_autofactors_from_v2_with_mcts_plan, AutoFactorDecision, AutoFactorError,
    AutoFactorMatrix, AutoFactorOptions, AutoFactorReport, AutoFactorRuntimeContractCatalog,
    AutoFactorRuntimeFormulaBlocker, AutoFactorRuntimeInputContract, AutoFactorTargetContract,
    AutoFactorV2Target, FactorExpr, LlmMutationSpec, LlmPriorSpec, LlmProbabilityBlendSpec,
    NamedFactorExpr,
};
pub use backtest::{run_binary_backtest, BacktestMetrics, SimulatedFill};
pub use factors_new::{
    register_automl_attributions, scan_into_registry, AutomlFactorAttribution, FactorMeta,
    FactorRegistry,
};
pub use factors_v2::{
    build_data_health_report, build_factor_observations_v2,
    build_factor_observations_v2_with_deribit,
    build_factor_observations_v2_with_deribit_and_pm_books, build_factor_stability_report,
    build_full_depth_execution_matrix, build_prediction_research_feedback,
    build_settlement_probability_promotion_gate_report, build_settlement_probability_report,
    build_settlement_probability_report_with_prior, factor_v2_descriptors,
    format_factor_combo_v1_report, format_factor_review_v2_report, format_factor_stability_report,
    format_factor_walk_forward_v2_report, format_fillability_review_v1_report,
    format_full_depth_execution_matrix_report, format_liquidity_gate_v1_report,
    format_liquidity_gated_alpha_v1_report, format_meta_label_walk_forward_v1_report,
    format_repricing_ic_report, format_settlement_probability_promotion_gate_report,
    format_settlement_probability_report, format_settlement_probability_walk_forward_report,
    format_trade_formation_v1_report, liquidity_gate_v1_with_deribit,
    liquidity_gate_v1_with_deribit_and_pm_books, liquidity_gated_alpha_v1_with_deribit,
    liquidity_gated_alpha_v1_with_deribit_and_pm_books, recompute_prediction_research_outcome,
    review_factors_v2, review_factors_v2_with_deribit, review_factors_v2_with_deribit_and_pm_books,
    review_factors_v2_with_deribit_and_pm_books_filtered, review_fillability_v1_with_deribit,
    review_fillability_v1_with_deribit_and_pm_books, review_repricing_ic_with_deribit,
    review_repricing_ic_with_deribit_and_pm_books, review_trade_formation_v1_with_deribit,
    review_trade_formation_v1_with_deribit_and_pm_books, validate_prediction_research_prior,
    walk_forward_factor_combo_v1_with_deribit,
    walk_forward_factor_combo_v1_with_deribit_and_pm_books, walk_forward_factors_v2_with_deribit,
    walk_forward_factors_v2_with_deribit_and_pm_books, walk_forward_meta_label_v1_with_deribit,
    walk_forward_meta_label_v1_with_deribit_and_pm_books,
    walk_forward_settlement_probability_report,
    walk_forward_settlement_probability_report_with_prior, BinanceDirectionBucketSummary,
    DataHealthReport, DeribitFeatureSnapshot, DirectionSideAuditLegSummary,
    DirectionSideAuditSummary, ExecutableEvBucketSummary, FactorComboComponent,
    FactorComboV1Aggregate, FactorComboV1Options, FactorComboV1Report, FactorComboV1Window,
    FactorFamily, FactorObservationV2, FactorReviewOptions, FactorReviewV2Report,
    FactorSelectionMetrics, FactorStabilityDecision, FactorStabilityOptions, FactorStabilityReport,
    FactorStabilityRow, FactorV2Descriptor, FactorWalkForwardAggregate, FactorWalkForwardOptions,
    FactorWalkForwardReport, FactorWalkForwardWindow, FillabilityBucketRow, FillabilityDecision,
    FillabilityReviewOptions, FillabilityReviewReport, FullDepthExecutionMatrixOptions,
    FullDepthExecutionMatrixReport, FullDepthExecutionMatrixRow, LiquidityGateV1Options,
    LiquidityGateV1Report, LiquidityGatedAlphaV1Options, LiquidityGatedAlphaV1Report,
    MetaLabelWalkForwardAggregate, MetaLabelWalkForwardOptions, MetaLabelWalkForwardReport,
    MetaLabelWalkForwardWindow, PredictionResearchCandidateFeedback,
    PredictionResearchCandidateMetrics, PredictionResearchFeedback, PredictionResearchGatePolicy,
    RepricingIcOptions, RepricingIcReport, RepricingIcRow, ReviewSide,
    SettlementProbabilityAblationRow, SettlementProbabilityAntiOverfitRow,
    SettlementProbabilityBaselineRow, SettlementProbabilityCalibrationRow,
    SettlementProbabilityDataQualityMode, SettlementProbabilityEdgeBucketRow,
    SettlementProbabilityPromotionGateOptions, SettlementProbabilityPromotionGateReport,
    SettlementProbabilityPromotionGateRow, SettlementProbabilityReport,
    SettlementProbabilityReportOptions, SettlementProbabilitySymbolHoldoutRow,
    SettlementProbabilityWalkForwardAggregate, SettlementProbabilityWalkForwardOptions,
    SettlementProbabilityWalkForwardReport, SettlementProbabilityWalkForwardWindow,
    SingleFactorReview, ThreeLayerArchive, TradeFormationPathRow, TradeFormationReviewOptions,
    TradeFormationReviewReport, TradeFormationRuleRow,
};
#[cfg(feature = "rl")]
pub use model::rl::{BinaryEventEnv, DqnAgent, Environment, ReplayBuffer};
#[cfg(feature = "ml")]
pub use model::supervised::{
    train_event_disjoint_binary, BinaryArtifactScope, BinaryBundleDigest, BinaryDatasetContract,
    BinaryDecisionRow, BinaryModelManifest, BinaryOosMetrics, BinaryProbabilityModel,
    BinarySettlementAuthority, BinaryTrainingConfig, EventDisjointBinarySplit, FeatureNormalizer,
};
#[cfg(any(feature = "ml", feature = "rl"))]
pub use model::{RlAgent, StrategyModel, Transition};
pub use ploy_operator_contracts::Regime;
pub use signal::{RegimeRouter, Signal, SignalSource, ThresholdRule};
