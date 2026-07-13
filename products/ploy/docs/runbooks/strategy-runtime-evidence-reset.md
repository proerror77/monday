# Strategy Runtime Evidence Reset Runbook

This runbook is for clearing contaminated dry-run or paper strategy runtime
evidence after a strategy cutover. It intentionally does not reset raw market
data, official settlements, Polymarket metadata, Binance/Deribit data, or CLOB
orderbook history.

Use this only when the evidence stage is `dry_run_candidate`,
`runtime_parity`, or a later promotion review that is blocked by stale
`strategy_runtime_orders` / `strategy_runtime_fills` rows.

## Safety Contract

- Never run a destructive reset while the target deployment is desired or
  observed `running`.
- The workflow always stops the target through the canonical control plane,
  verifies the worker is gone, and stops `ployd` under a restart trap before
  destructive deletion. There is no running-state bypass.
- Always run a preview first and keep the uploaded backup artifact.
- Destructive execution requires both:
  - `execute=true`
  - `confirm=delete-strategy-runtime-evidence`
- The reset workflow is scoped to dry-run/paper modes only:
  `dry_run,dryrun,paper`.

## PM5D Settlement Dry-Run Target

Current PM5D settlement-probability cleanup target:

```text
deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun
strategy_id=three_layer
runtime_modes=dry_run,dryrun,paper
```

## Preflight

1. Confirm the research semantic contract and evidence stage:

   ```bash
   rtk read docs/PROJECT_SEMANTICS.md
   rtk read docs/runbooks/strategy-research-cicd.md
   ```

2. Read the current deployment state:

   ```bash
   ssh tango-1-1 'set -a; . /opt/ploy/.env; set +a; /opt/ploy/bin/ployctl deployments list' \
     | rg 'pm5d.threelayer.settlement-probability-btc-eth.dryrun'
   ```

3. Read the current dry-run report:

   ```bash
   ssh tango-1-1 'set -a; . /opt/ploy/.env; set +a; token="${PLOY_ADMIN_TOKEN:-${PLOY_API_ADMIN_TOKEN:-${PLOY_API_KEY:-}}}"; curl -fsS -H "Authorization: Bearer ${token}" http://127.0.0.1:8081/api/reports/dry-run' \
     | jq '.strategies[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {summary,metrics,execution_diagnostics}'
   ```

4. Confirm there is an operator approval record. For PM5D settlement dry-run,
   the approval text should explicitly say:

   ```text
   同意暂停 dry-run 并清空 settlement-probability 的 runtime evidence
   ```

## Preview Reset

Run the GitHub workflow from `main` with `execute=false`:

```bash
gh workflow run reset-strategy-runtime-evidence.yml \
  --repo proerror77/ploy \
  --ref main \
  -f git_ref=main \
  -f deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun \
  -f strategy_id=three_layer \
  -f runtime_modes=dry_run,dryrun,paper \
  -f execute=false \
  -f confirm=
```

Download the artifact and inspect `manifest.json`. The preview should report
matched orders and fills, but `deleted_orders` must be `0`.

## Deployment Stop Guard

For `execute=true`, the workflow requests `desired_state=stopped`, waits for
desired and observed state to become stopped, verifies the worker PID/process
is gone, then stops `ployd` before deletion. An EXIT trap restarts `ployd` on
success or failure. Any missing host token, service, deployment, or worker-stop
evidence blocks deletion.

## Execute Reset

Run the workflow from `main`:

```bash
gh workflow run reset-strategy-runtime-evidence.yml \
  --repo proerror77/ploy \
  --ref main \
  -f git_ref=main \
  -f deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun \
  -f strategy_id=three_layer \
  -f runtime_modes=dry_run,dryrun,paper \
  -f execute=true \
  -f confirm=delete-strategy-runtime-evidence
```

The `tango-1-1` environment approval gate must remain enabled. After the run,
download the reset artifact and verify:

- `manifest.json` exists.
- `before.orders` / `before.fills` match the intended target rows.
- `deleted_orders` equals the expected target order count.
- `after.orders=0` and `after.fills=0` for the reset scope.
- Backup JSON files are present in the artifact.
- `post-reset-clean-baseline-gate.json` has `status=passed`.

## Resume Clean Observation

Resume only after reset artifacts are inspected:

```bash
ssh tango-1-1 '/opt/ploy/bin/ployctl deployments resume pm5d.threelayer.settlement-probability-btc-eth.dryrun'
```

Then verify:

```bash
curl -fsS --max-time 10 http://8.221.143.151/api/deployments \
  | jq '.[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {deployment_id,desired_state,observed_state,deployment_state}'

curl -fsS --max-time 10 http://8.221.143.151/api/reports/dry-run \
  | jq '.strategies[]? | select(.deployment_id=="pm5d.threelayer.settlement-probability-btc-eth.dryrun") | {summary,metrics,execution_diagnostics}'
```

Machine-check the clean baseline from the dry-run API payload:

```bash
curl -fsS --max-time 10 http://8.221.143.151/api/reports/dry-run \
  | python3 scripts/check_dryrun_candidate_gate.py \
      --mode clean-baseline \
      --deployment-id pm5d.threelayer.settlement-probability-btc-eth.dryrun
```

Or run the same check as an auditable GitHub Actions artifact:

```bash
gh workflow run dryrun-candidate-gate.yml \
  --repo proerror77/ploy \
  --ref main \
  -f git_ref=main \
  -f deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun \
  -f mode=clean-baseline
```

The destructive reset workflow also runs this clean-baseline gate automatically
after `execute=true`. The standalone workflow is for manual rechecks or later
promotion reviews.

The post-reset dry-run report should start from a clean baseline. A new
profitability claim needs a fresh observation window, not the reset itself.

## Promotion Blockers After Reset

Do not promote a PM5D strategy after reset until all of these are true:

- retained market data coverage is clean for the declared window;
- executable-price replay or walk-forward evidence is present;
- official settlement accounting is present for settlement-probability lanes;
- recorded replay/dry-run parity is strict-ready on the fresh sample;
- dry-run PnL, drawdown, fills, quote age, and capacity support the stated
  risk limits.
