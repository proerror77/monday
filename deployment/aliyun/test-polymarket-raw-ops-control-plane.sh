#!/usr/bin/env bash
# Static contract greps intentionally use literal shell expressions.
# shellcheck disable=SC1090,SC2016
set -euo pipefail

export LC_ALL=C
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly RUST_MANIFEST="$SCRIPT_DIR/../../rust_hft/Cargo.toml"
readonly VERIFY="$SCRIPT_DIR/../../rust_hft/target/debug/polymarket-raw-ops"
readonly POLICY="$SCRIPT_DIR/polymarket-shadow-gate-policy.jq"
readonly LEGACY_HEALTH_POLICY="$SCRIPT_DIR/polymarket-legacy-health-policy.jq"
readonly RUST_HEALTH_POLICY="$SCRIPT_DIR/polymarket-rust-health-policy.jq"
readonly GATE="$SCRIPT_DIR/polymarket-raw-ops-shadow-gate.sh"
readonly CUTOVER="$SCRIPT_DIR/polymarket-raw-ops-cutover.sh"
readonly WORKFLOW="$SCRIPT_DIR/../../.github/workflows/acr-publish.yml"

for command in cargo chmod cp grep jq mkdir mktemp mv rm sed sha256sum shellcheck; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing control-plane test dependency: %s\n' "$command" >&2
    exit 2
  }
done

shellcheck "$GATE" "$CUTOVER" "$0"
cargo build --quiet --manifest-path "$RUST_MANIFEST" -p hft-collector \
  --bin polymarket-raw-ops --no-default-features --locked
"$VERIFY" verify-shadow-parity --help >/dev/null

tmp_dir=$(mktemp -d)
tmp_dir=$(cd -- "$tmp_dir" && pwd -P)
trap 'rm -rf "$tmp_dir"' EXIT

# Exercise the production marker verifier itself. A marker is valid only when
# it contains the one content-addressed gate.json entry; sha256sum otherwise
# accepts an unrelated entry or a valid gate entry followed by extra entries.
marker_verifier="$tmp_dir/verify-gate-marker.sh"
sed -n '/^verify_gate_marker() {$/,/^}$/p' "$CUTOVER" >"$marker_verifier"
# shellcheck source=/dev/null
source "$marker_verifier"
declare -F verify_gate_marker >/dev/null || {
  printf 'cutover does not expose its gate-marker verifier to contract tests\n' >&2
  exit 1
}
marker_dir="$tmp_dir/marker"
mkdir "$marker_dir"
printf 'gate evidence\n' >"$marker_dir/gate.json"
printf 'unrelated evidence\n' >"$marker_dir/other.json"
(
  cd "$marker_dir"
  sha256sum gate.json >PASSED.sha256
)
verify_gate_marker "$marker_dir" || {
  printf 'gate-marker verifier rejected the exact gate.json marker\n' >&2
  exit 1
}
(
  cd "$marker_dir"
  sha256sum other.json >PASSED.sha256
)
if verify_gate_marker "$marker_dir"; then
  printf 'gate-marker verifier accepted a marker for another file\n' >&2
  exit 1
fi
(
  cd "$marker_dir"
  sha256sum gate.json other.json >PASSED.sha256
)
if verify_gate_marker "$marker_dir"; then
  printf 'gate-marker verifier accepted a multi-entry marker\n' >&2
  exit 1
fi

legacy="$tmp_dir/legacy"
rust="$tmp_dir/rust"
mkdir -p "$legacy" "$rust"
legacy_tape="$legacy/market-updates.ndjson"
rust_closed="$rust/market-updates.19700101T000400000000.ndjson"
: >"$legacy_tape"
: >"$rust_closed"
: >"$rust/market-updates.ndjson"

sequence=0
append_row() {
  local update=$1 row
  row=$(jq -cn --argjson sequence "$sequence" --argjson update "$update" \
    '{sequence:$sequence,recorded_at:"1970-01-01T00:03:20Z",update:$update}')
  printf '%s\n' "$row" >>"$legacy_tape"
  printf '%s\n' "$row" >>"$rust_closed"
  sequence=$((sequence + 1))
}

