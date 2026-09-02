#!/bin/sh
set -eu

usage() {
  cat <<'EOF' >&2
usage: cex-materialization-entrypoint.sh \
  --inventory /inventory/frozen.env \
  --raw-root /lake/raw \
  --output-root /lake/output \
  --work-dir /work \
  [--reference-root /reference/lake/raw] \
  [--binary-dir /usr/local/bin] \
  [--role all|slice|reduce] \
  [--shard-index N] \
  [--shard-count N] \
  [--dry-run]
EOF
  exit 2
}

log() {
  printf '%s\n' "$*" >&2
}

run_id=unknown
current_stage=preflight

stage_event() {
  event=$1
  stage=$2
  current=$3
  total=$4
  shift 4
  current_stage=$stage
  log "schema_version=monday.research_event.v1 component=cex-materialization event=$event run_id=$run_id stage=$stage current=$current total=$total $*"
}

progress_event() {
  progress_stage=$1
  progress_current=$2
  progress_total=$3
  progress_interval=$4
  shift 4
  if [ "$progress_current" -eq "$progress_total" ] || [ $((progress_current % progress_interval)) -eq 0 ]; then
    stage_event stage_progress "$progress_stage" "$progress_current" "$progress_total" "$@"
  fi
}

die() {
  log "schema_version=monday.research_event.v1 component=cex-materialization event=run_failed run_id=$run_id stage=$current_stage reason=$*"
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

ensure_unique_shard() {
  [ ! -e "$SHARD_RECEIPT_PATH" ] || die "shard receipt already exists: $SHARD_RECEIPT_PATH"
}

require_positive_int() {
  case "$2" in
    ''|*[!0-9]*|0)
      die "$1 must be a positive integer"
      ;;
  esac
}

require_nonnegative_int() {
  case "$2" in
    ''|*[!0-9]*)
      die "$1 must be a non-negative integer"
      ;;
  esac
}

shard_bounds() {
  shard_index=$1
  shard_count=$2
  segment_count=$3
  SHARD_START=$((shard_index * segment_count / shard_count + 1))
  SHARD_END=$(((shard_index + 1) * segment_count / shard_count))
  [ "$SHARD_START" -le "$SHARD_END" ] || die "shard $shard_index of $shard_count is empty for $segment_count segments"
}

slice_one_segment() {
  slice_index=$1
  raw_path=$(cat "$STATE_ROOT/raw-segment-$slice_index.path")
  slice_dir=$SLICE_ROOT/segment-$slice_index
  mkdir -p "$slice_dir"
  if ! "$SLICER_BIN" --segment "$raw_path" --output-dir "$slice_dir" --symbols "$symbol" >"$STATE_ROOT/slicer-$slice_index.json"; then
    die "slicer failed for segment $slice_index"
  fi
  slice_path=$(single_file "$slice_dir" '*.jsonl.zst')
  slice_manifest=$slice_path.manifest.json
  slice_success=$slice_path._SUCCESS
  [ -f "$slice_manifest" ] || die "slice manifest is missing: $slice_manifest"
  [ -f "$slice_success" ] || die "slice success marker is missing: $slice_success"
  slice_sha=$(sha256_file "$slice_path")
  slice_manifest_sha=$(sha256_file "$slice_manifest")
  observed_success=$(cat "$slice_success")
  [ "$observed_success" = "$slice_sha" ] || die "slice success marker content mismatch: $slice_success"
  printf '%s\n' "$slice_path" >"$STATE_ROOT/slice-$slice_index.path"
  printf '%s\n' "$slice_sha" >"$STATE_ROOT/slice-$slice_index.sha256"
  printf '%s\n' "$slice_manifest_sha" >"$STATE_ROOT/slice-$slice_index.manifest-sha256"
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
  publish_temporary=$publish_destination.$$.tmp
  log "schema_version=monday.research_event.v1 component=cex-materialization event=artifact_publish_start run_id=$run_id stage=publish artifact=$publish_label sha256=$publish_expected_sha"
  [ -f "$publish_source" ] || die "$publish_label source is missing: $publish_source"
  [ "$(sha256_file "$publish_source")" = "$publish_expected_sha" ] || die "$publish_label source SHA mismatch"
  [ ! -e "$publish_destination" ] || die "$publish_label destination already exists: $publish_destination"
  [ ! -e "$publish_temporary" ] || die "$publish_label temporary destination already exists: $publish_temporary"
  if ! cp "$publish_source" "$publish_temporary"; then
    rm -f "$publish_temporary"
    die "$publish_label temporary publish failed"
  fi
  if [ "$(sha256_file "$publish_temporary")" != "$publish_expected_sha" ]; then
    rm -f "$publish_temporary"
    die "$publish_label temporary readback SHA mismatch"
  fi
  if ! mv "$publish_temporary" "$publish_destination"; then
    rm -f "$publish_temporary"
    die "$publish_label promotion failed"
  fi
  [ -f "$publish_destination" ] || die "$publish_label was not published: $publish_destination"
  [ "$(sha256_file "$publish_destination")" = "$publish_expected_sha" ] || die "$publish_label published readback SHA mismatch"
  log "schema_version=monday.research_event.v1 component=cex-materialization event=artifact_publish_complete run_id=$run_id stage=publish artifact=$publish_label sha256=$publish_expected_sha"
}

