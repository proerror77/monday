# Project Constitution: ML Workspace

**Version**: 1.0.0
**Date**: 2025-10-08
**Scope**: Machine Learning research & training workspace for HFT market microstructure

---

## Purpose & Scope

This workspace is the **intelligence layer** of the HFT system, responsible for:
- LOB (Limit Order Book) microstructure research and feature engineering
- Deep learning model training (supervised & unsupervised)
- Backtesting and performance evaluation
- Model lifecycle management (training → validation → deployment)

**Boundaries**: This workspace focuses on **cold-path batch processing**. Real-time inference happens in the Rust HFT core. Communication with production systems occurs via ClickHouse (data source) and gRPC/Redis (model deployment).

---

## Core Principles

### 1. Reproducibility First
**Summary**: Every experiment, dataset, and model must be fully reproducible. No "magic numbers" or undocumented randomness.

**Rules**:
- MUST seed all RNGs (NumPy, PyTorch, Python random) using `utils/reproducibility.py`
- MUST version datasets with timestamps and content hashes
- MUST record all hyperparameters in training configs (YAML or code comments)
- MUST track experiments with unique IDs (timestamp-based or UUIDs)
- MUST NOT commit model weights; use artifact storage (S3/local with gitignore)

**Why**: Financial ML requires audit trails. Models deployed to production need exact recreation paths for debugging and compliance.

---

### 2. Feature Registry as Single Source of Truth
**Summary**: `utils/feature_registry.py` is the authoritative definition of all features. SQL builders and data loaders MUST respect this contract.

**Rules**:
- MUST define features in the registry with name, type, category, dependencies
- MUST compute feature hashes for version control
- MUST NOT hardcode feature lists in SQL queries; use `ch_queries.build_feature_sql()`
- MUST validate feature order consistency between ClickHouse and PyTorch tensors
- MUST document feature computation logic in registry or adjacent docs

**Why**: Feature drift breaks models silently. A single registry prevents train/test/production mismatches.

---

### 3. Fail-Fast Data Validation
**Summary**: Detect data quality issues early. Better to crash during QA than train on garbage.

**Rules**:
- MUST run `cli.py qa run` before any training job
- MUST check for schema mismatches, missing timestamps, and extreme outliers
- MUST log data quality metrics (coverage %, gap count, schema version)
- MUST reject datasets with >5% gaps unless explicitly flagged for imputation
- SHOULD implement table-specific validation rules in `workflows/qa/quality_checks.py`

**Why**: LOB data has microsecond timestamps and complex dependencies. Silent corruption wastes GPU hours.

---

### 4. Config-Driven Workflows
**Summary**: Prefer YAML configs over hardcoded parameters. CLI flags are for overrides, not defaults.

**Rules**:
- MUST provide example configs in `configs/` for all CLI subcommands
- MUST document required vs. optional config fields in docstrings
- MUST validate configs at load time (fail early if missing required keys)
- SHOULD use environment variables for secrets (ClickHouse passwords, S3 keys)
- SHOULD NOT commit `.env` files; use `.env.example` as template

**Why**: Configs enable team collaboration and automated pipelines without code changes.

---

### 5. Layered Architecture with Clear Contracts
**Summary**: Respect the layer boundaries defined in `docs/ARCHITECTURE.md`. Don't bypass abstractions.

**Rules**:
- **Kernel** (`lob_core/`): Pure research code, no external I/O except file reads
- **Components** (`components/`): Facades for workflows, return typed dicts (train/val tensors)
- **Workflows** (`workflows/`): Orchestration logic, call components and utils
- **Utils** (`utils/`): Stateless helpers, no business logic
- **CLI** (`cli.py`): Dispatch only, no heavy computation

**Contracts**:
- `FeatureEngineer.prepare()` MUST return `{train_features, train_labels, val_features, val_labels, sequence_length, feature_dim, feature_columns}`
- SQL builders MUST return column-ordered queries matching the feature registry
- Training loops MUST accept config dicts, not scattered kwargs

**Why**: Clear layers prevent spaghetti code and enable independent testing.

---

## Governance

### Roles & Ownership
- **ML Researcher**: Proposes new features, models, experiments
- **Data Engineer**: Maintains ClickHouse schemas, feature registry, QA pipelines
- **MLOps**: Manages training infrastructure, model versioning, deployment to Rust core

### Decision Making
- **Feature additions**: Require registry update + backward compatibility check
- **Breaking changes**: Require version bump in feature set hash
- **Model architecture changes**: Document in git commit message with rationale

### Change Control
- Constitution changes bump version (e.g., 1.0.0 → 1.1.0) and require README update
- Pull requests MUST pass `pytest tests/` and `python cli.py features verify`
- Breaking API changes MUST be announced in team channel before merge

---

## Quality & Testing

### Testing Policy
- **Unit tests**: Required for `utils/`, `components/`, `lob_core/` pure functions
- **Integration tests**: Required for workflows that touch ClickHouse or file I/O
- **E2E tests**: Optional but encouraged for full CLI commands on sample data

### Coverage Thresholds
- **Target**: 70% line coverage for `utils/` and `components/`
- **Minimum**: 50% for `lob_core/` (research code can be exploratory)
- **Exemptions**: CLI dispatch logic, one-off scripts in `workflows/scripts/`