for symbol in BTCUSDT ETHUSDT SOLUSDT XRPUSDT DOGEUSDT HYPEUSDT BNBUSDT; do
  append_row "$(jq -cn --arg symbol "$symbol" \
    '{kind:"market_metadata",market_id:("market-" + $symbol),
      condition_id:("condition-" + $symbol),symbol:$symbol,market_window_secs:300,
      source:"gamma_api",retrieved_at:"1970-01-01T00:03:20Z",
      market:{id:("market-" + $symbol),conditionId:("condition-" + $symbol),
        question:($symbol + " Up or Down"),slug:("market-" + $symbol),
        startDate:"1970-01-01T00:00:00Z",endDate:"1970-01-01T00:05:00Z",
        outcomes:["Up","Down"],clobTokenIds:[("up-" + $symbol),("down-" + $symbol)],
        orderPriceMinTickSize:0.01,orderMinSize:5,feesEnabled:true}}')"
done

trade=$(jq -cn \
  '{kind:"polymarket_trade",record_id:"trade-1",record_id_version:"v2",
    market_id:"market-BTCUSDT",condition_id:"condition-BTCUSDT",token_id:"token-1",
    symbol:"BTCUSDT",market_window_secs:300,side:"BUY",size:"1",price:"0.5",
    trade_ts:"1970-01-01T00:03:20Z",trade_ts_unix:200,
    transaction_hash:"0x1",proxy_wallet:"0x2",outcome:"Up",outcome_index:0,
    source:"polymarket_data_api",received_at:"1970-01-01T00:03:20Z",
    trade:{transactionHash:"0x1",conditionId:"condition-BTCUSDT",asset:"token-1",
      side:"BUY",timestamp:200,proxyWallet:"0x2",size:"1",price:"0.5",
      outcomeIndex:0,outcome:"Up"}}')
append_row "$trade"

append_row "$(jq -cn \
  '{kind:"market_settlement",market_id:"market-BTCUSDT",
    condition_id:"condition-BTCUSDT",symbol:"BTCUSDT",market_window_secs:300,
    winning_token_id:"token-1",winning_outcome:"Up",resolved_up_won:true,
    resolution_source:"gamma_api_closed_market",retrieved_at:"1970-01-01T00:03:20Z",
    market:{id:"market-BTCUSDT",conditionId:"condition-BTCUSDT",closed:true,
      outcomes:["Up","Down"],clobTokenIds:["token-1","token-2"],
      outcomePrices:["1","0"]}}')"

