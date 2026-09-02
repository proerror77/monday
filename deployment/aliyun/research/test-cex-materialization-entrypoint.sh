#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
ENTRYPOINT=$SCRIPT_DIR/scripts/cex-materialization-entrypoint.sh
JOB_TEMPLATE=$SCRIPT_DIR/k8s/cex-materialization-job.example.yaml

[ "$(grep -c ' /reference/lake/raw$' "$JOB_TEMPLATE")" -eq 2 ]
! grep -q ' /lake/reference$' "$JOB_TEMPLATE"
grep -q '^  activeDeadlineSeconds: 7200$' "$JOB_TEMPLATE"

for tool in awk find sed sha256sum mktemp chmod grep; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf 'missing required tool: %s\n' "$tool" >&2
    exit 1
  }
done

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
RAW_ROOT=$ROOT/raw
REF_ROOT=$ROOT/reference
OUT_ROOT=$ROOT/output
WORK_ROOT=$ROOT/work
BIN_DIR=$ROOT/bin
mkdir -p "$RAW_ROOT" "$REF_ROOT" "$OUT_ROOT" "$WORK_ROOT" "$BIN_DIR"

write_triplet() {
  base=$1
  data_content=$2
  manifest_content=$3
  mkdir -p "$(dirname "$base")"
  printf '%s\n' "$data_content" >"$base"
  printf '%s\n' "$manifest_content" >"$base.manifest.json"
  data_sha=$(sha256sum "$base" | awk '{print $1}')
  printf '%s\n' "$data_sha" >"$base._SUCCESS"
  manifest_sha=$(sha256sum "$base.manifest.json" | awk '{print $1}')
  printf '%s|%s\n' "$data_sha" "$manifest_sha"
}

raw1_rel=venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=01/part-1.jsonl.zst
raw2_rel=venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=02/part-2.jsonl.zst
ref1_rel=venue=binance_usdm/dataset=reference/date=2026-08-18/hour=01/batch=001/reference.ndjson
ref2_rel=venue=binance_usdm/dataset=reference/date=2026-08-18/hour=02/batch=002/reference.ndjson

raw1=$(write_triplet "$RAW_ROOT/$raw1_rel" raw-segment-1 raw-manifest-1)
raw2=$(write_triplet "$RAW_ROOT/$raw2_rel" raw-segment-2 raw-manifest-2)
ref1=$(write_triplet "$REF_ROOT/$ref1_rel" ref-segment-1 ref-manifest-1)
ref2=$(write_triplet "$REF_ROOT/$ref2_rel" ref-segment-2 ref-manifest-2)

cat >"$ROOT/inventory.env" <<EOF
RUN_ID=test-run-1
SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567
IMAGE_REF=registry/research-runner@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
MISSION_ID=data-btcusdt-usdm-test
MARKET=usdm
SYMBOL=BTCUSDT
BUCKET_MS=1000
LABEL_HORIZON_BUCKETS=5
TOP_DEPTH=5
OUTPUT_PREFIX=test-run-1
RAW_SEGMENT_COUNT=2
RAW_SEGMENT_1=$raw1_rel
RAW_SEGMENT_1_SHA256=$(printf '%s' "$raw1" | awk -F'|' '{print $1}')
RAW_SEGMENT_1_MANIFEST_SHA256=$(printf '%s' "$raw1" | awk -F'|' '{print $2}')
RAW_SEGMENT_2=$raw2_rel
RAW_SEGMENT_2_SHA256=$(printf '%s' "$raw2" | awk -F'|' '{print $1}')
RAW_SEGMENT_2_MANIFEST_SHA256=$(printf '%s' "$raw2" | awk -F'|' '{print $2}')
REFERENCE_COUNT=2
REFERENCE_1=$ref1_rel
REFERENCE_1_SHA256=$(printf '%s' "$ref1" | awk -F'|' '{print $1}')
REFERENCE_1_MANIFEST_SHA256=$(printf '%s' "$ref1" | awk -F'|' '{print $2}')
REFERENCE_2=$ref2_rel
REFERENCE_2_SHA256=$(printf '%s' "$ref2" | awk -F'|' '{print $1}')
REFERENCE_2_MANIFEST_SHA256=$(printf '%s' "$ref2" | awk -F'|' '{print $2}')
EOF

