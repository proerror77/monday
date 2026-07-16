import type { AgentRunCreateRequest } from '@/types';

export type StrategyFamily =
  | 'pm5d'
  | 'sports'
  | 'grok-builder'
  | 'market-making'
  | 'copy-trading';
export type EvidenceTarget = 'diagnostic' | 'factor_attribution' | 'executable_replay' | 'dry_run_candidate';
export type AutonomyMode = 'research_until_blocked' | 'paper_candidate' | 'monitor_only';
export type ArtifactKey = 'packet' | 'issue' | 'contract';
export type StepState = 'active' | 'ready' | 'blocked' | 'queued';

export interface BuilderForm {
  objective: string;
  family: StrategyFamily;
  symbols: string;
  target: EvidenceTarget;
  autonomy: AutonomyMode;
  budgetUsd: number;
  maxTurns: number;
}

export interface AgentStep {
  id: string;
  title: string;
  owner: string;
  detail: string;
  state: StepState;
  tools: string[];
}

export interface ToolCapability {
  action: string;
  tool: string;
  status: 'wired' | 'approval';
}

export const defaultObjective =
  'Find a PM5D BTC/ETH settlement-probability strategy that can move from diagnostic research to executable replay. Use Binance momentum, Polymarket quotes, full-depth CLOB fillability, and official settlement labels. Stop before dry-run unless every gate is proven.';

export const familyLabels: Record<StrategyFamily, string> = {
  pm5d: 'PM5D / binary options',
  sports: 'Sports markets',
  'grok-builder': 'Grok Builder / NBA comeback',
  'market-making': 'Market making',
  'copy-trading': 'Copy trading',
};

export const targetLabels: Record<EvidenceTarget, string> = {
  diagnostic: 'Diagnostic',
  factor_attribution: 'Factor attribution',
  executable_replay: 'Executable replay',
  dry_run_candidate: 'Dry-run candidate',
};

export const autonomyLabels: Record<AutonomyMode, string> = {
  research_until_blocked: '自动研究到 blocker',
  paper_candidate: '自动准备 paper candidate',
  monitor_only: '只监控并建议',
};

export const strategyProfiles: Record<StrategyFamily, string> = {
  pm5d: 'pm5d.settlement_probability.agent',
  sports: 'sports.event_edge.agent',
  'grok-builder': 'sports.nba_comeback.grok_builder.agent',
  'market-making': 'prediction_market_maker.agent',
  'copy-trading': 'copy_signal_replay.agent',
};

export const dataSurfaces: Record<StrategyFamily, string[]> = {
  pm5d: [
    'Binance spot/trade ticks',
    'Binance L2 / LOB',
    'Polymarket quote ticks',
    'Polymarket full CLOB depth',
    'Official settlement labels',
  ],
  sports: [
    'Official game state',
    'Polymarket quote ticks',
    'Injury/news checks',
    'Market depth and stale quote checks',
    'Official settlement labels',
  ],
  'grok-builder': [
    'ESPN live scoreboard and game details',
    'Polymarket sports market search and snapshots',
    'X.com / Grok-style injury, momentum, and sentiment checks',
    'Reward-to-risk and EV calculations',
    'Paper-only operator action review',
  ],
  'market-making': [
    'Polymarket full CLOB depth',
    'Quote freshness',
    'Inventory ledger',
    'Fill/reject history',
    'Queue diagnostics',
  ],
  'copy-trading': [
    'Source signal ledger',
    'Target market quote ticks',
    'Full-depth execution surface',
    'Latency and slippage audit',
    'Fill/reject history',
  ],
};

export const capabilityMap: ToolCapability[] = [
  { action: '读取平台状态', tool: 'get_system_status / get_trading_state / list_deployments', status: 'wired' },
  { action: '读取赛事状态', tool: 'scoreboard / game_details', status: 'wired' },
  { action: '发现市场', tool: 'search_markets / market_snapshot', status: 'wired' },
  { action: 'Grok/X 证据检查', tool: 'WebSearch / WebFetch', status: 'wired' },
  { action: '运行研究回放', tool: 'replay_deployment / run_backtest / compare_configs', status: 'wired' },
  { action: '风险与治理检查', tool: 'check_oversight', status: 'wired' },
  { action: '提交 paper intent', tool: 'submit_paper_intent', status: 'approval' },
  { action: 'live 部署变更', tool: 'apply_deployment', status: 'approval' },
];

