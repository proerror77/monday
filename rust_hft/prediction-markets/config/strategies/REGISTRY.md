# PLOY strategy configuration registry inside Monday

This directory retains PLOY strategy configurations for research, replay, backtesting,
paper-mode validation, and compatibility analysis.

## Current authority

- Strategy files do not authorize deployment or execution.
- Monday `rust_hft` remains the only production execution authority.
- Files containing `live` in their name are historical standalone fixtures. PLOY's
  production daemon gateway rejects every live operation.
- `new-ploy-runner --features full` is non-executing in Monday and may be used only
  with explicit dry-run/research inputs.

## Development use

Prefer a focused dry-run configuration and keep evidence tied to the exact config
digest used by the test or replay.

```bash
cargo +1.91 run -p new-ploy-runner --features full -- \
  run --config config/strategies/02-pm5d.v4-dryrun.toml --dry-run
```

Changes to strategy schemas or runtime contracts require focused tests in
`ploy-strategy-bundles`, contract/evidence updates, and the root PLOY CI workflow.
Do not dispatch nested deployment workflows or target former PLOY hosts.