cat >"$BIN_DIR/binance-market-tape-slicer" <<'EOF'
#!/bin/sh
set -eu
segment=
output_dir=
while [ $# -gt 0 ]; do
  case "$1" in
    --segment) segment=$2; shift 2 ;;
    --output-dir) output_dir=$2; shift 2 ;;
    --symbols) shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$segment" ] || exit 2
[ -n "$output_dir" ] || exit 2
mkdir -p "$output_dir"
slice=$output_dir/slice-001.jsonl.zst
printf 'slice:%s\n' "$(basename "$segment")" >"$slice"
printf 'slice-manifest\n' >"$slice.manifest.json"
slice_sha=$(sha256sum "$slice" | awk '{print $1}')
printf '%s\n' "$slice_sha" >"$slice._SUCCESS"
cat <<JSON
{
  "source": {},
  "slices": [
    {
      "file": "slice-001.jsonl.zst",
      "sha256": "$slice_sha",
      "manifest_sha256": "$(sha256sum "$slice.manifest.json" | awk '{print $1}')",
      "symbols": ["BTCUSDT"]
    }
  ]
}
JSON
EOF

cat >"$BIN_DIR/lob-pit-materializer" <<'EOF'
#!/bin/sh
set -eu
artifact_dir=
while [ $# -gt 0 ]; do
  case "$1" in
    --artifact-dir) artifact_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$artifact_dir" ] || exit 2
mkdir -p "$artifact_dir"
feature=$artifact_dir/feature-test.jsonl
report=$artifact_dir/materialization-test.materialization.json
printf 'feature-row\n' >"$feature"
printf 'verified-reference-evidence\n' >"$artifact_dir/decoy.reference.data"
feature_sha=$(sha256sum "$feature" | awk '{print $1}')
cat >"$report" <<JSON
{
  "artifact_path": "$feature",
  "artifact_sha256": "$feature_sha"
}
JSON
report_sha=$(sha256sum "$report" | awk '{print $1}')
cat <<JSON
{
  "report": {
    "artifact_path": "$feature",
    "artifact_sha256": "$feature_sha"
  },
  "report_path": "$report",
  "report_sha256": "$report_sha"
}
JSON
EOF

cat >"$BIN_DIR/binance-replay-parquet-materializer" <<'EOF'
#!/bin/sh
set -eu
artifact_dir=
while [ $# -gt 0 ]; do
  case "$1" in
    --artifact-dir) artifact_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$artifact_dir" ] || exit 2
mkdir -p "$artifact_dir"
artifact=$artifact_dir/replay-test.parquet
manifest=$artifact_dir/replay-test.canonical-manifest.json
printf 'parquet\n' >"$artifact"
artifact_sha=$(sha256sum "$artifact" | awk '{print $1}')
cat >"$manifest" <<JSON
{
  "artifact_path": "replay-test.parquet",
  "artifact_sha256": "$artifact_sha"
}
JSON
manifest_sha=$(sha256sum "$manifest" | awk '{print $1}')
cat <<JSON
{
  "manifest": {
    "artifact_path": "replay-test.parquet",
    "artifact_sha256": "$artifact_sha"
  },
  "manifest_path": "$manifest",
  "manifest_sha256": "$manifest_sha"
}
JSON
EOF

chmod +x "$BIN_DIR/binance-market-tape-slicer" "$BIN_DIR/lob-pit-materializer" "$BIN_DIR/binance-replay-parquet-materializer"

sh "$ENTRYPOINT" \
  --inventory "$ROOT/inventory.env" \
  --raw-root "$RAW_ROOT" \
  --reference-root "$REF_ROOT" \
  --output-root "$OUT_ROOT" \
  --work-dir "$WORK_ROOT" \
  --binary-dir "$BIN_DIR" >/dev/null 2>"$ROOT/run.log"

