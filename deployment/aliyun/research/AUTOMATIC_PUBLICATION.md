# Automatic research publication

Monday delivers software images and archives signed Build evidence in separate
stages. Neither stage starts research, resumes collection, changes trading risk,
or enables trading. `cex-runner` is the research worker image; `controller` is
research orchestration software. These software products are not strategy results.

| State | Evidence and meaning |
| --- | --- |
| CI software accepted | Exact main/SHA, three authenticated checks, original binary producer and offline smoke |
| OCI image delivered | `Publish OCI <repository>` succeeded, immutable digest pulled, source and actual executable bytes checked, retained per-product receipt |
| Signed Build archived | Original `Publish <repository>` signed and read back every required OSS object; the complete native publisher run must succeed |
| ACK import accepted | Independent verification of producer, publisher, signatures and artifact identities |
| Experiment completed | A separately authorized real-data run has models, cost-aware backtests and a reproducible manifest |

The OCI lane has ACR access and no OSS environment, OIDC permission, signing key,
or archive fee gate. Missing, invalid, expired or insufficient OSS operating
approval leaves the archive pending while images retain their independent state.
Manual reuse, explicit rebuild and main CI image carry use the same separation.
An ordinary-image selector authenticates main and the three checks independently;
`all` can deliver ordinary images even if a research artifact is unavailable.

Keep native `acr-publish.yml` and the exact signed publisher job names. Keep the
three checks, current main, original producer identity and attempt/job, build
inputs, binary smoke, authenticated downloads and OCI executable readback.
Signed archives retain the original native private ledger and signing rules.
Archive matrices stay serial because products share a source archive. Release,
Claude and monitor identities are unchanged.

## Completion and retries

A failed overall workflow or OSS stage does not erase a successful per-product
OCI job. The image baseline reads completed runs of all conclusions and each
original attempt. Controller, CEX and prediction advance independently. The
signed Build baseline still requires a successful native run and the
`Research products published [...] (SHA)` completion marker. Image jobs cannot
produce that marker. It names exactly `archive_products`; a successful subset
advances only those Builds and cannot claim the full requested product set.

Each OCI job retains `monday.research-oci-delivery.v1` for 90 days. It records
source, product, actual delivery run/attempt/job, immutable repository/digest,
OCI config digest and the authenticated software producer/manifest hash. A retry
reads the exact successful native job and its artifact, checks both before and
after the download, then pulls and verifies the digest against the currently
admitted binary artifact. The new delivery receipt records its own identity and
the original reused delivery. A registry tag alone is never delivery evidence.

Incomplete GitHub pagination, unreadable/expired/deleted positive receipts,
ambiguous producers and source/product/repository/attempt/job drift reject
reuse. Only a complete history with no successful delivery allows a first build.
Partial product success is retained even if another product fails.

OSS is stricter. Attempts after attempt 1 receive no new native allowance. A
previous successful, failed or cancelled signed archive job for the same source
and product requires reconciliation; automatic dispatch cannot refill its spent
allowance. Skipped archives consumed no allowance. Eligible products can archive
independently, but a failed publisher run remains inadmissible to ACK under the
unchanged native contract. Operating approval pins an existing history anchor
(past run ID and ordinal). The reader checks the complete ordinal sequence to
the current run and rejects missing/deleted visible runs, unknown job state and
an older queued run when a newer run is visible. Sources must follow the anchor
commit and have a commit time within the approved UTC window. Pre-window or
future-dated sources require reconciliation. Earlier retained same-source
archive jobs remain counted even before the anchor. No future run number is
predicted by the user.

This cross-run reconciliation requires administrators to retain monotonically
visible Actions history throughout the approved window. Actions is not a
permanent spending ledger: a deleted higher ordinal that ran before an older
queued run is not detectable from the remaining list. Stop archival and reconcile
if history is cleaned or this retention premise cannot be maintained. The
operating approval explicitly requires that premise; the public admission names
`retained-monotonic-github-history` as its basis. This does not provide an
unconditional cumulative cost guarantee or resumable cross-run private-ledger
recovery. No private ledger or session is uploaded.

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
An explicit research rebuild waits for authenticated source admission; only signing and OSS archival wait for the environment gate.

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

`MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY` is a public operating allowance
for software evidence archival. Its schema is
`monday.research-publication-operations-policy.v1`. It specifies repository,
workflow, allowed products, a fixed approved time window (at most seven days),
currency, reviewed price model, storage horizon and per-publication cost/usage
limits, an existing history anchor and `history_retention_required:true`. It does
not ask the user to guess a source SHA or future workflow number.
The estimator binds the actual source, run ID, run number and attempt into the
existing `monday.oss-publication-budget.v1` native envelope before authentication.
The scope is new software-source archival; it is separate from experiment,
scientific grant, ACK activation and trading budgets. The repository variable is
not installed or authorized by this document.

The older `monday.research-publication-budget-policy.v1` single-run CLI remains
available for offline compatibility. The automated workflow uses the operating
allowance. It never treats that allowance as a global cumulative invoice cap.
A fixed time window and a per-source limit do not bound the number of future
sources or the aggregate storage bill. No shared cross-run counter/database,
finite run-number slot protocol or new service is introduced.

The workflow estimates all selected products offline before exposing an archive
environment. Invalid approvals stop before OIDC, STS or OSS. Image preparation,
image CI carry and ACR delivery have no OSS budget dependency. Archive jobs
repeat admission and preserve the no-refill history/attempt checks.
The native issuer must enforce each allocation with a persistent private ledger.
Reserve requests and payload bytes before sending. Keep reservations after errors or interruption.
Source preflight and publication must share that ledger across processes.
Never upload credentials or the private ledger as workflow artifacts.
The workflow retains the public allocation and price admission with native Build evidence.
An `always()` artifact step also retains these public files and the native usage
summary when preparation or publication fails. It names only those three files;
the private ledger, signing key and session are never artifact inputs.
An unreadable or invalid ledger produces `usage_known:false` and fails execution.

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
