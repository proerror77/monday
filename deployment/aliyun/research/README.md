# Aliyun research plane

This directory is the Tokyo research-plane deployment boundary. It is separate
from the always-on Binance collector and from the live trading ECS.

## Architecture

```text
Binance public data
  -> collector ECS (raw replay-safe segments)
  -> OSS raw zone (.jsonl.zst + manifest + _SUCCESS)
  -> point-in-time materializer (selected partitions only)
  -> OSS canonical Parquet
  -> ClickHouse shared analytics and feature tables
  -> ACK Indexed Jobs using a prebuilt Rust image
  -> result artifacts
  -> one DuckDB aggregator for lineage and approval state
```

The storage authority remains split deliberately:

- OSS raw and canonical Parquet are immutable data evidence.
- ClickHouse is the shared high-speed query and feature plane. It is not the
  source of truth for approvals, run state, or complete sequential LOB replay.
- Rust replays one selected local partition in event order and applies explicit
  fee, latency, additional-slippage, and optional spread-crossing assumptions.
  The baseline does not model L3 queue position, partial fills, market impact,
  or venue capacity; an optional same-side top-N depth gate is only a
  conservative evidence check, not a capacity simulation.
- DuckDB has one writer and owns research/control-plane lineage. Parallel Pods
  must never open the same DuckDB file for writes.

The first Agentic Alpha path uses the Rust `lob-pit-materializer` binary to
validate the raw segment, replay Binance's Spot or USD-M sequence contract, and
emit one-second point-in-time rows. It rejects missing `_SUCCESS` markers,
SHA-256 mismatch, sequence gaps, unseeded diffs, and closing checkpoints that
do not match the replayed full book. Feature rows and the materialization report
are published to SHA-256-named immutable paths. Forward-mid labels are only
exposed at the future bucket's availability time.

No exchange credential belongs in this namespace. Research Pods receive public
datasets, ClickHouse read credentials, and result-write authority only.

## Bootstrap sizing

The current full Binance spot plus USD-M archive grows by about 16.3 GiB/day
after Zstandard compression. Plan for:

| Retention | Raw estimate | Raw + Parquet/manifests budget |
| --- | ---: | ---: |
| 14 days | 230 GiB | 300 GiB |
| 30 days | 490 GiB | 650 GiB |

Keep complete raw data in OSS. The initial ClickHouse hot tier should hold 14
days of aligned/derived data and 30 days of compact features, not a duplicate of
the complete raw tape.

Recommended first production shape:

- Managed ClickHouse, 8 vCPU / 32 GiB, private VPC endpoint, 200 GiB cache.
- ACK Standard control plane.
- One Spot system node, `ecs.u1-c1m2.large` (2 vCPU / 4 GiB), with a 40 GiB
  PL0 system disk. The economy bootstrap runs one CoreDNS replica; use two
  system nodes and two replicas when research-plane HA matters.
- Autoscaled Spot worker pool, `ecs.u1-c1m4.xlarge` (4 vCPU / 16 GiB), with a
  40 GiB PL0 system disk and 100 GiB PL1 work disk. The pool is labeled
  `workload=backtest`, scales from zero to four nodes, and returns to zero after
  jobs finish.
- One backtest Pod per worker; each Pod processes a batch of parameters.
- A prebuilt image from `rust_hft/deployment/docker/Dockerfile.research`.

The existing AWS/EKS manifests under `rust_hft/deployment/k8s` are not inputs to
this deployment.

## Monthly cost model

The following Tokyo prices were checked with Aliyun CLI on 2026-07-14. OSS and
small network charges are planning estimates because the billing endpoint did
not return a complete international price sheet.

| Component | Planning cost |
| --- | ---: |
| Existing 2C8G collector, 80 GiB system + 200 GiB data | CNY 482.53/month |
| Separate 2C8G trading ECS, 80 GiB system | CNY 410.53/month |
| Recommended 4C8G compute-optimized trading ECS, 80 GiB PL1 | CNY 886.69/month |
| Self-managed 4C16G ClickHouse, 40 GiB system + 500 GiB PL1 | CNY 1,546.48/month |
| Managed ClickHouse 8C32G with 200 GiB cache | about CNY 2,434/month |
| Spot 2C4G ACK system node with 40 GiB PL0 | about CNY 82-87/month |
| Spot 4C16G worker with 40 GiB PL0 + 100 GiB PL1 | about CNY 0.342/hour |
| OSS raw + Parquet, 14-day retention | about CNY 50/month |
| OSS raw + Parquet, 30-day retention | about CNY 100/month |
| Private networking, logs, alerts, small requests | CNY 50-150/month |

