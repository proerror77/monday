# Crypto microstructure ML boundary

PLOY does not own a generic derivatives-return trainer. Its native Burn model
predicts an official binary settlement probability on event-disjoint folds.
Binance microstructure is an input surface, not a label or execution authority.

Continuous-contract return models belong to Monday's `rust_hft/research-core/ml`
crate and are evaluated with purged walk-forward IC, RankIC, ICIR, costs,
turnover, and drawdown. PLOY models are evaluated with Brier score, log loss,
calibration, settlement PnL, and event-level capacity. The two model artifacts,
split rules, thresholds, and promotion evidence are intentionally separate.

See `docs/CRYPTO_LOB_ML_DEPLOY_CHECKLIST.md` and the repository-level
`docs/architecture/PREDICTION_MARKETS.md`.
