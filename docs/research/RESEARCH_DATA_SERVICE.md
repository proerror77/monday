# Shared ACK research data service

Issue [#1256](https://github.com/proerror77/monday/issues/1256) separates reusable
market data from an experiment's seed, model, and budget. `research-data-service`
is a Rust controller CLI for verified incremental ingestion and immutable
numerical views. All payload reads and writes run inside ACK. It does not submit
a Campaign, fit a model, open a sealed holdout, or write a research ledger.

The implemented boundary is:

```mermaid
flowchart LR
    C[Verified canonical market exports] --> Q[Admitted append-only receipt queue]
    Q --> I[One ACK ingestion controller]
    I --> H[ClickHouse typed features / separate targets]
    A[Agent DataRequest] --> V[Pinned partition union and time view]
    H --> V
    V --> P[Verified immutable Parquet shards and optional anchors]
    P --> M[Native ML readers and explicit prepared-cohort admission]
```

The existing raw collector verifier and `lob-pit-materializer` still own OSS
discovery, `_SUCCESS`, source/reference verification, point-in-time construction,
and normalized market exports. This CLI does **not** discover raw folders or
infer another normalization policy. The native publication bridge described
below enqueues those existing verified exports. The native coordinator needs
the explicit background data-plane configuration and its bounded scheduled
invocation before continuous OSS ingestion can be claimed. The opt-in publisher
hook and explicit prepared-cohort admission branch below provide that source
connection; their code alone does not prove deployment or real Campaign acceptance.

## Deployment and ownership

The executable requires `MONDAY_ACK_DATA_PLANE=1`. The published `--version`
reports its compiled `MONDAY_SOURCE_REVISION`; `source-unbound` is unsuitable for
production admission. The environment flag is an execution guard, not a signed
grant or an authentication mechanism.
Preparation also requires `MONDAY_DATA_PLATFORM_IMAGE` containing the actual
immutable converter image reference. The `monday.market_ready_receipt.v2` binds
the compiled source revision and that image; an unbound producer cannot publish
ready inputs. Kubernetes admission/readback must verify this environment value
against the actual running image. Campaign consumers pin the independently
approved converter source/image rather than trusting a caller-supplied string.

Use one controller Deployment and an ACK block-disk state root, for example
`/work/data-service`. The controller account owns `catalogue`, `claims`, `views`
and `queue-cursor.json`. Advisory locks protect cooperating processes on that
same filesystem; they do not provide cross-node consensus. Agents receive a
request tool and read-only published views, rather than write access to the
state root or raw ClickHouse credentials. No private ledger or signing key is
stored in this data service.

The common invocation is:

```text
research-data-service --state-root /work/data-service \
  --clickhouse-url http://monday-clickhouse.monday-research.svc:8123 \
  --database monday_analytics --user <admitted-role> <command>
```

`CLICKHOUSE_PASSWORD` supplies the role's password and is never printed. The
ingestion role needs bounded insert/read access to `cex_market_datasets`,
`cex_market_feature_frames`, and `cex_market_target_frames`. The ordinary
preparation role reads the feature table and registry; the separately admitted
supervised preparation role also reads targets. The service's HTTP queries set
bounded memory/result settings; configure fixed limits or constrained settings
with a read-only profile that permits those limits (`readonly=2`).

## Canonical ingestion receipts

An admitted `monday.market_ingest_receipt.v1` JSON document contains:

| Field | Contract |
| --- | --- |
| `sequence`, `previous_receipt_sha256` | Contiguous append-only queue position and hash of the preceding canonical JSON receipt; the first receipt is position 1 with a null predecessor. |
| `features` | Relative canonical feature manifest path and independent SHA-256. Only `monday.market_features.v1` source exports are accepted. |
| `targets` | Optional relative target manifest and SHA-256, bound to that exact feature dataset. |
| `source_manifest`, `source_manifest_sha256` | Pinned existing `monday.market_feature_sources.v1` file, original source revision and collector manifest/success-marker identities, source clocks and observed coverage. |
| `transform_sha256` | Coordinator-pinned normalization/schema implementation identity. |
| `allowed_ranges` | Ordered disjoint half-open `{start_ms,end_ms}` ranges admitted by the canonical coordinator. |
| `pre_holdout_supervised` | Explicit target admission; it must be true exactly when targets are present. |

The coordinator that supplies and seals these receipts must validate the actual
research grant and exclude sealed holdout windows. A receipt is not itself a
cryptographic grant. Every ingested feature observation is checked against its
admitted ranges before insertion. Every target observation **and its maturity
clock** must lie in one admitted range before any target write. Inserting a
broader supervised source and relying on a later query filter is rejected.

`ingest --receipt <file> --receipt-sha256 <hash> --artifact-root <mount>` verifies
the source manifest bytes, source-shard SHA-256, actual decoded clocks/counts,
causal availability and canonical recovery timestamp series IDs. Source lines
are rehashed while decoding, so a concurrent source change cannot commit.
Rows are inserted in batches of at most 4,096. Float32 channels and targets are
typed columns; no feature JSON column is used for the numerical training path.

The controller then reads **all actual numeric columns** back with paginated
RowBinary queries. It checks projected row identities, row count, clock order
and an ordered digest over exact integer/Float32/Float64 bits. A stored checksum
alone cannot make a dataset ready. Only after this passes does it insert the
complete registry version, read that registry back independently, and publish
the immutable catalogue entry. Interrupted inserts remain unavailable through
the request API; a retry reinserts identical keys and verifies the complete
content. It never declares partial rows complete.

`drain --queue <file> --queue-sha256 <hash> --artifact-root <mount>
--max-partitions 16` accepts a bounded JSON array of at most 1,024 receipts. It
verifies the full chain and previously committed cursor identity, processes at
most the requested number of new receipts, and atomically advances the cursor
only after each verified catalogue commit. Repeated unchanged drains perform
no source discovery or old-partition materialization. A source repair creates a
new content identity and receipt; it does not overwrite an old ready version.
Queue rollover/recovery requires an explicitly reviewed cursor/queue handoff;
truncation or rewriting is rejected rather than silently resetting progress.
At hourly receipt cadence, the 1,024-entry bound is about 42 days. Automatic
queue rollover is a later controller capability; current operators must retain
and hand off the prior cursor/hash chain before reaching that bound.

## Native publication bridge

`enqueue` accepts the already published native `campaign-inputs.json` and
`materialization-receipt.json`, with independent hashes for each, the published
run root, the global artifact root, and a controller-owned admission document:

```text
research-data-service --state-root /work/data-service enqueue \
  --campaign-inputs <run-root>/receipts/campaign-inputs.json \
  --campaign-inputs-sha256 <pinned-hash> \
  --materialization-receipt <run-root>/receipts/materialization-receipt.json \
  --materialization-receipt-sha256 <pinned-hash> \
  --run-root <run-root> --artifact-root <global-published-output-root> \
  --admission <controller-admission.json> --admission-sha256 <pinned-hash>
```

The admission schema is `monday.market_data_admission.v1`, containing
`transform_sha256`, ordered `allowed_ranges`, and `pre_holdout_supervised`.
Its authority comes from the existing canonical coordinator's grant checks;
the CLI does not invent or bypass a grant. It verifies the paired publication
receipts, immutable image digest, published path ownership, frozen inventory
hash, original PIT snapshot/report identity, and pinned feature/source/target
manifest identities. The bridge does not rerun normalization or scan OSS.
When the native invocation sets optional market feature start/end clocks,
manual `enqueue` calls must also provide the same
`--expected-feature-start-received-at-ns` / `--expected-feature-end-received-at-ns`
values. The publisher hook forwards them automatically. A changed or omitted
optional clock cannot silently reuse the old fixed inventory's export.

Under the sole controller lock it atomically replaces
`<state-root>/receipt-queue.json`, then independently reads and verifies its
sequence/hash chain. Retry of the same content/admission returns the existing
receipt and queue hash, even when the same immutable objects have another
published locator. A conflicting transform/target/admission fails without
modifying the queue. Consumer run IDs and optimizer parameters are excluded
from the ingestion receipt; the canonical data producer must continue to use
stable partition production rather than reconstructing data per experiment.
`enqueue` returns `queued`, not ready; `drain` performs the actual payload and
ClickHouse verification. A failed publication callback can retry that same
native publication identity without creating a second data partition.

The native `cex-materialization-entrypoint.sh` enables the callback only when
the fixed background job supplies all three
`MONDAY_DATA_PLATFORM_STATE_ROOT`, `MONDAY_DATA_PLATFORM_ADMISSION`, and
`MONDAY_DATA_PLATFORM_ADMISSION_SHA256` values, plus
`MONDAY_ACK_DATA_PLANE=1`. State must be on the admitted `/work` block disk;
source jobs must request canonical SOL USDM market encoder exports. Slice
shards cannot own the shared publisher. Optional endpoint/user are
`MONDAY_DATA_PLATFORM_CLICKHOUSE_URL` and `MONDAY_DATA_PLATFORM_USER`
(default `monday_writer`); credentials stay in the existing secret environment.

After immutable native publication, the hook enqueues the verified source and
drains at most 16 new partitions. An unchanged complete published inventory
retries through those exact receipts without raw discovery, slicing, or
normalization, even when its temporary work directory has disappeared. A
failed data callback preserves the native publication identity and queue cursor
for retry. A remaining backlog reports `preparing` with `pending_partitions`;
it is not a failed attempt or evidence of a complete requested view. The
scheduled background controller must retain one writer and process new
predeclared sealed inventories independently of consumer experiments.

## Fixed Agent DataRequest

`request --request <file> --request-sha256 <hash>` accepts only the
`monday.market_data_request.v1` fields below. Unknown fields, including consumer
experiment IDs and optimizer seeds, are rejected.

| Field | Contract |
| --- | --- |
| `sources` | Up to 512 ordered disjoint `{feature_dataset_sha256,target_dataset_sha256}` partitions. Daily/hourly versions are reusable, so adding a day ingests the new source partition rather than rewriting previous days. |
| `transform_sha256`, `input` | Exact normalization identity and ordered channel/context/cadence specification. Every selected source must agree. |
| `view` | Existing `SequenceViewV1`: history start, decision start, exclusive end, and decision stride. |
| `anchor_end_ms` | Exclusive end of eligible decision anchors. |
| `purpose` | `label_free` or `pre_holdout_supervised`; no sealed-holdout purpose exists. |
| `qualified_anchors` | Whether the immutable training grid should be prepared. Evaluation can retain all frames without creating a training anchor index. |

Label-free requests must omit target identities. They make no target query and
create no target file. Supervised requests must pin a matching admitted target
identity for every source. The source union is ordered and disjoint; duplicate,
overlapping, out-of-order, unrelated and out-of-grant partitions are blocked.
Actual canonical timestamp series IDs are preserved. A gap or series boundary
clears the causal context; the preparer never joins different recovery series.
The first actual observation must match the requested history start, and the
actual trailing watermark must cover the exclusive end. A request for 14 days
cannot silently return only its final day. Interior gaps are reported and
invalidate the corresponding causal contexts; they are never interpolated.

The canonical request digest is the shared preparation key. A missing source
returns `preparing`; an active owner of the same preparation claim also returns
`preparing`. Invalid admission, corruption, budget excess or incomplete end
coverage returns `blocked`. Only a verified immutable directory returns `ready`.

On first preparation, full source registry/content checks precede bounded view
reads. The service writes typed Parquet shards, independently decodes every
new shard and compares all integer/float bits to the verified query values.
Qualified anchors require a complete causal context on the declared time grid;
supervised anchors additionally require a matching mature target. Anchor counts
remain within the existing 32,768 training limit. The service publishes source
union provenance, actual series/gaps, feature/target manifests, optional anchors,
and a prepared-view digest, then atomically renames the staging directory. No
partially written directory can answer `ready`.

On repeat requests, the controller checks the pinned ready/manifests/anchor
identities and a separate physical cache certification bound to the prepared
view hash. Each block's device, inode, byte size, ctime and mtime must agree.
Unchanged fingerprints need no payload read or database connection. A changed
fingerprint or restored cache acquires one certification owner and requires
full SHA-256 and a complete typed decoding pass before recertification; a
same-size corruption fails. Certification never changes the immutable ready
receipt or dataset identity. These filesystem observations are an optimization
for controller-owned ACK block storage, not cryptographic proof against a
privileged filesystem writer. Agents and training workers must have read-only
access to the immutable blocks. The hot path does not discover OSS, align raw
events, or cut another dataset. Numerical readers also verify the published
shard content during their bounded decoding passes. The Parquet
manifest identities can therefore survive changes in experiment seed, batch
size, or model. Causal windows are constructed in memory in the reader; they
are not expanded into repeated 60-row disk windows.

Control-plane stdout is capped at 256 KiB. A ready response contains row/series/
gap counts, first/last observed coverage, at most 32 gap previews, and the pinned
`prepared-view.json` reference. Full partition/series/gap evidence remains in
the ACK artifact and is retrieved through the artifact protocol, rather than
being copied into workstation logs.

## Explicit prepared Campaign cohort

The prepared cohort path uses `monday.sol_market_encoder_cohort_request.v2` with
the original native `training_receipts` retained and an additional binding:

```json
{
  "prepared_training": {
    "ready_receipt": {
      "file": "shared/views/<fixed-request-hash>/_READY.json",
      "sha256": "<independently verified ready-receipt hash>"
    },
    "producer_source_revision": "<approved 40-hex converter source>",
    "producer_image": "<approved converter image>@sha256:<image-digest>"
  }
}
```

This is the additional block within the existing cohort request, rather than a
standalone replacement for its identity or native training receipts. Paths are
relative to the admitted input root. The fixed data request for this path uses
`purpose: "pre_holdout_supervised"`, the full ordered SOL 24-channel input,
the exact registered native training view, `anchor_end_ms = view.end_ms - 30000`
and `qualified_anchors: false`. Campaign admission independently derives and
binds its qualified anchors.

The existing composition command accepts this explicit v2 request:

```text
alpha-harness mission prepare-sequence-cohort \
  --request /work/request.v2.json --input-root /work/admitted-inputs \
  --output-root /work/prepared-cohort --inputs-out /work/cohort-inputs.v2.json
```

The admitted input root must contain the pinned native receipts/artifacts and
the ready prefix. It cannot resolve through symlinks or path traversal. The
original validation receipt and contiguous replay remain unchanged.

The new consumer verifies the original native receipt/source index, exact
converter source/image and complete decoded feature/target equivalence before
producing `Inputs.v2` / `SourceIndex.v2`. It preserves integer clocks, recovery
series, Float32 values, target availability, source gaps and point-in-time
lineage. Prepared shards do not replace the source grant, bypass source
verification, or create another Campaign completion entrypoint. The native
Campaign remains freeze → finalize → dispatch → generated execute, with its
existing cumulative scientific budget and terminal-result requirements.

## Resource and evidence limits

Each ClickHouse query has at most 4,096 rows, an 8 MiB response cap and a 256 MiB
query-memory limit. Canonical JSONL frames are capped at 32 KiB and metadata at
16 MiB. Prepared numerical shards and row groups follow
`prepared_market` limits (16 MiB / 4,096 rows). The request union has bounded
partition, coverage, gap and training-anchor metadata, and at most 14 days of
decisions plus the bounded causal-context/label edges. These are application bounds; cgroup memory,
query slots, disk capacity and concurrent preparation slots still need explicit
ACK deployment limits.

Published views are immutable replayable inputs, not optimizer checkpoints.
They grant no training trial, GPU, live runtime, sealed evaluation or trade
authority. The canonical Campaign still owns admission, budget, source/image
binding, settlement and independent result readback. Existing DuckDB remains
the single-owner control/evidence ledger.

An empty ClickHouse service, passing unit test, generated Parquet file or
successful preparation is not a completed scientific experiment. Deployment
acceptance requires real source ingestion and corruption/restart tests, the
real 14-day view's actual gaps/coverage, a repeat ready hit, and measured ACK
read/prepare timings. Keep Issue #1256 open until those runtime receipts and the
upstream incremental scheduling integration pass.

Focused ACK checks:

```text
cargo test --locked -p hft-collector --bin research-data-service data_service_
cargo clippy --locked -p hft-collector --bin research-data-service --no-deps -- -D warnings
```

Source tests cover fixed view identity, cursor-chain rewrites, atomic enqueue
deduplication/admission conflicts, exact RowBinary
float bits/value tampering, pre-holdout maturity, path traversal, line limits,
and ready reuse/cache restoration/same-size corruption without an available
database. Their fixtures are tests only;
they must never substitute for the real cloud ingestion acceptance run.

The existing `test-cex-materialization-entrypoint.sh` additionally checks
partial background configuration rejection, a failed data drain after native
publication, and restart through the same receipts with raw/reference roots
unavailable. Its positive callback fixtures require `TMPDIR` under the admitted
ACK `/work` tree; a non-ACK run explicitly skips those cases. These orchestration
doubles do not exercise real ClickHouse or substitute for source ingestion.