Expected totals:

| Profile | Monthly estimate | Boundary |
| --- | ---: | --- |
| Economy | about CNY 2,700 | 2C8G trading, self-managed 4C16G ClickHouse, 100 Spot node-hours |
| Recommended infrastructure | about CNY 4,300 | 4C8G trading, managed ClickHouse compute/cache, 400 Spot node-hours |
| Heavy research | about CNY 7,600-8,500 | 16C64G-class ClickHouse and about 1,000 Spot node-hours |

ACK Standard has no control-plane management fee, but worker ECS, disks,
outbound/NAT, load balancers, logging, and registry usage are still billed. The
recommended profile is the best operational trade-off once the system runs
daily parallel research. The economy profile is cheaper but makes ClickHouse
backup, upgrades, failure recovery, and disk operations the operator's job.

The current ACK bootstrap therefore idles at roughly CNY 102-137/month before
OSS: CNY 82-87 for the system node plus an estimated CNY 20-50 for the private
API load balancer and small monitoring traffic. Research compute adds about CNY
34 per 100 worker-hours at the 2026-07-14 Tokyo Spot price. These figures do not
include ClickHouse, which is not deployed yet.

For the recommended profile, keep a CNY 4,800-5,200 monthly payment budget until
the first complete invoice is available. That margin covers managed ClickHouse
durable storage, Spot-price movement, NAT/logging variance, snapshots, and
short-lived deployment overlap that the CNY 4,300 infrastructure subtotal does
not fully price.

## Deployment order

1. Provision the private managed ClickHouse instance in VPC
   `vpc-6wesy84ixw2esl6lb3ov5`, preferably zone `ap-northeast-1b`.
2. Create a database account with DDL rights only for schema initialization;
   create a separate read/write application account afterward.
3. Create an ACK Standard cluster, the small system node pool, and the Spot
   research node pool. Label research nodes `workload=backtest`. Do not place
   the trading runtime or its credentials in this cluster.
4. Publish the research image once per source revision. Parameter changes reuse
   the same immutable image and do not compile Rust again.
5. Apply the namespace, create the ClickHouse connection Secret, create the
   schema ConfigMap, and run the schema Job.
6. Validate a selected canonical Parquet partition, deterministically export it
   to `/work/data/input.ndjson` for the current runner, and stage that file in a
   local-cache PVC. Then create an Indexed backtest Job from the example
   manifest.
7. Upload result directories to OSS and let one aggregator import their
   manifests into DuckDB.

```bash
kubectl apply -f deployment/aliyun/research/k8s/namespace.yaml

kubectl -n monday-research create secret generic monday-clickhouse \
  --from-literal=host='REPLACE_PRIVATE_ENDPOINT' \
  --from-literal=port='9000' \
  --from-literal=user='REPLACE_SCHEMA_USER' \
  --from-literal=password='REPLACE_PASSWORD' \
  --from-literal=secure='false'

kubectl -n monday-research create configmap monday-clickhouse-schema \
  --from-file=schema.sql=deployment/aliyun/research/clickhouse/schema.sql

kubectl apply -f deployment/aliyun/research/k8s/clickhouse-schema-job.yaml

kubectl -n monday-research create configmap monday-backtest-config \
  --from-file=default.yaml=deployment/aliyun/research/backtest/default.yaml \
  --from-file=param_grid.yaml=rust_hft/config/backtest/param_grid.yaml
```

`backtest-job.example.yaml` is intentionally suspended and references two
operator-created PVCs:

- `monday-backtest-cache`: a selected, validated local dataset partition.
- `monday-backtest-results`: small result files that a single aggregator later
  uploads and records.

Do not mount the complete OSS bucket as the hot replay path. Stage only the
symbol/time partitions required by the current run.

## Current deployment gates

This bootstrap does not claim the complete research plane is live. Before
unsuspending the example Job, the following gates still need implementation or
cloud resources:

- A native canonical-Parquet reader for `hft-backtest`. Until that lands, the
  materializer must produce deterministic `input.ndjson` from a validated
  Parquet partition and bind both artifacts to the same manifest hash.
- A ClickHouse materializer/writer. The schema and schema Job are ready, but an
  empty schema is not an enabled analytics data path.