rewrite_materialization_artifact_path() {
  rewrite_source=$1
  rewrite_expected_path=$2
  rewrite_published_path=$3
  rewrite_output=$4
  observed_path=$(json_string_field artifact_path "$rewrite_source")
  [ "$observed_path" = "$rewrite_expected_path" ] || die "materialization report does not bind its local feature artifact"
  awk -v published="$rewrite_published_path" '
    /^[[:space:]]*"artifact_path":/ {
      count += 1
      printf "  \"artifact_path\": \"%s\",\n", published
      next
    }
    { print }
    END { if (count != 1) exit 1 }
  ' "$rewrite_source" >"$rewrite_output" || die "materialization report artifact path rewrite failed"
  [ "$(json_string_field artifact_path "$rewrite_output")" = "$rewrite_published_path" ] || die "materialization report published artifact path mismatch"
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
ROLE=all
SHARD_INDEX=
SHARD_COUNT=

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
    --role)
      ROLE=${2-}
      shift 2
      ;;
    --shard-index)
      SHARD_INDEX=${2-}
      shift 2
      ;;
    --shard-count)
      SHARD_COUNT=${2-}
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
case "$ROLE" in
  all|slice|reduce)
    ;;
  *)
    die "ROLE must be all, slice, or reduce"
    ;;
esac
case "$ROLE" in
  all)
    [ -z "$SHARD_INDEX" ] || die "shard-index is only valid with --role slice"
    [ -z "$SHARD_COUNT" ] || die "shard-count is only valid with --role slice or --role reduce"
    ;;
  slice)
    if [ -z "$SHARD_INDEX" ]; then
      SHARD_INDEX=${JOB_COMPLETION_INDEX-}
    fi
    require_nonnegative_int shard-index "$SHARD_INDEX"
    require_positive_int shard-count "$SHARD_COUNT"
    [ "$SHARD_COUNT" -le "$raw_segment_count" ] || die "shard-count must not exceed RAW_SEGMENT_COUNT"
    [ "$SHARD_INDEX" -lt "$SHARD_COUNT" ] || die "shard-index must be in 0..shard-count-1"
    shard_bounds "$SHARD_INDEX" "$SHARD_COUNT" "$raw_segment_count"
    ;;
  reduce)
    [ -z "$SHARD_INDEX" ] || die "shard-index is only valid with --role slice"
    require_positive_int shard-count "$SHARD_COUNT"
    [ "$SHARD_COUNT" -le "$raw_segment_count" ] || die "shard-count must not exceed RAW_SEGMENT_COUNT"
    ;;
esac