# A delayed historical trade recorded only after the common cutoff must not make
# the bounded snapshot flaky, even when its source timestamp is in-window.
late_trade=$(jq -c \
  '.record_id = "trade-after-cutoff"
    | .trade_ts_unix = 200
    | .trade_ts = "1970-01-01T00:03:20Z"
    | .trade.timestamp = 200
    | .post_cutoff_only = true
    | .trade.postCutoffOnly = true' <<<"$trade")
jq -cn --argjson sequence "$sequence" --argjson update "$late_trade" \
  '{sequence:$sequence,recorded_at:"1970-01-01T00:05:01Z",update:$update}' \
  >>"$legacy_tape"

parity="$tmp_dir/parity.json"
"$VERIFY" verify-shadow-parity \
  --legacy-spool "$legacy" --rust-spool "$rust" --started-at-unix 100 \
  --ended-at-unix 300 \
  --output "$parity"
jq -e '.passed == true and .checks.metadata_parity == true
  and ([.checks[]] | all)' "$parity" >/dev/null

rust_bad="$tmp_dir/rust-bad"
cp -R "$rust" "$rust_bad"
jq -cn --argjson update "$trade" \
  '{sequence:0,recorded_at:"1970-01-01T00:03:21Z",update:$update}' \
  >"$rust_bad/market-updates.ndjson"
if "$VERIFY" verify-shadow-parity \
  --legacy-spool "$legacy" --rust-spool "$rust_bad" --started-at-unix 100 \
  --ended-at-unix 300 \
  --output "$tmp_dir/bad-parity.json" 2>/dev/null; then
  printf 'parity verifier accepted a duplicate Rust trade ID\n' >&2
  exit 1
fi
jq -e '.passed == false and .checks.dedupe_parity == false' \
  "$tmp_dir/bad-parity.json" >/dev/null

rust_bad_metadata="$tmp_dir/rust-bad-metadata"
cp -R "$rust" "$rust_bad_metadata"
jq -c '
  if .update.kind == "market_metadata" and .update.symbol == "BTCUSDT"
  then .update.market.clobTokenIds = ["tampered-up", "tampered-down"]
  else . end
' "$rust_bad_metadata/market-updates.19700101T000400000000.ndjson" \
  >"$rust_bad_metadata/market-updates.rewritten"
mv "$rust_bad_metadata/market-updates.rewritten" \
  "$rust_bad_metadata/market-updates.19700101T000400000000.ndjson"
if "$VERIFY" verify-shadow-parity \
  --legacy-spool "$legacy" --rust-spool "$rust_bad_metadata" \
  --started-at-unix 100 --ended-at-unix 300 \
  --output "$tmp_dir/bad-metadata-parity.json" 2>/dev/null; then
  printf 'parity verifier accepted contradictory metadata values\n' >&2
  exit 1
fi
jq -e '.passed == false and .checks.metadata_parity == false' \
  "$tmp_dir/bad-metadata-parity.json" >/dev/null

candidate=$(printf 'a%.0s' {1..64})
source_revision=$(printf 'b%.0s' {1..40})
bundle=$(printf 'c%.0s' {1..64})
oss_config=$(printf 'd%.0s' {1..64})
legacy_cmdline=dffeb118d105e9312898460249f514eb982c20433cd20840ffb2107c64bbca4a
jq \
  --arg candidate "$candidate" \
  --arg source "$source_revision" \
  --arg bundle "$bundle" \
  --arg oss_config "$oss_config" \
  --arg legacy_cmdline "$legacy_cmdline" \
  '. + {
    schema:"monday.polymarket_shadow_gate.v1",
    candidate_sha256:$candidate,
    deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,
    oss_config_sha256:$oss_config,
    duration_seconds:3900,
    parity_window_started_at_unix:100,
    parity_window_ended_at_unix:400,
    completed_at:"2026-07-15T00:00:00Z",
    shadow_run_id:"run-1",
    production_eligible:true,
    legacy_runtime:{
      exec_start:"/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py",
      cmdline:"/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py",
      cmdline_sha256:$legacy_cmdline,
      fragment_path:"/etc/systemd/system/polymarket-reference-collector.service",
      drop_in_paths:[],main_pid:10,restarts:0
    },
    shadow_runtime:{
      exec_start:("/opt/monday/releases/polymarket-raw-ops/" + $candidate
        + "/polymarket-raw-ops collect-reference --spool-dir ${MONDAY_POLYMARKET_SHADOW_SPOOL}"),
      cmdline:("/opt/monday/releases/polymarket-raw-ops/" + $candidate
        + "/polymarket-raw-ops collect-reference --spool-dir "
        + "/data/monday/spool/polymarket-reference-rust-shadow/" + $candidate + "/run-1"),
      fragment_path:"/etc/systemd/system/polymarket-reference-collector-shadow@.service",
      drop_in_paths:[],main_pid:11,restarts:0
    },
    checks:(.checks + {health_freshness:true,candidate_identity:true,
      oss_readback_parity:true,market_oss_readback_parity:true}),
    metrics:(.metrics + {
      oss_uploaded_segments:1,oss_canonical_uploaded_segments:1,
      market_oss_uploaded_segments:1,market_oss_canonical_uploaded_segments:1
    })
  } | .passed = true' "$parity" >"$tmp_dir/gate.json"
jq -e -f "$POLICY" "$tmp_dir/gate.json" >/dev/null
jq '.duration_seconds = 3599' "$tmp_dir/gate.json" >"$tmp_dir/short.json"
if jq -e -f "$POLICY" "$tmp_dir/short.json" >/dev/null; then
  printf 'gate policy accepted a shadow shorter than one hour\n' >&2
  exit 1
fi
jq '.production_eligible = false' "$tmp_dir/gate.json" >"$tmp_dir/test-only.json"
if jq -e -f "$POLICY" "$tmp_dir/test-only.json" >/dev/null; then
  printf 'gate policy accepted test-only evidence\n' >&2
  exit 1
fi
jq '.parity_window_ended_at_unix = 399' "$tmp_dir/gate.json" \
  >"$tmp_dir/short-parity-tail.json"
if jq -e -f "$POLICY" "$tmp_dir/short-parity-tail.json" >/dev/null; then
  printf 'gate policy accepted a parity tail shorter than five minutes\n' >&2
  exit 1
fi
jq '.checks.metadata_parity = false | .passed = true' "$tmp_dir/gate.json" \
  >"$tmp_dir/bad-metadata-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/bad-metadata-gate.json" >/dev/null; then
  printf 'gate policy ignored failed metadata parity\n' >&2
  exit 1