- A single result Aggregator that uploads immutable result manifests to OSS and
  performs the only DuckDB write transaction.
- Public trade prints and other execution-relevant streams. The current
  full-market archiver proves depth replay continuity; it does not yet make a
  trade-tape or funding/index-data claim.
- Actual managed ClickHouse, ACK, worker-PVC, and registry resources. They must
  not be created while the available account balance cannot cover the existing
  collector plus the selected deployment profile.

## Build discipline

Build once for each source revision:

```bash
docker buildx build \
  --platform linux/amd64 \
  --file rust_hft/deployment/docker/Dockerfile.research \
  --tag REPLACE_REGISTRY/research-runner:REPLACE_GIT_SHA \
  --push \
  rust_hft
```

Build the `linux/amd64` image on a native amd64 CI/ACR builder. Apple Silicon
Docker Desktop can validate an arm64 image locally, but compiling x86 Rust under
QEMU is slower and can fail inside the emulator even when the Dockerfile and
source are valid.

Keep the amd64 builder disposable, but attach one reusable 200 GiB PL1 ESSD as
`/build-cache`. Create it with `DeleteWithInstance=false`, format it only on the
first attachment, mount it by UUID from `/etc/fstab`, then run:

```bash
deployment/aliyun/research/builder/enable-persistent-build-cache.sh
```

The script refuses to overwrite an unrelated `/etc/docker/daemon.json`. Once
Docker reports `/build-cache/docker` as its root, the existing BuildKit cache
mounts in `Dockerfile.research` survive builder stop/recreation without adding
`cargo-chef` or `sccache`.

Example Tokyo disk lifecycle:

```bash
aliyun ecs CreateDisk \
  --RegionId ap-northeast-1 \
  --ZoneId REPLACE_BUILDER_ZONE \
  --DiskName monday-rust-build-cache \
  --DiskCategory cloud_essd \
  --PerformanceLevel PL1 \
  --Size 200

aliyun ecs AttachDisk \
  --RegionId ap-northeast-1 \
  --InstanceId REPLACE_BUILDER_INSTANCE_ID \
  --DiskId REPLACE_CACHE_DISK_ID \
  --DeleteWithInstance false
```

On first attachment only, identify the new empty device with `lsblk`, create an
ext4 filesystem, mount it at `/build-cache`, and persist its UUID in
`/etc/fstab`. On later builders, attach and mount the existing filesystem; do
not format it again.

The image contains eight stable entrypoints:

- `/usr/local/bin/hft-backtest`
- `/usr/local/bin/alpha-harness`
- `/usr/local/bin/lob-pit-materializer`
- `/usr/local/bin/binance-market-tape-slicer`
- `/usr/local/bin/binance-replay-parquet-materializer`
- `/usr/local/bin/monday-prediction-research`
- `/usr/local/bin/monday-prediction-evaluator`
- `/usr/local/bin/monday-prediction-snapshot`

`k8s/alpha-mission-job.example.yaml` is the current blessed BTCUSDT top-of-book
GP screening and baseline Mission against a pre-materialized PIT feature file.
It stops before the blocked Factor-Bank subset MCTS stage. The one-time signed
OSS URLs belong in a Kubernetes Secret and must never be committed. Use distinct
DuckDB files and result objects per parallel Mission; a later single-writer
aggregator may merge their immutable evidence.

`k8s/prediction-mission-job.example.yaml` uses the same image, restricted Pod
security context, signed-URL input transport, and immutable result upload for one
event-settlement mission. Its evaluator remains the prediction-specific
event-disjoint binary and the Job contains no exchange credential or execution
entrypoint. Mission v4 uses the built-in deterministic research profile and the
Job contains no LLM endpoint, model, API key, or provider environment.

Create a private submission JSON; never commit signed URLs. Include IDs,
digest-pinned image/evaluator, `standard-v1`, URL+SHA pairs, attempt-bound result,
catalog partition identity, and an optional complete resume pair. Snapshot
admission supplies the exact cohort, partition view, policy, snapshot, task, and
image identities injected into the Job. Render and review offline with
`alpha-harness prediction dispatch render --submission FILE --namespace NS`.
Submit with `alpha-harness prediction dispatch submit --submission FILE --context
CONTEXT --namespace NS`. The query-free result URL is the duplicate guard; each
Job has isolated storage. Treat rendered Secret `stringData` as sensitive.

