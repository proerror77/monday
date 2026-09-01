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
  --binary-dir "$BIN_DIR" >/dev/null

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

printf 'cex materialization contract: ok\n'
