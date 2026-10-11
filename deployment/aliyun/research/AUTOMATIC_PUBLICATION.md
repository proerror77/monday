# Automatic research publication

Monday publishes admitted artifacts automatically. Publication does not start
research, resume collection, change risk, or enable trading.

Keep the native `acr-publish.yml` workflow and existing research job names.
Keep all three authenticated checks, current main, original producer identity,
binary smoke, immutable software downloads, signed Build proofs, OCI executable
readback, publication completion, and credential cleanup.
Keep research matrix publication serial because products share a source archive.
Keep Release, both Claude workflows, and the monitor OIDC template unchanged.
Do not install the historical human probe or its global Release hold.

## Identity and admission

Each research product references its own existing environment:

| Product | Environment |
| --- | --- |
| `cex-runner` | `monday-research-cex` |
| `controller` | `monday-research-controller` |
| `prediction-runner` | `monday-research-prediction` |

Each environment permits exactly branch `main`, using a custom branch policy.
The guard rejects reviewer, wait timer, and unknown rules in its environment
readback. GitHub enforces any separately configured App protection rules before
the job starts. Their absence has not been verified by this reader. The route
never approves, removes, or bypasses existing protection.

An ordinary repository variable, `MONDAY_RESEARCH_AUTOMATIC_PUBLICATION`, pins
the repository ID, owner ID, actual default subject prefix, and three immutable
environment IDs. The schema is `monday.automatic_research_publication.v1`.
Use the actual readback values. Do not commit account-specific configuration.

The GET-only gate checks those pins, exact run/attempt/source/workflow, current
main, environment rules, and each selected OSS policy subject. It emits an
environment matrix only after every selected product passes. Missing metadata,
API errors, recreated environments, or source drift fail without that matrix.
The publisher repeats these checks before any credential-bearing step.
Derive the default subject prefix from the pinned repository identity.
Require the documented `use_default` field; do not require API extension fields.
If the API reports immutable subjects or a different prefix, reject that drift.

Ordinary image publication uses a separate matrix without an environment or
OIDC capability. It retains the existing steps and source admission.
Manual recovery retains its existing required checks and producer rules.
An explicit research rebuild also waits for the environment gate.

## Trust boundary

Retain the repository's default OIDC template. This preserves Claude and monitor
authentication. An environment subject distinguishes the product identity.
The existing RAM provider can validate issuer, audience, and subject.
It cannot identify the workflow path from this default environment subject.
Another trusted main workflow could reference the same environment.
Main workflow code, main write permissions, environment administrators, and
configured cloud administrators remain trusted.

Environment GET and job creation are separate operations. If an administrator
deletes an environment after admission, GitHub may recreate its name. The
publisher rejects the changed immutable ID before using cloud credentials.
If an administrator changes settings after the last check, no client check can
provide an atomic lock. This is an administrative trust boundary, not evidence
of a bypass by an unprivileged caller. Keep environment identities and branch
rules stable while their admitted publication runs are active.

## Persistent scope and rollout

The original exact `role_prefixes` configuration binds one source and Build set.
It cannot support future releases without another configuration change.
The OSS contract owner must supply the stable namespace interface separately.
Each runtime session must still use the exact native source/Build plan.

A persistent grant for the existing bucket's `research/builds/*` and
`research/sources/*` is system publication authority. The shared object layout
does not let RAM infer a product from a Build hash. Different roles distinguish
identities; they do not prove server-side isolation between those object hashes.
Never claim the optional session policy restricts a caller who omits it.

Before enabling the identities, approve exact environment and RAM changes once.
Reuse the current provider and STS audience. Do not change repository OIDC,
monitor trust, Claude, signing keys, collectors, or live runtime permissions.
The ordinary OSS map remains owned by its existing configuration writer.
Missing pins or OSS settings leave research publication blocked; they do not
disable Release or ordinary image publication.

Read back every applied identity and policy. Verify native positive and negative
authorization, executable bytes, signed proofs, and registry digests before
claiming publication success. A local fixture is not that native evidence.
Rollback first removes new cloud trust/grants; do not delete an environment
while it remains cloud-trusted. Preserve existing immutable published artifacts.

No cloud writes, new charges, or production transitions are authorized by this
document. Actual storage, requests, and traffic need the approved account budget.
No per-run human approval is added by the implementation.

## Publication budget

