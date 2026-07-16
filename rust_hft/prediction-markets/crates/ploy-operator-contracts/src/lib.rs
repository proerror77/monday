pub mod audit;
pub mod deployments;
pub mod diagnostics;
pub mod errors;
pub mod events;
pub mod reports;
pub mod schemas;
pub mod system;
pub mod trading;

pub use audit::AuditLogEntry;
pub use deployments::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentRuntimeMode, DeploymentState,
    DeploymentStateSummary, DeploymentSummary, DesiredState, ObservedState,
};
pub use diagnostics::{
    agent_run_contract_value, compute_oversight_report, validate_agent_run_contract,
    validate_agent_run_create_request, AgentRunCreateRequest, AgentRunCreateResponse,
    AgentRunRecord, AgentToolCallRecord, DeploymentDiagnosticsMetrics, DeploymentDiagnosticsReport,
    DiagnosticsEvidence, DiagnosticsFinding, OversightRecommendedAction, OversightReport,
    OversightSignal, OversightSnapshotEvent, PlatformDiagnosticsReport, ProposalActionKind,
    ProposalCreateRequest, ProposalDecisionRequest, ProposalSnapshotEvent, ProposalStatus,
    SafetyProposal,
};
pub use errors::ControlPlaneErrorResponse;
pub use events::{
    AlertSnapshotEvent, DeploymentSnapshotEvent, LogEntry, MetricsSnapshotEvent, OperatorEvent,
    StatusUpdate, SystemSnapshotEvent, TradingSnapshotEvent, WsMessage,
};
pub use reports::{
    DryRunClosedTradeRow, DryRunDailyRow, DryRunDailyWindowRow, DryRunEquityPoint,
    DryRunExecutionDiagnostics, DryRunMetrics, DryRunOpenPositionRow, DryRunPairingReport,
    DryRunPerformanceReport, DryRunRuntimeEvidence, DryRunStrategyReport, DryRunSummary,
    DryRunSymbolRow, DryRunWindowRow, NumberOrText,
};
pub use system::{
    ActiveAlert, AlertKind, AlertSeverity, HeartbeatState, HeartbeatStatus, PlatformMetrics,
    SystemControlResponse, SystemStatus,
};
pub use trading::{
    FillSnapshot, IntentPurpose, MarketData, OrderControlResponse, OrderReplaceRequest,
    OrderSnapshot, PaperIntentRequest, PaperIntentResponse, PnlSnapshotResponse, PositionResponse,
    PositionSnapshotResponse, Regime, RiskSnapshotResponse, TradeResponse, TradingIntentSnapshot,
    TradingStateSnapshot,
};

pub const CRATE_MARKER: &str = "ploy-operator-contracts";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