grep -Fq "progress_event raw_verification \"\$seen\" \"\$verify_total\" 10" "$ENTRYPOINT"
grep -Fq "progress_event reference_verification \"\$i\" \"\$reference_count\" 50" "$ENTRYPOINT"
grep -Fq "progress_event slicing \"\$i\" \"\$raw_segment_count\" 10" "$ENTRYPOINT"
for event in \
  'event=stage_start run_id=test-run-1 stage=raw_verification current=0 total=2' \
  'event=stage_progress run_id=test-run-1 stage=raw_verification current=2 total=2' \
  'event=stage_complete run_id=test-run-1 stage=raw_verification current=2 total=2' \
  'event=stage_start run_id=test-run-1 stage=reference_verification current=0 total=2' \
  'event=stage_progress run_id=test-run-1 stage=reference_verification current=2 total=2' \
  'event=stage_complete run_id=test-run-1 stage=reference_verification current=2 total=2' \
  'event=stage_start run_id=test-run-1 stage=slicing current=0 total=2' \
  'event=stage_progress run_id=test-run-1 stage=slicing current=2 total=2' \
  'event=stage_complete run_id=test-run-1 stage=slicing current=2 total=2' \
  'event=stage_start run_id=test-run-1 stage=pit_materialization current=0 total=1' \
  'event=stage_complete run_id=test-run-1 stage=pit_materialization current=1 total=1' \
  'event=stage_start run_id=test-run-1 stage=replay_materialization current=0 total=1' \
  'event=stage_complete run_id=test-run-1 stage=replay_materialization current=1 total=1' \
  'event=stage_start run_id=test-run-1 stage=receipt_build current=0 total=1' \
  'event=stage_complete run_id=test-run-1 stage=receipt_build current=1 total=1' \
  'event=stage_start run_id=test-run-1 stage=publish current=0 total=1' \
  'event=stage_complete run_id=test-run-1 stage=publish current=1 total=1'; do
  grep -Fq "$event" "$ROOT/run.log"
done
grep -Fq 'schema_version=monday.research_event.v1 component=cex-materialization event=run_start run_id=test-run-1 mission_id=data-btcusdt-usdm-test market=usdm symbol=BTCUSDT' "$ROOT/run.log"
grep -Fq 'event=stage_progress run_id=test-run-1 stage=raw_verification current=2 total=2 last_index=2 content_sha256=' "$ROOT/run.log"
grep -Fq 'event=stage_progress run_id=test-run-1 stage=reference_verification current=2 total=2 last_index=2 data_sha256=' "$ROOT/run.log"
grep -Fq 'event=stage_progress run_id=test-run-1 stage=slicing current=2 total=2 last_index=2 slice_sha256=' "$ROOT/run.log"
grep -Fq 'event=stage_complete run_id=test-run-1 stage=pit_materialization current=1 total=1 feature_sha256=' "$ROOT/run.log"
grep -Fq 'event=stage_complete run_id=test-run-1 stage=replay_materialization current=1 total=1 replay_artifact_sha256=' "$ROOT/run.log"
grep -Fq 'event=artifact_publish_complete run_id=test-run-1 stage=publish artifact=campaign_inputs' "$ROOT/run.log"
grep -Fq 'event=run_complete run_id=test-run-1 mission_id=data-btcusdt-usdm-test feature_sha256=' "$ROOT/run.log"

RUN_ROOT=$OUT_ROOT/test-run-1
[ -f "$WORK_ROOT/staged-output/artifacts/materialization/decoy.reference.data" ]
[ ! -e "$RUN_ROOT/artifacts/materialization/decoy.reference.data" ]
[ -f "$RUN_ROOT/receipts/campaign-inputs.json" ]
[ -f "$RUN_ROOT/receipts/materialization-receipt.json" ]
[ -f "$RUN_ROOT/receipts/frozen-inventory.env" ]
[ -f "$RUN_ROOT/artifacts/materialization/feature-test.jsonl" ]
[ -f "$RUN_ROOT/artifacts/replay/replay-test.parquet" ]
[ -f "$RUN_ROOT/artifacts/replay/replay-test.canonical-manifest.json" ]
published_report=$(find "$RUN_ROOT/artifacts/materialization" -maxdepth 1 -type f -name '*.materialization.json')
[ -n "$published_report" ]
published_report_sha=$(sha256sum "$published_report" | awk '{print $1}')
[ "$(basename "$published_report")" = "$published_report_sha.materialization.json" ]
grep -q "\"artifact_path\": \"$RUN_ROOT/artifacts/materialization/feature-test.jsonl\"" "$published_report"
if find "$RUN_ROOT" -type f -name '*.reference.*' | grep -q .; then
  printf 'local reference evidence was unexpectedly published\n' >&2
  exit 1
