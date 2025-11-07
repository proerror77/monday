Project Structure Overview

- algorithms/: Model architectures and training utilities
- components/: Reusable data/feature interfaces (e.g., FeatureEngineer)
- utils/: Shared helpers (time, ClickHouse SQL builders, config)
- workflows/: Pipelines and jobs (stage2/3, dataset build, QA)
- configs/: YAML/JSON configs for CLI commands
- tools/: Maintenance scripts and helpers
- docs/: Project docs and references (see also _cleanup/docs for legacy docs)
- models/: Saved checkpoints and exports (gitignored)
- workspace/: Local scratch outputs (gitignored)
- logs/: Logs (gitignored)

Entrypoint CLI

- `python cli.py qa run --config configs/qa.yaml`
- `python cli.py dataset build --config configs/features_build.yaml`
- `python cli.py train fit --config configs/train_fit.yaml`
- `python cli.py features verify --config configs/qa.yaml`
- `python cli.py unsup train --symbol WLFIUSDT --start 2025-09-01T00:00:00Z --end 2025-09-08T00:00:00Z --seq_len 60 --epochs 10`
 - `python cli.py backtest dl --model ./models/dl_model_xxx.pt --symbol WLFIUSDT --start 2025-09-03T00:00:00Z --end 2025-09-05T23:59:59Z --horizon 5 --trades_output results/trades.json --plot_output results/equity.png --summary_output results/summary.json`

Notes

- Prefer YAML configs over long CLI arguments (see `configs/`).
- Do not commit credentials; use `.env` and environment variables. See `.env.example`.
- See docs/ARCHITECTURE.md for layer boundaries and data contracts.
Caching

- `FEATURE_CACHE_ENABLE=true` (default) enables local Parquet caching.
- `FEATURE_CACHE_DIR=./cache/features` controls path.
- `FEATURE_CACHE_TTL_HOURS=24` controls TTL; `0` disables TTL.
- Use `--refresh` flag in CLI subcommands to bypass cache.