### Performance Budgets
- Feature extraction: <30s for 1M rows on 8-core CPU
- Training epoch: <5min on single V100 GPU for LSTM (batch size 128, seq len 60)
- Dataset build: <10min for 8 symbols × 7 days on standard ClickHouse instance

**SLOs**:
- QA checks: p95 latency <2min for 24h data window
- Model training: 90% jobs complete within 2h (including hyperparameter search)

---

## Security & Privacy

### Data Classification
- **Public**: Aggregated statistics, model performance metrics
- **Internal**: Raw LOB data, feature values, model weights
- **Secret**: ClickHouse credentials, S3 access keys, gRPC TLS certificates

### Secrets Management
- MUST use `.env` for local development (gitignored)
- MUST use AWS Secrets Manager or equivalent in production
- MUST rotate credentials quarterly
- MUST NOT log sensitive data (passwords, API keys, PII)

### Threat Model
- **Data exfiltration**: LOB data contains proprietary trading signals; limit access
- **Model theft**: Weights are intellectual property; use signed checkpoints
- **Supply chain**: Pin package versions in `requirements.txt`; audit dependencies quarterly

---

## Observability

### Logging Standards
- **Format**: Structured JSON via `loguru` (key-value pairs, not free text)
- **Levels**: DEBUG (development only), INFO (normal ops), WARNING (recoverable issues), ERROR (job failures)
- **Required fields**: `timestamp`, `level`, `message`, `job_id`, `symbol` (if applicable)

### Metrics & Tracing
- SHOULD emit training metrics to MLflow or WandB (loss, IC, IR, Sharpe)
- MUST log feature extraction timings to identify bottlenecks
- SHOULD track ClickHouse query latencies for large scans

### Alerting Policy
- **Training failures**: Notify on Slack/Email if job crashes 2+ times in 24h
- **Data quality**: Alert if gap rate >10% or schema changes detected
- **Model performance**: Warn if validation IC drops >30% vs. training IC

---

## Reproducibility (ML/Data)

### Datasets & Lineage
- MUST record data source (ClickHouse table + query timestamp)
- MUST log feature set version (hash from registry)
- MUST store preprocessed datasets in versioned Parquet files
- SHOULD include data provenance in model metadata (JSON sidecar)

### Experiment Tracking
- MUST log hyperparameters: learning rate, batch size, sequence length, model architecture
- MUST save training curves (loss, validation metrics) as CSV or plots
- SHOULD use unique experiment IDs (e.g., `exp_20251008_143022_lstm_ic`)
- SHOULD tag experiments with git commit hash for code version

### Model Versioning
- MUST export models in TorchScript format (`.pt`) for Rust interop
- MUST include metadata: feature columns, normalization params, training timestamp
- MUST NOT overwrite production models; use atomic rename after validation
- SHOULD archive failed experiments with reason codes (e.g., `ic_too_low`, `overfitting`)

---

## Documentation & DX

### Required Docs
- **README.md**: Quickstart guide with CLI examples
- **ARCHITECTURE.md**: Layer descriptions and data contracts
- **Runbook**: Steps to reproduce training from scratch (in `docs/` or wiki)
- **Changelog**: Keep `CHANGELOG.md` for major feature additions

### Code Style & Review
- **Style**: Follow PEP 8; use `black` formatter (line length 100)
- **Type hints**: Required for public functions in `utils/` and `components/`
- **Docstrings**: Required for classes and non-trivial functions (Google style)
- **Reviews**: At least one approving review before merge to main

### Tooling & Automation
- **Linter**: `ruff` or `flake8` for static checks
- **Formatter**: `black` with 100 char line limit
- **CI**: GitHub Actions to run `pytest` and `black --check`
- **Pre-commit hooks**: Recommended for formatting and test execution

---

## Non-goals

- **Real-time inference**: Handled by Rust HFT core, not this workspace
- **Order execution**: Trading logic lives in Rust; this workspace only generates signals
- **High-frequency data ingestion**: ClickHouse collector (Rust) handles streaming
- **UI dashboards**: Use external tools (Grafana, Streamlit); focus on pipelines

---

## Glossary

- **LOB**: Limit Order Book (bid/ask levels and trades)
- **IC**: Information Coefficient (correlation between prediction and actual return)
- **IR**: Information Ratio (IC mean / IC std)
- **Feature Registry**: `utils/feature_registry.py` - single source of truth for features
- **FeatureEngineer**: Component that transforms raw data into training tensors
- **QA**: Data quality assurance checks (`cli.py qa run`)
- **Cold-path**: Batch processing (training, backtesting) vs. hot-path (real-time execution)
- **gRPC**: Communication protocol for deploying models to Rust core
- **TorchScript**: PyTorch's export format for production inference

---

## Sync Notes

**Last Updated**: 2025-10-08
**Updated Sections**: All (initial creation)
**Required Template Updates**: None (templates use this as reference)
**Backwards-Compat Notes**: N/A (v1.0.0 baseline)

---

**Enforcement**: This constitution is living documentation. Violations should be flagged in code reviews. Major deviations require constitution amendment (version bump + team discussion).