inventory_sha256=$(sha256_file "$INVENTORY")
RUN_ROOT=$OUTPUT_ROOT/$output_prefix
ARTIFACT_ROOT=$RUN_ROOT/artifacts
MATERIALIZATION_DIR=$ARTIFACT_ROOT/materialization
REPLAY_DIR=$ARTIFACT_ROOT/replay
RECEIPT_DIR=$RUN_ROOT/receipts
SHARD_ROOT=$RUN_ROOT/shards
SLICE_PUBLISH_ROOT=$ARTIFACT_ROOT/slices
LOCAL_RUN_ROOT=$WORK_DIR/staged-output
LOCAL_ARTIFACT_ROOT=$LOCAL_RUN_ROOT/artifacts
LOCAL_MATERIALIZATION_DIR=$LOCAL_ARTIFACT_ROOT/materialization
LOCAL_REPLAY_DIR=$LOCAL_ARTIFACT_ROOT/replay
LOCAL_RECEIPT_DIR=$LOCAL_RUN_ROOT/receipts
SLICE_ROOT=$WORK_DIR/slices
STATE_ROOT=$WORK_DIR/state
SHARD_DIR=
SHARD_RECEIPT_PATH=
if [ "$ROLE" = "slice" ]; then
  SHARD_DIR=$SHARD_ROOT/$SHARD_INDEX
  SHARD_RECEIPT_PATH=$SHARD_DIR/receipt.json
fi

mkdir -p "$SLICE_ROOT" "$STATE_ROOT"
case "$ROLE" in
  all)
    ensure_unique_prefix
    ;;
  slice)
    ensure_unique_shard
    ;;
  reduce)
    [ ! -e "$RECEIPT_DIR/campaign-inputs.json" ] || die "reduce publish already exists: $RECEIPT_DIR/campaign-inputs.json"
    ;;
esac

if [ "$DRY_RUN" -ne 1 ] && [ "$ROLE" != "slice" ]; then
  mkdir -p "$LOCAL_MATERIALIZATION_DIR" "$LOCAL_REPLAY_DIR" "$LOCAL_RECEIPT_DIR"
fi

[ "$DRY_RUN" -eq 1 ] || {
  if [ "$ROLE" != "reduce" ]; then
    [ -x "$SLICER_BIN" ] || die "slicer binary is not executable: $SLICER_BIN"
  fi
  if [ "$ROLE" != "slice" ]; then
    [ -x "$PIT_BIN" ] || die "materializer binary is not executable: $PIT_BIN"
    [ -x "$REPLAY_BIN" ] || die "replay binary is not executable: $REPLAY_BIN"
  fi
}

log "schema_version=monday.research_event.v1 component=cex-materialization event=run_start run_id=$run_id mission_id=$mission_id market=$market symbol=$symbol bucket_ms=$bucket_ms label_horizon_buckets=$label_horizon_buckets top_depth=$top_depth raw_segments=$raw_segment_count references=$reference_count source_revision=$source_revision image_ref=$image_ref inventory_sha256=$inventory_sha256 dry_run=$DRY_RUN role=$ROLE shard_index=${SHARD_INDEX:-none} shard_count=${SHARD_COUNT:-none}"

verify_raw_range() {
  verify_start=$1
  verify_end=$2
  verify_total=$((verify_end - verify_start + 1))
  [ "$verify_end" -ge "$verify_start" ] || die "raw verification range is empty"
  stage_event stage_start raw_verification 0 "$verify_total"
  i=$verify_start
  seen=0
  while [ "$i" -le "$verify_end" ]; do
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
    seen=$((seen + 1))
    progress_event raw_verification "$seen" "$verify_total" 10 "last_index=$i content_sha256=$sha manifest_sha256=$manifest_sha"
    i=$((i + 1))
  done
  stage_event stage_complete raw_verification "$verify_total" "$verify_total"
}

if [ "$ROLE" != "reduce" ]; then
  if [ "$ROLE" = "slice" ]; then
    verify_raw_range "$SHARD_START" "$SHARD_END"
  else
    verify_raw_range 1 "$raw_segment_count"
  fi
fi

if [ "$reference_count" -gt 0 ]; then
  [ -n "$REFERENCE_ROOT" ] || die "REFERENCE_ROOT is required when REFERENCE_COUNT > 0"