fi
grep -q '"schema_version": "monday.cex_campaign_inputs.v1"' "$RUN_ROOT/receipts/campaign-inputs.json"
grep -q '"readback_scope": "same-mounted-ossfs-prefix"' "$RUN_ROOT/receipts/campaign-inputs.json"
grep -q '"output_object_base_url": "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization"' "$RUN_ROOT/receipts/campaign-inputs.json"
grep -q '"relative_path": "artifacts/materialization/feature-test.jsonl"' "$RUN_ROOT/receipts/campaign-inputs.json"
grep -q '"object_url": "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/test-run-1/artifacts/materialization/feature-test.jsonl"' "$RUN_ROOT/receipts/campaign-inputs.json"

if sh "$ENTRYPOINT" \
  --inventory "$ROOT/inventory.env" \
  --raw-root "$RAW_ROOT" \
  --reference-root "$REF_ROOT" \
  --output-root "$OUT_ROOT" \
  --work-dir "$WORK_ROOT-second" \
  --binary-dir "$BIN_DIR" >/dev/null 2>"$ROOT/replay.err"; then
  printf 'expected duplicate prefix preflight to fail\n' >&2
  exit 1
fi
grep -q 'output prefix already exists' "$ROOT/replay.err"
grep -q 'schema_version=monday.research_event.v1 component=cex-materialization event=run_failed' "$ROOT/replay.err"

SHARD_ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT" "$SHARD_ROOT"' EXIT
RAW4=$SHARD_ROOT/raw
OUT4=$SHARD_ROOT/output
REF4=$SHARD_ROOT/reference
mkdir -p "$RAW4" "$OUT4" "$REF4"
raw3=$(write_triplet "$RAW4/venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=03/part-3.jsonl.zst" raw-segment-3 raw-manifest-3)
raw4=$(write_triplet "$RAW4/venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=04/part-4.jsonl.zst" raw-segment-4 raw-manifest-4)
ref3=$(write_triplet "$REF4/venue=binance/market=usdm/dataset=reference/batch-001/reference.ndjson" ref-segment-1 ref-manifest-1)

cat >"$SHARD_ROOT/inventory.env" <<EOF
RUN_ID=test-run-shards
SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567
IMAGE_REF=registry/research-runner@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
MISSION_ID=data-btcusdt-usdm-test
MARKET=usdm
SYMBOL=BTCUSDT
BUCKET_MS=1000
LABEL_HORIZON_BUCKETS=5
TOP_DEPTH=5
OUTPUT_PREFIX=test-run-shards
RAW_SEGMENT_COUNT=4
RAW_SEGMENT_1=venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=01/part-1.jsonl.zst
RAW_SEGMENT_1_SHA256=$(printf '%s' "$raw1" | awk -F'|' '{print $1}')
RAW_SEGMENT_1_MANIFEST_SHA256=$(printf '%s' "$raw1" | awk -F'|' '{print $2}')
RAW_SEGMENT_2=venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=02/part-2.jsonl.zst
RAW_SEGMENT_2_SHA256=$(printf '%s' "$raw2" | awk -F'|' '{print $1}')
RAW_SEGMENT_2_MANIFEST_SHA256=$(printf '%s' "$raw2" | awk -F'|' '{print $2}')
RAW_SEGMENT_3=venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=03/part-3.jsonl.zst
RAW_SEGMENT_3_SHA256=$(printf '%s' "$raw3" | awk -F'|' '{print $1}')
RAW_SEGMENT_3_MANIFEST_SHA256=$(printf '%s' "$raw3" | awk -F'|' '{print $2}')
RAW_SEGMENT_4=venue=binance/market=usdm/dataset=usdm_all/shard=all/date=2026-08-18/hour=04/part-4.jsonl.zst
RAW_SEGMENT_4_SHA256=$(printf '%s' "$raw4" | awk -F'|' '{print $1}')
RAW_SEGMENT_4_MANIFEST_SHA256=$(printf '%s' "$raw4" | awk -F'|' '{print $2}')
REFERENCE_COUNT=1
REFERENCE_1=venue=binance/market=usdm/dataset=reference/batch-001/reference.ndjson
REFERENCE_1_SHA256=$(printf '%s' "$ref3" | awk -F'|' '{print $1}')
REFERENCE_1_MANIFEST_SHA256=$(printf '%s' "$ref3" | awk -F'|' '{print $2}')
EOF