fi
jq '.checks.market_oss_readback_parity = false | .passed = true' \
  "$tmp_dir/gate.json" >"$tmp_dir/bad-market-upload-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/bad-market-upload-gate.json" >/dev/null; then
  printf 'gate policy ignored failed market-tape uploader readback\n' >&2
  exit 1
fi
jq '.metrics.oss_canonical_uploaded_segments = 0' "$tmp_dir/gate.json" \
  >"$tmp_dir/noncanonical-reference-upload.json"
if jq -e -f "$POLICY" "$tmp_dir/noncanonical-reference-upload.json" >/dev/null; then
  printf 'gate policy accepted a noncanonical reference upload\n' >&2
  exit 1
fi
jq '.metrics.market_oss_canonical_uploaded_segments = 0' "$tmp_dir/gate.json" \
  >"$tmp_dir/noncanonical-market-upload.json"
if jq -e -f "$POLICY" "$tmp_dir/noncanonical-market-upload.json" >/dev/null; then
  printf 'gate policy accepted a noncanonical market-tape upload\n' >&2
  exit 1
fi
jq 'del(.oss_config_sha256)' "$tmp_dir/gate.json" >"$tmp_dir/unbound-oss-config.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-oss-config.json" >/dev/null; then
  printf 'gate policy accepted evidence without an OSS configuration identity\n' >&2
  exit 1
fi
jq '.legacy_runtime.restarts = 1' "$tmp_dir/gate.json" >"$tmp_dir/restarted-legacy.json"
if jq -e -f "$POLICY" "$tmp_dir/restarted-legacy.json" >/dev/null; then
  printf 'gate policy accepted a restarted legacy collector\n' >&2
  exit 1
fi
jq '.legacy_runtime.drop_in_paths = ["/etc/systemd/system/polymarket-reference-collector.service.d/override.conf"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-drop-in.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-drop-in.json" >/dev/null; then
  printf 'gate policy accepted a legacy collector with a systemd drop-in\n' >&2
  exit 1
fi
jq '.legacy_runtime.cmdline_sha256 = "not-a-digest"' "$tmp_dir/gate.json" \
  >"$tmp_dir/unverified-legacy-cmdline.json"
if jq -e -f "$POLICY" "$tmp_dir/unverified-legacy-cmdline.json" >/dev/null; then
  printf 'gate policy accepted an unverified legacy command line\n' >&2
  exit 1
fi
jq '.legacy_runtime.cmdline_sha256 = ("f" * 64)' "$tmp_dir/gate.json" \
  >"$tmp_dir/mismatched-legacy-cmdline-digest.json"
if jq -e -f "$POLICY" "$tmp_dir/mismatched-legacy-cmdline-digest.json" >/dev/null; then
  printf 'gate policy accepted a mismatched legacy command-line digest\n' >&2
  exit 1
fi
jq '.legacy_runtime.cmdline += " --once"' "$tmp_dir/gate.json" \
  >"$tmp_dir/legacy-once-cmdline.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-once-cmdline.json" >/dev/null; then
  printf 'gate policy accepted a legacy --once command line\n' >&2
  exit 1
fi
jq '.shadow_runtime.drop_in_paths = ["/etc/systemd/system/polymarket-reference-collector-shadow@.service.d/override.conf"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/shadow-drop-in.json"
if jq -e -f "$POLICY" "$tmp_dir/shadow-drop-in.json" >/dev/null; then
  printf 'gate policy accepted a Rust shadow with a systemd drop-in\n' >&2
  exit 1
fi
jq '.shadow_runtime.cmdline += " --once"' "$tmp_dir/gate.json" \
  >"$tmp_dir/shadow-once-cmdline.json"
if jq -e -f "$POLICY" "$tmp_dir/shadow-once-cmdline.json" >/dev/null; then
  printf 'gate policy accepted a Rust shadow --once command line\n' >&2
  exit 1
fi