fi

if [ "$ROLE" != "slice" ]; then
  stage_event stage_start reference_verification 0 "$reference_count"
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
    progress_event reference_verification "$i" "$reference_count" 50 "last_index=$i data_sha256=$sha manifest_sha256=$manifest_sha"
    i=$((i + 1))
  done
  stage_event stage_complete reference_verification "$reference_count" "$reference_count"
fi

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
  printf '  "role": "%s",\n' "$ROLE"
  printf '  "raw_segment_count": %s,\n' "$raw_segment_count"
  printf '  "reference_count": %s\n' "$reference_count"
  printf '}\n'
  log "schema_version=monday.research_event.v1 component=cex-materialization event=run_complete run_id=$run_id mode=dry_run inventory_sha256=$inventory_sha256 role=$ROLE"
  exit 0
fi

load_shard_receipts() {
  stage_event stage_start shard_reduce 0 "$SHARD_COUNT"
  seen_indexes=$STATE_ROOT/seen-indexes
  : >"$seen_indexes"
  shard_i=0
  while [ "$shard_i" -lt "$SHARD_COUNT" ]; do
    shard_bounds "$shard_i" "$SHARD_COUNT" "$raw_segment_count"
    expected_start=$SHARD_START
    expected_end=$SHARD_END
    shard_dir=$SHARD_ROOT/$shard_i
    shard_receipt=$shard_dir/receipt.json
    shard_manifest=$shard_dir/segments
    shard_success=$shard_dir/_SUCCESS
    [ -f "$shard_receipt" ] || die "shard receipt is missing: $shard_receipt"
    [ -f "$shard_manifest" ] || die "shard segment list is missing: $shard_manifest"
    [ -f "$shard_success" ] || die "shard success marker is missing: $shard_success"
    [ "$(json_string_field schema_version "$shard_receipt")" = "monday.cex_materialization_shard_receipt.v1" ] || die "shard $shard_i schema drifted"
    [ "$(json_string_field run_id "$shard_receipt")" = "$run_id" ] || die "shard $shard_i run_id drifted"
    [ "$(json_string_field inventory_sha256 "$shard_receipt")" = "$inventory_sha256" ] || die "shard $shard_i inventory SHA drifted"
    [ "$(json_string_field shard_index "$shard_receipt")" = "$shard_i" ] || die "shard $shard_i index drifted"
    [ "$(json_string_field shard_count "$shard_receipt")" = "$SHARD_COUNT" ] || die "shard $shard_i count drifted"
    [ "$(json_string_field segment_start "$shard_receipt")" = "$expected_start" ] || die "shard $shard_i segment_start drifted"
    [ "$(json_string_field segment_end "$shard_receipt")" = "$expected_end" ] || die "shard $shard_i segment_end drifted"
    receipt_sha=$(sha256_file "$shard_receipt")
    [ "$(cat "$shard_success")" = "$receipt_sha" ] || die "shard $shard_i success marker mismatch"
    expected_next=$expected_start
    while IFS='	' read -r segment_index relative_path slice_sha slice_manifest_sha || [ -n "$segment_index" ]; do
      [ -n "$segment_index" ] || continue
      case "$segment_index" in
        ''|*[!0-9]*|0)
          die "shard $shard_i has an invalid segment index: $segment_index"
          ;;
      esac
      [ "$segment_index" -eq "$expected_next" ] || die "shard $shard_i segment out of order: expected $expected_next observed $segment_index"
      [ "$segment_index" -le "$expected_end" ] || die "shard $shard_i segment $segment_index exceeds shard bounds"
      grep -qx "$segment_index" "$seen_indexes" && die "duplicate sliced segment: $segment_index"
      printf '%s\n' "$segment_index" >>"$seen_indexes"
      canonical_relpath "$relative_path" || die "shard $shard_i segment $segment_index path is unsafe"
      published_slice=$RUN_ROOT/$relative_path
      published_manifest=$published_slice.manifest.json
      published_success=$published_slice._SUCCESS
      [ -f "$published_slice" ] || die "published slice is missing: $published_slice"
      [ -f "$published_manifest" ] || die "published slice manifest is missing: $published_manifest"
      [ -f "$published_success" ] || die "published slice success marker is missing: $published_success"
      [ "$(sha256_file "$published_slice")" = "$slice_sha" ] || die "published slice SHA mismatch: $published_slice"
      [ "$(sha256_file "$published_manifest")" = "$slice_manifest_sha" ] || die "published slice manifest SHA mismatch: $published_manifest"
      [ "$(cat "$published_success")" = "$slice_sha" ] || die "published slice success marker mismatch: $published_success"
      printf '%s\n' "$published_slice" >"$STATE_ROOT/slice-$segment_index.path"
      printf '%s\n' "$slice_sha" >"$STATE_ROOT/slice-$segment_index.sha256"
      printf '%s\n' "$slice_manifest_sha" >"$STATE_ROOT/slice-$segment_index.manifest-sha256"
      expected_next=$((expected_next + 1))
    done <"$shard_manifest"
    [ "$expected_next" -eq $((expected_end + 1)) ] || die "shard $shard_i did not cover $expected_start..$expected_end"
    progress_event shard_reduce $((shard_i + 1)) "$SHARD_COUNT" 1
    shard_i=$((shard_i + 1))
  done
  observed_count=$(sed '/^$/d' "$seen_indexes" | wc -l | tr -d ' ')
  [ "$observed_count" = "$raw_segment_count" ] || die "reducer observed $observed_count slices, expected $raw_segment_count"
  i=1
  while [ "$i" -le "$raw_segment_count" ]; do
    grep -qx "$i" "$seen_indexes" || die "reducer is missing sliced segment $i"
    i=$((i + 1))
  done
  stage_event stage_complete shard_reduce "$SHARD_COUNT" "$SHARD_COUNT"
}