Read Job and Pod milestones without mutation using `alpha-harness prediction
dispatch status --context CONTEXT --namespace NS --job-name JOB`. Snapshot-ready
and evaluator-started remain `null` unless `--evidence execution-evidence.json`
is supplied. Evidence is accepted only when its mission ID, mission SHA, and
snapshot SHA match the immutable Job annotations; a mismatch fails closed.

The URL Secret must always contain `resume-url` and `resume-sha256`; set both to
empty strings for the first attempt. A paused or failed runner still uploads its
results and append-only state to the attempt's immutable result URL before the
Job fails. To resume a paused run, create a new Job and a new result PUT URL,
then set `resume-url` and `resume-sha256` to the previous result bundle. The
harness restores only the bundle's `results/` state and the prediction LoopRun
revalidates its mission, policy, and snapshot identity before continuing.

ACK research workers are private and have no public NAT path. Sign feature,
materialization, and result URLs against the regional internal OSS endpoint
`https://oss-ap-northeast-1-internal.aliyuncs.com`; a public OSS URL will time
out from the worker pool. The result PUT signature must cover both
`Content-Type: application/zip` and `x-oss-forbid-overwrite: true`, matching the
native runner request. Delete the short-lived URL Secret after the Job reaches a
terminal state.

Treat Kubernetes completion as transport evidence only. Read `bundle_sha256`
from the Job's final JSON log, download the immutable result object, verify that
SHA-256 and `unzip -t`. For a continuous Mission, confirm every walk-forward
record reports `purged-walk-forward-v4`; a complete result may legitimately
contain zero sealed evaluations when no candidate passes. For a prediction
Mission, confirm `artifacts/execution-evidence.json` reports lane
`prediction_market`, the submitted mission and snapshot SHA-256 values, and the
mission-pinned evaluator version. A failed Job can still have a valid immutable
evidence bundle; use its runner exit code and LoopRun ledger to distinguish a
resumable pause from a terminal failure.

Run many parameter batches without rebuilding:

```bash
cargo run --manifest-path rust_hft/Cargo.toml --release -p hft-backtest -- \
  --config rust_hft/config/backtest/default.yaml \
  --grid rust_hft/config/backtest/param_grid.yaml \
  --shard-index 0 \
  --shard-count 8 \
  --output rust_hft/runs/backtest-sweep
```

The research image runs the same Rust binary directly and contains no Python
runtime. The Docker BuildKit cache preserves Cargo registry, git, and target
artifacts between image builds. ACK Jobs never run `cargo build` or `cargo run`.

`k8s/source-test-job.example.yaml` is the separate, non-production exception
for source validation. It uses only a digest-pinned `research-source-test`
image built from the current exact `main` SHA by the trusted ACR workflow. Its
entrypoint accepts only `binance-bstocks-attestation` and `bybit-spot`, each
running a fixed `cargo test --offline --locked` command. It has no mounted
Secret, ConfigMap, PVC, data input, or exchange credential; it cannot invoke
an execution binary, and the private image pull uses the namespace `monday-acr`
reference only. The template begins suspended, has a short terminal TTL, and
proves neither application runtime nor Testnet/Live authority. It is independent
of the suspended `monday-spot-cex-baseline-689`, which remains frozen pending
separate governance reconciliation by its owner.

The publisher accepts only the current `main` SHA and an allowed source-test
profile. It derives the immutable publication tag
`source-test-<40-hex-source-sha>-<profile>`, refuses to overwrite that exact
SHA/profile tag, and records the tag, profile, and immutable digest used by a
Job; it creates neither a service nor a persistent build cache. Historical
source-SHA-only tags remain read-only history and cannot be relabeled or reused
as proof for another profile. The Job TTL cleans up its Pod and Job after the
receipt. Registry retention or removal remains an ACR-owner action after that
receipt. Each receipt is evidence only for the exact source revision and
verified profile named by its image; later commits or different profiles require
a newly published digest.

Because the selector admits source-test publication only from `refs/heads/main`,
the workflow-level `acr-publish-${{ github.ref }}` concurrency group with
`cancel-in-progress: false` serializes its tag-absence probe, build,
verification, push, and digest readback. A later dispatch cannot reach the same
tag's push until the first has finished, at which point the absence probe fails
closed. Personal Edition has no immutable-image-tag setting, so this publisher
lock and its exact digest record are the CI-path protection; registry writer
access remains a separate ACR-owner boundary.
