# Agentic Alpha Harness Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first Rust-first contract layer for the Agentic Alpha Harness: manifests, factor DSL, search proposals, Factor Bank records, and promotion gate evaluation.

**Architecture:** Add small Rust crates under `rust_hft/research-core/` so research, evaluation, promotion, and runtime integration share typed contracts without coupling to hot execution crates. Keep implementation minimal: schemas, deterministic validation, focused tests, and workspace wiring only.

**Tech Stack:** Rust 2021, `serde`, `thiserror`, `chrono`, focused Cargo package checks/tests. No Python changes in this plan.

## Global Constraints

- Use Rust wherever it should be the durable system boundary.
- Reduce Python usage over time.
- Do not put agentic research logic directly in the Rust hot execution path.
- Do not let LLM, RL, MCTS, GP, or Bayesian search bypass deterministic validation and risk gates.
- Do not hardcode live-small risk percentages as product constants.
- Do not validate every development change by compiling the whole Rust workspace. Use targeted validation lanes.
- Keep new crates small, acyclic, and feature-light.
- Full workspace validation is not required for this plan.

---

## File Structure

Create these Rust crates:

- `rust_hft/research-core/manifest/`: typed manifest IDs, references, data/feature/label/search/evaluation/promotion/live/harness manifests.
- `rust_hft/research-core/factor-dsl/`: minimal canonical factor AST contract and deterministic display helpers.
- `rust_hft/research-core/search-protocol/`: proposal artifacts shared by GP/QD/MCTS/RL/LLM/Bayes.
- `rust_hft/research-core/factor-bank/`: Factor Bank asset records, status, metrics, lineage references.
- `rust_hft/research-core/promotion-gate/`: deterministic gate inputs, failure reasons, and pass/fail evaluation.

Modify:

- `rust_hft/Cargo.toml`: add new crates to workspace members, not default members.

Do not modify hot runtime crates in this plan.

---

### Task 1: Add Manifest Contract Crate

**Files:**
- Create: `rust_hft/research-core/manifest/Cargo.toml`
- Create: `rust_hft/research-core/manifest/src/lib.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Produces: `ManifestId`, `ManifestRef`, `DataManifest`, `FeatureManifest`, `LabelManifest`, `SearchManifest`, `EvaluationManifest`, `PromotionManifest`, `LiveRolloutManifest`, `HarnessManifest`
- Consumes: workspace `serde`, `chrono`, `thiserror`

- [ ] **Step 1: Create the crate manifest**

Create `rust_hft/research-core/manifest/Cargo.toml`:

```toml
[package]
name = "hft-research-manifest"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
chrono = { workspace = true, features = ["serde"] }
serde = { workspace = true, features = ["derive"] }
thiserror = { workspace = true }
```

- [ ] **Step 2: Write the contract types and tests**

Create `rust_hft/research-core/manifest/src/lib.rs`:

```rust
//! Manifest contracts for reproducible research, evaluation, promotion, and live rollout.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest id cannot be empty")]
    EmptyId,
    #[error("manifest reference kind cannot be empty")]
    EmptyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ManifestId(String);

impl ManifestId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ManifestError::EmptyId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRef {
    pub id: ManifestId,
    pub kind: String,
}

