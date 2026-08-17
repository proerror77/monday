#!/usr/bin/env bash
#
# Contract tests for binance-lob-slice-materialize.sh. Runs the driver against
# a /tmp fixture lake with a stubbed `aliyun ossutil` CLI (same stub pattern as
# test-data-completeness-check.sh) plus stub slicer/materializer binaries, so
# the suite runs fully offline.
#
# Contract under test (#911 review):
#   * OSS enumeration is recursive: data objects live under hour=HH/ one level
#     below the listed date= prefix, so a shallow listing sees no segments.
#   * Zero matching segments is a failure, not a green run.
#   * The slice cache identity carries the requested symbol set: widening
#     --symbols on a reused WORK_DIR re-slices instead of silently skipping
#     the new symbols, and any symbol still pending fails the run.
#   * A run-manifest build/publish failure is a terminal nonzero exit.
#
# Usage: ./test-binance-lob-slice-materialize.sh
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
driver="$script_dir/binance-lob-slice-materialize.sh"
test_root=$(mktemp -d /tmp/monday-slice-materialize-test.XXXXXX)
trap 'chmod -R u+w "$test_root" 2>/dev/null; rm -rf "$test_root"' EXIT

stub_dir="$test_root/bin"
fake_oss="$test_root/oss"
out_file="$test_root/out"
err_file="$test_root/err"
call_log="$test_root/calls.log"
mkdir -p "$stub_dir" "$fake_oss"

BUCKET=monday-lob-apne1-1045353359
DATE=2026-08-16
PREFIX="lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all/date=$DATE/hour=12"
SHA_A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
SHA_B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

pass_count=0
fail_count=0

expect() {
  # $1 description, $2 condition (0 = pass)
  if [ "$2" -eq 0 ]; then
    pass_count=$((pass_count + 1))
    printf 'PASS: %s\n' "$1"
  else
    fail_count=$((fail_count + 1))
    printf 'FAIL: %s\n' "$1"
  fi
}

rc_is() { [ "$RC" -eq "$1" ]; }
grep_err() { grep -q "$1" "$err_file"; }

