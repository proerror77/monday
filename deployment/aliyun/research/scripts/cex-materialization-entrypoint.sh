#!/bin/sh
set -eu

usage() {
  cat <<'EOF' >&2
usage: cex-materialization-entrypoint.sh \
  --inventory /inventory/frozen.env \
  --raw-root /lake/raw \
  --output-root /lake/output \
  --work-dir /work \
  [--reference-root /lake/reference] \
  [--binary-dir /usr/local/bin] \
  [--dry-run]
EOF
  exit 2
}

log() {
  printf '%s\n' "$*" >&2
}

die() {
  log "breach: $*"
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"
}

safe_key() {
  case "$1" in
    ''|*[!A-Z0-9_]*)
      return 1
      ;;
  esac
  return 0
}

safe_value() {
  case "$1" in
    *[\`\$\;\&\|\<\>\(\)\{\}\[\]\!\?\*\"\'\	\ ]*)
      return 1
      ;;
  esac
  return 0
}

inventory_load() {
  [ -f "$INVENTORY" ] || die "inventory file does not exist: $INVENTORY"
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      ''|'#'*)
        continue
        ;;
    esac
    key=${line%%=*}
    value=${line#*=}
    [ "$key" != "$line" ] || die "inventory line is not key=value: $line"
    safe_key "$key" || die "inventory key is not canonical: $key"
    safe_value "$value" || die "inventory value for $key contains shell metacharacters"
    export "$key=$value"
  done <"$INVENTORY"
}

inventory_get() {
  safe_key "$1" || die "invalid lookup key: $1"
  eval "printf '%s' \"\${$1-}\""
}

require_inventory_var() {
  value=$(inventory_get "$1")
  [ -n "$value" ] || die "inventory variable is required: $1"
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

json_string_field() {
  field=$1
  file=$2
  matches=$(sed -n "s/^[[:space:]]*\"$field\": \"\\([^\"]*\\)\"[,]*/\\1/p" "$file")
  count=$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')
  [ "$count" -eq 1 ] || die "expected exactly one $field field in $file, observed $count"
  printf '%s\n' "$matches" | sed -n '1p'
}

canonical_relpath() {
  case "$1" in
    ''|/*|*'..'*)
      return 1
      ;;
  esac
  return 0
}

OUTPUT_BUCKET=monday-lob-apne1-1045353359
OUTPUT_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com
OUTPUT_LANE_ROOT=research/cex-materialization
OUTPUT_OBJECT_BASE_URL=https://$OUTPUT_BUCKET.$OUTPUT_ENDPOINT/$OUTPUT_LANE_ROOT

relative_run_path() {
  case "$1" in
    "$RUN_ROOT"/*)
      printf '%s\n' "${1#"$RUN_ROOT"/}"
      ;;
    *)
      die "path escapes run root: $1"
      ;;
  esac
}

object_url_for() {
  rel=$(relative_run_path "$1")
  printf '%s/%s/%s\n' "$OUTPUT_OBJECT_BASE_URL" "$output_prefix" "$rel"
}

ensure_unique_prefix() {
  [ ! -e "$RUN_ROOT" ] || die "output prefix already exists: $RUN_ROOT"
}

published_path_for() {
  publish_source=$1
  publish_source_root=$2
  publish_destination_root=$3
  publish_label=$4
  case "$publish_source" in
    "$publish_source_root"/*)
      publish_name=${publish_source#"$publish_source_root"/}
      ;;
    *)
      die "$publish_label path escapes local artifact root: $publish_source"
      ;;
  esac
  canonical_relpath "$publish_name" || die "$publish_label path is not canonical: $publish_name"
  case "$publish_name" in
    */*)
      die "$publish_label must be a direct child of its artifact directory: $publish_source"
      ;;
  esac
  printf '%s/%s\n' "$publish_destination_root" "$publish_name"
}

publish_verified_file() {
  publish_label=$1
  publish_source=$2
  publish_destination=$3
  publish_expected_sha=$4
  [ -f "$publish_source" ] || die "$publish_label source is missing: $publish_source"
  [ "$(sha256_file "$publish_source")" = "$publish_expected_sha" ] || die "$publish_label source SHA mismatch"
  [ ! -e "$publish_destination" ] || die "$publish_label destination already exists: $publish_destination"
  cp "$publish_source" "$publish_destination"
  [ -f "$publish_destination" ] || die "$publish_label was not published: $publish_destination"
  [ "$(sha256_file "$publish_destination")" = "$publish_expected_sha" ] || die "$publish_label published readback SHA mismatch"
}

triplet_paths() {
  root=$1
  rel=$2
  canonical_relpath "$rel" || die "relative path is unsafe: $rel"
  data=$root/$rel
  manifest=$data.manifest.json
  success=$data._SUCCESS
  printf '%s\n%s\n%s\n' "$data" "$manifest" "$success"
}

verify_triplet() {
  label=$1
  root=$2
  rel=$3
  expected_sha=$4
  expected_manifest_sha=$5
  set +x
  paths=$(triplet_paths "$root" "$rel")
  data=$(printf '%s\n' "$paths" | sed -n '1p')
  manifest=$(printf '%s\n' "$paths" | sed -n '2p')
  success=$(printf '%s\n' "$paths" | sed -n '3p')
  [ -f "$data" ] || die "$label data file is missing: $data"
  [ -f "$manifest" ] || die "$label manifest file is missing: $manifest"
  [ -f "$success" ] || die "$label success marker is missing: $success"
  observed_sha=$(sha256_file "$data")
  [ "$observed_sha" = "$expected_sha" ] || die "$label data SHA mismatch: expected $expected_sha observed $observed_sha"
  observed_manifest_sha=$(sha256_file "$manifest")
  [ "$observed_manifest_sha" = "$expected_manifest_sha" ] || die "$label manifest SHA mismatch: expected $expected_manifest_sha observed $observed_manifest_sha"
  expected_success=$(printf '%s\n' "$expected_sha")
  observed_success=$(cat "$success")
  [ "$observed_success" = "$expected_sha" ] || die "$label success marker content mismatch"
  success_sha=$(sha256_file "$success")
  printf '%s|%s|%s|%s|%s|%s\n' "$data" "$manifest" "$success" "$observed_sha" "$observed_manifest_sha" "$success_sha"
}

single_file() {
  directory=$1
  pattern=$2
  matches=$(find "$directory" -maxdepth 1 -type f -name "$pattern" | sort)
  count=$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')
  [ "$count" -eq 1 ] || die "expected exactly one $pattern in $directory, observed $count"
  printf '%s\n' "$matches" | sed -n '1p'
}

raw_segment_count=0
reference_count=0
inventory_sha256=
run_id=
source_revision=
image_ref=
mission_id=
market=
symbol=
bucket_ms=
label_horizon_buckets=
top_depth=
output_prefix=

INVENTORY=
RAW_ROOT=
REFERENCE_ROOT=
OUTPUT_ROOT=
WORK_DIR=
BINARY_DIR=/usr/local/bin
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --inventory)
      INVENTORY=${2-}
      shift 2
      ;;
    --raw-root)
      RAW_ROOT=${2-}
      shift 2
      ;;
    --reference-root)
      REFERENCE_ROOT=${2-}
      shift 2
      ;;
    --output-root)
      OUTPUT_ROOT=${2-}
      shift 2
      ;;
    --work-dir)
      WORK_DIR=${2-}
      shift 2
      ;;
    --binary-dir)
      BINARY_DIR=${2-}
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    *)
      usage
      ;;
  esac
done

[ -n "$INVENTORY" ] || usage
[ -n "$RAW_ROOT" ] || usage
[ -n "$OUTPUT_ROOT" ] || usage
[ -n "$WORK_DIR" ] || usage

require_tool awk
require_tool find
require_tool sed
require_tool sha256sum

SLICER_BIN=$BINARY_DIR/binance-market-tape-slicer
PIT_BIN=$BINARY_DIR/lob-pit-materializer
REPLAY_BIN=$BINARY_DIR/binance-replay-parquet-materializer

inventory_load

for key in RUN_ID SOURCE_REVISION IMAGE_REF MISSION_ID MARKET SYMBOL BUCKET_MS LABEL_HORIZON_BUCKETS TOP_DEPTH OUTPUT_PREFIX RAW_SEGMENT_COUNT; do
  require_inventory_var "$key"
done

run_id=$(inventory_get RUN_ID)
source_revision=$(inventory_get SOURCE_REVISION)
image_ref=$(inventory_get IMAGE_REF)
mission_id=$(inventory_get MISSION_ID)
market=$(inventory_get MARKET)
symbol=$(inventory_get SYMBOL)
bucket_ms=$(inventory_get BUCKET_MS)
label_horizon_buckets=$(inventory_get LABEL_HORIZON_BUCKETS)
top_depth=$(inventory_get TOP_DEPTH)
output_prefix=$(inventory_get OUTPUT_PREFIX)
raw_segment_count=$(inventory_get RAW_SEGMENT_COUNT)
reference_count=$(inventory_get REFERENCE_COUNT)
reference_count=${reference_count:-0}

case "$source_revision" in
  [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]\
[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]\
[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]\
[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f])
    ;;
  *)
    die "SOURCE_REVISION must be exactly 40 lowercase hex characters"
    ;;
esac
case "$market" in
  usdm)
    ;;
  *)
    die "MARKET must be usdm"
    ;;
esac
case "$symbol" in
  *[!A-Z0-9]*|'')
    die "SYMBOL must be canonical uppercase"
    ;;
esac
case "$raw_segment_count" in
  ''|*[!0-9]*|0)
    die "RAW_SEGMENT_COUNT must be a positive integer"
    ;;
esac
case "$reference_count" in
  ''|*[!0-9]*)
    die "REFERENCE_COUNT must be a non-negative integer"
    ;;
esac
case "$bucket_ms$label_horizon_buckets$top_depth" in
  *[!0-9]*)
    die "BUCKET_MS, LABEL_HORIZON_BUCKETS, and TOP_DEPTH must be numeric"
    ;;
esac
canonical_relpath "$output_prefix" || die "OUTPUT_PREFIX must be a safe relative path"
case "$output_prefix" in
  ''|"$OUTPUT_LANE_ROOT"|"$OUTPUT_LANE_ROOT"/*)
    die "OUTPUT_PREFIX must be a run-specific suffix under $OUTPUT_LANE_ROOT, not the full lane path"
    ;;
esac

inventory_sha256=$(sha256_file "$INVENTORY")
RUN_ROOT=$OUTPUT_ROOT/$output_prefix
ARTIFACT_ROOT=$RUN_ROOT/artifacts
MATERIALIZATION_DIR=$ARTIFACT_ROOT/materialization
REPLAY_DIR=$ARTIFACT_ROOT/replay
RECEIPT_DIR=$RUN_ROOT/receipts
LOCAL_RUN_ROOT=$WORK_DIR/staged-output
LOCAL_ARTIFACT_ROOT=$LOCAL_RUN_ROOT/artifacts
LOCAL_MATERIALIZATION_DIR=$LOCAL_ARTIFACT_ROOT/materialization
LOCAL_REPLAY_DIR=$LOCAL_ARTIFACT_ROOT/replay
LOCAL_RECEIPT_DIR=$LOCAL_RUN_ROOT/receipts
SLICE_ROOT=$WORK_DIR/slices
STATE_ROOT=$WORK_DIR/state

ensure_unique_prefix
mkdir -p "$SLICE_ROOT" "$STATE_ROOT"

if [ "$DRY_RUN" -ne 1 ]; then
  mkdir -p "$LOCAL_MATERIALIZATION_DIR" "$LOCAL_REPLAY_DIR" "$LOCAL_RECEIPT_DIR"
fi

[ "$DRY_RUN" -eq 1 ] || {
  [ -x "$SLICER_BIN" ] || die "slicer binary is not executable: $SLICER_BIN"
  [ -x "$PIT_BIN" ] || die "materializer binary is not executable: $PIT_BIN"
  [ -x "$REPLAY_BIN" ] || die "replay binary is not executable: $REPLAY_BIN"
}

i=1
while [ "$i" -le "$raw_segment_count" ]; do
  rel=$(inventory_get "RAW_SEGMENT_$i")
  sha=$(inventory_get "RAW_SEGMENT_${i}_SHA256")
  manifest_sha=$(inventory_get "RAW_SEGMENT_${i}_MANIFEST_SHA256")
  [ -n "$rel" ] || die "RAW_SEGMENT_$i is required"
  [ -n "$sha" ] || die "RAW_SEGMENT_${i}_SHA256 is required"
  [ -n "$manifest_sha" ] || die "RAW_SEGMENT_${i}_MANIFEST_SHA256 is required"
  verified=$(verify_triplet "raw segment $i" "$RAW_ROOT" "$rel" "$sha" "$manifest_sha")
  data=$(printf '%s' "$verified" | awk -F'|' '{print $1}')
  printf '%s\n' "$data" >"$STATE_ROOT/raw-segment-$i.path"
  printf '%s\n' "$sha" >"$STATE_ROOT/raw-segment-$i.sha256"
  printf '%s\n' "$manifest_sha" >"$STATE_ROOT/raw-segment-$i.manifest-sha256"
  i=$((i + 1))
done

if [ "$reference_count" -gt 0 ]; then
  [ -n "$REFERENCE_ROOT" ] || die "REFERENCE_ROOT is required when REFERENCE_COUNT > 0"
fi

i=1
while [ "$i" -le "$reference_count" ]; do
  rel=$(inventory_get "REFERENCE_$i")
  sha=$(inventory_get "REFERENCE_${i}_SHA256")
  manifest_sha=$(inventory_get "REFERENCE_${i}_MANIFEST_SHA256")
  [ -n "$rel" ] || die "REFERENCE_$i is required"
  [ -n "$sha" ] || die "REFERENCE_${i}_SHA256 is required"
  [ -n "$manifest_sha" ] || die "REFERENCE_${i}_MANIFEST_SHA256 is required"
  verified=$(verify_triplet "reference batch $i" "$REFERENCE_ROOT" "$rel" "$sha" "$manifest_sha")
  data=$(printf '%s' "$verified" | awk -F'|' '{print $1}')
  printf '%s\n' "$data" >"$STATE_ROOT/reference-$i.path"
  printf '%s\n' "$sha" >"$STATE_ROOT/reference-$i.sha256"
  printf '%s\n' "$manifest_sha" >"$STATE_ROOT/reference-$i.manifest-sha256"
  i=$((i + 1))
done

if [ "$DRY_RUN" -eq 1 ]; then
  printf '{\n'
  printf '  "schema_version": "monday.cex_materialization_dry_run.v1",\n'
  printf '  "run_id": "%s",\n' "$run_id"
  printf '  "source_revision": "%s",\n' "$source_revision"
  printf '  "image_ref": "%s",\n' "$image_ref"
  printf '  "mission_id": "%s",\n' "$mission_id"
  printf '  "market": "%s",\n' "$market"
  printf '  "symbol": "%s",\n' "$symbol"
  printf '  "output_prefix": "%s",\n' "$output_prefix"
  printf '  "output_object_base_url": "%s",\n' "$OUTPUT_OBJECT_BASE_URL"
  printf '  "inventory_sha256": "%s",\n' "$inventory_sha256"
  printf '  "raw_segment_count": %s,\n' "$raw_segment_count"
  printf '  "reference_count": %s\n' "$reference_count"
  printf '}\n'
  exit 0
fi

i=1
while [ "$i" -le "$raw_segment_count" ]; do
  raw_path=$(cat "$STATE_ROOT/raw-segment-$i.path")
  slice_dir=$SLICE_ROOT/segment-$i
  mkdir -p "$slice_dir"
  "$SLICER_BIN" --segment "$raw_path" --output-dir "$slice_dir" --symbols "$symbol" >"$STATE_ROOT/slicer-$i.json"
  slice_path=$(single_file "$slice_dir" '*.jsonl.zst')
  slice_manifest=$slice_path.manifest.json
  slice_success=$slice_path._SUCCESS
  [ -f "$slice_manifest" ] || die "slice manifest is missing: $slice_manifest"
  [ -f "$slice_success" ] || die "slice success marker is missing: $slice_success"
  slice_sha=$(sha256_file "$slice_path")
  slice_manifest_sha=$(sha256_file "$slice_manifest")
  observed_success=$(cat "$slice_success")
  [ "$observed_success" = "$slice_sha" ] || die "slice success marker content mismatch: $slice_success"
  printf '%s\n' "$slice_path" >"$STATE_ROOT/slice-$i.path"
  printf '%s\n' "$slice_sha" >"$STATE_ROOT/slice-$i.sha256"
  printf '%s\n' "$slice_manifest_sha" >"$STATE_ROOT/slice-$i.manifest-sha256"
  i=$((i + 1))
done

pit_stdout=$STATE_ROOT/lob-pit-materializer.json
set -- "$PIT_BIN" \
  --mission-id "$mission_id" \
  --symbol "$symbol" \
  --market "$market" \
  --bucket-ms "$bucket_ms" \
  --label-horizon-buckets "$label_horizon_buckets" \
  --top-depth "$top_depth" \
  --artifact-dir "$LOCAL_MATERIALIZATION_DIR"
i=1
while [ "$i" -le "$raw_segment_count" ]; do
  set -- "$@" \
    --segment "$(cat "$STATE_ROOT/slice-$i.path")" \
    --segment-content-sha256 "$(cat "$STATE_ROOT/slice-$i.sha256")" \
    --segment-manifest-sha256 "$(cat "$STATE_ROOT/slice-$i.manifest-sha256")"
  i=$((i + 1))
done
i=1
while [ "$i" -le "$reference_count" ]; do
  set -- "$@" \
    --reference-data "$(cat "$STATE_ROOT/reference-$i.path")" \
    --reference-data-sha256 "$(cat "$STATE_ROOT/reference-$i.sha256")" \
    --reference-manifest-sha256 "$(cat "$STATE_ROOT/reference-$i.manifest-sha256")"
  i=$((i + 1))
done
"$@" >"$pit_stdout"

feature_path=$(json_string_field artifact_path "$pit_stdout")
feature_sha=$(json_string_field artifact_sha256 "$pit_stdout")
materialization_path=$(json_string_field report_path "$pit_stdout")
materialization_sha=$(json_string_field report_sha256 "$pit_stdout")
[ -f "$feature_path" ] || die "feature artifact is missing: $feature_path"
[ -f "$materialization_path" ] || die "materialization report is missing: $materialization_path"
[ "$(sha256_file "$feature_path")" = "$feature_sha" ] || die "feature artifact readback SHA mismatch"
[ "$(sha256_file "$materialization_path")" = "$materialization_sha" ] || die "materialization report readback SHA mismatch"

replay_stdout=$STATE_ROOT/replay-materializer.json
set -- "$REPLAY_BIN" \
  --mission-id "$mission_id" \
  --symbol "$symbol" \
  --market "$market" \
  --artifact-dir "$LOCAL_REPLAY_DIR"
i=1
while [ "$i" -le "$raw_segment_count" ]; do
  set -- "$@" \
    --segment "$(cat "$STATE_ROOT/slice-$i.path")" \
    --segment-content-sha256 "$(cat "$STATE_ROOT/slice-$i.sha256")" \
    --segment-manifest-sha256 "$(cat "$STATE_ROOT/slice-$i.manifest-sha256")"
  i=$((i + 1))
done
"$@" >"$replay_stdout"

replay_manifest_path=$(json_string_field manifest_path "$replay_stdout")
replay_manifest_sha=$(json_string_field manifest_sha256 "$replay_stdout")
replay_artifact_rel=$(json_string_field artifact_path "$replay_stdout")
replay_artifact_sha=$(json_string_field artifact_sha256 "$replay_stdout")
replay_artifact_path=$LOCAL_REPLAY_DIR/$replay_artifact_rel
[ -f "$replay_artifact_path" ] || die "replay artifact is missing: $replay_artifact_path"
[ -f "$replay_manifest_path" ] || die "replay manifest is missing: $replay_manifest_path"
[ "$(sha256_file "$replay_artifact_path")" = "$replay_artifact_sha" ] || die "replay artifact readback SHA mismatch"
[ "$(sha256_file "$replay_manifest_path")" = "$replay_manifest_sha" ] || die "replay manifest readback SHA mismatch"

feature_publish_path=$(published_path_for "$feature_path" "$LOCAL_MATERIALIZATION_DIR" "$MATERIALIZATION_DIR" "feature artifact")
materialization_publish_path=$(published_path_for "$materialization_path" "$LOCAL_MATERIALIZATION_DIR" "$MATERIALIZATION_DIR" "materialization report")
replay_artifact_publish_path=$(published_path_for "$replay_artifact_path" "$LOCAL_REPLAY_DIR" "$REPLAY_DIR" "replay artifact")
replay_manifest_publish_path=$(published_path_for "$replay_manifest_path" "$LOCAL_REPLAY_DIR" "$REPLAY_DIR" "replay manifest")

cp "$INVENTORY" "$LOCAL_RECEIPT_DIR/frozen-inventory.env"
inventory_copy_sha=$(sha256_file "$LOCAL_RECEIPT_DIR/frozen-inventory.env")
[ "$inventory_copy_sha" = "$inventory_sha256" ] || die "inventory copy SHA mismatch"
feature_url=$(object_url_for "$feature_publish_path")
materialization_url=$(object_url_for "$materialization_publish_path")
replay_artifact_url=$(object_url_for "$replay_artifact_publish_path")
replay_manifest_url=$(object_url_for "$replay_manifest_publish_path")

cat >"$LOCAL_RECEIPT_DIR/campaign-inputs.json" <<EOF
{
  "schema_version": "monday.cex_campaign_inputs.v1",
  "run_id": "$run_id",
  "source_revision": "$source_revision",
  "image_ref": "$image_ref",
  "mission_id": "$mission_id",
  "market": "$market",
  "symbol": "$symbol",
  "output_prefix": "$output_prefix",
  "output_object_base_url": "$OUTPUT_OBJECT_BASE_URL",
  "readback_scope": "same-mounted-ossfs-prefix",
  "feature": {
    "relative_path": "$(relative_run_path "$feature_publish_path")",
    "object_url": "$feature_url",
    "sha256": "$feature_sha"
  },
  "materialization": {
    "relative_path": "$(relative_run_path "$materialization_publish_path")",
    "object_url": "$materialization_url",
    "sha256": "$materialization_sha"
  },
  "replay_artifact": {
    "relative_path": "$(relative_run_path "$replay_artifact_publish_path")",
    "object_url": "$replay_artifact_url",
    "sha256": "$replay_artifact_sha"
  },
  "replay_manifest": {
    "relative_path": "$(relative_run_path "$replay_manifest_publish_path")",
    "object_url": "$replay_manifest_url",
    "sha256": "$replay_manifest_sha"
  }
}
EOF
campaign_inputs_sha=$(sha256_file "$LOCAL_RECEIPT_DIR/campaign-inputs.json")

{
  printf '{\n'
  printf '  "schema_version": "monday.cex_materialization_receipt.v1",\n'
  printf '  "run_id": "%s",\n' "$run_id"
  printf '  "source_revision": "%s",\n' "$source_revision"
  printf '  "image_ref": "%s",\n' "$image_ref"
  printf '  "inventory_sha256": "%s",\n' "$inventory_sha256"
  printf '  "campaign_inputs_sha256": "%s",\n' "$campaign_inputs_sha"
  printf '  "feature_sha256": "%s",\n' "$feature_sha"
  printf '  "materialization_sha256": "%s",\n' "$materialization_sha"
  printf '  "replay_artifact_sha256": "%s",\n' "$replay_artifact_sha"
  printf '  "replay_manifest_sha256": "%s"\n' "$replay_manifest_sha"
  printf '}\n'
} >"$LOCAL_RECEIPT_DIR/materialization-receipt.json"

materialization_receipt_sha=$(sha256_file "$LOCAL_RECEIPT_DIR/materialization-receipt.json")
mkdir -p "$MATERIALIZATION_DIR" "$REPLAY_DIR" "$RECEIPT_DIR"
publish_verified_file "feature artifact" "$feature_path" "$feature_publish_path" "$feature_sha"
publish_verified_file "materialization report" "$materialization_path" "$materialization_publish_path" "$materialization_sha"
publish_verified_file "replay artifact" "$replay_artifact_path" "$replay_artifact_publish_path" "$replay_artifact_sha"
publish_verified_file "replay manifest" "$replay_manifest_path" "$replay_manifest_publish_path" "$replay_manifest_sha"
publish_verified_file "frozen inventory" "$LOCAL_RECEIPT_DIR/frozen-inventory.env" "$RECEIPT_DIR/frozen-inventory.env" "$inventory_sha256"
publish_verified_file "campaign inputs" "$LOCAL_RECEIPT_DIR/campaign-inputs.json" "$RECEIPT_DIR/campaign-inputs.json" "$campaign_inputs_sha"
publish_verified_file "materialization receipt" "$LOCAL_RECEIPT_DIR/materialization-receipt.json" "$RECEIPT_DIR/materialization-receipt.json" "$materialization_receipt_sha"

log "run_id=$run_id feature_sha256=$feature_sha materialization_sha256=$materialization_sha replay_artifact_sha256=$replay_artifact_sha replay_manifest_sha256=$replay_manifest_sha"