export const gateOrder = [
  'hypothesis',
  'data audit',
  'factor attribution',
  'executable replay',
  'runtime parity',
  'dry-run approval',
  'live approval',
];

export function compactSymbols(value: string) {
  return value
    .split(',')
    .map((item) => item.trim().toUpperCase())
    .filter(Boolean);
}

export function slugify(value: string) {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 46);
}

export function targetGateIndex(target: EvidenceTarget) {
  if (target === 'diagnostic') return 1;
  if (target === 'factor_attribution') return 2;
  if (target === 'executable_replay') return 3;
  return 5;
}

function grokBuilderSection(form: BuilderForm) {
  if (form.family !== 'grok-builder') return '';

  return `
Grok Builder rules:
- Treat Grok/X evidence as decision support, not execution authority
- Use ESPN live state before any market or sentiment step
- Search X.com/web for injury, rotation, momentum, and betting sentiment context
- Emit an explicit grok_decision value: trade, pass, or not_queried
- Default to PASS or MONITOR when Grok/X evidence is stale, unavailable, or contradictory
- Paper intent remains approval-gated; live orders remain blocked`;
}

export function buildAgentSteps(form: BuilderForm): AgentStep[] {
  const executableRequested =
    form.target === 'executable_replay' || form.target === 'dry_run_candidate';
  const dryRunRequested = form.target === 'dry_run_candidate';
  const grokRequested = form.family === 'grok-builder';

  return [
    {
      id: 'intake',
      title: 'Objective intake',
      owner: 'Orchestrator',
      detail: '目标、预算、停止条件',
      state: form.objective.trim() ? 'ready' : 'blocked',
      tools: ['get_system_status', 'get_trading_state', 'list_deployments'],
    },
    {
      id: 'data',
      title: grokRequested ? 'Grok evidence scout' : 'Data scout',
      owner: grokRequested ? 'Grok Builder agent' : 'Research agent',
      detail: grokRequested ? 'ESPN、Polymarket、X/Grok 证据' : '数据面与缺口识别',
      state: 'active',
      tools: grokRequested ? ['scoreboard', 'search_markets', 'WebSearch'] : ['search_markets'],
    },
    {
      id: 'factor',
      title: 'Factor loop',
      owner: 'Analysis agent',
      detail: targetLabels[form.target],
      state: form.target === 'diagnostic' ? 'queued' : 'active',
      tools: ['run_backtest', 'compare_configs'],
    },
    {
      id: 'replay',
      title: 'Executable replay',
      owner: 'Replay agent',
      detail: '价格、深度、成交假设',
      state: executableRequested ? 'active' : 'blocked',
      tools: ['replay_deployment'],
    },
    {
      id: 'risk',
      title: 'Risk officer',
      owner: 'Control agent',
      detail: '仓位、kill switch、parity',
      state: dryRunRequested ? 'active' : 'queued',
      tools: ['check_oversight', 'compare_configs'],
    },
    {
      id: 'complete',
      title: 'Completion signal',
      owner: 'Orchestrator',
      detail: 'success / blocked / partial',
      state: dryRunRequested ? 'queued' : 'ready',
      tools: ['complete_task'],
    },
  ];
}