publish_slice_segment() {
  publish_index=$1
  slice_path=$(cat "$STATE_ROOT/slice-$publish_index.path")
  slice_sha=$(cat "$STATE_ROOT/slice-$publish_index.sha256")
  slice_manifest_sha=$(cat "$STATE_ROOT/slice-$publish_index.manifest-sha256")
  slice_name=$(basename "$slice_path")
  canonical_relpath "$slice_name" || die "slice filename is not canonical: $slice_name"
  dest_dir=$SLICE_PUBLISH_ROOT/segment-$publish_index
  mkdir -p "$dest_dir"
  dest_slice=$dest_dir/$slice_name
  dest_manifest=$dest_slice.manifest.json
  dest_success=$dest_slice._SUCCESS
  publish_verified_file "slice_$publish_index" "$slice_path" "$dest_slice" "$slice_sha"
  publish_verified_file "slice_${publish_index}_manifest" "$slice_path.manifest.json" "$dest_manifest" "$slice_manifest_sha"
  publish_verified_file "slice_${publish_index}_success" "$slice_path._SUCCESS" "$dest_success" "$(sha256_file "$slice_path._SUCCESS")"
  [ "$(cat "$dest_success")" = "$slice_sha" ] || die "published slice success marker content mismatch"
  printf '%s\t%s\t%s\t%s\n' "$publish_index" "$(relative_run_path "$dest_slice")" "$slice_sha" "$slice_manifest_sha"
}

if [ "$ROLE" = "reduce" ]; then
  load_shard_receipts
