Architecture Overview

Layers

- Kernel (lob_core/): Core library for LOB-native research: data structures, ingestion, features, labeling, alpha search, models, and the end-to-end training pipeline.
- Components (components/): Facades for external workflows (e.g., FeatureEngineer) that produce train/val tensors.
- Algorithms (algorithms/): Model implementations (DL Transformer regressor, TCN-GRU unsupervised, RL scaffolding).
- Workflows (workflows/): End-to-end jobs wired to the CLI (QA checks, dataset build, DL backtest/predict).
- Utils (utils/): Shared utilities: ClickHouse client, SQL builders, time, reproducibility, feature registry/validators.
- CLI (cli.py): Single entrypoint dispatching to QA, dataset, training and verification subcommands.

Data Contracts

- Feature registry (utils/feature_registry.py) is the single source of truth for feature names/order/version.
- SQL builders (utils/ch_queries.py) select columns in the exact registry order. Normalization strategy is explicit: either in-DB (z-score) or in Python, but not both.
- FeatureEngineer returns a dict with:
  - train_features: torch.Tensor [N, T, F]
  - train_labels: torch.Tensor [N]
  - val_features: torch.Tensor [M, T, F]
  - val_labels: torch.Tensor [M]
  - sequence_length: int, feature_dim: int, feature_columns: List[str]

CLI Commands

- qa run: wraps workflows/qa/quality_checks.run_checks
- dataset build: builds a multi-symbol Parquet dataset from ClickHouse
- train fit: extracts sequences via FeatureEngineer and trains DLTrainer
- features verify: quick coverage/schema verification using QA helpers

Security & Reproducibility

- Secrets are not committed. Use .env.example as a template.
- Reproducibility utilities (utils/reproducibility.py) seed all RNGs.
- Logging is standardized via Python logging; prefer structured JSON in services.

Migration Notes

- Legacy one-off scripts were removed or moved under docs/archive or demos/ to reduce confusion.
- Demos (e.g., lob_core_demo.py) are under demos/ and not part of the CLI.

