# Historical PLOY Research and Runtime Workflow Guide

This file no longer defines an executable Monday workflow. The former standalone
PLOY workflow map referenced nested GitHub Actions files that Monday does not run;
the obsolete map remains available in Git history only.

Use these current authorities instead:

- `rust_hft/alpha-harness/README.md` for CEX and shared research transport
  capability;
- `docs/architecture/PREDICTION_MARKETS.md` for prediction-market research and
  migration boundaries;
- the root `.github/workflows/ploy-ci.yml` for prediction-market validation; and
- the root `AGENTS.md` for Code, CI, merge, release, runtime, and readback rules.

Research evidence does not authorize deployment. Paper, Shadow, or Live
transitions require their own exact artifact, approval, runtime, and independent
readback evidence. LiveSmall remains disabled.