elif [ "$ROLE" = "slice" ]; then
  shard_total=$((SHARD_END - SHARD_START + 1))
  stage_event stage_start slicing 0 "$shard_total"
  mkdir -p "$SHARD_DIR" "$SLICE_PUBLISH_ROOT"
  : >"$STATE_ROOT/shard-segments"
  i=$SHARD_START
  seen=0
  while [ "$i" -le "$SHARD_END" ]; do
    slice_one_segment "$i"
    publish_slice_segment "$i" >>"$STATE_ROOT/shard-segments"
    seen=$((seen + 1))
    progress_event slicing "$seen" "$shard_total" 10 "last_index=$i slice_sha256=$(cat "$STATE_ROOT/slice-$i.sha256") manifest_sha256=$(cat "$STATE_ROOT/slice-$i.manifest-sha256") state_file=slicer-$i.json"
    i=$((i + 1))
  done
  stage_event stage_complete slicing "$shard_total" "$shard_total"
  {
    printf '{\n'
    printf '  "schema_version": "monday.cex_materialization_shard_receipt.v1",\n'
    printf '  "run_id": "%s",\n' "$run_id"
    printf '  "inventory_sha256": "%s",\n' "$inventory_sha256"
    printf '  "shard_index": "%s",\n' "$SHARD_INDEX"
    printf '  "shard_count": "%s",\n' "$SHARD_COUNT"
    printf '  "segment_start": "%s",\n' "$SHARD_START"
    printf '  "segment_end": "%s"\n' "$SHARD_END"
    printf '}\n'
  } >"$STATE_ROOT/shard-receipt.json"
  shard_receipt_sha=$(sha256_file "$STATE_ROOT/shard-receipt.json")
  publish_verified_file shard_receipt "$STATE_ROOT/shard-receipt.json" "$SHARD_RECEIPT_PATH" "$shard_receipt_sha"
  publish_verified_file shard_segment_list "$STATE_ROOT/shard-segments" "$SHARD_DIR/segments" "$(sha256_file "$STATE_ROOT/shard-segments")"
  printf '%s\n' "$shard_receipt_sha" >"$STATE_ROOT/shard-success"
  publish_verified_file shard_success "$STATE_ROOT/shard-success" "$SHARD_DIR/_SUCCESS" "$(sha256_file "$STATE_ROOT/shard-success")"
  log "schema_version=monday.research_event.v1 component=cex-materialization event=run_complete run_id=$run_id role=slice shard_index=$SHARD_INDEX shard_count=$SHARD_COUNT segment_start=$SHARD_START segment_end=$SHARD_END"
  exit 0
else
  stage_event stage_start slicing 0 "$raw_segment_count"
  i=1
  while [ "$i" -le "$raw_segment_count" ]; do
    slice_one_segment "$i"
    progress_event slicing "$i" "$raw_segment_count" 10 "last_index=$i slice_sha256=$(cat "$STATE_ROOT/slice-$i.sha256") manifest_sha256=$(cat "$STATE_ROOT/slice-$i.manifest-sha256") state_file=slicer-$i.json"
    i=$((i + 1))
  done
  stage_event stage_complete slicing "$raw_segment_count" "$raw_segment_count"
fi

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
stage_event stage_start pit_materialization 0 1
if ! "$@" >"$pit_stdout"; then
  die "PIT materializer failed"
fi

feature_path=$(json_string_field artifact_path "$pit_stdout")
feature_sha=$(json_string_field artifact_sha256 "$pit_stdout")
materialization_path=$(json_string_field report_path "$pit_stdout")
materialization_sha=$(json_string_field report_sha256 "$pit_stdout")
[ -f "$feature_path" ] || die "feature artifact is missing: $feature_path"
[ -f "$materialization_path" ] || die "materialization report is missing: $materialization_path"
[ "$(sha256_file "$feature_path")" = "$feature_sha" ] || die "feature artifact readback SHA mismatch"
[ "$(sha256_file "$materialization_path")" = "$materialization_sha" ] || die "materialization report readback SHA mismatch"
feature_publish_path=$(published_path_for "$feature_path" "$LOCAL_MATERIALIZATION_DIR" "$MATERIALIZATION_DIR" "feature artifact")
rewritten_materialization=$STATE_ROOT/materialization-published-path.json
rewrite_materialization_artifact_path "$materialization_path" "$feature_path" "$feature_publish_path" "$rewritten_materialization"
materialization_sha=$(sha256_file "$rewritten_materialization")
materialization_rewritten_path=$LOCAL_MATERIALIZATION_DIR/$materialization_sha.materialization.json
[ ! -e "$materialization_rewritten_path" ] || die "rewritten materialization report already exists: $materialization_rewritten_path"
mv "$rewritten_materialization" "$materialization_rewritten_path"
rm -f "$materialization_path"
materialization_path=$materialization_rewritten_path
stage_event stage_complete pit_materialization 1 1 "feature_sha256=$feature_sha materialization_sha256=$materialization_sha"

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
stage_event stage_start replay_materialization 0 1
if ! "$@" >"$replay_stdout"; then
  die "replay materializer failed"
