# crypto_lob_ml Strategy Fix PR Bootstrap

This file exists to bootstrap a dedicated PR branch for `crypto_lob_ml` strategy repair.

Planned implementation commits will:
- add offline training pipeline
- wire model env loading into runtime
- move live execution to model-first decision logic
- keep coordinator-level risk controls intact