jq -n '{
  updated_at:"2026-07-15T00:00:01Z",last_success_at:"2026-07-15T00:00:01Z",
  target_markets:14,api_errors:[],malformed_trade_rows:0,
  truncated_trade_markets:[],stale_trade_markets:[],stale_settlement_markets:[],
  overdue_unresolved_markets:[]
}' >"$tmp_dir/legacy-health.json"
jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/legacy-health.json" >/dev/null
for mutation in \
  '.api_errors = ["Gamma unavailable"]' \
  '.malformed_trade_rows = 1' \
  '.truncated_trade_markets = ["condition-1"]' \
  '.stale_trade_markets = ["condition-1"]' \
  '.stale_settlement_markets = ["market-1"]' \
  '.overdue_unresolved_markets = ["market-1"]'; do
  jq "$mutation" "$tmp_dir/legacy-health.json" >"$tmp_dir/bad-legacy-health.json"
  if jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/bad-legacy-health.json" >/dev/null; then
    printf 'legacy health policy accepted failure mutation: %s\n' "$mutation" >&2
    exit 1
  fi
done

jq -n '{
  updated_at:"2026-07-15T00:00:01Z",last_success_at:"2026-07-15T00:00:01Z",
  target_markets:14,missing_target_symbols:[],api_errors:[],malformed_trade_rows:0,
  trade_poll_budget:192,eligible_trade_markets:14,priority_trade_markets:8,
  selected_trade_markets:14,deferred_trade_markets:0,priority_trade_backlog:0,
  trade_polls:14,successful_trade_polls:14,
  truncated_trade_markets:[],non_object_trade_markets:[],invalid_settlement_markets:[],
  invalid_end_time_markets:[],stale_trade_markets:[],stale_settlement_markets:[],
  overdue_unresolved_markets:[]
}' >"$tmp_dir/rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/rust-health.json" >/dev/null
for mutation in \
  'del(.last_success_at)' \
  '.priority_trade_backlog = 1' \
  '.selected_trade_markets = 13' \
  '.deferred_trade_markets = 1' \
  '.eligible_trade_markets = 13' \
  '.successful_trade_polls = 15' \
  '.missing_target_symbols = ["BTCUSDT"]' \
  '.api_errors = ["Gamma unavailable"]' \
  '.non_object_trade_markets = ["condition-1"]' \
  '.invalid_settlement_markets = ["market-1"]' \
  '.invalid_end_time_markets = ["market-1"]'; do
  jq "$mutation" "$tmp_dir/rust-health.json" >"$tmp_dir/bad-rust-health.json"
  if jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/bad-rust-health.json" >/dev/null; then
    printf 'Rust health policy accepted failure mutation: %s\n' "$mutation" >&2
    exit 1
  fi
done

grep -Fq 'readonly REQUIRED_DURATION_SECONDS=3600' "$GATE"
grep -Fq 'readonly PARITY_TAIL_SECONDS=300' "$GATE"
grep -Fq 'verify-shadow-parity' "$GATE"
[[ ! -e "$SCRIPT_DIR/verify-polymarket-shadow-parity.py" ]]
if grep -Fq 'python3 "$PARITY_VERIFIER"' "$GATE"; then
  printf 'shadow gate still invokes the retired Python parity verifier\n' >&2
  exit 1
fi
grep -Fq 'parity_window_ended_at_unix' "$GATE"
grep -Fq 'common_cutoff' "$GATE"
grep -Fq 'shadow_spool="$shadow_parent/$run_id"' "$GATE"
grep -Fq 'MONDAY_POLYMARKET_SHADOW_SPOOL' \
  "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fq 'crypto_expiry_reference_rust_shadow' "$GATE"