mkdir -p "$(dirname "$RAW4/$raw1_rel")" "$(dirname "$RAW4/$raw2_rel")"
cp "$RAW_ROOT/$raw1_rel" "$RAW4/$raw1_rel"
cp "$RAW_ROOT/$raw1_rel.manifest.json" "$RAW4/$raw1_rel.manifest.json"
cp "$RAW_ROOT/$raw1_rel._SUCCESS" "$RAW4/$raw1_rel._SUCCESS"
cp "$RAW_ROOT/$raw2_rel" "$RAW4/$raw2_rel"
cp "$RAW_ROOT/$raw2_rel.manifest.json" "$RAW4/$raw2_rel.manifest.json"
cp "$RAW_ROOT/$raw2_rel._SUCCESS" "$RAW4/$raw2_rel._SUCCESS"

run_role() {
  role=$1
  work=$2
  shift 2
  sh "$ENTRYPOINT" \
    --inventory "$SHARD_ROOT/inventory.env" \
    --raw-root "$RAW4" \
    --reference-root "$REF4" \
    --output-root "$OUT4" \
    --work-dir "$work" \
    --binary-dir "$BIN_DIR" \
    --role "$role" \
    "$@"
}

run_role slice "$SHARD_ROOT/work-0" --shard-index 0 --shard-count 2 >/dev/null
run_role slice "$SHARD_ROOT/work-1" --shard-index 1 --shard-count 2 >/dev/null
[ -f "$OUT4/test-run-shards/shards/0/receipt.json" ]
[ -f "$OUT4/test-run-shards/shards/1/receipt.json" ]
[ "$(wc -l <"$OUT4/test-run-shards/shards/0/segments" | tr -d ' ')" = 2 ]
[ "$(wc -l <"$OUT4/test-run-shards/shards/1/segments" | tr -d ' ')" = 2 ]
run_role reduce "$SHARD_ROOT/work-reduce" --shard-count 2 >/dev/null
[ -f "$OUT4/test-run-shards/receipts/campaign-inputs.json" ]
[ -f "$OUT4/test-run-shards/receipts/materialization-receipt.json" ]
grep -q '"relative_path": "artifacts/materialization/feature-test.jsonl"' "$OUT4/test-run-shards/receipts/campaign-inputs.json"

if run_role slice "$SHARD_ROOT/work-0-again" --shard-index 0 --shard-count 2 >/dev/null 2>"$SHARD_ROOT/slice-dup.err"; then
  printf 'expected duplicate shard receipt to fail\n' >&2
  exit 1
fi
grep -q 'shard receipt already exists' "$SHARD_ROOT/slice-dup.err"

if run_role reduce "$SHARD_ROOT/work-reduce-again" --shard-count 2 >/dev/null 2>"$SHARD_ROOT/reduce-dup.err"; then
  printf 'expected duplicate reduce publish to fail\n' >&2
  exit 1
fi
grep -q 'reduce publish already exists' "$SHARD_ROOT/reduce-dup.err"

