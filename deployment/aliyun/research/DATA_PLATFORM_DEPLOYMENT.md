# Research data plane deployment (#1256)

This is a deployment contract, not a deployment receipt. The implementation is
under ACK validation; a successful Kubernetes dry run does not validate SQL,
Rust binaries, data equivalence, throughput, or recovery.

## Target and ownership

- Cluster: `c4d1db514b47a4c40995d2ec4bcc8c0ca`, context
  `monday-research-apne1`, namespace `monday-research`, Tokyo.
- Contract: `research-data-platform-1256`. One named controller owns its live
  mutation journal and source/build/runtime receipts. Record controller and
  concrete resource UIDs before starting the transition.
- Shared database: `monday-clickhouse`, internal HTTP/native ports, database
  `monday_analytics`. Start with one CPU instance and retained 100 GiB ESSD.
  Raw LOB remains in OSS; this capacity is for canonical numerical inputs.
- The database and its request/ingest controller use a separately owned
  `workload=research-data` node pool. Build/normalization and scientific training
  remain on the appropriate `workload=backtest` workers. A Study's pool cleanup
  must not release the shared database or its state volume.
- Preserve existing SOL and other research PVCs. The new shared service has
  its own lifetime and budget; never attach it to a scientific Study's cleanup.
- No trading/collector cutover, GPU, shared writable DuckDB, or model-fit budget
  is granted by creating this data plane.

## Admission and resource window

Before a paid resource is created, record the actual independent platform budget,
absolute deadline and cleanup reserve (bounded pilot), or the approved daily cap
and ongoing service owner (persistent deployment). Refresh purchase eligibility,
worker/storage prices and available capacity. Do not reuse an expired SOL lease.

On 2026-09-30, ACK had zero nodes and no deployed ClickHouse. The ECS price query
returned `INSUFFICIENT_BALANCE_FOR_POSTPAY_ORDER`: the account did not meet the
CNY 100 balance threshold. The quoted Spot worker with 40 GiB system + 100 GiB
work disks was CNY 0.341184/hour; that is neither a committed price nor a complete
  service cost. No paid startup is admissible until purchase eligibility and the
  independent budget are resolved.

An isolated on-demand 2 vCPU / 8 GiB data node with 40 GiB PL0 system + 20 GiB PL1
work disks was quoted at CNY 0.736168/hour on the same date. Database and 40 GiB
prepared-state PVCs are additional costs. Prefer a persistent data pool for the
shared service and disposable Spot backtest workers for builds/normalization;
freeze refreshed full costs before selecting the actual lifetime.

Use a CPU `workload=backtest` worker for builds. Source, toolchains, mutable build state,
data, verification and durable outputs stay in ACK block storage. Never compile
or stage source on an `ack-system` node or download market data to the Mac.
The Mac may submit controls and read bounded receipts. Validate available `/work`
space, build user namespaces, cgroup limits and rootless cleanup before compiling.

## Software evidence

1. Freeze the source commit and code archive SHA. Upload code only; verify and
   extract the archive in ACK using `research-data-build-job.example.yaml`.
   Use a task-owned build PVC and new Job. Read its UID and terminal evidence.
2. The initial Job exports tested binaries to its ACK PVC through the `binaries`
   target of `Dockerfile.research-data`; it does not push a runtime image.
   Read back the exact source, lock/toolchain, test verdict and binary checksums.
3. Complete the current-source repository CI/review/merge controls. Production
   publication requires current main and the three authenticated GitHub checks
   in [publication policy](../../../docs/agents/publication.md). An ACK test or
   local commit does not satisfy those checks. Research validation and builds
   must execute in ACK; an unavailable ACK CI executor is a blocker, not a reason
   to silently use a workstation or a GitHub-hosted compiler.
4. Build the admitted `runtime` target in ACK, push to the established ACR, and
   read its immutable digest, OCI source label, binary version and smoke receipt.
   An annotation alone is not provenance. Bind the deployment to that receipt.
5. Clean the task's builder state only after publishing durable receipts. Rootless
   mapped UID files need an observed cleanup result; a shell trap is not proof.

## Database and input deployment

Arm failure cleanup before the first mutation. Read and compare prior target
UIDs/config/source/digest immediately before each change. For a first deployment,
the rollback is replicas zero and suspended task Jobs; preserve new data PVCs and
recovery receipts. Release only task-owned capacity by exact identity. Do not
reset another controller's ASG or delete borrowed study volumes.

Create independent strong secret values for admin, writer, feature reader and
supervised task reader. The admin secret is confined to schema/maintenance Jobs.
Agents submit fixed data requests and do not receive database credentials.
Load `clickhouse/config.xml` and `users.xml` into `monday-clickhouse-config` and
the reviewed schema into an immutable SHA-named ConfigMap.

Apply `k8s/research-data-clickhouse.yaml` with replicas zero, then admit one
replica. Network access is internal and restricted to admitted Pods. Verify real
UID/image ID, persistent mount, `/ping`, SQL roles and bounded query settings.
Execute a new `research-data-schema-job.example.yaml`, read its terminal output,
and compare actual table columns/engines/keys with the Rust contracts.

The [data service](../../../docs/research/RESEARCH_DATA_SERVICE.md) consumes
pinned canonical exports through a single-owner receipt queue, validates actual
content before publishing complete, and prepares immutable Parquet views.
The existing raw/reference verifier and PIT materializer remain responsible for
OSS sealing, normalization and causal alignment. Wire their successful publication
receipts into the queue; merely installing this binary does not create background
OSS acquisition. Fixed requests use `research-data-request-job.example.yaml`,
their own immutable request ConfigMap, and the controller's ACK state volume.
  RWO state must have one ingestion owner; colocate callers or route requests
  through that owner. Never treat a per-node lock as a distributed lease.

Existing Campaign source indices and original receipt verification remain strict.
The Parquet reader and full-pass numerical equivalence helpers are implemented,
but conversion must separately bind the native SourceIndex, prepared service
receipt, converter source/image, view and anchor identities before production
Campaign admission. Do not alter a signed old cohort in place. Format selection
in the ML reader alone is not proof of Campaign integration.

## Runtime gate and closeout

First verify a real admitted SOL partition: source sealing and hash binding,
insert, actual contents, incomplete retry, repeated request, same-size corruption,
and label isolation. Then bound the 14-day backfill and report true gaps/coverage.
Compare native and prepared samples (including float bits, series and clocks),
anchors, train-only scaling and labels before training. Charge any actual fit to
its existing signed scientific budget; do not manufacture a new fit allowance.

Measure first verified batch, full scan, second epoch/new seed, memory/network
bytes and 1/10/30/100 read requests with independent query/training slots. Verify
incremental new-day work and restoration of an old pinned view after restart.
Publish small runtime/readback receipts and immutable restoration evidence in
ACK/OSS. Close #1256 only after its runtime acceptance, not after a PR merge,
PVC Bound, Pod Running, schema Job Complete, or a count query.
