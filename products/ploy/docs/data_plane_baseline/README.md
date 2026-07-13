# Data Plane Baseline Artifacts

This folder stores reproducible baseline artifacts for Phase 0 (#21).

## Seed baseline snapshot

- Run ID: `phase0-seed-20260303`
- Generated at: `2026-03-03T04:05:26Z`
- Collector mode: accelerated virtual-time capture (`--time-scale 2880`)
- Virtual duration: 24h (`--duration-secs 86400`)
- Sampling interval: 15m virtual (`--interval-secs 900`)

Files:

- `phase0-seed-20260303.samples.jsonl`
- `phase0-seed-20260303.summary.json`
- `phase0-seed-20260303.baseline.json`
- `phase0-seed-20260303.symbol_rates.csv`
- `phase0-seed-20260303.source_rates.csv`
- `phase0-seed-20260303.rollback_report.json`

Regeneration steps are documented in
`docs/PHASE0_DATA_PLANE_BASELINE_RUNBOOK.md`.