MISSING=$SHARD_ROOT/missing
mkdir -p "$MISSING/raw" "$MISSING/output" "$MISSING/reference"
cp -R "$RAW4/." "$MISSING/raw/"
cp -R "$REF4/." "$MISSING/reference/"
sed 's/OUTPUT_PREFIX=test-run-shards/OUTPUT_PREFIX=test-run-missing/' "$SHARD_ROOT/inventory.env" >"$MISSING/inventory.env"
sh "$ENTRYPOINT" \
  --inventory "$MISSING/inventory.env" \
  --raw-root "$MISSING/raw" \
  --reference-root "$MISSING/reference" \
  --output-root "$MISSING/output" \
  --work-dir "$MISSING/work-0" \
  --binary-dir "$BIN_DIR" \
  --role slice \
  --shard-index 0 \
  --shard-count 2 >/dev/null
if sh "$ENTRYPOINT" \
  --inventory "$MISSING/inventory.env" \
  --raw-root "$MISSING/raw" \
  --reference-root "$MISSING/reference" \
  --output-root "$MISSING/output" \
  --work-dir "$MISSING/work-reduce" \
  --binary-dir "$BIN_DIR" \
  --role reduce \
  --shard-count 2 >/dev/null 2>"$MISSING/reduce.err"; then
  printf 'expected missing shard reducer to fail\n' >&2
  exit 1
fi
grep -q 'shard receipt is missing' "$MISSING/reduce.err"

DUPSEG=$SHARD_ROOT/dupseg
mkdir -p "$DUPSEG/raw" "$DUPSEG/output" "$DUPSEG/reference"
cp -R "$RAW4/." "$DUPSEG/raw/"
cp -R "$REF4/." "$DUPSEG/reference/"
sed 's/OUTPUT_PREFIX=test-run-shards/OUTPUT_PREFIX=test-run-dupseg/' "$SHARD_ROOT/inventory.env" >"$DUPSEG/inventory.env"
sh "$ENTRYPOINT" \
  --inventory "$DUPSEG/inventory.env" \
  --raw-root "$DUPSEG/raw" \
  --reference-root "$DUPSEG/reference" \
  --output-root "$DUPSEG/output" \
  --work-dir "$DUPSEG/work-0" \
  --binary-dir "$BIN_DIR" \
  --role slice \
  --shard-index 0 \
  --shard-count 2 >/dev/null
sh "$ENTRYPOINT" \
  --inventory "$DUPSEG/inventory.env" \
  --raw-root "$DUPSEG/raw" \
  --reference-root "$DUPSEG/reference" \
  --output-root "$DUPSEG/output" \
  --work-dir "$DUPSEG/work-1" \
  --binary-dir "$BIN_DIR" \
  --role slice \
  --shard-index 1 \
  --shard-count 2 >/dev/null
cp "$DUPSEG/output/test-run-dupseg/shards/0/segments" "$DUPSEG/output/test-run-dupseg/shards/1/segments"
if sh "$ENTRYPOINT" \
  --inventory "$DUPSEG/inventory.env" \
  --raw-root "$DUPSEG/raw" \
  --reference-root "$DUPSEG/reference" \
  --output-root "$DUPSEG/output" \
  --work-dir "$DUPSEG/work-reduce" \
  --binary-dir "$BIN_DIR" \
  --role reduce \
  --shard-count 2 >/dev/null 2>"$DUPSEG/reduce.err"; then
  printf 'expected duplicate segment reducer to fail\n' >&2
  exit 1
fi
grep -q 'duplicate sliced segment' "$DUPSEG/reduce.err"

if sh "$ENTRYPOINT" \
  --inventory "$SHARD_ROOT/inventory.env" \
  --raw-root "$RAW4" \
  --output-root "$OUT4" \
  --work-dir "$SHARD_ROOT/work-bad-count" \
  --binary-dir "$BIN_DIR" \
  --role slice \
  --shard-index 0 \
  --shard-count 8 >/dev/null 2>"$SHARD_ROOT/count.err"; then
  printf 'expected shard-count above RAW_SEGMENT_COUNT to fail\n' >&2
  exit 1
fi
grep -q 'shard-count must not exceed RAW_SEGMENT_COUNT' "$SHARD_ROOT/count.err"

printf 'cex materialization contract: ok\n'
