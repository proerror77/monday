ML Workspace

Unified, config-driven ML workspace for market microstructure research and training.

Key Directories

- algorithms/: model code (DL/TCN-GRU/RL)
- components/: high-level building blocks (FeatureEngineer)
- utils/: shared helpers (time, ClickHouse client/SQL, reproducibility)
- workflows/: end-to-end jobs (QA, dataset build, DL backtest/predict)
- lob_core/: LOB-native research kernel (features/labeling/alpha/models/pipeline)
- configs/: YAML examples for CLI commands
- docs/: docs and architecture notes (see docs/ARCHITECTURE.md)
- demos/: self-contained demos
- models/, workspace/, logs/, results/: artifacts (gitignored)

CLI Quickstart

- QA checks: `python cli.py qa run --config configs/qa.yaml`
- Build multi-symbol dataset: `python cli.py dataset build --config configs/features_build.yaml`
- Train (supervised DL): `python cli.py train fit --config configs/train_fit.yaml`
- Verify features/tables: `python cli.py features verify --config configs/qa.yaml`
- Unsupervised TCN-GRU: `python cli.py unsup train --symbol WLFIUSDT --start 2025-09-01T00:00:00Z --end 2025-09-08T00:00:00Z --seq_len 60 --epochs 10`
 - Backtest (DL): `python cli.py backtest dl --model ./models/dl_model_xxx.pt --symbol WLFIUSDT --start 2025-09-03T00:00:00Z --end 2025-09-05T23:59:59Z --horizon 5 --trades_output results/trades.json --plot_output results/equity.png --summary_output results/summary.json`

Conventions

- Prefer YAML configs over long CLI flags.
- Never commit credentials. Use `.env` locally and environment variables in CI. See `.env.example`.
- Artifacts are ignored via `.gitignore`.
Caching

- Set `FEATURE_CACHE_ENABLE=true` to enable local Parquet caching (default).
- `FEATURE_CACHE_DIR=./cache/features` controls where files are stored.
- `FEATURE_CACHE_TTL_HOURS=24` controls TTL (set `0` to never expire).
- Pass `--refresh` to CLI commands to bypass cache on-demand.