# Stubbed aliyun CLI: `ossutil ls` is backed by the fixture tree under
# FAKE_OSS_ROOT and only ever lists recursively (a shallow listing would just
# return hour= prefixes); `ossutil cp` copies one fixture object.
cat >"$stub_dir/aliyun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$STUB_CALL_LOG"
[[ ${1:-} == ossutil ]] || exit 2
case ${2:-} in
  ls)
    uri=$3
    [[ $uri == oss://"$STUB_OSS_BUCKET"/* ]] || exit 2
    key=${uri#oss://"$STUB_OSS_BUCKET"/}
    [[ -d $FAKE_OSS_ROOT/$key ]] || exit 0
    (cd "$FAKE_OSS_ROOT" && find "$key" -type f | sort | sed "s|^|oss://$STUB_OSS_BUCKET/|")
    ;;
  cp)
    src=$3
    dst=$4
    key=${src#oss://"$STUB_OSS_BUCKET"/}
    cp "$FAKE_OSS_ROOT/$key" "$dst"
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$stub_dir/aliyun"

# Stubbed slicer: emits a one-slice report covering the requested symbols (or
# STUB_SLICER_SYMBOLS when set, to simulate a slice set that does not cover
# the whole request).
cat >"$stub_dir/slicer" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'slicer %s\n' "$*" >>"$STUB_CALL_LOG"
out=""
symbols=""
while [[ $# -gt 0 ]]; do
  case $1 in
    --output-dir) out=$2; shift 2 ;;
    --symbols) symbols=$2; shift 2 ;;
    *) shift ;;
  esac
done
effective=${STUB_SLICER_SYMBOLS:-$symbols}
json_symbols=$(printf '%s' "$effective" | tr ',' '\n' | jq -R . | jq -cs .)
mkdir -p "$out"
printf 'slice\n' >"$out/slice-001.jsonl.zst"
jq -nc --argjson syms "$json_symbols" --arg sha "$STUB_SHA_A" --arg msha "$STUB_SHA_B" \
  '{source:{file:"part-0000.jsonl.zst",sha256:$sha,manifest_sha256:$msha,events:1,decompressed_bytes:1,declared_symbols:2,selected_symbols:($syms|length)},slices:[{file:"slice-001.jsonl.zst",sha256:$sha,manifest_sha256:$msha,symbols:$syms,events:1,decompressed_bytes:1,compressed_bytes:1,start_received_at_ns:1,end_received_at_ns:2}]}'
EOF
chmod +x "$stub_dir/slicer"

# Stubbed materializer: always succeeds and prints the publication JSON the
# driver reduces into its ok records.
cat >"$stub_dir/materializer" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'materializer %s\n' "$*" >>"$STUB_CALL_LOG"
jq -nc --arg sha "$STUB_SHA_A" --arg msha "$STUB_SHA_B" \
  '{manifest:{mission_id:"m",rows:1,artifact_sha256:$sha},manifest_path:"/tmp/x",manifest_sha256:$msha}'
EOF
chmod +x "$stub_dir/materializer"

WORK_DIR=""
ARTIFACT_DIR=""

run_driver() {
  # $1 = scenario name (isolates work/artifact dirs); remaining args passthrough
  local scenario=$1
  shift
  WORK_DIR="$test_root/work-$scenario"
  ARTIFACT_DIR="$test_root/artifacts-$scenario"
  set +e
  env \
    STUB_OSS_BUCKET="$BUCKET" \
    STUB_CALL_LOG="$call_log" \
    STUB_SHA_A="$SHA_A" \
    STUB_SHA_B="$SHA_B" \
    ${STUB_SLICER_SYMBOLS:+STUB_SLICER_SYMBOLS="$STUB_SLICER_SYMBOLS"} \
    FAKE_OSS_ROOT="$fake_oss" \
    SLICER_BIN="$stub_dir/slicer" \
    MATERIALIZER_BIN="$stub_dir/materializer" \
    PATH="$stub_dir:$PATH" \
    "$driver" --market spot --start-date "$DATE" --end-date "$DATE" \
    --work-dir "$WORK_DIR" --artifact-dir "$ARTIFACT_DIR" "$@" \
    >"$out_file" 2>"$err_file"
  RC=$?
  set -e
  if [ "$RC" -ne 0 ] && [ -s "$err_file" ]; then
    printf 'stderr: %s\n' "$(cat "$err_file")" >&2
  fi
}

reset_fixture() {
  STUB_SLICER_SYMBOLS=""
  : >"$call_log"
  rm -rf "$fake_oss"
  mkdir -p "$fake_oss/$PREFIX"
  : >"$fake_oss/$PREFIX/part-0000.jsonl.zst"
  : >"$fake_oss/$PREFIX/part-0000.jsonl.zst.manifest.json"
  : >"$fake_oss/$PREFIX/part-0000.jsonl.zst._SUCCESS"
}

run_manifest() {
  jq -e "$1" "$ARTIFACT_DIR/slice-materialization-run.json" >/dev/null 2>&1
}

# --- scenario: happy path enumerates recursively and materializes ------------
reset_fixture
run_driver happy --symbols BTCUSDT
expect 'happy: exit 0' "$(rc_is 0 && echo 0 || echo 1)"
expect 'happy: listing is recursive' \
  "$(grep -q 'ossutil ls oss://[^ ]* --recursive --short-format' "$call_log" && echo 0 || echo 1)"
expect 'happy: one segment seen' \
  "$(run_manifest '.segments_seen == 1' && echo 0 || echo 1)"
expect 'happy: one symbol materialized' \
  "$(run_manifest '(.materialized | length) == 1 and .materialized[0].symbol == "BTCUSDT"' && echo 0 || echo 1)"
expect 'happy: no failures' "$(run_manifest '.failed == []' && echo 0 || echo 1)"

# --- scenario: zero segments fail closed --------------------------------------
reset_fixture
rm -rf "$fake_oss/lake"
run_driver empty --symbols BTCUSDT
expect 'empty: exit nonzero' "$(! rc_is 0 && echo 0 || echo 1)"
expect 'empty: stderr names the breach' \
  "$(grep_err 'no market-tape segments' && echo 0 || echo 1)"
expect 'empty: manifest records the failure' \
  "$(run_manifest '.segments_seen == 0 and .failed[0].error == "no_segments_found"' && echo 0 || echo 1)"

# --- scenario: widening --symbols re-slices on the same WORK_DIR ---------------
reset_fixture
run_driver widen --symbols BTCUSDT
expect 'widen: first run exit 0' "$(rc_is 0 && echo 0 || echo 1)"
run_driver widen --symbols BTCUSDT,ETHUSDT
expect 'widen: second run exit 0' "$(rc_is 0 && echo 0 || echo 1)"
expect 'widen: slicer ran again for the new symbol set' \
  "$([ "$(grep -c '^slicer ' "$call_log")" -eq 2 ] && echo 0 || echo 1)"
expect 'widen: both symbols materialized' \
  "$(run_manifest '[.materialized[].symbol] | sort == ["BTCUSDT", "ETHUSDT"]' && echo 0 || echo 1)"
expect 'widen: no failures' "$(run_manifest '.failed == []' && echo 0 || echo 1)"

# --- scenario: a symbol left pending fails the run -----------------------------
reset_fixture
STUB_SLICER_SYMBOLS=BTCUSDT
run_driver pending --symbols BTCUSDT,ETHUSDT
expect 'pending: exit nonzero' "$(! rc_is 0 && echo 0 || echo 1)"
expect 'pending: failure names the pending symbol' \
  "$(run_manifest '(.failed | map(.error)) == ["symbol_pending"] and .failed[0].symbol == "ETHUSDT"' && echo 0 || echo 1)"
expect 'pending: covered symbol still recorded' \
  "$(run_manifest '.materialized | length == 1' && echo 0 || echo 1)"

# --- scenario: run-manifest publish failure is terminal ------------------------
reset_fixture
mkdir -p "$test_root/artifacts-blocked"
chmod 555 "$test_root/artifacts-blocked"
set +e
env \
  STUB_OSS_BUCKET="$BUCKET" \
  STUB_CALL_LOG="$call_log" \
  STUB_SHA_A="$SHA_A" \
  STUB_SHA_B="$SHA_B" \
  FAKE_OSS_ROOT="$fake_oss" \
  SLICER_BIN="$stub_dir/slicer" \
  MATERIALIZER_BIN="$stub_dir/materializer" \
  PATH="$stub_dir:$PATH" \
  "$driver" --market spot --start-date "$DATE" --end-date "$DATE" \
  --symbols BTCUSDT --work-dir "$test_root/work-blocked" \
  --artifact-dir "$test_root/artifacts-blocked" >"$out_file" 2>"$err_file"
RC=$?
set -e
chmod 755 "$test_root/artifacts-blocked"
expect 'manifest: exit nonzero when publish fails' "$(! rc_is 0 && echo 0 || echo 1)"
expect 'manifest: stderr names the publish failure' \
  "$(grep_err 'failed to build the run manifest' && echo 0 || echo 1)"

printf '\n%d passed, %d failed\n' "$pass_count" "$fail_count"
[ "$fail_count" -eq 0 ]