grep -Fq 'crypto_expiry_market_rust_shadow' "$GATE"
grep -Fq '.canonical_uploaded_segments' "$GATE"
grep -Fq 'oss_config_sha256' "$GATE"
grep -Fq 'oss_config_sha256' "$CUTOVER"
grep -Fq 'polymarket-legacy-health-policy.jq' "$GATE"
grep -Fq 'polymarket-legacy-health-policy.jq' "$CUTOVER"
grep -Fq 'polymarket-rust-health-policy.jq' "$GATE"
grep -Fq 'polymarket-rust-health-policy.jq' "$CUTOVER"
grep -Fq 'oss_readback_parity:true' "$GATE"
grep -Fq 'market_oss_readback_parity:true' "$GATE"
grep -Fq '.missing_target_symbols == []' "$RUST_HEALTH_POLICY"
grep -Fq 'systemctl restart "$COLLECTOR_UNIT"' "$CUTOVER"
grep -Fq 'clear_health_before_restart "$evidence_dir" pre-cutover' "$CUTOVER"
grep -Fq 'readlink -f "/proc/$pid/exe"' "$CUTOVER"
grep -Fq 'FragmentPath' "$CUTOVER"
grep -Fq 'DropInPaths' "$CUTOVER"
grep -Fq 'NRestarts' "$CUTOVER"
grep -Fq 'verify_shadow_identity' "$GATE"
grep -Fq 'health_advanced=true' "$CUTOVER"
grep -Fq 'updated_epoch >= started_epoch' "$CUTOVER"
grep -Fq 'gate_legacy_pid' "$CUTOVER"
grep -Fq 'pinned_upload_env' "$GATE"
grep -Fq 'pinned_upload_env' "$CUTOVER"
grep -Fq 'EnvironmentFile=$pinned_upload_env' "$CUTOVER"
grep -Fq 'journalctl --unit "$COLLECTOR_UNIT"' "$CUTOVER"
grep -Fq 'restore_legacy "$evidence_dir"' "$CUTOVER"
grep -Fq 'restore_status=$?' "$CUTOVER"
grep -Fq 'control/polymarket-legacy-health-policy.jq' "$CUTOVER"
if grep -Fq 'if ! restore_legacy' "$CUTOVER"; then
  printf 'automatic rollback still invokes restore_legacy from a negated conditional\n' >&2
  exit 1
fi
grep -Fq 'verify_oneshot_success "$MARKET_UPLOAD_UNIT"' "$CUTOVER"
grep -Fq '/opt/monday/bin/polymarket_reference_collector.py' "$CUTOVER"
grep -Fq 'control-plane bundle changed after the shadow gate' "$CUTOVER"
grep -Fq 'shadow gate evidence is stale or from the future' "$CUTOVER"
grep -Fq 'secure_release_directory "$release_dir"' "$GATE"
grep -Fq 'secure_release_directory "$candidate_release_dir"' "$CUTOVER"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/polymarket-reference-upload.service"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/polymarket-market-tape-upload.service"

release_chmod_line=$(grep -n '^  chmod 0755 "$staging"$' "$GATE" \
  | cut -d: -f1 || true)
release_move_line=$(grep -n '^  mv "$staging" "$release_dir"$' "$GATE" \
  | cut -d: -f1 || true)
[[ $release_chmod_line =~ ^[1-9][0-9]*$ \
  && $release_move_line =~ ^[1-9][0-9]*$ \
  && $release_chmod_line -lt $release_move_line ]] || {
  printf 'shadow gate does not make the release directory traversable before publish\n' >&2
  exit 1
}

cutover_stop_line=$(grep -n '^systemctl stop "$COLLECTOR_UNIT"$' "$CUTOVER" | cut -d: -f1)
cutover_clear_line=$(grep -n '^clear_health_before_restart "$evidence_dir" pre-cutover$' \
  "$CUTOVER" | cut -d: -f1)
cutover_restart_line=$(grep -n '^systemctl restart "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
((cutover_stop_line < cutover_clear_line && cutover_clear_line < cutover_restart_line)) || {
  printf 'cutover no longer follows stop -> clear stale health -> explicit restart\n' >&2
  exit 1
}
rollback_branch_line=$(grep -n '^if \[\[ \$mode == rollback \]\]; then$' "$CUTOVER" \
  | cut -d: -f1)
cutover_policy_line=$(grep -n '^secure_regular_file "$POLICY"$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
((rollback_branch_line < cutover_policy_line)) || {
  printf 'manual rollback still depends on the current cutover bundle preflight\n' >&2
  exit 1
}
grep -Fq 'shadow_restarts=$(systemctl show --property=NRestarts' "$GATE"

market_upload_line=$(grep -n '^market_upload_json=' "$GATE" | cut -d: -f1)
legacy_final_line=$(grep -n 'verify_legacy_identity "$legacy_pid"' "$GATE" \
  | tail -1 | cut -d: -f1)
oss_final_line=$(grep -n 'verify_current_oss_config' "$GATE" | tail -1 | cut -d: -f1)
((legacy_final_line > market_upload_line && oss_final_line > market_upload_line)) || {
  printf 'gate does not revalidate legacy identity and OSS config after both uploads\n' >&2
  exit 1
}
grep -Fq 'cd artifact' "$WORKFLOW"
if grep -Fq 'sha256sum artifact/polymarket-raw-ops' "$WORKFLOW"; then
  printf 'workflow checksum still embeds the stripped artifact directory\n' >&2
  exit 1
fi

printf 'Polymarket raw-ops control-plane tests passed\n'
