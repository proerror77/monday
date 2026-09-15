# Reusable Campaign execution

`alpha-harness mission campaign-workflow --plan workflow.json --ledger /campaign-root/authority/ledger.duckdb --work-dir /campaign-root/workflows/h1`

Run this coordinator in ACK using the published controller image. Its plan is
data, not an experiment-specific executable. Existing collector/fresh-input
preparation supplies immutable input receipts; the native preparer retains
verified snapshots and freezes. Data, models, archives and their independent
verification remain in ACK/OSS. A laptop receives compact status/evidence only.

## Declared plan

```json
{
  "schema_version": "monday.cex_campaign_workflow.v1",
  "preparation_plans": [
    {"path": "h5-preparation.json", "sha256": "PIN_EXACT_PLAN_SHA256"},
    {"path": "h10-preparation.json", "sha256": "PIN_EXACT_PLAN_SHA256"},
    {"path": "h30-preparation.json", "sha256": "PIN_EXACT_PLAN_SHA256"}
  ],
  "deadline_at": "REPLACE_WITH_ORIGINAL_ABSOLUTE_DEADLINE",
  "context": "REPLACE_WITH_APPROVED_ACK_CONTEXT",
  "namespace": "monday-research",
  "members": [
    {"id": "h5", "control": {"path": "h5-control.json", "sha256": "PIN_EXACT_CONTROL_SHA256"}, "signer": {"path": "/platform/campaign-signer", "sha256": "PIN_SIGNER_SHA256"}},
    {"id": "h10", "control": {"path": "h10-control.json", "sha256": "PIN_EXACT_CONTROL_SHA256"}, "signer": {"path": "/platform/campaign-signer", "sha256": "PIN_SIGNER_SHA256"}},
    {"id": "h30", "control": {"path": "h30-control.json", "sha256": "PIN_EXACT_CONTROL_SHA256"}, "signer": {"path": "/platform/campaign-signer", "sha256": "PIN_SIGNER_SHA256"}}
  ]
}
```

These placeholders are deliberately not runnable authority. Controls and the
platform signer retain their existing trust boundary; the coordinator cannot
issue a grant, renew an expired budget, open a holdout or authorize trading.
Use the same audited signer across plans. Hash changes require a new plan/state,
not an edit to an already admitted run. Preparation files use the native
`cex_campaign_preparation_plan` contract and globally unique member IDs. H1 sets
`supervised_model_scope` to `ridge_only`, declares matching 5/10/30-second
`holding.horizon_millis` and label horizons, and pins the source/calendar/costs.

Different horizons need different label materializations. Supply their separate
receipts as preparation groups; reuse valid source artifacts in cloud storage.
Before bulk admission, the coordinator validates all member plans and computes
one comparison-family trial bound across the groups. Each worker reserves only
its own declared trials; factor and model scores use the shared statistical
bound. Repeating or recovering a group retains that exact bound.

## Execution and recovery

The coordinator calls the bundled canonical controller with prepared evidence:
`campaign-freeze` reuse → `campaign-finalize` → `dispatch submit` → generated
`campaign-execute --pre-holdout`. It waits on the authenticated Job UID and
verifies the terminal evidence, settlement and native result. It never treats
successful submission or `Job Complete` alone as experiment completion.

On recovery it checks each retained member. Completed members are independently
read back and skipped. Missing summaries are rebuilt from terminal evidence.
Incomplete members resume their recorded stage; source, image, request, UID or
hash drift fails closed. A failed Job, corrupt checkpoint, unknown identity or
expired deadline is explicit in `workflow-status.json` and produces a nonzero
coordinator exit. No identical automatic retry loop is introduced. The original
absolute deadline is retained; after expiry only previously dispatched work
can be read back, without new signing or dispatch.

A normal research negative, including zero trades after costs, remains a
completed member and does not cancel another member. H1's label precheck reports
quantiles and the count/fraction of overlapping labels above cost using only
the predeclared development/validation view. It never cancels an arm. Independent
selection and sealed-test labels are excluded. Holding exceptions cannot pass
the replay promotion gate. Fixed holding comparisons end without automatic
changes to the entry policy or H2/H3 trials.

MLP workflows must explicitly declare their frozen gradient, loss, convergence
and update-budget controls. They stop on the first eligible train-only
convergence window; exhausting the maximum without convergence is not success.
A Ridge-only plan does not instantiate CART or MLP or require an irrelevant
MLP optimizer budget. Workflow recovery reuses completed artifacts; it does not
claim to resume an interrupted optimizer from an intermediate checkpoint.

The report retains native result references and all member outcomes. It does
not recompute metrics, fabricate missing models, refund unknown consumption or
replace the signed accounting ledger. IC/ICIR and executable P&L keep their
existing distinct meanings; zero-trade Sharpe is not evidence of profitability.

## ACK coordinator

The [workflow Job template](../../deployment/aliyun/research/k8s/campaign-workflow-job.example.yaml)
overrides the controller image's default readback entrypoint with the native
workflow command. Bind the plan, ledger, platform signer and existing approved
Kubernetes context. The older controller **readback-only** Role cannot dispatch
Jobs; use the platform's approved dispatcher identity and verify its required
namespace permissions before a run. This change does not silently expand that
Role or create another credential source. Keep active authority-ledger/WAL files
on the established stable block volume; the preparer opens its separately selected
attestation ledger through the read-only API. The signed control remains responsible
for execution accounting. Render resources and the Job deadline from the actual
approved study budget; the template is not a runnable default grant. ACK
workers follow the [CPU-default accelerator contract](ACK_RESEARCH_ACCELERATOR.md);
do not attach `nvidia.com/gpu` to make Ridge or ndarray MLP faster.