fi

replay_manifest_path=$(json_string_field manifest_path "$replay_stdout")
replay_manifest_sha=$(json_string_field manifest_sha256 "$replay_stdout")
replay_artifact_rel=$(json_string_field artifact_path "$replay_stdout")
replay_artifact_sha=$(json_string_field artifact_sha256 "$replay_stdout")
replay_artifact_path=$LOCAL_REPLAY_DIR/$replay_artifact_rel
[ -f "$replay_artifact_path" ] || die "replay artifact is missing: $replay_artifact_path"
[ -f "$replay_manifest_path" ] || die "replay manifest is missing: $replay_manifest_path"
[ "$(sha256_file "$replay_artifact_path")" = "$replay_artifact_sha" ] || die "replay artifact readback SHA mismatch"
[ "$(sha256_file "$replay_manifest_path")" = "$replay_manifest_sha" ] || die "replay manifest readback SHA mismatch"
stage_event stage_complete replay_materialization 1 1 "replay_artifact_sha256=$replay_artifact_sha replay_manifest_sha256=$replay_manifest_sha"

materialization_publish_path=$(published_path_for "$materialization_path" "$LOCAL_MATERIALIZATION_DIR" "$MATERIALIZATION_DIR" "materialization report")
replay_artifact_publish_path=$(published_path_for "$replay_artifact_path" "$LOCAL_REPLAY_DIR" "$REPLAY_DIR" "replay artifact")
replay_manifest_publish_path=$(published_path_for "$replay_manifest_path" "$LOCAL_REPLAY_DIR" "$REPLAY_DIR" "replay manifest")

stage_event stage_start receipt_build 0 1
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
stage_event stage_complete receipt_build 1 1 "campaign_inputs_sha256=$campaign_inputs_sha materialization_receipt_sha256=$materialization_receipt_sha"
stage_event stage_start publish 0 1
mkdir -p "$MATERIALIZATION_DIR" "$REPLAY_DIR" "$RECEIPT_DIR"
publish_verified_file feature_artifact "$feature_path" "$feature_publish_path" "$feature_sha"
publish_verified_file materialization_report "$materialization_path" "$materialization_publish_path" "$materialization_sha"
publish_verified_file replay_artifact "$replay_artifact_path" "$replay_artifact_publish_path" "$replay_artifact_sha"
publish_verified_file replay_manifest "$replay_manifest_path" "$replay_manifest_publish_path" "$replay_manifest_sha"
publish_verified_file frozen_inventory "$LOCAL_RECEIPT_DIR/frozen-inventory.env" "$RECEIPT_DIR/frozen-inventory.env" "$inventory_sha256"
publish_verified_file campaign_inputs "$LOCAL_RECEIPT_DIR/campaign-inputs.json" "$RECEIPT_DIR/campaign-inputs.json" "$campaign_inputs_sha"
publish_verified_file materialization_receipt "$LOCAL_RECEIPT_DIR/materialization-receipt.json" "$RECEIPT_DIR/materialization-receipt.json" "$materialization_receipt_sha"
stage_event stage_complete publish 1 1 "artifact_count=7 campaign_inputs_sha256=$campaign_inputs_sha materialization_receipt_sha256=$materialization_receipt_sha"

log "schema_version=monday.research_event.v1 component=cex-materialization event=run_complete run_id=$run_id mission_id=$mission_id feature_sha256=$feature_sha materialization_sha256=$materialization_sha replay_artifact_sha256=$replay_artifact_sha replay_manifest_sha256=$replay_manifest_sha campaign_inputs_sha256=$campaign_inputs_sha materialization_receipt_sha256=$materialization_receipt_sha"