export function buildAutomationPacket(form: BuilderForm) {
  const symbols = compactSymbols(form.symbols);
  const profile = strategyProfiles[form.family];
  const surfaces = dataSurfaces[form.family];

  return `# Agentic Strategy Run Packet

objective_slug: ${slugify(form.objective) || 'agentic-strategy-run'}
strategy_profile: ${profile}
autonomy_mode: ${form.autonomy}
target_evidence: ${form.target}
symbols: ${symbols.join(', ') || 'TBD'}
max_turns: ${form.maxTurns}
budget_usd: ${form.budgetUsd.toFixed(2)}

Outcome:
${form.objective.trim()}

Agent loop:
- Orchestrator reads platform context and defines stop conditions
- Data scout verifies market/data surfaces and missing blockers
- Research agent runs diagnostics, replay, or config comparison through sidecar tools
- Replay agent validates executable price, CLOB depth, and settlement accounting
- Risk officer checks parity, stake limits, kill switches, and promotion blockers
- Orchestrator must finish with success, partial, or blocked

Required data:
${surfaces.map((surface) => `- ${surface}`).join('\n')}

Tool/action parity:
${capabilityMap.map((item) => `- ${item.action}: ${item.tool} (${item.status})`).join('\n')}
${grokBuilderSection(form)}

Promotion policy:
- diagnostic and factor_attribution never deploy
- executable_replay can only produce a research handoff
- dry_run_candidate requires replay, runtime parity, risk limits, and operator approval
- live remains blocked until explicit operator approval and deployment guardrails pass`;
}

export function buildResearchIssue(form: BuilderForm) {
  const symbols = compactSymbols(form.symbols);
  return `# Research: ${slugify(form.objective) || 'agentic-strategy-run'}

created_by: strategy_builder.agentic_ui
target_evidence: ${form.target}
autonomy_mode: ${form.autonomy}

Hypothesis:
${form.objective.trim()}

Scope:
- profile: ${strategyProfiles[form.family]}
- symbols: ${symbols.join(', ') || 'TBD'}
- max_turns: ${form.maxTurns}
- budget_usd: ${form.budgetUsd.toFixed(2)}

Required proof:
- Data coverage is explicit
- Grok/X evidence is cited as support only when queried
- Factor attribution or diagnostic result is attached
- Replay/dry-run parity is ready before live consideration
- Operator approval remains required for paper intent and deployment changes

Failure criteria:
- Unsupported runtime inputs
- Missing full-depth execution surface
- Missing official settlement or exit label
- Negative executable replay after fees and slippage

Decision:
continue`;
}

export function buildRunContract(form: BuilderForm) {
  const symbols = compactSymbols(form.symbols);
  const sportsLike = form.family === 'sports' || form.family === 'grok-builder';
  const requiresExecutableReplay =
    form.target === 'executable_replay' || form.target === 'dry_run_candidate';
  const requiresRuntimeParity = form.target === 'dry_run_candidate';
  const requiresFullDepthClob =
    requiresExecutableReplay &&
    (form.family === 'pm5d' || form.family === 'market-making' || form.family === 'copy-trading');
  return `[agentic_strategy_run]
profile = "${strategyProfiles[form.family]}"
autonomy_mode = "${form.autonomy}"
target_evidence = "${form.target}"
symbols = [${symbols.map((symbol) => `"${symbol}"`).join(', ')}]
model_tier = "balanced"
max_turns = ${form.maxTurns}
budget_usd = ${form.budgetUsd.toFixed(2)}
completion_signal = "required"
promotion_ready = false

[agentic_strategy_run.tools]
get_system_status = true
get_trading_state = true
list_deployments = true
search_markets = true
run_backtest = true
replay_deployment = true
compare_configs = true
check_oversight = true
scoreboard = ${sportsLike}
game_details = ${sportsLike}
market_snapshot = ${sportsLike || form.family === 'market-making'}
web_search = ${sportsLike}
submit_paper_intent = "approval_required"
apply_deployment = "approval_required"

[agentic_strategy_run.gates]
requires_data_audit = true
requires_grok_decision = ${form.family === 'grok-builder'}
requires_executable_replay = ${requiresExecutableReplay}
requires_full_depth_clob = ${requiresFullDepthClob}
requires_runtime_parity = ${requiresRuntimeParity}
requires_operator_approval = true`;
}

export function buildAgentRunCreateRequest(form: BuilderForm): AgentRunCreateRequest {
  return {
    objective: form.objective.trim(),
    strategy_profile: strategyProfiles[form.family],
    autonomy_mode: form.autonomy,
    target_evidence: form.target,
    symbols: compactSymbols(form.symbols),
    max_turns: form.maxTurns,
    budget_usd: form.budgetUsd,
    run_packet: buildAutomationPacket(form),
    run_contract: buildRunContract(form),
  };
}