impl ManifestRef {
    pub fn new(id: ManifestId, kind: impl Into<String>) -> Result<Self, ManifestError> {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(ManifestError::EmptyKind);
        }
        Ok(Self { id, kind })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub uri: String,
    pub content_type: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataManifest {
    pub id: ManifestId,
    pub sources: Vec<String>,
    pub symbols: Vec<String>,
    pub time_range: TimeRange,
    pub artifact_refs: Vec<ArtifactRef>,
    pub schema_versions: BTreeMap<String, String>,
    pub quality_summary: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureManifest {
    pub id: ManifestId,
    pub data_manifest: ManifestRef,
    pub feature_set_id: String,
    pub operators: Vec<String>,
    pub windows: Vec<String>,
    pub normalization: String,
    pub availability_policy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelManifest {
    pub id: ManifestId,
    pub feature_manifest: ManifestRef,
    pub horizon: String,
    pub barrier_config: BTreeMap<String, f64>,
    pub fee_bps: f64,
    pub slippage_bps: f64,
    pub funding_cost_bps: f64,
    pub label_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchManifest {
    pub id: ManifestId,
    pub engine: String,
    pub seed: Option<u64>,
    pub model_or_prompt_version: Option<String>,
    pub search_space: BTreeMap<String, String>,
    pub parent_run_ids: Vec<ManifestId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationManifest {
    pub id: ManifestId,
    pub search_manifest: ManifestRef,
    pub evaluator_version: String,
    pub metrics: BTreeMap<String, f64>,
    pub costs: BTreeMap<String, f64>,
    pub walk_forward_split: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionManifest {
    pub id: ManifestId,
    pub asset_id: String,
    pub evaluation_manifest: ManifestRef,
    pub gate_results: BTreeMap<String, bool>,
    pub approval_mode: String,
    pub rollout_limits: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveRolloutManifest {
    pub id: ManifestId,
    pub promotion_manifest: ManifestRef,
    pub runtime_config_ref: String,
    pub risk_policy_ref: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attribution: BTreeMap<String, f64>,
    pub rollback_result: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessManifest {
    pub id: ManifestId,
    pub harness_version: String,
    pub agents: Vec<String>,
    pub prompt_versions: BTreeMap<String, String>,
    pub tool_permissions: BTreeMap<String, Vec<String>>,
    pub evaluator_versions: BTreeMap<String, String>,
    pub memory_snapshot_ref: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_manifest_id() {
        assert_eq!(ManifestId::new("  ").unwrap_err(), ManifestError::EmptyId);
    }

    #[test]
    fn builds_manifest_ref() {
        let id = ManifestId::new("data-20260708").unwrap();
        let reference = ManifestRef::new(id, "data_manifest").unwrap();
        assert_eq!(reference.kind, "data_manifest");
        assert_eq!(reference.id.as_str(), "data-20260708");
    }
}
```

- [ ] **Step 3: Wire the crate into the workspace**

Modify `rust_hft/Cargo.toml` under `[workspace].members` after `# Infra Services` or before applications:

```toml
    # Research Core
    "research-core/manifest",
```

Do not add this crate to `default-members`.

- [ ] **Step 4: Validate only this crate**

Run:

```bash
cd rust_hft
cargo test -p hft-research-manifest --locked
```

Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add rust_hft/Cargo.toml rust_hft/research-core/manifest
git commit -m "feat: add research manifest contracts"
```

---

### Task 2: Add Factor DSL Contract Crate

**Files:**
- Create: `rust_hft/research-core/factor-dsl/Cargo.toml`
- Create: `rust_hft/research-core/factor-dsl/src/lib.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `hft-research-manifest::ManifestId`
- Produces: `FactorAst`, `FactorOperator`, `FactorTerminal`, `FactorProgram`

- [ ] **Step 1: Create crate manifest**

Create `rust_hft/research-core/factor-dsl/Cargo.toml`:

```toml
[package]
name = "hft-factor-dsl"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
hft-research-manifest = { path = "../manifest" }
serde = { workspace = true, features = ["derive"] }
thiserror = { workspace = true }
```

- [ ] **Step 2: Write AST types**

Create `rust_hft/research-core/factor-dsl/src/lib.rs`:

```rust
//! Canonical factor DSL and program-factor AST contracts.

use hft_research_manifest::ManifestId;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FactorDslError {
    #[error("operator arity mismatch for {operator}: expected {expected}, got {actual}")]
    ArityMismatch {
        operator: String,
        expected: usize,
        actual: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorTerminal {
    Field(String),
    Constant(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorOperator {
    Add,
    Sub,
    Mul,
    Div,
    Abs,
    Log,
    Rank,
    ZScore,
    Delta,
    Mean,
    Std,
    GreaterThan,
    LessThan,
    IfElse,
}

impl FactorOperator {
    pub fn arity(&self) -> usize {
        match self {
            Self::Abs | Self::Log | Self::Rank => 1,
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::ZScore
            | Self::Delta
            | Self::Mean
            | Self::Std
            | Self::GreaterThan
            | Self::LessThan => 2,
            Self::IfElse => 3,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Abs => "abs",
            Self::Log => "log",
            Self::Rank => "rank",
            Self::ZScore => "zscore",
            Self::Delta => "delta",
            Self::Mean => "mean",
            Self::Std => "std",
            Self::GreaterThan => ">",
            Self::LessThan => "<",
            Self::IfElse => "if_else",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorAst {
    Terminal(FactorTerminal),
    Call {
        operator: FactorOperator,
        args: Vec<FactorAst>,
    },
}

impl FactorAst {
    pub fn call(operator: FactorOperator, args: Vec<FactorAst>) -> Result<Self, FactorDslError> {
        let expected = operator.arity();
        let actual = args.len();
        if expected != actual {
            return Err(FactorDslError::ArityMismatch {
                operator: operator.symbol().to_string(),
                expected,
                actual,
            });
        }
        Ok(Self::Call { operator, args })
    }
}

impl fmt::Display for FactorAst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FactorAst::Terminal(FactorTerminal::Field(name)) => write!(f, "{name}"),
            FactorAst::Terminal(FactorTerminal::Constant(value)) => write!(f, "{value}"),
            FactorAst::Call { operator, args } if args.len() == 2 && operator.symbol().len() == 1 => {
                write!(f, "({} {} {})", args[0], operator.symbol(), args[1])
            }
            FactorAst::Call { operator, args } => {
                let rendered = args.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
                write!(f, "{}({})", operator.symbol(), rendered)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorProgram {
    pub id: String,
    pub ast: FactorAst,
    pub feature_manifest_id: ManifestId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_arity() {
        let arg = FactorAst::Terminal(FactorTerminal::Field("oi".to_string()));
        let err = FactorAst::call(FactorOperator::Add, vec![arg]).unwrap_err();
        assert_eq!(
            err,
            FactorDslError::ArityMismatch {
                operator: "+".to_string(),
                expected: 2,
                actual: 1
            }
        );
    }

    #[test]
    fn renders_binary_formula() {
        let left = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
        let right = FactorAst::Terminal(FactorTerminal::Field("cvd_slope_5m".to_string()));
        let ast = FactorAst::call(FactorOperator::Mul, vec![left, right]).unwrap();
        assert_eq!(ast.to_string(), "(oi_delta_5m * cvd_slope_5m)");
    }
}
```

- [ ] **Step 3: Wire workspace**

Add to `rust_hft/Cargo.toml` `[workspace].members` under `# Research Core`:

```toml
    "research-core/factor-dsl",
```

- [ ] **Step 4: Validate only this crate**

Run:

```bash
cd rust_hft
cargo test -p hft-factor-dsl --locked
```

Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add rust_hft/Cargo.toml rust_hft/research-core/factor-dsl
git commit -m "feat: add factor DSL contracts"
```

---

### Task 3: Add Search Proposal Protocol Crate

**Files:**
- Create: `rust_hft/research-core/search-protocol/Cargo.toml`
- Create: `rust_hft/research-core/search-protocol/src/lib.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `hft-factor-dsl::FactorAst`, `hft-research-manifest::ManifestId`
- Produces: `SearchEngineKind`, `ProposalArtifact`, `MctsTrace`

- [ ] **Step 1: Create crate manifest**

Create `rust_hft/research-core/search-protocol/Cargo.toml`:

```toml
[package]
name = "hft-search-protocol"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
chrono = { workspace = true, features = ["serde"] }
hft-factor-dsl = { path = "../factor-dsl" }
hft-research-manifest = { path = "../manifest" }
serde = { workspace = true, features = ["derive"] }
thiserror = { workspace = true }
```

- [ ] **Step 2: Write proposal types**

Create `rust_hft/research-core/search-protocol/src/lib.rs`:

```rust
//! Search proposal contracts for GP, QD, MCTS, RL, LLM, and Bayesian engines.

use chrono::{DateTime, Utc};
use hft_factor_dsl::FactorAst;
use hft_research_manifest::ManifestId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SearchProtocolError {
    #[error("proposal id cannot be empty")]
    EmptyProposalId,
    #[error("MCTS node {node_id} references itself as parent")]
    SelfParent { node_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchEngineKind {
    GeneticProgramming,
    QualityDiversity,
    Mcts,
    ReinforcementLearning,
    LlmProposer,
    BayesianOptimizer,
    ManualSeed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalArtifact {
    pub proposal_id: String,
    pub engine: SearchEngineKind,
    pub search_manifest_id: ManifestId,
    pub parent_factor_ids: Vec<String>,
    pub ast: FactorAst,
    pub parameters: BTreeMap<String, String>,
    pub rationale: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ProposalArtifact {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.proposal_id.trim().is_empty() {
            return Err(SearchProtocolError::EmptyProposalId);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MctsTraceNode {
    pub node_id: String,
    pub parent_node_id: Option<String>,
    pub visits: u64,
    pub total_reward: f64,
    pub best_reward: f64,
}

impl MctsTraceNode {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.parent_node_id.as_deref() == Some(self.node_id.as_str()) {
            return Err(SearchProtocolError::SelfParent {
                node_id: self.node_id.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MctsTrace {
    pub root_node_id: String,
    pub nodes: Vec<MctsTraceNode>,
    pub backpropagation_truncated_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_proposal_id() {
        let ast = FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field("oi".to_string()));
        let artifact = ProposalArtifact {
            proposal_id: " ".to_string(),
            engine: SearchEngineKind::ManualSeed,
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            parent_factor_ids: vec![],
            ast,
            parameters: BTreeMap::new(),
            rationale: None,
            created_at: Utc::now(),
        };
        assert_eq!(artifact.validate().unwrap_err(), SearchProtocolError::EmptyProposalId);
    }

    #[test]
    fn rejects_mcts_self_parent() {
        let node = MctsTraceNode {
            node_id: "n1".to_string(),
            parent_node_id: Some("n1".to_string()),
            visits: 1,
            total_reward: 0.1,
            best_reward: 0.1,
        };
        assert_eq!(
            node.validate().unwrap_err(),
            SearchProtocolError::SelfParent {
                node_id: "n1".to_string()
            }
        );
    }
}
```

- [ ] **Step 3: Wire workspace**

Add to `rust_hft/Cargo.toml` under `# Research Core`:

```toml
    "research-core/search-protocol",
```

- [ ] **Step 4: Validate only this crate**

Run:

```bash
cd rust_hft
cargo test -p hft-search-protocol --locked
```

Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add rust_hft/Cargo.toml rust_hft/research-core/search-protocol
git commit -m "feat: add search proposal protocol"
```

---

### Task 4: Add Factor Bank Contract Crate

**Files:**
- Create: `rust_hft/research-core/factor-bank/Cargo.toml`
- Create: `rust_hft/research-core/factor-bank/src/lib.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `hft-factor-dsl::FactorAst`, `hft-research-manifest::{ManifestId, ManifestRef}`
- Produces: `FactorAsset`, `FactorStatus`, `FactorMetrics`, `FactorLineage`

- [ ] **Step 1: Create crate manifest**

Create `rust_hft/research-core/factor-bank/Cargo.toml`:

```toml
[package]
name = "hft-factor-bank"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
chrono = { workspace = true, features = ["serde"] }
hft-factor-dsl = { path = "../factor-dsl" }
hft-research-manifest = { path = "../manifest" }
serde = { workspace = true, features = ["derive"] }
thiserror = { workspace = true }
```

- [ ] **Step 2: Write Factor Bank records**

Create `rust_hft/research-core/factor-bank/src/lib.rs`:

```rust
//! Factor Bank contracts for auditable alpha assets.

use chrono::{DateTime, Utc};
use hft_factor_dsl::FactorAst;
use hft_research_manifest::{ManifestId, ManifestRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FactorBankError {
    #[error("factor id cannot be empty")]
    EmptyFactorId,
    #[error("live full candidate is bookkeeping only in MVP")]
    LiveFullCandidateNotExecutable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorType {
    Formula,
    Program,
    ModelFeature,
    Model,
    Ensemble,
    AllocatorPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorStatus {
    Generated,
    QuickTestPassed,
    FullBacktestPassed,
    PaperTrading,
    LiveShadow,
    LiveSmallPendingApproval,
    LiveSmall,
    LiveFullCandidate,
    Decayed,
    Retired,
    Rejected,
}

impl FactorStatus {
    pub fn executable_in_mvp(&self) -> bool {
        matches!(self, Self::PaperTrading | Self::LiveShadow | Self::LiveSmall)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorMetrics {
    pub rank_ic: Option<f64>,
    pub icir: Option<f64>,
    pub net_sharpe: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub turnover: Option<f64>,
    pub custom: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorLineage {
    pub parent_factor_ids: Vec<String>,
    pub source_engine: String,
    pub search_manifest_id: ManifestId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorAsset {
    pub factor_id: String,
    pub factor_type: FactorType,
    pub ast: FactorAst,
    pub lineage: FactorLineage,
    pub data_manifest: ManifestRef,
    pub feature_manifest: ManifestRef,
    pub label_manifest: ManifestRef,
    pub evaluation_manifests: Vec<ManifestRef>,
    pub metrics: FactorMetrics,
    pub correlation_cluster: Option<String>,
    pub regime_metrics: BTreeMap<String, FactorMetrics>,
    pub symbol_metrics: BTreeMap<String, FactorMetrics>,
    pub promotion_status: FactorStatus,
    pub live_decay_state: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl FactorAsset {
    pub fn validate(&self) -> Result<(), FactorBankError> {
        if self.factor_id.trim().is_empty() {
            return Err(FactorBankError::EmptyFactorId);
        }
        if self.promotion_status == FactorStatus::LiveFullCandidate {
            return Err(FactorBankError::LiveFullCandidateNotExecutable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_dsl::FactorTerminal;
    use hft_research_manifest::{ManifestId, ManifestRef};

    fn reference(id: &str, kind: &str) -> ManifestRef {
        ManifestRef::new(ManifestId::new(id).unwrap(), kind).unwrap()
    }

    fn asset_with_status(status: FactorStatus) -> FactorAsset {
        let now = Utc::now();
        FactorAsset {
            factor_id: "factor-1".to_string(),
            factor_type: FactorType::Formula,
            ast: FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
            lineage: FactorLineage {
                parent_factor_ids: vec![],
                source_engine: "manual".to_string(),
                search_manifest_id: ManifestId::new("search-1").unwrap(),
            },
            data_manifest: reference("data-1", "data_manifest"),
            feature_manifest: reference("feature-1", "feature_manifest"),
            label_manifest: reference("label-1", "label_manifest"),
            evaluation_manifests: vec![reference("eval-1", "evaluation_manifest")],
            metrics: FactorMetrics {
                rank_ic: Some(0.03),
                icir: Some(1.2),
                net_sharpe: Some(1.5),
                max_drawdown: Some(0.05),
                turnover: Some(2.0),
                custom: BTreeMap::new(),
            },
            correlation_cluster: None,
            regime_metrics: BTreeMap::new(),
            symbol_metrics: BTreeMap::new(),
            promotion_status: status,
            live_decay_state: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn rejects_empty_factor_id() {
        let mut asset = asset_with_status(FactorStatus::Generated);
        asset.factor_id = " ".to_string();
        assert_eq!(asset.validate().unwrap_err(), FactorBankError::EmptyFactorId);
    }

    #[test]
    fn blocks_live_full_candidate_execution_in_mvp() {
        let asset = asset_with_status(FactorStatus::LiveFullCandidate);
        assert_eq!(
            asset.validate().unwrap_err(),
            FactorBankError::LiveFullCandidateNotExecutable
        );
    }
}
```

- [ ] **Step 3: Wire workspace**

Add to `rust_hft/Cargo.toml` under `# Research Core`:

```toml
    "research-core/factor-bank",
```

- [ ] **Step 4: Validate only this crate**

Run:

```bash
cd rust_hft
cargo test -p hft-factor-bank --locked
```

Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add rust_hft/Cargo.toml rust_hft/research-core/factor-bank
git commit -m "feat: add factor bank contracts"
```

---

### Task 5: Add Promotion Gate Contract Crate

**Files:**
- Create: `rust_hft/research-core/promotion-gate/Cargo.toml`
- Create: `rust_hft/research-core/promotion-gate/src/lib.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `hft-factor-bank::{FactorAsset, FactorStatus}`
- Produces: `PromotionGateInput`, `PromotionGateDecision`, `GateFailure`

- [ ] **Step 1: Create crate manifest**

Create `rust_hft/research-core/promotion-gate/Cargo.toml`:

```toml
[package]
name = "hft-promotion-gate"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
chrono = { workspace = true, features = ["serde"] }
hft-factor-bank = { path = "../factor-bank" }
hft-factor-dsl = { path = "../factor-dsl" }
hft-research-manifest = { path = "../manifest" }
serde = { workspace = true, features = ["derive"] }
thiserror = { workspace = true }
```

- [ ] **Step 2: Write deterministic gate evaluator**

Create `rust_hft/research-core/promotion-gate/src/lib.rs`:

```rust
//! Deterministic promotion gate contracts for paper, shadow, and live-small.

use hft_factor_bank::{FactorAsset, FactorStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetStage {
    PaperTrading,
    LiveShadow,
    LiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateFailure {
    MissingEvaluationManifest,
    MissingRankIc,
    MissingNetSharpe,
    MissingMaxDrawdown,
    RankIcBelowFloor,
    NetSharpeBelowFloor,
    MaxDrawdownAboveCeiling,
    ApprovalRequired,
    NotEligibleForTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionGateInput {
    pub target_stage: TargetStage,
    pub min_rank_ic: f64,
    pub min_net_sharpe: f64,
    pub max_drawdown_ceiling: f64,
    pub first_same_class_approval_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionGateDecision {
    pub passed: bool,
    pub failures: Vec<GateFailure>,
}

pub fn evaluate_promotion(asset: &FactorAsset, input: &PromotionGateInput) -> PromotionGateDecision {
    let mut failures = Vec::new();

    if asset.evaluation_manifests.is_empty() {
        failures.push(GateFailure::MissingEvaluationManifest);
    }

    match asset.metrics.rank_ic {
        Some(value) if value >= input.min_rank_ic => {}
        Some(_) => failures.push(GateFailure::RankIcBelowFloor),
        None => failures.push(GateFailure::MissingRankIc),
    }

    match asset.metrics.net_sharpe {
        Some(value) if value >= input.min_net_sharpe => {}
        Some(_) => failures.push(GateFailure::NetSharpeBelowFloor),
        None => failures.push(GateFailure::MissingNetSharpe),
    }

    match asset.metrics.max_drawdown {
        Some(value) if value <= input.max_drawdown_ceiling => {}
        Some(_) => failures.push(GateFailure::MaxDrawdownAboveCeiling),
        None => failures.push(GateFailure::MissingMaxDrawdown),
    }

    let status_ok = match input.target_stage {
        TargetStage::PaperTrading => matches!(
            asset.promotion_status,
            FactorStatus::QuickTestPassed | FactorStatus::FullBacktestPassed
        ),
        TargetStage::LiveShadow => matches!(asset.promotion_status, FactorStatus::PaperTrading),
        TargetStage::LiveSmall => matches!(asset.promotion_status, FactorStatus::LiveShadow),
    };

    if !status_ok {
        failures.push(GateFailure::NotEligibleForTarget);
    }

    if input.target_stage == TargetStage::LiveSmall && !input.first_same_class_approval_present {
        failures.push(GateFailure::ApprovalRequired);
    }

    PromotionGateDecision {
        passed: failures.is_empty(),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_bank::{FactorLineage, FactorMetrics, FactorType};
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_research_manifest::{ManifestId, ManifestRef};
    use std::collections::BTreeMap;

    fn reference(id: &str, kind: &str) -> ManifestRef {
        ManifestRef::new(ManifestId::new(id).unwrap(), kind).unwrap()
    }

    fn asset(status: FactorStatus) -> FactorAsset {
        let now = chrono::Utc::now();
        FactorAsset {
            factor_id: "factor-1".to_string(),
            factor_type: FactorType::Formula,
            ast: FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
            lineage: FactorLineage {
                parent_factor_ids: vec![],
                source_engine: "manual".to_string(),
                search_manifest_id: ManifestId::new("search-1").unwrap(),
            },
            data_manifest: reference("data-1", "data_manifest"),
            feature_manifest: reference("feature-1", "feature_manifest"),
            label_manifest: reference("label-1", "label_manifest"),
            evaluation_manifests: vec![reference("eval-1", "evaluation_manifest")],
            metrics: FactorMetrics {
                rank_ic: Some(0.04),
                icir: Some(1.3),
                net_sharpe: Some(1.6),
                max_drawdown: Some(0.04),
                turnover: Some(2.0),
                custom: BTreeMap::new(),
            },
            correlation_cluster: None,
            regime_metrics: BTreeMap::new(),
            symbol_metrics: BTreeMap::new(),
            promotion_status: status,
            live_decay_state: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn live_small_requires_first_same_class_approval() {
        let input = PromotionGateInput {
            target_stage: TargetStage::LiveSmall,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown_ceiling: 0.05,
            first_same_class_approval_present: false,
        };
        let decision = evaluate_promotion(&asset(FactorStatus::LiveShadow), &input);
        assert!(!decision.passed);
        assert!(decision.failures.contains(&GateFailure::ApprovalRequired));
    }

    #[test]
    fn live_small_passes_when_metrics_status_and_approval_pass() {
        let input = PromotionGateInput {
            target_stage: TargetStage::LiveSmall,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown_ceiling: 0.05,
            first_same_class_approval_present: true,
        };
        let decision = evaluate_promotion(&asset(FactorStatus::LiveShadow), &input);
        assert!(decision.passed);
        assert!(decision.failures.is_empty());
    }
}
```

- [ ] **Step 3: Wire workspace**

Add to `rust_hft/Cargo.toml` under `# Research Core`:

```toml
    "research-core/promotion-gate",
```

- [ ] **Step 4: Validate only this crate**

Run:

```bash
cd rust_hft
cargo test -p hft-promotion-gate --locked
```

Expected: `2 passed`.

- [ ] **Step 5: Commit**

```bash
git add rust_hft/Cargo.toml rust_hft/research-core/promotion-gate
git commit -m "feat: add promotion gate contract"
```

---

### Task 6: Add Research Core Validation Notes

**Files:**
- Create: `rust_hft/research-core/README.md`
- Create: `rust_hft/research-core/VALIDATION.md`

**Interfaces:**
- Consumes: crate names from Tasks 1-5
- Produces: operator-facing targeted validation commands

- [ ] **Step 1: Create research-core README**

Create `rust_hft/research-core/README.md`:

```markdown
# Research Core

Rust-first contracts for the Agentic Alpha Harness.

This tree owns durable schemas and deterministic gates for:

- manifests
- factor DSL
- search proposals
- Factor Bank records
- promotion gates

It does not own hot-path execution, exchange adapters, order routing, or LLM orchestration.
```

- [ ] **Step 2: Create validation guide**

Create `rust_hft/research-core/VALIDATION.md`:

```markdown
# Research Core Validation

Do not run the full Rust workspace for every research-core change.

Use targeted checks:

```bash
cargo test -p hft-research-manifest --locked
cargo test -p hft-factor-dsl --locked
cargo test -p hft-search-protocol --locked
cargo test -p hft-factor-bank --locked
cargo test -p hft-promotion-gate --locked
```

Use a broader check only when changing workspace dependencies, shared feature flags, or runtime integration:

```bash
cargo check --workspace --locked
```
```

- [ ] **Step 3: Validate docs diff**

Run:

```bash
git diff --check -- rust_hft/research-core/README.md rust_hft/research-core/VALIDATION.md
```

Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add rust_hft/research-core/README.md rust_hft/research-core/VALIDATION.md
git commit -m "docs: document research core validation"
```

---

## Self-Review Checklist

- Spec coverage:
  - Rust-first durable contracts: Tasks 1-5.
  - Reduced Python: no Python added; Rust contracts prepare replacement path.
  - Targeted validation: every task has crate-scoped checks.
  - Manifest requirement: Task 1.
  - Factor DSL: Task 2.
  - Search proposal protocol including MCTS lineage guardrails: Task 3.
  - Factor Bank: Task 4.
  - Promotion gate and live-small first approval: Task 5.
  - Project structure and validation docs: Task 6.

- Placeholder scan:
  - This plan contains no placeholder markers or unspecified test steps.

- Type consistency:
  - `ManifestId` and `ManifestRef` are defined in Task 1 and reused by Tasks 2-5.
  - `FactorAst` is defined in Task 2 and reused by Tasks 3-5.
  - `FactorAsset` and `FactorStatus` are defined in Task 4 and reused by Task 5.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-08-agentic-alpha-harness-contracts.md`.

Two execution options:

1. **Subagent-Driven (recommended)** - Dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