The offline estimator reads the catalog and source archive from one exact commit.
It reserves 512 MiB per executable and 1 MiB per metadata object.
These are native publisher payload bounds, not measured executable sizes.
Each Build has three metadata objects. Each product also publishes its source archive.
The request counter also reserves two OIDC and two STS calls for each product.
Their authentication payload reservations are separate from OSS billing categories.
Each STS request reserves 128 KiB. Each authentication response reserves 64 KiB.
PUT and versioning responses reserve 4 KiB. The source probe reserves 1 MiB.
The estimate counts shared programs and source again for each product.
This avoids relying on a cache hit or an existing object to reduce the budget.

Run the estimator without cloud access:

```bash
bash .github/scripts/research-publication-budget.sh estimate \
  <exact-source-sha> cex-runner,controller,prediction-runner 744 estimate.json
```

`MONDAY_RESEARCH_PUBLICATION_BUDGET` is a public, single-run spending approval.
Its schema is `monday.research-publication-budget-policy.v1`.
It pins repository, source, selected products, workflow, next workflow run number,
attempt 1, expiry, price model, storage horizon, and four usage limits.
GitHub assigns a unique run number within this workflow.
Read the next number before configuration. A race rejects the mismatched run.
The admission then binds the actual run ID to the native allocation.
Another run, a rerun, another source, an expired model, or an insufficient limit stops publication.
It does not renew the approval automatically.

The workflow checks all selected products before its first native OSS or authentication call.
It also checks the budget before an explicit manual union build can start.
The native issuer must enforce each allocation with a persistent private ledger.
Reserve requests and payload bytes before sending. Keep reservations after errors or interruption.
Source preflight and publication must share that ledger across processes.
Never upload credentials or the private ledger as workflow artifacts.
The workflow retains the public allocation and price admission with native Build evidence.

The Tokyo model uses the same-account CNY quote observed on 2026-10-11.
It expires on 2026-11-10. Refresh the quote and reviewed model before further admission.
The estimate rounds bytes up to whole decimal GB and uses integer micro-CNY.
It reserves one complete quoted request unit for every attempt, including failures.
The model uses 0.01 CNY per request unit, 0.812 CNY per GB of public egress,
and a conservative 0.000198 CNY per GB-hour of Standard LRS storage.
It adds 64 KiB per request as an overhead allowance in the price estimate.
That allowance is not a bound on network or billed bytes.
Free quotas, account discounts, successful deduplication, and free error responses do not reduce admission.

| Operation | Fee category | Budget treatment |
| --- | --- | --- |
| PutObject | OSS PutRequest; new stored bytes | Reserve request, upload body and possible new storage |
| GetObject/readback | OSS GetRequest; public egress | Reserve request and complete permitted response |
| GetBucketVersioning/config readback | OSS GetRequest | Reserve request and bounded response |
| GetBucketOverwriteConfig | Management read; exact billing code unverified | Separate rollout; reserve one full request unit |
| PutBucketOverwriteConfig | Management write; exact billing code unverified | Separate rollout; reserve one full request unit |
| OIDC and AssumeRoleWithOIDC | Authentication; RAM has no product fee | Count calls and response payload; no OSS request charge |
| Existing ACR Personal upload/download | Free within service limits | No instance purchase or upgrade; outside OSS counters |
| ACK, DB, real data download or research compute | Separate workload | Not authorized or included by this publication budget |

OSS documents chargeable 2xx/3xx requests and free 4xx/5xx requests and traffic.
Reservations remain consumed regardless of response status.
See [request classification](https://help.aliyun.com/zh/oss/api-operation-calling-fees),
[traffic classification](https://help.aliyun.com/zh/oss/traffic-fees),
[storage billing](https://help.aliyun.com/zh/oss/storage-fees), and
[ACR billing](https://help.aliyun.com/zh/acr/product-overview/billing-description).

This is an application usage and price admission, not a cloud invoice hard cap.
HTTP/TLS buffering, provider billing, rounding, tax, and other account users remain outside its counters.
Objects continue to incur storage charges after the run stops.
The 744-hour horizon is a cost model. It does not delete or expire objects.
No deletion, lifecycle rule, or extra bucket permission is installed here.

A first publication can use the single-run approval above.
Continued publication needs a separately scoped spending decision.
A per-publication limit alone cannot cap total charges across future sources.
Set an aggregate period, count and storage decision before claiming a continuing total budget.
Stopping future writes still leaves the accumulated storage cost.
