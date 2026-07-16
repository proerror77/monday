export interface TodayStats {
  total_trades: number;
  successful_trades: number;
  failed_trades: number;
  total_volume: number;
  pnl: number;
  win_rate: number;
  avg_trade_time_ms: number;
  active_positions: number;
}

export interface HarnessMemoryEvent {
  kind?: string;
  run_id?: string;
  cycle_kind?: string;
  category?: string;
  summary?: string;
  suggested_change?: string;
  subagent_profile?: string;
  created_at?: string;
  [key: string]: unknown;
}

export interface HarnessMemoryResponse {
  context: string;
  events: HarnessMemoryEvent[];
  event_count: number;
  updated_at: string;
}

export type {
  ActiveAlert,
  AgentRunCreateRequest,
  AgentRunCreateResponse,
  AgentRunRecord,
  AgentToolCallRecord,
  AlertKind,
  AlertSeverity,
  AuditLogEntry,
  ControlPlaneErrorResponse,
  DeploymentApplyRequest,
  DeploymentControlRequest,
  DeploymentState,
  DeploymentSummary,
  DesiredState,
  DryRunClosedTradeRow,
  DryRunDailyRow,
  DryRunDailyWindowRow,
  DryRunEquityPoint,
  DryRunExecutionDiagnostics,
  DryRunMetrics,
  DryRunOpenPositionRow,
  DryRunPairingReport,
  DryRunPerformanceReport,
  DryRunStrategyReport,
  DryRunSummary,
  DryRunSymbolRow,
  DryRunWindowRow,
  HeartbeatState,
  HeartbeatStatus,
  IntentPurpose,
  JsonValue,
  LogEntry,
  MarketData,
  ObservedState,
  OperatorEvent,
  PlatformMetrics,
  PositionResponse,
  PnlSnapshotResponse as TradingPnlSnapshot,
  RiskSnapshotResponse as TradingRiskSnapshot,
  StatusUpdate,
  SystemControlResponse,
  SystemStatus,
  TradeResponse,
  TradingStateSnapshot,
} from './operator-contracts';

import type {
  DeploymentControlRequest as DeploymentControlRequestContract,
  JsonValue,
  OperatorEvent as OperatorEventContract,
  PositionResponse as PositionResponseContract,
  TradeResponse as TradeResponseContract,
} from './operator-contracts';

export type Trade = TradeResponseContract;
export type Position = PositionResponseContract;
export type UpdateDeploymentStateRequest = DeploymentControlRequestContract;
export type WsMessage = OperatorEventContract;

export interface StrategyConfig {
  symbols: string[];
  min_move: number;
  max_entry: number;
  shares: number;
  predictive: boolean;
  exit_edge_floor?: number | null;
  exit_price_band?: number | null;
  time_decay_exit_secs?: number | null;
  liquidity_exit_spread_bps?: number | null;
}

export interface SecurityEvent {
  id: string;
  timestamp: string;
  event_type: 'DUPLICATE_ORDER' | 'VERSION_CONFLICT' | 'STALE_QUOTE' | 'NONCE_RECOVERY';
  severity: 'LOW' | 'MEDIUM' | 'HIGH' | 'CRITICAL';
  details: string;
  metadata?: Record<string, JsonValue>;
}

export interface DeploymentStateSummary {
  enabled: number;
  draining: number;
  disabled: number;
  archived: number;
}

export interface PnLDataPoint {
  timestamp: string;
  cumulative_pnl: number;
  trade_count: number;
}

export interface RunningStrategy {
  name: string;
  status: 'running' | 'paused' | 'error';
  pnl_usd: number;
  order_count: number;
  domain: 'crypto' | 'sports' | 'politics';
}

export interface RiskData {
  risk_state: 'Normal' | 'Elevated' | 'Halted';
  daily_pnl_usd: number;
  daily_loss_limit_usd: number;
  queue_depth: number;
  positions: Array<{
    market: string;
    side: 'Yes' | 'No';
    size: number;
    pnl_usd: number;
  }>;
  circuit_breaker_events: Array<{
    timestamp: string;
    reason: string;
    state: string;
  }>;
}

export interface MarketDataHealthSource {
  source_id: string;
  table_name: string;
  latest_at: string | null;
  stale_after_seconds: number;
  approx_rows: number;
}

export interface DeribitIvSample {
  currency: string;
  instrument_name: string;
  mark_iv: string | number | null;
  bid_iv: string | number | null;
  ask_iv: string | number | null;
  underlying_price: string | number | null;
  fetched_at: string;
}

export interface DeribitGreeksSample {
  currency: string;
  instrument_name: string;
  mark_iv: string | number | null;
  delta: string | number | null;
  gamma: string | number | null;
  vega: string | number | null;
  theta: string | number | null;
  underlying_price: string | number | null;
  fetched_at: string;
}

export interface MarketDataHealth {
  generated_at: string;
  sources: MarketDataHealthSource[];
  deribit_iv_samples: DeribitIvSample[];
  deribit_greeks_samples: DeribitGreeksSample[];
}
