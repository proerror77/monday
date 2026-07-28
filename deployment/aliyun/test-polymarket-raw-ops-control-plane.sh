#!/usr/bin/env bash
# Static contract greps intentionally use literal shell expressions.
# Extracted production snippets invoke test doubles and variables indirectly.
# shellcheck disable=SC1090,SC2016,SC2034,SC2317,SC2329
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
readonly CI_WORKFLOW="$SCRIPT_DIR/../../.github/workflows/ci.yml"
readonly README="$SCRIPT_DIR/README.md"
readonly POLYMARKET_COMPILER_DOCKERFILE="$SCRIPT_DIR/../../rust_hft/deployment/docker/Dockerfile.polymarket-evidence-compiler"

if command -v gsha256sum >/dev/null 2>&1; then
  sha256sum() {
    command gsha256sum "$@"
  }
fi

for command in cargo chmod cp grep jq ln mkdir mktemp mv rm sed sha256sum shellcheck \
  sync wc; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing control-plane test dependency: %s\n' "$command" >&2
    exit 2
  }
done

shellcheck "$GATE" "$CUTOVER" "$0"
cargo build --quiet --manifest-path "$RUST_MANIFEST" -p hft-collector \
  --bin polymarket-raw-ops --no-default-features --locked
"$VERIFY" verify-shadow-parity --help >/dev/null

[[ -f $POLYMARKET_COMPILER_DOCKERFILE ]] || {
  printf 'missing Polymarket evidence compiler Dockerfile\n' >&2
  exit 1
}
grep -Fq 'polymarket-evidence-compiler' "$WORKFLOW"
grep -Fq 'Dockerfile.polymarket-evidence-compiler' "$WORKFLOW"
grep -Fq 'org.opencontainers.image.revision=${{ needs.selector.outputs.source_sha }}' "$WORKFLOW"
grep -Fq 'Verify Polymarket compiler source binding' "$WORKFLOW"
grep -Fq '/usr/local/bin/polymarket-raw-ops' "$POLYMARKET_COMPILER_DOCKERFILE"
if grep -Fq '/usr/local/bin/binance-lob-archiver' "$POLYMARKET_COMPILER_DOCKERFILE"; then
  printf 'Polymarket evidence compiler image includes a Binance LOB executable\n' >&2
  exit 1
fi

tmp_dir=$(mktemp -d)
tmp_dir=$(cd -- "$tmp_dir" && pwd -P)
trap 'rm -rf "$tmp_dir"' EXIT

# A short test gate must not continue after the settlement-safe clamp places
# the start at or beyond the common cutoff.
parity_window_verifier="$tmp_dir/valid-parity-window.sh"
sed -n '/^valid_parity_window() {$/,/^}$/p' "$GATE" >"$parity_window_verifier"
sed -n '/^bounded_parity_window_start() {$/,/^}$/p' "$GATE" \
  >>"$parity_window_verifier"
# shellcheck source=/dev/null
source "$parity_window_verifier"
valid_parity_window 100 1000 || {
  printf 'parity-window verifier rejected an ordered window\n' >&2
  exit 1
}
if valid_parity_window 1900 1800 || valid_parity_window 1800 1800; then
  printf 'parity-window verifier accepted a clamped start at/after cutoff\n' >&2
  exit 1
fi
[[ $(bounded_parity_window_start 100 1000 true 900) == 999 ]] || {
  printf 'short test gate did not cap parity start below the cutoff\n' >&2
  exit 1
}
[[ $(bounded_parity_window_start 100 1900 false 900) == 1000 ]] || {
  printf 'production gate did not preserve its settlement-safe start\n' >&2
  exit 1
}

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

# Exercise the installed immutable release manifest parser. The manifest is the
# sole binding for source, candidate, control-manifest, and control-archive IDs.
(
release_manifest_verifier="$tmp_dir/verify-release-manifest.sh"
sed -n \
  -e '/^release_control_assets() {$/,/^}$/p' \
  -e '/^verify_release_manifest() {$/,/^}$/p' \
  -e '/^verify_release_binding() {$/,/^}$/p' \
  -e '/^verify_control_release() {$/,/^}$/p' "$GATE" \
  >"$release_manifest_verifier"
readonly RELEASE_MANIFEST_SCHEMA=monday.polymarket_raw_ops_release.v1
readonly RELEASE_MANIFEST="$tmp_dir/polymarket-raw-ops-release.json"
readonly -a BUNDLE_ASSETS=(
  polymarket-raw-ops-shadow-gate.sh
  candidate-only-control
)
secure_control_file() {
  [[ -f $1 && ! -L $1 ]]
}
# shellcheck source=/dev/null
source "$release_manifest_verifier"
release_manifest_dir="$tmp_dir/release-manifest"
mkdir "$release_manifest_dir"
release_manifest="$release_manifest_dir/polymarket-raw-ops-release.json"
candidate_file="$release_manifest_dir/polymarket-raw-ops"
printf 'verified candidate\n' >"$candidate_file"
cat >"$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh" <<'EOF'
readonly -a BUNDLE_ASSETS=(
  polymarket-raw-ops-shadow-gate.sh
  baseline-only-control
)
EOF
: >"$release_manifest_dir/baseline-only-control"
candidate_sha=$(sha256sum "$candidate_file" | awk '{print $1}')
source_revision=$(printf 'b%.0s' {1..40})
bundle_fixture_sha=$(
  cd "$release_manifest_dir"
  sha256sum polymarket-raw-ops-shadow-gate.sh baseline-only-control \
    | sha256sum | awk '{print $1}'
)
installed_bundle_sha=$bundle_fixture_sha
control_archive_sha=$(printf 'd%.0s' {1..64})
jq -S -n \
  --arg candidate "$candidate_sha" \
  --arg source "$source_revision" \
  --arg control_manifest "$bundle_fixture_sha" \
  --arg control_archive "$control_archive_sha" \
  '{schema:"monday.polymarket_raw_ops_release.v1",source_revision:$source,
    candidate:{file:"polymarket-raw-ops",sha256:$candidate},
    control_manifest:{file:"polymarket-raw-ops-control-assets.sha256",
      sha256:$control_manifest},
    control_archive:{file:"polymarket-raw-ops-control.tar.gz",
      sha256:$control_archive}}' >"$release_manifest"
release_manifest_sha=$(sha256sum "$release_manifest" | awk '{print $1}')
bundle_sha256() {
  printf '%s\n' "$bundle_fixture_sha"
}
verify_release_manifest "$release_manifest" || {
  printf 'release manifest verifier rejected a valid identity binding\n' >&2
  exit 1
}
verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file" || {
  printf 'release binding rejected a valid immutable release\n' >&2
  exit 1
}
verify_control_release "$release_manifest_dir" "$candidate_sha" "$candidate_file" || {
  printf 'global controls rejected the active Rust baseline release\n' >&2
  exit 1
}
if verify_control_release "$release_manifest_dir" "$(printf '0%.0s' {1..64})" \
  "$candidate_file"; then
  printf 'global controls accepted a different Rust baseline release\n' >&2
  exit 1
fi
rm "$release_manifest_dir/baseline-only-control"
if verify_control_release "$release_manifest_dir" "$candidate_sha" "$candidate_file"; then
  printf 'global controls accepted a missing bundled control asset\n' >&2
  exit 1
fi
: >"$release_manifest_dir/baseline-only-control"
cp "$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh" \
  "$release_manifest_dir/valid-shadow-gate.sh"
printf '%s\n' 'readonly -a BUNDLE_ASSETS=(' \
  '  polymarket-raw-ops-shadow-gate.sh' '  -option-like-control' ')' \
  >"$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh"
if release_control_assets "$release_manifest_dir" >/dev/null; then
  printf 'release control parser accepted an option-like asset name\n' >&2
  exit 1
fi
mv "$release_manifest_dir/valid-shadow-gate.sh" \
  "$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh"
if verify_release_binding "$release_manifest" "$(printf '0%.0s' {1..64})" \
  "$candidate_sha" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file"; then
  printf 'release binding accepted a different manifest identity\n' >&2
  exit 1
fi
if verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$(printf '0%.0s' {1..64})" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file"; then
  printf 'release binding accepted a different candidate identity\n' >&2
  exit 1
fi
printf 'tampered candidate\n' >>"$candidate_file"
if verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file" >/dev/null 2>&1; then
  printf 'release binding accepted modified candidate bytes\n' >&2
  exit 1
fi
printf 'verified candidate\n' >"$candidate_file"
bundle_fixture_sha=$(printf 'e%.0s' {1..64})
if verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$(printf 'c%.0s' {1..64})" \
  "$control_archive_sha" "$candidate_file"; then
  printf 'release binding accepted a modified installed control bundle\n' >&2
  exit 1
fi
bundle_fixture_sha=$installed_bundle_sha
jq '.extra = true' "$release_manifest" >"$release_manifest_dir/extra.json"
if verify_release_manifest "$release_manifest_dir/extra.json"; then
  printf 'release manifest verifier accepted an extra identity field\n' >&2
  exit 1
fi
jq '.candidate.file = "other-binary"' "$release_manifest" \
  >"$release_manifest_dir/wrong-file.json"
if verify_release_manifest "$release_manifest_dir/wrong-file.json"; then
  printf 'release manifest verifier accepted a different candidate filename\n' >&2
  exit 1
fi
printf '{}\n%s\n' "$(<"$release_manifest")" \
  >"$release_manifest_dir/multiple.json"
if verify_release_manifest "$release_manifest_dir/multiple.json" 2>/dev/null; then
  printf 'release manifest verifier accepted multiple JSON values\n' >&2
  exit 1
fi
)

# Exercise the Rust-only wrapper around the established systemd identity check.
baseline_identity_contract="$tmp_dir/verify-baseline-identity.sh"
sed -n '/^verify_baseline_identity() {$/,/^}$/p' "$GATE" >"$baseline_identity_contract"
exercise_rust_baseline_identity() (
  set -euo pipefail
  RUST_ACTIVE_BINARY="$tmp_dir/active" baseline_mode=rust_release legacy_pid=42
  RUST_PRODUCTION_EXEC='/opt/monday/bin/polymarket-raw-ops collect-reference'
  legacy_restarts=1 legacy_invocation_id=$(printf '1%.0s' {1..32})
  baseline_release_sha=$(printf '9%.0s' {1..64})
  baseline_release_path="$tmp_dir/$baseline_release_sha/polymarket-raw-ops"
  mkdir -p "${baseline_release_path%/*}"
  printf 'test\n' >"$baseline_release_path"; chmod +x "$baseline_release_path"
  mock_active=$baseline_release_path mock_proc=$baseline_release_path
  mock_digest=true mock_runtime=true
  verify_runtime_identity() { [[ $mock_runtime == true ]]; }
  verify_legacy_identity() { return 1; }
  secure_release_directory() { return 0; }
  secure_control_file() { return 0; }
  readlink() {
    [[ $3 == "$RUST_ACTIVE_BINARY" ]] && printf '%s\n' "$mock_active" \
      || printf '%s\n' "$mock_proc"
  }
  sha256sum() { cat >/dev/null; [[ $mock_digest == true ]]; }
  # shellcheck source=/dev/null
  source "$baseline_identity_contract"
  verify_baseline_identity
  for drift in active proc digest runtime; do
    mock_active=$baseline_release_path mock_proc=$baseline_release_path
    mock_digest=true mock_runtime=true
    case "$drift" in
      active) mock_active=/tmp/wrong ;;
      proc) mock_proc=/tmp/wrong ;;
      digest) mock_digest=false ;;
      runtime) mock_runtime=false ;;
    esac
    verify_baseline_identity && { printf 'accepted %s baseline drift\n' "$drift" >&2; exit 1; }
  done
  return 0
)
exercise_rust_baseline_identity

# The long shadow observation must not start when the production cutover target
# already contains state that the promotion path will reject. Keep the same
# read-only contract in both scripts so the Gate and final cutover cannot drift.
gate_cutover_target_contract=$(sed -n \
  '/^verify_cutover_target_preflight() {$/,/^}$/p' "$GATE")
cutover_target_contract=$(sed -n \
  '/^verify_cutover_target_preflight() {$/,/^}$/p' "$CUTOVER")
[[ -n $gate_cutover_target_contract \
  && $gate_cutover_target_contract == "$cutover_target_contract" ]] || {
  printf 'shadow and cutover target preflight contracts are missing or differ\n' >&2
  exit 1
}
cutover_target_contract_file="$tmp_dir/cutover-target-preflight.sh"
sed -n '/^release_control_assets() {$/,/^}$/p' "$GATE" \
  >"$cutover_target_contract_file"
printf '%s\n' "$gate_cutover_target_contract" >>"$cutover_target_contract_file"
gate_cutover_target_line=$(grep -n '^verify_cutover_target_preflight ' "$GATE" \
  | cut -d: -f1)
gate_shadow_start_line=$(grep -n '^systemctl start "$shadow_unit"$' "$GATE" \
  | cut -d: -f1)
gate_started_at_line=$(grep -n '^started_at_unix=' "$GATE" | cut -d: -f1)
gate_start_uptime_line=$(grep -n '^start_uptime=' "$GATE" | cut -d: -f1)
gate_rust_control_line=$(grep -n \
  '^  || verify_control_release "$CONTROL_DIR" "$baseline_release_sha"' "$GATE" \
  | cut -d: -f1)
cutover_target_line=$(grep -n '^verify_cutover_target_preflight ' "$CUTOVER" \
  | cut -d: -f1)
cutover_transition_line=$(grep -n '^transition_started=true$' "$CUTOVER" \
  | cut -d: -f1)
[[ $gate_cutover_target_line =~ ^[1-9][0-9]*$ \
  && $gate_shadow_start_line =~ ^[1-9][0-9]*$ \
  && $gate_started_at_line =~ ^[1-9][0-9]*$ \
  && $gate_start_uptime_line =~ ^[1-9][0-9]*$ \
  && $gate_rust_control_line =~ ^[1-9][0-9]*$ \
  && $cutover_target_line =~ ^[1-9][0-9]*$ \
  && $cutover_transition_line =~ ^[1-9][0-9]*$ \
  && $gate_cutover_target_line -lt $gate_started_at_line \
  && $gate_cutover_target_line -lt $gate_start_uptime_line \
  && $gate_cutover_target_line -lt $gate_shadow_start_line \
  && $gate_rust_control_line -lt $gate_started_at_line \
  && $gate_rust_control_line -lt $gate_start_uptime_line \
  && $gate_rust_control_line -lt $gate_shadow_start_line \
  && $cutover_target_line -lt $cutover_transition_line ]] || {
  printf 'cutover target preflight runs after observation or transition begins\n' >&2
  exit 1
}
(
  baseline_mode=legacy_python
  active_binary="$tmp_dir/active-polymarket-raw-ops"
  control_dir="$tmp_dir/global-control"
  release_manifest_name=polymarket-raw-ops-release.json
  readonly -a BUNDLE_ASSETS=(
    polymarket-raw-ops-shadow-gate.sh
    polymarket-raw-ops-cutover.sh
    polymarket-shadow-gate-policy.jq
  )
  drop_in_unit=
  fragment_drift_unit=
  insecure_unit_file=
  unsafe_control_parent=false
  systemctl() {
    [[ $1 == show && $3 == --value && $# -eq 4 ]] || return 64
    case "$2" in
      --property=DropInPaths)
        [[ $4 != "$drop_in_unit" ]] \
          || printf '%s\n' /run/systemd/system.control/stale.conf
        ;;
      --property=FragmentPath)
        if [[ $4 == "$fragment_drift_unit" ]]; then
          printf '/run/systemd/transient/%s\n' "$4"
        else
          printf '/etc/systemd/system/%s\n' "$4"
        fi
        ;;
      *) return 64 ;;
    esac
  }
  direct_directory() { [[ -d $1 && ! -L $1 ]]; }
  secure_root_chain() { [[ -d $1 && ! -L $1 ]]; }
  secure_root_chain_or_absent() { [[ $unsafe_control_parent == false ]]; }
  secure_test_file() {
    if [[ $1 == /etc/systemd/system/* ]]; then
      [[ ${1##*/} != "$insecure_unit_file" ]]
    else
      [[ -f $1 && ! -L $1 ]]
    fi
  }
  # shellcheck source=/dev/null
  source "$cutover_target_contract_file"

  verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file || {
    printf 'cutover target preflight rejected clean legacy state\n' >&2
    exit 1
  }

  unsafe_control_parent=true
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an unsafe absent control parent\n' >&2
    exit 1
  fi
  unsafe_control_parent=false

  mkdir "$control_dir"
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an empty global control directory\n' >&2
    exit 1
  fi
  : >"$control_dir/polymarket-raw-ops-control-assets.sha256"
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an incomplete global control directory\n' >&2
    exit 1
  fi

  printf '%s\n' 'readonly -a BUNDLE_ASSETS=(' \
    '  polymarket-raw-ops-shadow-gate.sh' '  baseline-only-control' ')' \
    >"$control_dir/polymarket-raw-ops-shadow-gate.sh"
  : >"$control_dir/baseline-only-control"
  : >"$control_dir/$release_manifest_name"
  verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file || {
    printf 'cutover target preflight rejected a complete global control directory\n' >&2
    exit 1
  }

  fragment_drift_unit=polymarket-reference-upload.service
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted a noncanonical unit fragment\n' >&2
    exit 1
  fi
  fragment_drift_unit=

  insecure_unit_file=polymarket-reference-upload.timer
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an insecure unit file\n' >&2
    exit 1
  fi
  insecure_unit_file=

  for drop_in_unit in polymarket-reference-collector.service \
    polymarket-reference-upload.service polymarket-reference-upload.timer \
    polymarket-market-tape-upload.service polymarket-market-tape-upload.timer; do
    if verify_cutover_target_preflight \
      "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
      secure_test_file; then
      printf 'cutover target preflight accepted a drop-in for %s\n' \
        "$drop_in_unit" >&2
      exit 1
    fi
  done
  drop_in_unit=

  : >"$active_binary"
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted a stale Rust target in legacy mode\n' >&2
    exit 1
  fi
  baseline_mode=rust_release
  verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file || {
    printf 'cutover target preflight rejected a governed Rust baseline target\n' >&2
    exit 1
  }
)

# Rollback must preserve the baseline release's own control membership rather
# than substituting the candidate script's BUNDLE_ASSETS.
rollback_control_contract="$tmp_dir/rollback-control-files.sh"
sed -n '/^rollback_control_files() {$/,/^}$/p' "$CUTOVER" \
  >"$rollback_control_contract"
sed -n '/^remove_snapshotted_control_files() {$/,/^}$/p' "$CUTOVER" \
  >>"$rollback_control_contract"
(
  RELEASE_MANIFEST=/tmp/polymarket-raw-ops-release.json
  # shellcheck source=/dev/null
  source "$rollback_control_contract"
  rollback_state="$tmp_dir/rollback-control-state.json"
  jq -n '{control_files:["polymarket-raw-ops-shadow-gate.sh",
      "baseline-only-control","polymarket-raw-ops-release.json"]}' \
    >"$rollback_state"
  [[ $(rollback_control_files "$rollback_state") == \
      $'polymarket-raw-ops-shadow-gate.sh\nbaseline-only-control\npolymarket-raw-ops-release.json' ]] \
    || {
      printf 'rollback control list rejected release-specific baseline assets\n' >&2
      exit 1
    }
  jq '.control_files[1] = "-option-like-control"' "$rollback_state" \
    >"$rollback_state.invalid"
  if rollback_control_files "$rollback_state.invalid" >/dev/null; then
    printf 'rollback control list accepted an option-like asset\n' >&2
    exit 1
  fi
  CONTROL_DIR=$tmp_dir/remove-control-files
  mkdir "$CONTROL_DIR"
  rm_calls=0
  rm() {
    rm_calls=$((rm_calls + 1))
    [[ $rm_calls -ne 1 ]]
  }
  jq '.control_dir_present = true' "$rollback_state" >"$rollback_state.present"
  if remove_snapshotted_control_files "$rollback_state.present"; then
    printf 'control cleanup hid an earlier removal failure\n' >&2
    exit 1
  fi
  [[ $rm_calls -eq 1 ]] || {
    printf 'control cleanup continued after a removal failure\n' >&2
    exit 1
  }
  rm_calls=0
  if remove_snapshotted_control_files "$rollback_state"; then
    printf 'control cleanup accepted a missing directory-state field\n' >&2
    exit 1
  fi
  [[ $rm_calls -eq 0 ]] || {
    printf 'control cleanup removed files with invalid directory state\n' >&2
    exit 1
  }
  jq '.control_dir_present = false' "$rollback_state" >"$rollback_state.absent"
  remove_snapshotted_control_files "$rollback_state.absent" || {
    printf 'control cleanup rejected an explicitly absent control directory\n' >&2
    exit 1
  }
  [[ $rm_calls -eq 0 ]] || {
    printf 'control cleanup removed files for an absent control directory\n' >&2
    exit 1
  }
  jq '.control_dir_present = "false"' "$rollback_state" >"$rollback_state.invalid-state"
  rm_calls=0
  if remove_snapshotted_control_files "$rollback_state.invalid-state"; then
    printf 'control cleanup accepted a non-boolean directory-state field\n' >&2
    exit 1
  fi
  [[ $rm_calls -eq 0 ]] || {
    printf 'control cleanup removed files with a non-boolean directory state\n' >&2
    exit 1
  }
)
grep -Fq 'control_assets=$(release_control_assets "$CONTROL_DIR")' "$CUTOVER"
grep -Fq 'remove_snapshotted_control_files "$rollback_dir/state.json"' "$CUTOVER"
[[ $(grep -Fc 'control_files=$(rollback_control_files "$rollback_dir/state.json")' \
  "$CUTOVER") -eq 1 ]]

# Both controls must carry the same root-chain contract. Run it with deterministic
# stat/direct-directory doubles so these macOS-hosted tests cover Linux ownership
# semantics without requiring local root-owned fixtures.
root_chain_contract="$tmp_dir/root-chain-contract.sh"
gate_root_chain_contract=$(sed -n \
  -e '/^valid_absolute_path() {$/,/^}$/p' \
  -e '/^secure_root_directory() {$/,/^}$/p' \
  -e '/^secure_root_chain() {$/,/^}$/p' \
  -e '/^secure_root_chain_or_absent() {$/,/^}$/p' \
  -e '/^secure_collector_directory() {$/,/^}$/p' "$GATE")
cutover_root_chain_contract=$(sed -n \
  -e '/^valid_absolute_path() {$/,/^}$/p' \
  -e '/^secure_root_directory() {$/,/^}$/p' \
  -e '/^secure_root_chain() {$/,/^}$/p' \
  -e '/^secure_root_chain_or_absent() {$/,/^}$/p' \
  -e '/^secure_collector_directory() {$/,/^}$/p' "$CUTOVER")
[[ -n $gate_root_chain_contract \
  && $gate_root_chain_contract == "$cutover_root_chain_contract" ]] || {
  printf 'shadow and cutover trusted-directory contracts differ\n' >&2
  exit 1
}
printf '%s\n' "$gate_root_chain_contract" >"$root_chain_contract"
(
  mock_root_uid=0
  mock_root_mode=755
  mock_leaf_mode=750
  mock_leaf_owner=hftcollector
  mock_leaf_group=hftcollector
  direct_directory() {
    return 0
  }
  stat() {
    [[ $1 == -c && $3 == -- && $# -eq 4 ]] || return 64
    case "$2" in
      %u) printf '%s\n' "$mock_root_uid" ;;
      %a)
        if [[ $4 == /trusted/spool ]]; then
          printf '%s\n' "$mock_leaf_mode"
        else
          printf '%s\n' "$mock_root_mode"
        fi
        ;;
      %U) printf '%s\n' "$mock_leaf_owner" ;;
      %G) printf '%s\n' "$mock_leaf_group" ;;
      *) return 65 ;;
    esac
  }
  # shellcheck source=/dev/null
  source "$root_chain_contract"
  secure_root_chain /trusted/leaf || exit 1
  secure_collector_directory /trusted/spool || exit 1
  mock_root_mode=775
  if secure_root_chain /trusted/leaf; then
    printf 'root-chain contract accepted a writable ancestor\n' >&2
    exit 1
  fi
  mock_root_mode=755
  mock_root_uid=1000
  if secure_root_chain /trusted/leaf; then
    printf 'root-chain contract accepted a non-root ancestor\n' >&2
    exit 1
  fi
  mock_root_uid=0
  mock_leaf_mode=770
  if secure_collector_directory /trusted/spool; then
    printf 'collector-directory contract accepted writable leaf permissions\n' >&2
    exit 1
  fi
  if secure_root_chain /trusted/../leaf; then
    printf 'root-chain contract accepted a parent traversal\n' >&2
    exit 1
  fi
  if secure_root_chain_or_absent /trusted/missing/../leaf; then
    printf 'absent root-chain contract accepted a parent traversal\n' >&2
    exit 1
  fi
  dangling_parent="$tmp_dir/root-chain-dangling"
  mkdir "$dangling_parent"
  ln -s missing "$dangling_parent/link"
  if secure_root_chain_or_absent "$dangling_parent/link/child"; then
    printf 'root-chain contract accepted a dangling intermediate symlink\n' >&2
    exit 1
  fi
)

# Exercise the journal cursor/restart detector with deterministic journal JSON.
# Both release scripts must carry the exact same implementation so one behavior
# test covers the code that guards both the shadow stop and production cutover.
cutover_journal_contract=$(sed -n \
  -e '/^journal_cursor() {$/,/^}$/p' \
  -e '/^verify_no_restart_after_cursor() {$/,/^}$/p' "$CUTOVER")
gate_journal_contract=$(sed -n \
  -e '/^journal_cursor() {$/,/^}$/p' \
  -e '/^verify_no_restart_after_cursor() {$/,/^}$/p' "$GATE")
[[ -n $cutover_journal_contract \
  && $cutover_journal_contract == "$gate_journal_contract" ]] || {
  printf 'shadow and cutover journal restart guards are missing or differ\n' >&2
  exit 1
}
journal_contract="$tmp_dir/journal-contract.sh"
printf '%s\n' "$cutover_journal_contract" >"$journal_contract"
# shellcheck source=/dev/null
source "$journal_contract"

journal_test_unit=polymarket-contract-test.service
journal_cursor_fixture='s=contract-cursor'
journal_fixture=
journal_failure_mode=none
journalctl() {
  local arguments="$*"
  if [[ $arguments == '--sync' ]]; then
    [[ $journal_failure_mode != sync ]] || return 70
    return 0
  fi
  if [[ $arguments == \
    "--unit $journal_test_unit --lines=0 --show-cursor --no-pager" ]]; then
    [[ $journal_failure_mode != cursor ]] || return 71
    printf '%s\n' "-- cursor: $journal_cursor_fixture"
    return 0
  fi
  if [[ $arguments == \
    "--unit $journal_test_unit --after-cursor $journal_cursor_fixture --output=json --no-pager" ]]; then
    [[ $journal_failure_mode != after-cursor ]] || return 72
    [[ -z $journal_fixture ]] || printf '%s\n' "$journal_fixture"
    return 0
  fi
  return 64
}

captured_cursor=$(journal_cursor "$journal_test_unit")
[[ $captured_cursor == "$journal_cursor_fixture" ]] || {
  printf 'journal cursor helper did not return the exact synchronized cursor\n' >&2
  exit 1
}
journal_cursor_fixture=
if journal_cursor "$journal_test_unit" >/dev/null; then
  printf 'journal cursor helper accepted an empty cursor\n' >&2
  exit 1
fi
journal_cursor_fixture='s=contract-cursor'
journal_failure_mode=sync
if journal_cursor "$journal_test_unit" >/dev/null; then
  printf 'journal cursor helper ignored journal synchronization failure\n' >&2
  exit 1
fi
journal_failure_mode=cursor
if journal_cursor "$journal_test_unit" >/dev/null; then
  printf 'journal cursor helper ignored cursor query failure\n' >&2
  exit 1
fi
journal_failure_mode=none

expected_invocation_id=$(printf '1%.0s' {1..32})
other_invocation_id=$(printf '2%.0s' {1..32})
journal_fixture=$(jq -cn '{MESSAGE_ID:"unrelated"}')
verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id" || {
    printf 'journal restart guard rejected an unrelated event\n' >&2
    exit 1
  }
journal_fixture=$(jq -cn --arg invocation "$expected_invocation_id" \
  '{MESSAGE_ID:"39f53479d3a045ac8e11786248231fbf",INVOCATION_ID:$invocation,
    _SYSTEMD_INVOCATION_ID:$invocation}')
verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id" || {
    printf 'journal restart guard rejected the expected invocation\n' >&2
    exit 1
  }
journal_fixture=$(jq -cn --arg invocation "$expected_invocation_id" \
  '{MESSAGE_ID:"5eb03494b6584870a536b337290809b3",INVOCATION_ID:$invocation}')
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard accepted an automatic restart event\n' >&2
  exit 1
fi
journal_fixture=$(jq -cn --arg invocation "$other_invocation_id" \
  '{MESSAGE_ID:"be02cf6855d2428ba40df7e9d022f03d",INVOCATION_ID:$invocation}')
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard accepted UNIT_FAILED from a different invocation ID\n' >&2
  exit 1
fi
journal_fixture=$(jq -cn --arg invocation "$other_invocation_id" \
  '{MESSAGE_ID:"unrelated",_SYSTEMD_INVOCATION_ID:$invocation}')
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard accepted a different systemd invocation ID\n' >&2
  exit 1
fi
journal_fixture=
journal_failure_mode=sync
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard ignored journal synchronization failure\n' >&2
  exit 1
fi
journal_failure_mode=after-cursor
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard ignored journal query failure\n' >&2
  exit 1
fi
journal_failure_mode=none
journal_fixture='{not-json}'
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id" \
  2>/dev/null; then
  printf 'journal restart guard accepted malformed journal JSON\n' >&2
  exit 1
fi

# Execute the production success-marker statements against temporary evidence.
# The marker must be a unique, exact checksum of cutover.json and a rerun must
# fail closed rather than replacing existing evidence.
cutover_marker_publisher="$tmp_dir/publish-cutover-marker.sh"
sed -n '/^success_marker="\$evidence_dir\/PASSED.sha256"$/,/^sync -f "\$evidence_dir"$/p' \
  "$CUTOVER" >"$cutover_marker_publisher"
[[ -s $cutover_marker_publisher ]] || {
  printf 'cutover success-marker publisher is missing\n' >&2
  exit 1
}
publish_cutover_marker() (
  evidence_dir=$1
  secure_root_chain() {
    [[ -d $1 && ! -L $1 ]]
  }
  die() {
    printf 'marker publication failed: %s\n' "$*" >&2
    exit 1
  }
  mv() {
    if [[ ${1:-} == -Tf && $# -eq 3 ]]; then
      command mv -f "$2" "$3"
    else
      command mv "$@"
    fi
  }
  # shellcheck source=/dev/null
  source "$cutover_marker_publisher"
)

cutover_success_dir="$tmp_dir/cutover-success"
mkdir "$cutover_success_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$cutover_success_dir/cutover.json"
publish_cutover_marker "$cutover_success_dir"
[[ $(wc -l <"$cutover_success_dir/PASSED.sha256") -eq 1 ]] || {
  printf 'cutover success marker is not exactly one line\n' >&2
  exit 1
}
expected_cutover_marker=$(cd "$cutover_success_dir" && sha256sum cutover.json)
[[ $(<"$cutover_success_dir/PASSED.sha256") == "$expected_cutover_marker" ]] || {
  printf 'cutover success marker does not bind the exact cutover.json\n' >&2
  exit 1
}
(
  cd "$cutover_success_dir"
  sha256sum --check --strict PASSED.sha256 >/dev/null
)
if publish_cutover_marker "$cutover_success_dir" 2>/dev/null; then
  printf 'cutover marker publisher overwrote existing success evidence\n' >&2
  exit 1
fi

# Exercise the production rollback-evidence helpers and EXIT trap. Rollback
# must durably revoke PASSED before restore can mutate runtime state, then give
# the preserved evidence its final invalid/rolled-back name.
rollback_evidence_helpers="$tmp_dir/rollback-evidence-helpers.sh"
sed -n \
  -e '/^verify_named_marker() {$/,/^}$/p' \
  -e '/^prepare_rollback_evidence() {$/,/^}$/p' \
  -e '/^finalize_rollback_evidence() {$/,/^}$/p' \
  "$CUTOVER" >"$rollback_evidence_helpers"
[[ -s $rollback_evidence_helpers ]] || {
  printf 'rollback evidence helpers are missing\n' >&2
  exit 1
}
# shellcheck source=/dev/null
source "$rollback_evidence_helpers"

# Exercise the public rollback branch through its filesystem/system boundaries.
# Admission may recover markerless or pending evidence at the saved baseline,
# but must reject any unrelated active release before mutation begins.
manual_lineage_contract="$tmp_dir/manual-lineage-contract.sh"
sed -n '/^if \[\[ \$mode == rollback \]\]; then$/,/^# Cutover depends/p' "$CUTOVER" \
  | sed '$d' >"$manual_lineage_contract"
[[ -s $manual_lineage_contract ]] || {
  printf 'manual rollback lineage contract is missing\n' >&2
  exit 1
}
lineage_root="$tmp_dir/manual-lineage"
lineage_release_root="$lineage_root/releases"
lineage_evidence_root="$lineage_root/evidence"
lineage_active="$lineage_root/bin/polymarket-raw-ops"
mkdir -p "$lineage_release_root" "$lineage_evidence_root" "${lineage_active%/*}"
make_lineage_release() {
  local label=$1 staging sha path
  staging=$(mktemp "$lineage_root/release.XXXXXX")
  printf '%s\n' "$label" >"$staging"
  sha=$(sha256sum "$staging" | awk '{print $1}')
  path="$lineage_release_root/$sha/polymarket-raw-ops"
  mkdir -p "${path%/*}"
  mv "$staging" "$path"
  chmod +x "$path"
  printf '%s\n' "$path"
}
lineage_candidate=$(make_lineage_release candidate)
lineage_saved=$(make_lineage_release saved-baseline)
lineage_third=$(make_lineage_release unrelated-third-release)
lineage_candidate_sha=${lineage_candidate%/*}; lineage_candidate_sha=${lineage_candidate_sha##*/}
lineage_saved_sha=${lineage_saved%/*}; lineage_saved_sha=${lineage_saved_sha##*/}
make_lineage_evidence() {
  local name=$1 marker_state=$2 evidence manifest_sha
  evidence="$lineage_evidence_root/$name"
  mkdir -p "$evidence/rollback"
  jq -n --arg candidate "$lineage_candidate_sha" --arg saved "$lineage_saved" \
    --arg saved_sha "$lineage_saved_sha" \
    '{baseline_mode:"rust_release",candidate_sha256:$candidate,
      active_symlink:{target:$saved,sha256:$saved_sha}}' >"$evidence/rollback/state.json"
  (cd "$evidence/rollback" && sha256sum state.json >manifest.sha256)
  if [[ $marker_state == pending ]]; then
    manifest_sha=$(sha256sum "$evidence/rollback/manifest.sha256" | awk '{print $1}')
    jq -n --arg candidate "$lineage_candidate_sha" --arg manifest "$manifest_sha" \
      '{candidate_sha256:$candidate,rollback_manifest_sha256:$manifest}' \
      >"$evidence/cutover.json"
    (cd "$evidence" && sha256sum cutover.json >PASSED.rollback-pending.sha256)
  fi
  printf '%s\n' "$evidence"
}
exercise_manual_lineage() (
  set -euo pipefail
  local evidence=$1 active=$2 mutation_log=$1/mutations
  EVIDENCE_ROOT=$lineage_evidence_root RELEASE_ROOT=$lineage_release_root
  ACTIVE_BINARY=$lineage_active mode=rollback
  set -- rollback "$evidence"
  : >"$mutation_log"
  rm -f "$ACTIVE_BINARY"
  ln -s "$active" "$ACTIVE_BINARY"
  secure_root_chain() { [[ -d $1 && ! -L $1 ]]; }
  secure_release_directory() { [[ -d $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  prepare_rollback_evidence() { printf 'prepare\n' >>"$mutation_log"; }
  restore_legacy() { printf 'restore\n' >>"$mutation_log"; }
  finalize_rollback_evidence() { printf 'finalize\n' >>"$mutation_log"; }
  die() { printf 'manual lineage rejected: %s\n' "$*" >&2; exit 1; }
  # shellcheck source=/dev/null
  source "$manual_lineage_contract"
)
markerless_lineage=$(make_lineage_evidence markerless none)
exercise_manual_lineage "$markerless_lineage" "$lineage_saved" >/dev/null
[[ $(<"$markerless_lineage/mutations") == $'prepare\nrestore\nfinalize' ]] || {
  printf 'markerless rollback snapshot was not admitted\n' >&2
  exit 1
}
pending_lineage=$(make_lineage_evidence pending-saved pending)
exercise_manual_lineage "$pending_lineage" "$lineage_saved" >/dev/null
[[ $(<"$pending_lineage/mutations") == $'prepare\nrestore\nfinalize' ]] || {
  printf 'pending rollback rejected the exact saved baseline\n' >&2
  exit 1
}
third_lineage=$(make_lineage_evidence pending-third pending)
set +e
exercise_manual_lineage "$third_lineage" "$lineage_third" >/dev/null 2>&1
third_lineage_status=$?
set -e
[[ $third_lineage_status -ne 0 && ! -s $third_lineage/mutations ]] || {
  printf 'manual rollback admitted an unrelated active release\n' >&2
  exit 1
}

cutover_failure_cleanup="$tmp_dir/cutover-failure-cleanup.sh"
sed -n '/^on_exit() {$/,/^}$/p' "$CUTOVER" >"$cutover_failure_cleanup"
[[ -s $cutover_failure_cleanup ]] || {
  printf 'cutover automatic failure cleanup is missing\n' >&2
  exit 1
}
exercise_failed_cutover_cleanup() (
  set +e
  evidence_dir=$1
  restore_result=$2
  cutover_succeeded=false
  transition_started=true
  restore_legacy() {
    [[ ! -e $evidence_dir/PASSED.sha256 \
      && -f $evidence_dir/PASSED.rollback-pending.sha256 ]] || return 90
    printf '%s\n' pending >"$evidence_dir/restore-observed-pending"
    return "$restore_result"
  }
  secure_regular_file() {
    [[ -f $1 && ! -L $1 ]]
  }
  secure_root_chain() {
    [[ -d $1 && ! -L $1 ]]
  }
  die() {
    printf 'rollback evidence transition failed: %s\n' "$*" >&2
    exit 1
  }
  mv() {
    if [[ ${1:-} == -Tf && $# -eq 3 ]]; then
      command mv -f "$2" "$3"
    else
      command mv "$@"
    fi
  }
  # shellcheck source=/dev/null
  source "$cutover_failure_cleanup"
  false
  on_exit
)

automatic_failure_dir="$tmp_dir/cutover-automatic-failure"
mkdir "$automatic_failure_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$automatic_failure_dir/cutover.json"
publish_cutover_marker "$automatic_failure_dir"
set +e
exercise_failed_cutover_cleanup "$automatic_failure_dir" 0 >/dev/null 2>&1
automatic_failure_status=$?
set -e
[[ $automatic_failure_status -ne 0 \
  && ! -e $automatic_failure_dir/PASSED.sha256 \
  && ! -e $automatic_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $automatic_failure_dir/cutover.json \
  && -f $automatic_failure_dir/PASSED.invalid.sha256 \
  && -f $automatic_failure_dir/restore-observed-pending ]] || {
  printf 'automatic rollback left success-looking cutover evidence\n' >&2
  exit 1
}
(
  cd "$automatic_failure_dir"
  sha256sum --check --strict PASSED.invalid.sha256 >/dev/null
)

automatic_restore_failure_dir="$tmp_dir/cutover-automatic-restore-failure"
mkdir "$automatic_restore_failure_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$automatic_restore_failure_dir/cutover.json"
publish_cutover_marker "$automatic_restore_failure_dir"
set +e
exercise_failed_cutover_cleanup "$automatic_restore_failure_dir" 1 >/dev/null 2>&1
automatic_restore_failure_status=$?
set -e
[[ $automatic_restore_failure_status -ne 0 \
  && ! -e $automatic_restore_failure_dir/PASSED.sha256 \
  && -f $automatic_restore_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $automatic_restore_failure_dir/cutover.json \
  && ! -e $automatic_restore_failure_dir/PASSED.invalid.sha256 \
  && -f $automatic_restore_failure_dir/restore-observed-pending ]] || {
  printf 'failed automatic restore left ambiguous cutover evidence\n' >&2
  exit 1
}
(
  cd "$automatic_restore_failure_dir"
  sha256sum --check --strict PASSED.rollback-pending.sha256 >/dev/null
)

automatic_finalize_conflict_dir="$tmp_dir/cutover-automatic-finalize-conflict"
mkdir "$automatic_finalize_conflict_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$automatic_finalize_conflict_dir/cutover.json"
publish_cutover_marker "$automatic_finalize_conflict_dir"
printf 'preserve existing forensic marker\n' \
  >"$automatic_finalize_conflict_dir/PASSED.invalid.sha256"
set +e
exercise_failed_cutover_cleanup "$automatic_finalize_conflict_dir" 0 >/dev/null 2>&1
automatic_finalize_conflict_status=$?
set -e
[[ $automatic_finalize_conflict_status -ne 0 \
  && ! -e $automatic_finalize_conflict_dir/PASSED.sha256 \
  && -f $automatic_finalize_conflict_dir/PASSED.rollback-pending.sha256 \
  && $(<"$automatic_finalize_conflict_dir/PASSED.invalid.sha256") \
    == 'preserve existing forensic marker' ]] || {
  printf 'rollback finalization overwrote conflicting forensic evidence\n' >&2
  exit 1
}

# Execute the manual rollback transition itself. A failed restoration must
# leave rollback-pending evidence, and a retry must finalize it before exit 0.
manual_rollback_contract="$tmp_dir/manual-rollback-contract.sh"
sed -n '/^  prepare_rollback_evidence "\$rollback_evidence"/,/^  exit 0$/p' \
  "$CUTOVER" >"$manual_rollback_contract"
[[ -s $manual_rollback_contract ]] || {
  printf 'manual rollback evidence transition is missing\n' >&2
  exit 1
}
exercise_manual_rollback() (
  set -euo pipefail
  rollback_evidence=$1
  restore_result=$2
  restore_legacy() {
    [[ ! -e $rollback_evidence/PASSED.sha256 \
      && -f $rollback_evidence/PASSED.rollback-pending.sha256 ]] || return 90
    printf '%s\n' pending >"$rollback_evidence/restore-observed-pending"
    return "$restore_result"
  }
  secure_regular_file() {
    [[ -f $1 && ! -L $1 ]]
  }
  secure_root_chain() {
    [[ -d $1 && ! -L $1 ]]
  }
  die() {
    printf 'manual rollback evidence transition failed: %s\n' "$*" >&2
    exit 1
  }
  mv() {
    if [[ ${1:-} == -Tf && $# -eq 3 ]]; then
      command mv -f "$2" "$3"
    else
      command mv "$@"
    fi
  }
  # shellcheck source=/dev/null
  source "$manual_rollback_contract"
)

manual_failure_dir="$tmp_dir/manual-rollback-failure"
mkdir "$manual_failure_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$manual_failure_dir/cutover.json"
publish_cutover_marker "$manual_failure_dir"
set +e
exercise_manual_rollback "$manual_failure_dir" 1 >/dev/null 2>&1
manual_failure_status=$?
set -e
[[ $manual_failure_status -ne 0 \
  && ! -e $manual_failure_dir/PASSED.sha256 \
  && -f $manual_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $manual_failure_dir/cutover.json \
  && -f $manual_failure_dir/restore-observed-pending \
  && ! -e $manual_failure_dir/PASSED.rolled-back.sha256 \
  && ! -e $manual_failure_dir/cutover.rolled-back.json ]] || {
  printf 'failed manual restoration did not leave rollback-pending evidence\n' >&2
  exit 1
}
(
  cd "$manual_failure_dir"
  sha256sum --check --strict PASSED.rollback-pending.sha256 >/dev/null
)

set +e
exercise_manual_rollback "$manual_failure_dir" 0 >/dev/null 2>&1
manual_success_status=$?
set -e
[[ $manual_success_status -eq 0 \
  && ! -e $manual_failure_dir/PASSED.sha256 \
  && ! -e $manual_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $manual_failure_dir/cutover.json \
  && -f $manual_failure_dir/PASSED.rolled-back.sha256 \
  && ! -e $manual_failure_dir/cutover.rolled-back.json ]] || {
  printf 'successful manual rollback retained success-looking evidence\n' >&2
  exit 1
}
(
  cd "$manual_failure_dir"
  sha256sum --check --strict PASSED.rolled-back.sha256 >/dev/null
)

restore_legacy_contract="$tmp_dir/restore-legacy-contract.sh"
sed -n '/^restore_legacy() (/,/^)/p' "$CUTOVER" >"$restore_legacy_contract"
[[ -s $restore_legacy_contract ]] || {
  printf 'restore_legacy contract is missing\n' >&2
  exit 1
}
exercise_restore_manifest_guard() (
  set +e
  evidence_dir=$1
  rollback_dir=$evidence_dir/rollback
  mutation_log=$evidence_dir/mutation.log
  mkdir -p "$rollback_dir/control"
  printf 'legacy health policy\n' >"$rollback_dir/control/polymarket-legacy-health-policy.jq"
  printf '{}\n' >"$rollback_dir/state.json"
  printf 'keep spool state\n' >"$rollback_dir/spool.keep"
  (
    cd "$rollback_dir"
    sha256sum control/polymarket-legacy-health-policy.jq >manifest.sha256
  )
  jq -n --arg wrong "$(printf '0%.0s' {1..64})" \
    '{rollback_manifest_sha256:$wrong}' >"$evidence_dir/cutover.json"
  : >"$mutation_log"
  secure_root_chain() { [[ -e $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  clear_health_before_restart() { printf 'clear_health\n' >>"$mutation_log"; }
  atomic_install() { printf 'atomic_install\n' >>"$mutation_log"; }
  systemctl() { printf 'systemctl %s\n' "$*" >>"$mutation_log"; return 0; }
  rm() { printf 'rm %s\n' "$*" >>"$mutation_log"; return 0; }
  verify_fresh_legacy_runtime() { printf 'verify_fresh_legacy_runtime\n' >>"$mutation_log"; return 0; }
  verify_saved_unit_state() { printf 'verify_saved_unit_state\n' >>"$mutation_log"; return 0; }
  die() {
    printf 'restore manifest guard failed: %s\n' "$*" >&2
    exit 1
  }
  # shellcheck source=/dev/null
  source "$restore_legacy_contract"
  restore_legacy "$evidence_dir" >/dev/null 2>&1
)
restore_guard_dir="$tmp_dir/restore-manifest-guard"
mkdir "$restore_guard_dir"
set +e
exercise_restore_manifest_guard "$restore_guard_dir"
restore_manifest_guard_status=$?
set -e
[[ $restore_manifest_guard_status -ne 0 \
  && ! -s $restore_guard_dir/mutation.log \
  && -f $restore_guard_dir/rollback/state.json \
  && -f $restore_guard_dir/rollback/spool.keep ]] || {
  printf 'restore_legacy mutated runtime or deleted saved state before manifest lineage validation\n' >&2
  exit 1
}

exercise_restore_control_list_guard() (
  set +e
  evidence_dir=$1
  rollback_dir=$evidence_dir/rollback
  mutation_log=$evidence_dir/mutation.log
  mkdir -p "$rollback_dir/control"
  printf 'legacy health policy\n' >"$rollback_dir/control/polymarket-legacy-health-policy.jq"
  jq -n '{baseline_mode:"legacy_python",control_dir_present:true}' \
    >"$rollback_dir/state.json"
  (
    cd "$rollback_dir"
    sha256sum state.json control/polymarket-legacy-health-policy.jq >manifest.sha256
  )
  : >"$mutation_log"
  secure_root_chain() { [[ -e $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  systemctl() { printf 'systemctl %s\n' "$*" >>"$mutation_log"; return 0; }
  rm() { printf 'rm %s\n' "$*" >>"$mutation_log"; return 0; }
  die() {
    printf 'restore control-list guard failed: %s\n' "$*" >&2
    exit 1
  }
  # shellcheck source=/dev/null
  source "$rollback_control_contract"
  # shellcheck source=/dev/null
  source "$restore_legacy_contract"
  restore_legacy "$evidence_dir" >/dev/null 2>&1
)
restore_control_guard_dir="$tmp_dir/restore-control-list-guard"
mkdir "$restore_control_guard_dir"
set +e
exercise_restore_control_list_guard "$restore_control_guard_dir"
restore_control_guard_status=$?
set -e
[[ $restore_control_guard_status -ne 0 \
  && ! -s $restore_control_guard_dir/mutation.log ]] || {
  printf 'restore_legacy mutated runtime before validating rollback control membership\n' >&2
  exit 1
}

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
  local update=$1 recorded_at=${2:-1970-01-01T00:03:20Z} row
  row=$(jq -cn --argjson sequence "$sequence" --argjson update "$update" \
    --arg recorded_at "$recorded_at" \
    '{sequence:$sequence,recorded_at:$recorded_at,update:$update}')
  printf '%s\n' "$row" >>"$legacy_tape"
  printf '%s\n' "$row" >>"$rust_closed"
  sequence=$((sequence + 1))
}

for symbol in BTCUSDT ETHUSDT SOLUSDT XRPUSDT DOGEUSDT HYPEUSDT BNBUSDT; do
  case $symbol in
    BTCUSDT) market_name=Bitcoin ;;
    ETHUSDT) market_name=Ethereum ;;
    SOLUSDT) market_name=Solana ;;
    XRPUSDT) market_name=XRP ;;
    DOGEUSDT) market_name=Dogecoin ;;
    HYPEUSDT) market_name=Hyperliquid ;;
    BNBUSDT) market_name='Binance Coin' ;;
    *) printf 'unsupported fixture symbol: %s\n' "$symbol" >&2; exit 1 ;;
  esac
  append_row "$(jq -cn --arg symbol "$symbol" --arg market_name "$market_name" \
    '{kind:"market_metadata",market_id:("market-" + $symbol),
      condition_id:("condition-" + $symbol),symbol:$symbol,market_window_secs:300,
      source:"gamma_api",retrieved_at:"1970-01-01T00:03:20Z",
      market:{id:("market-" + $symbol),conditionId:("condition-" + $symbol),
        question:($market_name + " Up or Down"),slug:("market-" + $symbol),
        startDate:"1970-01-01T00:00:00Z",endDate:"1970-01-01T00:05:00Z",
        outcomes:["Up","Down"],clobTokenIds:[("up-" + $symbol),("down-" + $symbol)],
        orderPriceMinTickSize:0.01,orderMinSize:5,makerBaseFee:1000,takerBaseFee:1000,
        feesEnabled:true,negRisk:false}}')"
done

trade_id=$(printf '%s' '0x1|condition-BTCUSDT|up-BTCUSDT|BUY|200|0x2|1|0.5|0' \
  | sha256sum | awk '{print $1}')
trade=$(jq -cn --arg record_id "$trade_id" \
  '{kind:"polymarket_trade",record_id:$record_id,record_id_version:"v2",
    market_id:"market-BTCUSDT",condition_id:"condition-BTCUSDT",token_id:"up-BTCUSDT",
    symbol:"BTCUSDT",market_window_secs:300,side:"BUY",size:"1",price:"0.5",
    trade_ts:"1970-01-01T00:03:20Z",trade_ts_unix:200,
    transaction_hash:"0x1",proxy_wallet:"0x2",outcome:"Up",outcome_index:0,
    source:"polymarket_data_api",received_at:"1970-01-01T00:03:20Z",
    trade:{transactionHash:"0x1",conditionId:"condition-BTCUSDT",asset:"up-BTCUSDT",
      side:"BUY",timestamp:200,proxyWallet:"0x2",size:"1",price:"0.5",
      outcomeIndex:0,outcome:"Up"}}')
append_row "$trade"

late_settlement_trade_id=$(printf '%s' \
  '0xlate|condition-BTCUSDT|up-BTCUSDT|BUY|319|0x2|1|0.5|0' \
  | sha256sum | awk '{print $1}')
late_settlement_trade=$(jq -cn --arg record_id "$late_settlement_trade_id" \
  '{kind:"polymarket_trade",record_id:$record_id,record_id_version:"v2",
    market_id:"market-BTCUSDT",condition_id:"condition-BTCUSDT",token_id:"up-BTCUSDT",
    symbol:"BTCUSDT",market_window_secs:300,side:"BUY",size:"1",price:"0.5",
    trade_ts:"1970-01-01T00:05:19Z",trade_ts_unix:319,
    transaction_hash:"0xlate",proxy_wallet:"0x2",outcome:"Up",outcome_index:0,
    source:"polymarket_data_api",received_at:"1970-01-01T00:05:21Z",
    trade:{transactionHash:"0xlate",conditionId:"condition-BTCUSDT",asset:"up-BTCUSDT",
      side:"BUY",timestamp:319,proxyWallet:"0x2",size:"1",price:"0.5",
      outcomeIndex:0,outcome:"Up"}}')
append_row "$late_settlement_trade" "1970-01-01T00:05:21Z"

append_row "$(jq -cn \
  '{kind:"market_settlement",market_id:"market-BTCUSDT",
    condition_id:"condition-BTCUSDT",symbol:"BTCUSDT",market_window_secs:300,
    winning_token_id:"up-BTCUSDT",winning_outcome:"Up",resolved_up_won:true,
    resolution_source:"gamma_api_closed_market",retrieved_at:"1970-01-01T00:03:20Z",
    market:{id:"market-BTCUSDT",conditionId:"condition-BTCUSDT",
      question:"BTCUSDT Up or Down",startDate:"1970-01-01T00:00:00Z",
      endDate:"1970-01-01T00:05:00Z",closed:true,
      outcomes:["Up","Down"],clobTokenIds:["up-BTCUSDT","down-BTCUSDT"],
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
  '{sequence:$sequence,recorded_at:"1970-01-01T00:16:41Z",update:$update}' \
  >>"$legacy_tape"

parity="$tmp_dir/parity.json"
"$VERIFY" verify-shadow-parity \
  --legacy-spool "$legacy" --rust-spool "$rust" --started-at-unix 100 \
  --ended-at-unix 1000 \
  --output "$parity"
jq -e '.passed == true and .checks.metadata_parity == true
  and ([.checks[]] | all)
  and .metrics.legacy_trade_count == 2
  and .metrics.rust_trade_count == 2
  and .metrics.trade_shared_value_mismatch_ids == []
  and (.metrics.normalized_trade_sha256 | test("^[a-f0-9]{64}$"))
  and (.metrics.normalized_metadata_sha256 | test("^[a-f0-9]{64}$"))
  and (.metrics.normalized_settlement_sha256 | test("^[a-f0-9]{64}$"))' \
  "$parity" >/dev/null
jq -e 'select(.update.transaction_hash == "0xlate")
  | .update.trade_ts_unix == 319
    and .update.trade.timestamp == 319
    and .update.received_at == "1970-01-01T00:05:21Z"' \
  "$legacy_tape" "$rust_closed" >/dev/null

rust_bad="$tmp_dir/rust-bad"
cp -R "$rust" "$rust_bad"
jq -cn --argjson update "$trade" \
  '{sequence:0,recorded_at:"1970-01-01T00:03:21Z",update:$update}' \
  >"$rust_bad/market-updates.ndjson"
if "$VERIFY" verify-shadow-parity \
  --legacy-spool "$legacy" --rust-spool "$rust_bad" --started-at-unix 100 \
  --ended-at-unix 1000 \
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
  --started-at-unix 100 --ended-at-unix 1000 \
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
release_manifest_sha=$(printf '1%.0s' {1..64})
control_archive_sha=$(printf '2%.0s' {1..64})
legacy_invocation_id=$(printf 'e%.0s' {1..32})
shadow_invocation_id=$(printf 'f%.0s' {1..32})
legacy_cmdline=dffeb118d105e9312898460249f514eb982c20433cd20840ffb2107c64bbca4a
jq \
  --arg candidate "$candidate" \
  --arg source "$source_revision" \
  --arg bundle "$bundle" \
  --arg oss_config "$oss_config" \
  --arg release_manifest_sha "$release_manifest_sha" \
  --arg control_archive_sha "$control_archive_sha" \
  --arg legacy_invocation_id "$legacy_invocation_id" \
  --arg shadow_invocation_id "$shadow_invocation_id" \
  --arg legacy_cmdline "$legacy_cmdline" \
  '. + {
    schema:"monday.polymarket_shadow_gate.v1",
    candidate_sha256:$candidate,
    baseline_mode:"legacy_python",
    deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,
    release_manifest_sha256:$release_manifest_sha,
    control_archive_sha256:$control_archive_sha,
    oss_config_sha256:$oss_config,
    duration_seconds:4201,
    parity_window_started_at_unix:100,
    parity_window_ended_at_unix:1000,
    completed_at:"2026-07-15T00:00:00Z",
    shadow_run_id:"run-1",
    production_eligible:true,
    baseline_health_snapshot:{
      updated_at:"2026-07-15T00:00:00Z",
      last_success_at:"2026-07-15T00:00:00Z",
      target_markets:14,api_errors:[],malformed_trade_rows:0,
      truncated_trade_markets:[],stale_trade_markets:[],
      stale_settlement_markets:[],overdue_unresolved_markets:[]
    },
    legacy_runtime:{
      exec_start:"/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py",
      cmdline:"/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py",
      cmdline_sha256:$legacy_cmdline,
      fragment_path:"/etc/systemd/system/polymarket-reference-collector.service",
      drop_in_paths:[],main_pid:10,restarts:1,
      invocation_id:$legacy_invocation_id
    },
    shadow_runtime:{
      exec_start:("/opt/monday/releases/polymarket-raw-ops/" + $candidate
        + "/polymarket-raw-ops collect-reference --spool-dir ${MONDAY_POLYMARKET_SHADOW_SPOOL}"),
      cmdline:("/opt/monday/releases/polymarket-raw-ops/" + $candidate
        + "/polymarket-raw-ops collect-reference --spool-dir "
        + "/data/monday/spool/polymarket-reference-rust-shadow/" + $candidate + "/run-1"),
      fragment_path:"/etc/systemd/system/polymarket-reference-collector-shadow@.service",
      drop_in_paths:[],main_pid:11,restarts:0,
      invocation_id:$shadow_invocation_id,
      memory_events:{
        start:{high:0,max:0,oom:0,oom_kill:0,oom_group_kill:0},
        end:{high:0,max:0,oom:0,oom_kill:0,oom_group_kill:0}
      }
    },
    checks:(.checks + {health_freshness:true,candidate_identity:true,
      memory_events_stable:true,oss_readback_parity:true,
      market_oss_readback_parity:true}),
    metrics:(.metrics + {
      oss_uploaded_segments:1,oss_canonical_uploaded_segments:1,
      market_oss_uploaded_segments:1,market_oss_canonical_uploaded_segments:1
    })
  } | .passed = true' "$parity" >"$tmp_dir/gate.json"
jq -e -f "$POLICY" "$tmp_dir/gate.json" >/dev/null
jq 'del(.baseline_health_snapshot)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-baseline-health-snapshot.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-baseline-health-snapshot.json" >/dev/null; then
  printf 'gate policy accepted legacy evidence without its frozen health snapshot\n' >&2
  exit 1
fi
jq '.shadow_runtime.memory_events.start.high = 1
    | .shadow_runtime.memory_events.end.high = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/nonzero-memory-high-baseline.json"
if jq -e -f "$POLICY" "$tmp_dir/nonzero-memory-high-baseline.json" >/dev/null; then
  printf 'gate policy accepted a nonzero memory.events high baseline\n' >&2
  exit 1
fi
jq '.shadow_runtime.memory_events.end.high += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/growing-memory-high.json"
if jq -e -f "$POLICY" "$tmp_dir/growing-memory-high.json" >/dev/null; then
  printf 'gate policy accepted a growing memory.events high counter\n' >&2
  exit 1
fi
for memory_counter_path in \
  start.max start.oom start.oom_kill start.oom_group_kill \
  end.max end.oom end.oom_kill end.oom_group_kill; do
  memory_counter_file=${memory_counter_path//./-}
  jq --arg path "$memory_counter_path" \
    'setpath((["shadow_runtime","memory_events"] + ($path | split("."))); 1)' \
    "$tmp_dir/gate.json" >"$tmp_dir/nonzero-memory-$memory_counter_file.json"
  if jq -e -f "$POLICY" \
    "$tmp_dir/nonzero-memory-$memory_counter_file.json" >/dev/null; then
    printf 'gate policy accepted nonzero memory.events %s\n' \
      "$memory_counter_path" >&2
    exit 1
  fi
done
jq '.metrics.trade_maturity_lag_seconds = 599' \
  "$tmp_dir/gate.json" >"$tmp_dir/short-trade-maturity.json"
if jq -e -f "$POLICY" "$tmp_dir/short-trade-maturity.json" >/dev/null; then
  printf 'gate policy accepted a shortened trade maturity lag\n' >&2
  exit 1
fi
jq '.metrics.trade_event_window_ended_at_unix += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-trade-end.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-trade-end.json" >/dev/null; then
  printf 'gate policy accepted an unbound mature trade window\n' >&2
  exit 1
fi
jq '.parity_window_ended_at_unix = 700
    | .metrics.trade_event_window_ended_at_unix = 100
    | .metrics.settlement_event_window_ended_at_unix = 100' \
  "$tmp_dir/gate.json" >"$tmp_dir/empty-mature-trade-window.json"
if jq -e -f "$POLICY" "$tmp_dir/empty-mature-trade-window.json" >/dev/null; then
  printf 'gate policy accepted an empty mature trade window\n' >&2
  exit 1
fi
jq '.metrics.rust_trade_metadata_context_mismatch_market_ids = ["missing-context"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/trade-context-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/trade-context-mismatch.json" >/dev/null; then
  printf 'gate policy accepted a trade metadata context mismatch\n' >&2
  exit 1
fi
jq '.metrics.legacy_only_trade_ids = ["missing-from-rust"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-trade-omission.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-trade-omission.json" >/dev/null; then
  printf 'gate policy accepted a mature trade missing from Rust\n' >&2
  exit 1
fi
jq '.metrics.rust_only_trade_ids = ["missing-from-legacy"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/rust-trade-addition.json"
jq -e -f "$POLICY" "$tmp_dir/rust-trade-addition.json" >/dev/null
jq '.metrics.trade_metadata_shared_value_mismatch_market_ids = ["market-extra"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/trade-metadata-value-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/trade-metadata-value-mismatch.json" >/dev/null; then
  printf 'gate policy accepted contradictory trade metadata context\n' >&2
  exit 1
fi
jq '.metrics.rust_settlement_metadata_context_mismatch_market_ids = ["market-extra"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/settlement-context-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/settlement-context-mismatch.json" >/dev/null; then
  printf 'gate policy accepted a settlement metadata context mismatch\n' >&2
  exit 1
fi
jq 'del(.metrics.normalized_settlement_sha256)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-settlement-digest.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-settlement-digest.json" >/dev/null; then
  printf 'gate policy accepted evidence without a normalized settlement digest\n' >&2
  exit 1
fi
jq '.metrics.normalized_settlement_sha256 = "not-a-digest"' \
  "$tmp_dir/gate.json" >"$tmp_dir/invalid-settlement-digest.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-settlement-digest.json" >/dev/null; then
  printf 'gate policy accepted an invalid normalized settlement digest\n' >&2
  exit 1
fi
jq '.metrics.rust_only_metadata_ids = ["future-market"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/rust-metadata-superset.json"
jq -e -f "$POLICY" "$tmp_dir/rust-metadata-superset.json" >/dev/null
jq '.metrics.legacy_only_metadata_ids = ["missing-from-rust"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-metadata-omission.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-metadata-omission.json" >/dev/null; then
  printf 'gate policy accepted metadata missing from Rust\n' >&2
  exit 1
fi
jq '.metrics.legacy_only_settlement_ids = ["missing-from-rust"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-settlement-omission.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-settlement-omission.json" >/dev/null; then
  printf 'gate policy accepted a mature settlement missing from Rust\n' >&2
  exit 1
fi
jq '.metrics.settlement_shared_values_match = false' \
  "$tmp_dir/gate.json" >"$tmp_dir/settlement-value-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/settlement-value-mismatch.json" >/dev/null; then
  printf 'gate policy accepted contradictory settlement values\n' >&2
  exit 1
fi
jq '.metrics.settlement_event_window_started_at_unix += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-settlement-start.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-settlement-start.json" >/dev/null; then
  printf 'gate policy accepted an unbound settlement start window\n' >&2
  exit 1
fi
jq '.metrics.settlement_event_window_started_at_unix = -800' \
  "$tmp_dir/gate.json" >"$tmp_dir/non-saturating-settlement-start.json"
if jq -e -f "$POLICY" "$tmp_dir/non-saturating-settlement-start.json" >/dev/null; then
  printf 'gate policy accepted a negative settlement start below the Unix epoch\n' >&2
  exit 1
fi
jq '.metrics.settlement_event_window_ended_at_unix += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-settlement-end.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-settlement-end.json" >/dev/null; then
  printf 'gate policy accepted an unbound settlement end window\n' >&2
  exit 1
fi
jq '.duration_seconds = 4200' "$tmp_dir/gate.json" >"$tmp_dir/short.json"
if jq -e -f "$POLICY" "$tmp_dir/short.json" >/dev/null; then
  printf 'gate policy accepted a shadow shorter than one hour plus its maturity tail\n' >&2
  exit 1
fi
jq '.production_eligible = false' "$tmp_dir/gate.json" >"$tmp_dir/test-only.json"
if jq -e -f "$POLICY" "$tmp_dir/test-only.json" >/dev/null; then
  printf 'gate policy accepted test-only evidence\n' >&2
  exit 1
fi
jq '.parity_window_ended_at_unix = 700' "$tmp_dir/gate.json" \
  >"$tmp_dir/short-parity-tail.json"
if jq -e -f "$POLICY" "$tmp_dir/short-parity-tail.json" >/dev/null; then
  printf 'gate policy accepted a parity tail too short for mature trades\n' >&2
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
jq 'del(.release_manifest_sha256)' "$tmp_dir/gate.json" \
  >"$tmp_dir/unbound-release-manifest.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-release-manifest.json" >/dev/null; then
  printf 'gate policy accepted evidence without a release manifest identity\n' >&2
  exit 1
fi
jq 'del(.control_archive_sha256)' "$tmp_dir/gate.json" \
  >"$tmp_dir/unbound-control-archive.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-control-archive.json" >/dev/null; then
  printf 'gate policy accepted evidence without a control archive identity\n' >&2
  exit 1
fi
jq '.legacy_runtime.restarts = -1' "$tmp_dir/gate.json" >"$tmp_dir/negative-restarts.json"
if jq -e -f "$POLICY" "$tmp_dir/negative-restarts.json" >/dev/null; then
  printf 'gate policy accepted a negative legacy restart counter\n' >&2
  exit 1
fi
jq '.legacy_runtime.restarts = 1.5' "$tmp_dir/gate.json" >"$tmp_dir/fractional-restarts.json"
if jq -e -f "$POLICY" "$tmp_dir/fractional-restarts.json" >/dev/null; then
  printf 'gate policy accepted a fractional legacy restart counter\n' >&2
  exit 1
fi
jq 'del(.legacy_runtime.invocation_id)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-legacy-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-legacy-invocation.json" >/dev/null; then
  printf 'gate policy accepted evidence without a legacy invocation ID\n' >&2
  exit 1
fi
jq '.legacy_runtime.invocation_id = ("E" * 32)' "$tmp_dir/gate.json" \
  >"$tmp_dir/invalid-legacy-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-legacy-invocation.json" >/dev/null; then
  printf 'gate policy accepted a malformed legacy invocation ID\n' >&2
  exit 1
fi
jq 'del(.shadow_runtime.invocation_id)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-shadow-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-shadow-invocation.json" >/dev/null; then
  printf 'gate policy accepted evidence without a shadow invocation ID\n' >&2
  exit 1
fi
jq '.shadow_runtime.invocation_id = ("f" * 31)' "$tmp_dir/gate.json" \
  >"$tmp_dir/invalid-shadow-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-shadow-invocation.json" >/dev/null; then
  printf 'gate policy accepted a malformed shadow invocation ID\n' >&2
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
baseline_sha=$(printf '9%.0s' {1..64})
jq --arg baseline "$baseline_sha" '.baseline_mode = "rust_release"
  | .baseline_health_snapshot = null
  | .legacy_runtime += {
      exec_start:"/opt/monday/bin/polymarket-raw-ops collect-reference",
      cmdline:"/opt/monday/bin/polymarket-raw-ops collect-reference",
      cmdline_sha256:"7b06db4beb374f013a090e023289f8b026f39c324ee527f194b706656f6a1f94",
      fragment_path:"/etc/systemd/system/polymarket-reference-collector.service",
      drop_in_paths:[],
      main_pid:12,restarts:0,invocation_id:("1" * 32),
      release_sha256:$baseline,
      release_path:("/opt/monday/releases/polymarket-raw-ops/" + $baseline + "/polymarket-raw-ops"),
      proc_exe:("/opt/monday/releases/polymarket-raw-ops/" + $baseline + "/polymarket-raw-ops")
    }
' "$tmp_dir/gate.json" >"$tmp_dir/rust-release-gate.json"
jq -e -f "$POLICY" "$tmp_dir/rust-release-gate.json" >/dev/null
while IFS='|' read -r name filter; do
  jq "$filter" "$tmp_dir/rust-release-gate.json" >"$tmp_dir/rust-release-$name.json"
  if jq -e -f "$POLICY" "$tmp_dir/rust-release-$name.json" >/dev/null; then
    printf 'rust release gate accepted %s drift\n' "$name" >&2
    exit 1
  fi
done <<'EOF'
release_path|.legacy_runtime.release_path = "/tmp/polymarket-raw-ops"
release_sha|.legacy_runtime.release_sha256 = "not-a-digest"
proc_exe|.legacy_runtime.proc_exe = "/tmp/polymarket-raw-ops"
candidate_equals_baseline|.candidate_sha256 = .legacy_runtime.release_sha256
EOF
jq '.legacy_runtime += {
    release_sha256:("'"$baseline_sha"'"),
    release_path:("/opt/monday/releases/polymarket-raw-ops/" + "'"$baseline_sha"'" + "/polymarket-raw-ops"),
    proc_exe:("/opt/monday/releases/polymarket-raw-ops/" + "'"$baseline_sha"'" + "/polymarket-raw-ops")
  }' "$tmp_dir/gate.json" >"$tmp_dir/legacy-mixed-rust-fields.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-mixed-rust-fields.json" >/dev/null; then
  printf 'gate policy accepted legacy baseline evidence with Rust-only release fields\n' >&2
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
  '.stale_settlement_markets = ["market-1"]'; do
  jq "$mutation" "$tmp_dir/legacy-health.json" >"$tmp_dir/bad-legacy-health.json"
  if jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/bad-legacy-health.json" >/dev/null; then
    printf 'legacy health policy accepted failure mutation: %s\n' "$mutation" >&2
    exit 1
  fi
done
jq '.overdue_unresolved_markets = ["historical-market"]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/historical-overdue-legacy-health.json"
jq -e -f "$LEGACY_HEALTH_POLICY" \
  "$tmp_dir/historical-overdue-legacy-health.json" >/dev/null
for mutation in \
  'del(.overdue_unresolved_markets)' \
  '.overdue_unresolved_markets = "market-1"' \
  '.overdue_unresolved_markets = [""]' \
  '.overdue_unresolved_markets = [1]'; do
  jq "$mutation" "$tmp_dir/legacy-health.json" >"$tmp_dir/bad-legacy-overdue.json"
  if jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/bad-legacy-overdue.json" >/dev/null; then
    printf 'legacy health policy accepted invalid overdue evidence: %s\n' "$mutation" >&2
    exit 1
  fi
done
legacy_health_classifier="$tmp_dir/legacy-health-classifier.sh"
sed -n \
  -e '/^baseline_health_requires_continuous_freshness()/,/^}/p' \
  -e '/^legacy_health_sample_state()/,/^}/p' \
  -e '/^legacy_health_transition()/,/^}/p' "$GATE" \
  >"$legacy_health_classifier"
# shellcheck source=/dev/null
source "$legacy_health_classifier"
if baseline_health_requires_continuous_freshness legacy_python; then
  printf 'legacy Python health incorrectly requires continuous 240-second freshness\n' >&2
  exit 1
fi
baseline_health_requires_continuous_freshness rust_release
daemon_reload_line=$(grep -nF 'systemctl daemon-reload' "$GATE" | tail -1 | cut -d: -f1)
snapshot_line=$(grep -nF 'baseline_health_snapshot=$(fresh_baseline_health_snapshot' \
  "$GATE" | cut -d: -f1)
gate_start_line=$(grep -nF 'started_at_unix=$(date -u +%s)' "$GATE" | cut -d: -f1)
shadow_start_line=$(grep -nF 'systemctl start "$shadow_unit"' "$GATE" | cut -d: -f1)
if ! ((daemon_reload_line < snapshot_line && snapshot_line < gate_start_line \
  && gate_start_line < shadow_start_line)); then
  printf 'legacy health is not frozen immediately at the shadow Gate start boundary\n' >&2
  exit 1
fi
[[ $(legacy_health_sample_state \
  "$tmp_dir/legacy-health.json" "$LEGACY_HEALTH_POLICY" legacy_python) == clean ]]
jq '.api_errors = ["trades condition-1: The read operation timed out"]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/transient-legacy-health.json"
[[ $(legacy_health_sample_state \
  "$tmp_dir/transient-legacy-health.json" "$LEGACY_HEALTH_POLICY" legacy_python) \
  == transient_api_error ]]
[[ $(legacy_health_sample_state \
  "$tmp_dir/transient-legacy-health.json" "$LEGACY_HEALTH_POLICY" rust_release) \
  == fatal ]]
jq '.malformed_trade_rows = 1' "$tmp_dir/transient-legacy-health.json" \
  >"$tmp_dir/fatal-legacy-health.json"
[[ $(legacy_health_sample_state \
  "$tmp_dir/fatal-legacy-health.json" "$LEGACY_HEALTH_POLICY" legacy_python) == fatal ]]
[[ $(legacy_health_transition clean '' 0 240) == advance: ]]
startup_transient=$(legacy_health_transition transient_api_error '' 0 240)
[[ $startup_transient == wait:0 ]]
[[ $(legacy_health_transition transient_api_error \
  "${startup_transient#*:}" 240 240) == wait:0 ]]
[[ $(legacy_health_transition clean "${startup_transient#*:}" 240 240) == advance: ]]
[[ $(legacy_health_transition transient_api_error \
  "${startup_transient#*:}" 241 240) == expired:0 ]]
[[ $(legacy_health_transition clean "${startup_transient#*:}" 241 240) == expired:0 ]]
[[ $(legacy_health_transition fatal '' 0 240) == fatal: ]]

jq -n '{
  updated_at:"2026-07-15T00:00:01Z",last_success_at:"2026-07-15T00:00:01Z",
  cycle_started_at:"2026-07-15T00:00:00Z",cycle_duration_ms:1000,
  target_markets:120,
  missing_target_symbols:[],api_errors:[],malformed_trade_rows:0,
  trade_poll_budget:112,trade_poll_concurrency:4,trade_request_spacing_ms:100,
  priority_trade_markets_before_market_details:108,
  market_detail_budget:2,market_detail_eligible:3,market_detail_priority:2,
  market_detail_selected:2,market_detail_deferred:1,market_detail_priority_deferred:0,
  trade_poll_budget_after_market_details:110,
  eligible_trade_markets:112,priority_trade_markets:110,
  selected_trade_markets:110,deferred_trade_markets:2,priority_trade_backlog:0,
  trade_polls:110,successful_trade_polls:110,
  truncated_trade_markets:[],non_object_trade_markets:[],invalid_settlement_markets:[],
  invalid_end_time_markets:[],stale_trade_markets:[],stale_settlement_markets:[],
  overdue_unresolved_markets:[]
}' >"$tmp_dir/rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/rust-health.json" >/dev/null
jq '.overdue_unresolved_markets = ["historical-market"]' \
  "$tmp_dir/rust-health.json" >"$tmp_dir/historical-overdue-rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" \
  "$tmp_dir/historical-overdue-rust-health.json" >/dev/null
for mutation in \
  'del(.overdue_unresolved_markets)' \
  '.overdue_unresolved_markets = "market-1"' \
  '.overdue_unresolved_markets = [""]' \
  '.overdue_unresolved_markets = [1]'; do
  jq "$mutation" "$tmp_dir/rust-health.json" >"$tmp_dir/bad-rust-overdue.json"
  if jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/bad-rust-overdue.json" >/dev/null; then
    printf 'Rust health policy accepted invalid overdue evidence: %s\n' "$mutation" >&2
    exit 1
  fi
done
jq '
  .priority_trade_markets_before_market_details = 112
  | .market_detail_budget = 0
  | .market_detail_selected = 0
  | .market_detail_deferred = 3
  | .market_detail_priority_deferred = 2
  | .trade_poll_budget_after_market_details = 112
  | .priority_trade_markets = 112
  | .selected_trade_markets = 112
  | .deferred_trade_markets = 0
  | .trade_polls = 112
  | .successful_trade_polls = 112
' "$tmp_dir/rust-health.json" >"$tmp_dir/saturated-rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/saturated-rust-health.json" >/dev/null
for mutation in \
  'del(.last_success_at)' \
  'del(.cycle_started_at)' \
  '.cycle_duration_ms = -1' \
  '.cycle_duration_ms = 180001' \
  '.trade_poll_budget = 111' \
  '.trade_poll_budget = 113' \
  '.trade_poll_concurrency = 0' \
  '.trade_poll_concurrency = 5' \
  '.trade_poll_concurrency = 193' \
  'del(.priority_trade_markets_before_market_details)' \
  '.priority_trade_markets_before_market_details = 113' \
  '.market_detail_budget = 3' \
  '.market_detail_priority = 4' \
  '.market_detail_selected = 1' \
  '.market_detail_deferred = 2' \
  '.market_detail_priority_deferred = 1' \
  '.trade_poll_budget_after_market_details = 109' \
  'del(.trade_request_spacing_ms)' \
  '.trade_request_spacing_ms = 99' \
  '.priority_trade_markets = 107' \
  '.priority_trade_backlog = 1' \
  '.selected_trade_markets = 109' \
  '.deferred_trade_markets = 1' \
  '.eligible_trade_markets = 111' \
  '.successful_trade_polls = 0' \
  '.successful_trade_polls = 111' \
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
grep -Fq 'readonly PARITY_TAIL_SECONDS=601' "$GATE"
grep -Fq 'readonly SETTLEMENT_EVENT_LOOKBACK_SECONDS=900' "$GATE"
grep -Fq 'bounded_parity_window_start' "$GATE"
grep -Fq 'readonly MAX_ACCEPTED_CYCLE_SECONDS=180' "$GATE"
grep -Fq 'readonly INITIAL_HEALTH_GRACE_SECONDS=60' "$GATE"
grep -Fq 'readonly HEALTH_SETTLE_SECONDS=$((MAX_ACCEPTED_CYCLE_SECONDS + INITIAL_HEALTH_GRACE_SECONDS))' "$GATE"
gate_health_budget_contract="$tmp_dir/gate-health-budget-contract.sh"
sed -n \
  -e '/^readonly MAX_ACCEPTED_CYCLE_SECONDS=/p' \
  -e '/^readonly INITIAL_HEALTH_GRACE_SECONDS=/p' \
  -e '/^readonly MAX_HEALTH_SILENCE_SECONDS=/p' "$GATE" \
  >"$gate_health_budget_contract"
# shellcheck source=/dev/null
source "$gate_health_budget_contract"
expected_health_silence_seconds=$((MAX_ACCEPTED_CYCLE_SECONDS + INITIAL_HEALTH_GRACE_SECONDS))
[[ $expected_health_silence_seconds -eq 240 ]] || {
  printf 'health freshness budget drifted from the expected 240-second contract\n' >&2
  exit 1
}
gate_health_silence_seconds=$MAX_HEALTH_SILENCE_SECONDS
[[ $gate_health_silence_seconds -eq $expected_health_silence_seconds ]] || {
  printf 'shadow gate freshness budget %ss does not cover the 240-second cycle budget\n' \
    "$gate_health_silence_seconds" >&2
  exit 1
}
cutover_health_silence_seconds=$(
  sed -n 's/^readonly MAX_HEALTH_SILENCE_SECONDS=//p' "$CUTOVER"
)
[[ $cutover_health_silence_seconds -eq $expected_health_silence_seconds ]] || {
  printf 'cutover freshness budget %ss does not cover the 240-second cycle budget\n' \
    "$cutover_health_silence_seconds" >&2
  exit 1
}
grep -Fq 'legacy_health_state=$(legacy_health_sample_state' "$GATE"
grep -Fq '"$legacy_health" "$release_control_dir/${LEGACY_HEALTH_POLICY##*/}"' \
  "$GATE"
grep -Fq '    "$baseline_mode")' "$GATE"
grep -Fq 'legacy_health_result=$(legacy_health_transition' "$GATE"
grep -Fq '"$now_uptime" "$MAX_HEALTH_SILENCE_SECONDS")' "$GATE"
grep -Fq 'legacy_health_decision=${legacy_health_result%%:*}' "$GATE"
grep -Fq 'legacy_api_error_started_at=${legacy_health_result#*:}' "$GATE"
legacy_transition_line=$(grep -nF \
  'legacy_health_result=$(legacy_health_transition' "$GATE" | cut -d: -f1)
health_settle_line=$(grep -nF \
  '  if ((elapsed >= HEALTH_SETTLE_SECONDS)); then' "$GATE" | cut -d: -f1)
if ((legacy_transition_line >= health_settle_line)); then
  printf 'legacy health recovery budget starts after the shadow settle delay\n' >&2
  exit 1
fi
grep -Fq 'if [[ $legacy_health_decision == advance ]]; then' "$GATE"
grep -Fq '&& [[ $legacy_health_decision == advance ]]; then' \
  "$GATE"
grep -Fq 'if ((elapsed >= HEALTH_SETTLE_SECONDS)); then' "$GATE"
if grep -Fq 'if ((elapsed >= HEALTH_SETTLE_SECONDS)) || [[ $test_only == true ]]; then' "$GATE"; then
  printf 'short shadow gate bypasses the initial health settle window\n' >&2
  exit 1
fi
grep -Fq 'verify-shadow-parity' "$GATE"
[[ ! -e "$SCRIPT_DIR/verify-polymarket-shadow-parity.py" ]]
if grep -Fq 'python3 "$PARITY_VERIFIER"' "$GATE"; then
  printf 'shadow gate still invokes the retired legacy parity verifier\n' >&2
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
grep -Fq 'ControlGroup' "$GATE"
grep -Fq 'valid_absolute_path "$control_group"' "$GATE"
grep -Fq '[[ $control_group == /system.slice/*' "$GATE"
grep -Fq '${control_group##*/} == "$shadow_unit"' "$GATE"
grep -Fq 'proc_binding == "0::$control_group"' "$GATE"
grep -Fq 'cgroup_dir="/sys/fs/cgroup$control_group"' "$GATE"
grep -Fq 'direct_directory "$cgroup_dir"' "$GATE"
grep -Fq '$(readlink -f -- "$file") == "$file"' "$GATE"
grep -Fq 'stable_memory_events_snapshot' "$GATE"
grep -Fq 'memory_events:{start:$memory_events_start,end:$memory_events_end}' "$GATE"
grep -Fq 'memory_events_stable:true' "$GATE"
grep -Fq 'systemctl freeze "$shadow_unit"' "$GATE"
grep -Fq 'FreezerState' "$GATE"
grep -Fq '[[ $shadow_freezer_state == frozen ]]' "$GATE"
grep -Fq 'systemctl kill --kill-whom=main --signal=SIGTERM "$shadow_unit"' "$GATE"

cleanup_thaw_line=$(grep -n '^    systemctl thaw "$shadow_unit"' "$GATE" | cut -d: -f1)
cleanup_stop_line=$(grep -n '^    systemctl stop "$shadow_unit" >/dev/null' "$GATE" \
  | cut -d: -f1)
[[ $cleanup_thaw_line =~ ^[1-9][0-9]*$ \
  && $cleanup_stop_line =~ ^[1-9][0-9]*$ \
  && $cleanup_thaw_line -lt $cleanup_stop_line ]] || {
  printf 'shadow cleanup does not thaw before stop\n' >&2
  exit 1
}

freeze_line=$(grep -n '^systemctl freeze "$shadow_unit"' "$GATE" | cut -d: -f1)
freezer_state_line=$(grep -n '^shadow_freezer_state=.*FreezerState' "$GATE" | cut -d: -f1)
final_memory_line=$(grep -n '^memory_events_end=$(stable_memory_events_snapshot' "$GATE" \
  | tail -1 | cut -d: -f1)
kill_line=$(grep -n '^systemctl kill --kill-whom=main --signal=SIGTERM' "$GATE" \
  | cut -d: -f1)
final_thaw_line=$(grep -n '^systemctl thaw "$shadow_unit"' "$GATE" \
  | tail -1 | cut -d: -f1 || true)
thawed_state_line=$(grep -n '^shadow_thawed_state=.*FreezerState' "$GATE" \
  | cut -d: -f1 || true)
final_stop_line=$(grep -n '^systemctl stop "$shadow_unit"$' "$GATE" | tail -1 | cut -d: -f1)
[[ $freeze_line =~ ^[1-9][0-9]*$ \
  && $freezer_state_line =~ ^[1-9][0-9]*$ \
  && $final_memory_line =~ ^[1-9][0-9]*$ \
  && $kill_line =~ ^[1-9][0-9]*$ \
  && $final_thaw_line =~ ^[1-9][0-9]*$ \
  && $thawed_state_line =~ ^[1-9][0-9]*$ \
  && $final_stop_line =~ ^[1-9][0-9]*$ \
  && $freeze_line -lt $freezer_state_line \
  && $freezer_state_line -lt $final_memory_line \
  && $final_memory_line -lt $kill_line \
  && $kill_line -lt $final_thaw_line \
  && $final_thaw_line -lt $thawed_state_line \
  && $thawed_state_line -lt $final_stop_line \
  && $kill_line -lt $final_stop_line ]] || {
  printf 'shadow final freeze/snapshot/kill/thaw/stop sequence is unsafe\n' >&2
  exit 1
}
grep -Fq '[[ $shadow_thawed_state == running ]]' "$GATE"

validator_functions="$tmp_dir/control-group-validator.sh"
sed -n '/^valid_absolute_path() {$/,/^}$/p' "$GATE" >"$validator_functions"
sed -n '/^valid_shadow_control_group() {$/,/^}$/p' "$GATE" >>"$validator_functions"
grep -Fq 'valid_absolute_path()' "$validator_functions"
grep -Fq 'valid_shadow_control_group()' "$validator_functions"
# shellcheck disable=SC1090
source "$validator_functions"
shadow_unit="polymarket-reference-collector-shadow@${candidate}.service"
valid_shadow_control_group \
  "/system.slice/system-polymarket\\x2dreference\\x2dcollector\\x2dshadow.slice/$shadow_unit"
for invalid_control_group in \
  "/system.slice/system-polymarket.slice/not-$shadow_unit" \
  "/system.slice/../$shadow_unit" \
  "/system.slice//nested/$shadow_unit"; do
  if valid_shadow_control_group "$invalid_control_group"; then
    printf 'control-group validator accepted unsafe path: %s\n' \
      "$invalid_control_group" >&2
    exit 1
  fi
done
grep -Fq '.missing_target_symbols == []' "$RUST_HEALTH_POLICY"
grep -Fq 'systemctl restart "$COLLECTOR_UNIT"' "$CUTOVER"
grep -Fq 'clear_health_before_restart "$evidence_dir" pre-cutover' "$CUTOVER"
grep -Fq 'readlink -f "/proc/$pid/exe"' "$CUTOVER"
grep -Fq 'FragmentPath' "$CUTOVER"
grep -Fq 'DropInPaths' "$CUTOVER"
grep -Fq 'NRestarts' "$CUTOVER"
[[ $(grep -Fc '[[ $invocation_id == "$expected_invocation_id" ]]' "$GATE") -eq 2 ]]
[[ $(grep -Fc '[[ $invocation_id == "$expected_invocation_id" ]]' "$CUTOVER") -eq 2 ]]
grep -Fq 'invocation_id:$rust_invocation_id' "$CUTOVER"
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
grep -Fxq 'Type=exec' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryHigh=672M' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryMax=768M' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryHigh=672M' "$SCRIPT_DIR/polymarket-reference-collector.service"
grep -Fxq 'MemoryMax=768M' "$SCRIPT_DIR/polymarket-reference-collector.service"
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
legacy_drain_line=$(grep -n '^verify_oneshot_success "$REFERENCE_UPLOAD_UNIT"' "$CUTOVER" \
  | head -1 | cut -d: -f1)
legacy_cursor_line=$(grep -n '^legacy_stop_cursor=$(journal_cursor "$COLLECTOR_UNIT")' \
  "$CUTOVER" | cut -d: -f1)
legacy_final_runtime_line=$(grep -n \
  '^verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" "$gate_legacy_invocation_id"' \
  "$CUTOVER" \
  | tail -1 | cut -d: -f1)
legacy_final_health_line=$(grep -n '^verify_legacy_health "$pre_stop_health_not_before"' \
  "$CUTOVER" | cut -d: -f1)
legacy_final_oss_line=$(grep -n \
  'OSS configuration changed during the legacy uploader drain' "$CUTOVER" \
  | cut -d: -f1)
legacy_journal_guard_line=$(grep -n '^verify_no_restart_after_cursor' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
legacy_stopped_counter_line=$(grep -n '^stopped_legacy_restarts=' "$CUTOVER" \
  | cut -d: -f1)
legacy_stopped_equality_line=$(grep -n \
  '^\[\[ \$stopped_legacy_restarts == "\$gate_legacy_restarts" \]\]' "$CUTOVER" \
  | cut -d: -f1)
cutover_clear_line=$(grep -n '^clear_health_before_restart "$evidence_dir" pre-cutover$' \
  "$CUTOVER" | cut -d: -f1)
cutover_reset_line=$(grep -n '^systemctl reset-failed "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
cutover_restart_line=$(grep -n '^systemctl restart "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
((legacy_drain_line < legacy_cursor_line \
  && legacy_cursor_line < legacy_final_health_line \
  && legacy_final_health_line < legacy_final_oss_line \
  && legacy_final_oss_line < legacy_final_runtime_line \
  && legacy_final_runtime_line < cutover_stop_line \
  && cutover_stop_line < legacy_journal_guard_line \
  && legacy_journal_guard_line < legacy_stopped_counter_line \
  && cutover_stop_line < legacy_stopped_counter_line \
  && legacy_stopped_counter_line < legacy_stopped_equality_line \
  && legacy_stopped_equality_line < cutover_clear_line \
  && cutover_clear_line < cutover_reset_line \
  && cutover_reset_line < cutover_restart_line)) || {
  printf 'cutover no longer proves restart-free legacy stop before reset/restart\n' >&2
  exit 1
}
rollback_reload_line=$(grep -n '^  systemctl daemon-reload$' "$CUTOVER" | cut -d: -f1)
rollback_reset_line=$(grep -n '^  systemctl reset-failed "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
rollback_restart_line=$(grep -n '^  systemctl restart "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
((rollback_reload_line < rollback_reset_line && rollback_reset_line < rollback_restart_line)) \
  || {
    printf 'rollback does not reset the inherited restart counter before verification\n' >&2
    exit 1
  }
rollback_state_line=$(grep -n '^  verify_saved_unit_state' "$CUTOVER" | cut -d: -f1)
rollback_final_runtime_line=$(grep -n '^  verify_fresh_legacy_runtime' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
((rollback_state_line < rollback_final_runtime_line)) || {
  printf 'rollback lacks a final runtime check after restoring unit state\n' >&2
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
grep -Fq 'legacy_restarts=$(systemctl show --property=NRestarts' "$GATE"
grep -Fq \
  'verify_legacy_identity "$legacy_pid" "$legacy_restarts" "$legacy_invocation_id"' \
  "$GATE"
grep -Fq \
  'verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" "$gate_legacy_invocation_id"' \
  "$CUTOVER"

shadow_cursor_line=$(grep -n '^shadow_stop_cursor=$(journal_cursor "$shadow_unit")' "$GATE" \
  | cut -d: -f1)
shadow_final_runtime_line=$(grep -n \
  '^verify_shadow_identity "$initial_shadow_pid" "$shadow_invocation_id"' "$GATE" \
  | tail -1 | cut -d: -f1)
shadow_stop_line=$(grep -n '^systemctl stop "$shadow_unit"$' "$GATE" | tail -1 | cut -d: -f1)
shadow_journal_guard_line=$(grep -n \
  '^verify_no_restart_after_cursor "$shadow_unit" "$shadow_stop_cursor" "$shadow_invocation_id"' \
  "$GATE" | cut -d: -f1)
shadow_stopped_counter_line=$(grep -n '^stopped_shadow_restarts=' "$GATE" | cut -d: -f1)
shadow_stopped_equality_line=$(grep -n '^\[\[ \$stopped_shadow_restarts == 0 \]\]' \
  "$GATE" | cut -d: -f1)
((shadow_cursor_line < shadow_final_runtime_line \
  && shadow_final_runtime_line < shadow_stop_line \
  && shadow_stop_line < shadow_journal_guard_line \
  && shadow_journal_guard_line < shadow_stopped_counter_line \
  && shadow_stopped_counter_line < shadow_stopped_equality_line)) || {
  printf 'shadow lacks exact invocation, journal, and restart-counter stop guards\n' >&2
  exit 1
}

cutover_sync_line=$(grep -n '^sync "$evidence_dir/cutover.json"$' "$CUTOVER" | cut -d: -f1)
cutover_rollback_sync_line=$(grep -n '^sync -f "$rollback_dir"$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_release_sync_line=$(grep -n '^sync -f "$candidate_binary"$' "$CUTOVER" \
  | cut -d: -f1)
cutover_final_runtime_line=$(grep -n \
  '^verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id"' \
  "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_final_upload_line=$(grep -n '^verify_upload_units "$pinned_upload_env"' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_final_oss_line=$(grep -n \
  'pinned OSS configuration changed before cutover completion' "$CUTOVER" \
  | cut -d: -f1)
cutover_marker_hash_line=$(grep -n '^  sha256sum cutover.json >' "$CUTOVER" | cut -d: -f1)
cutover_marker_move_line=$(grep -n '^mv -Tf "$success_marker_tmp" "$success_marker"$' \
  "$CUTOVER" | cut -d: -f1)
cutover_marker_sync_line=$(grep -n '^sync "$success_marker"$' "$CUTOVER" | cut -d: -f1)
cutover_marker_dir_sync_line=$(grep -n '^sync -f "$evidence_dir"$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_success_line=$(grep -n '^cutover_succeeded=true$' "$CUTOVER" | cut -d: -f1)
cutover_trap_off_line=$(grep -n '^trap - EXIT$' "$CUTOVER" | tail -1 | cut -d: -f1)
((cutover_sync_line < cutover_rollback_sync_line \
  && cutover_rollback_sync_line < cutover_release_sync_line \
  && cutover_release_sync_line < cutover_final_runtime_line \
  && cutover_final_runtime_line < cutover_final_upload_line \
  && cutover_final_upload_line < cutover_final_oss_line \
  && cutover_final_oss_line < cutover_marker_hash_line \
  && cutover_marker_hash_line < cutover_marker_move_line \
  && cutover_marker_move_line < cutover_marker_sync_line \
  && cutover_marker_sync_line < cutover_marker_dir_sync_line \
  && cutover_marker_dir_sync_line < cutover_success_line \
  && cutover_success_line < cutover_trap_off_line)) || {
  printf 'cutover publishes success before final verification or durable marker sync\n' >&2
  exit 1
}

snapshot_manifest_line=$(grep -n \
  '^    sha256sum state.json systemd/\* bin/\* config/\* control/\* >manifest.sha256$' \
  "$CUTOVER" | cut -d: -f1)
snapshot_sync_line=$(grep -n '^  sync -f "$rollback_dir"$' "$CUTOVER" | head -1 \
  | cut -d: -f1)
((snapshot_manifest_line < snapshot_sync_line)) || {
  printf 'rollback snapshot is not synchronized after its checksum manifest\n' >&2
  exit 1
}

rollback_pending_move_line=$(grep -n '^    mv -Tf "$marker" "$pending"' "$CUTOVER" \
  | cut -d: -f1)
rollback_pending_file_sync_line=$(grep -n '^    sync "$pending"' "$CUTOVER" \
  | cut -d: -f1)
rollback_pending_dir_sync_line=$(grep -n '^    sync -f "$evidence_dir"' "$CUTOVER" \
  | head -1 | cut -d: -f1)
((rollback_pending_move_line < rollback_pending_file_sync_line \
  && rollback_pending_file_sync_line < rollback_pending_dir_sync_line)) || {
  printf 'rollback can start before PASSED invalidation is durably synchronized\n' >&2
  exit 1
}

restore_manifest_extract_line=$(grep -n \
  "expected_manifest_sha=.*'\\.rollback_manifest_sha256'" "$CUTOVER" | cut -d: -f1)
restore_manifest_compare_line=$(grep -n \
  'actual_manifest_sha == "\$expected_manifest_sha"' "$CUTOVER" | cut -d: -f1)
restore_first_stop_line=$(grep -n '^  systemctl stop "\$REFERENCE_UPLOAD_TIMER" "\$MARKET_UPLOAD_TIMER"$' \
  "$CUTOVER" | cut -d: -f1)
((restore_manifest_extract_line < restore_manifest_compare_line \
  && restore_manifest_compare_line < restore_first_stop_line)) || {
  printf 'restore_legacy can mutate runtime before validating rollback manifest lineage\n' >&2
  exit 1
}

active_target_line=$(grep -n '^    active_target=$(readlink -f -- "\$ACTIVE_BINARY")$' "$CUTOVER" \
  | cut -d: -f1)
active_target_guard_line=$(grep -n \
  'active_target == "\$RELEASE_ROOT"/\*/polymarket-raw-ops' "$CUTOVER" | cut -d: -f1)
active_rm_line=$(grep -n '^    rm -f "\$ACTIVE_BINARY"$' "$CUTOVER" | cut -d: -f1)
active_link_line=$(grep -n '^ln -s "\$candidate_binary" "\$temporary_link"$' "$CUTOVER" \
  | cut -d: -f1)
((active_target_line < active_target_guard_line \
  && active_target_guard_line < active_rm_line \
  && active_rm_line < active_link_line)) || {
  printf 'cutover no longer proves active Rust symlink lineage before replacement\n' >&2
  exit 1
}

if grep -Eq 'rm -[^[:space:]]* .*state\.json|unlink .*state\.json' "$CUTOVER"; then
  printf 'cutover explicitly deletes rollback state JSON\n' >&2
  exit 1
fi
if grep -Eq 'rm -[^[:space:]]* .*/data/monday/spool/polymarket-reference|unlink .*/data/monday/spool/polymarket-reference' \
  "$CUTOVER"; then
  printf 'cutover explicitly deletes the production spool\n' >&2
  exit 1
fi

automatic_prepare_line=$(grep -n \
  '^    prepare_rollback_evidence "$evidence_dir" || {' "$CUTOVER" | cut -d: -f1)
automatic_restore_line=$(grep -n '^    restore_legacy "$evidence_dir" >/dev/null$' "$CUTOVER" \
  | cut -d: -f1)
automatic_finalize_line=$(grep -n \
  '^      finalize_rollback_evidence "$evidence_dir" invalid || status=1$' "$CUTOVER" \
  | cut -d: -f1)
automatic_failure_exit_line=$(grep -n '^  exit "$status"$' "$CUTOVER" | cut -d: -f1)
((automatic_prepare_line < automatic_restore_line \
  && automatic_restore_line < automatic_finalize_line \
  && automatic_finalize_line < automatic_failure_exit_line)) || {
  printf 'automatic rollback does not revoke success before restore and finalize evidence\n' >&2
  exit 1
}

manual_prepare_line=$(grep -n \
  'prepare_rollback_evidence "$rollback_evidence"' "$CUTOVER" | tail -1 | cut -d: -f1)
manual_restore_line=$(grep -n '^  restore_legacy "$rollback_evidence" >/dev/null$' "$CUTOVER" \
  | cut -d: -f1)
manual_finalize_line=$(grep -n \
  '^  finalize_rollback_evidence "$rollback_evidence" rolled-back' "$CUTOVER" \
  | cut -d: -f1)
manual_print_line=$(grep -n '^  printf '\''%s\\n'\'' "$rollback_evidence"$' "$CUTOVER" \
  | cut -d: -f1)
manual_exit_line=$(grep -n '^  exit 0$' "$CUTOVER" | head -1 | cut -d: -f1)
((manual_prepare_line < manual_restore_line \
  && manual_restore_line < manual_finalize_line \
  && manual_finalize_line < manual_print_line \
  && manual_print_line < manual_exit_line)) || {
  printf 'manual rollback does not revoke success before restore and finalize evidence\n' >&2
  exit 1
}

market_upload_line=$(grep -n '^market_upload_json=' "$GATE" | cut -d: -f1)
baseline_final_line=$(grep -n 'verify_baseline_identity' "$GATE" \
  | tail -1 | cut -d: -f1)
oss_final_line=$(grep -n 'verify_current_oss_config' "$GATE" | tail -1 | cut -d: -f1)
((baseline_final_line > market_upload_line && oss_final_line > market_upload_line)) || {
  printf 'gate does not revalidate baseline identity and OSS config after both uploads\n' >&2
  exit 1
}
grep -Fq 'cd artifact' "$WORKFLOW"
if grep -Fq 'sha256sum artifact/polymarket-raw-ops' "$WORKFLOW"; then
  printf 'workflow checksum still embeds the stripped artifact directory\n' >&2
  exit 1
fi
extract_bundle_assets() {
  sed -n '/^[[:space:]]*readonly -a BUNDLE_ASSETS=($/,/^[[:space:]]*)$/p' "$1" \
    | sed '1d;$d;s/^[[:space:]]*//'
}
workflow_assets=$(sed -n \
  '/^[[:space:]]*control_assets=($/,/^[[:space:]]*)$/p' "$WORKFLOW" \
  | sed '1d;$d;s/^[[:space:]]*//')
readme_assets=$(sed -n \
  '/^control_assets=($/,/^)/p' "$README" \
  | sed '1d;$d;s/^[[:space:]]*//')
gate_assets=$(extract_bundle_assets "$GATE")
cutover_assets=$(extract_bundle_assets "$CUTOVER")
[[ -n $workflow_assets && $workflow_assets == "$gate_assets" \
  && $workflow_assets == "$cutover_assets" \
  && $workflow_assets == "$readme_assets" ]] || {
  printf 'ACR artifact and release-control bundle asset lists differ\n' >&2
  exit 1
}
grep -Fq '@${{ steps.build.outputs.digest }}' "$WORKFLOW"
if grep -F 'IMAGE:' "$WORKFLOW" | grep -Fq ':${{ github.sha }}'; then
  printf 'workflow still extracts the collector from a mutable SHA tag\n' >&2
  exit 1
fi
grep -Fq 'monday.polymarket_raw_ops_release.v1' "$WORKFLOW"
grep -Fq '> polymarket-raw-ops-release.json' "$WORKFLOW"
grep -Fq "jq -er '.source_revision' polymarket-raw-ops-release.json" "$WORKFLOW"
grep -Fq "jq -er '.control_manifest.sha256' polymarket-raw-ops-release.json" "$WORKFLOW"
grep -Fq "jq -er '.control_archive.sha256' polymarket-raw-ops-release.json" "$WORKFLOW"
workflow_candidate_line=$(grep -n '^            candidate_sha=' "$WORKFLOW" | cut -d: -f1)
workflow_manifest_line=$(grep -n '^              > polymarket-raw-ops-release.json$' "$WORKFLOW" \
  | cut -d: -f1)
workflow_sidecar_line=$(grep -n '^              > source-revision.txt$' "$WORKFLOW" \
  | cut -d: -f1)
((workflow_candidate_line < workflow_manifest_line \
  && workflow_manifest_line < workflow_sidecar_line)) || {
  printf 'workflow publishes release sidecars before the immutable manifest exists\n' >&2
  exit 1
}
grep -Fq 'actual_candidate_sha=$(sha256sum polymarket-raw-ops' "$README"
grep -Fq 'release_manifest_sha=$(sha256sum polymarket-raw-ops-release.json' "$README"
grep -Fq 'candidate_control_dir="/opt/monday/candidates/polymarket-raw-ops/$release_manifest_sha"' \
  "$README"
grep -Fq 'actual_control_archive_sha=$(sha256sum polymarket-raw-ops-control.tar.gz' \
  "$README"
grep -Fq 'sha256sum --check --strict' "$README"
grep -Fq 'sha256sum polymarket-raw-ops-control-assets.sha256' "$README"
grep -Fq 'sha256sum -c "$artifact_dir/polymarket-raw-ops-control-assets.sha256"' "$README"
grep -Fq '"${control_assets[@]}" | LC_ALL=C sort' "$README"
grep -Fq 'tar -tzf polymarket-raw-ops-control.tar.gz | LC_ALL=C sort' "$README"
grep -Fq '"$control_dir"/polymarket-reference-{collector,upload}.service' "$README"
grep -Fq 'flock -n /run/monday/polymarket-raw-ops.lock' "$README"
grep -Fq 'sync -f "$candidate_control_parent"' "$README"
grep -Fq 'pinned_control_dir="/opt/monday/releases/polymarket-raw-ops/$candidate_sha/control"' \
  "$README"
if grep -Fq 'deployment/aliyun/polymarket-reference-collector.service' "$README"; then
  printf 'README installs the production unit from a mutable checkout\n' >&2
  exit 1
fi
if grep -Fq ':(exclude,glob)deployment/aliyun/polymarket-raw-ops-' "$CI_WORKFLOW"; then
  printf 'Rust-only CI excludes an entire migration-control script\n' >&2
  exit 1
fi
grep -Fq -- '--allow-match-regex "${legacy_runtime_reference_allowlist}"' "$CI_WORKFLOW"
grep -Fq 'deployment/aliyun/polymarket-raw-ops-cutover[.]sh:' "$CI_WORKFLOW"

grep -Fq 'candidate CLI digest differs from the verified release manifest' "$GATE"
grep -Fq 'source CLI revision differs from the verified release manifest' "$GATE"
gate_final_binding_line=$(grep -n '^  verify_release_binding "\$pinned_release_manifest"' "$GATE" \
  | tail -1 | cut -d: -f1)
gate_marker_line=$(grep -n '^  marker="\$evidence_dir/PASSED.sha256"$' "$GATE" \
  | cut -d: -f1)
gate_marker_sync_line=$(grep -n '^  sync "\$marker"$' "$GATE" | cut -d: -f1)
gate_marker_dir_sync_line=$(grep -n '^  sync -f "\$evidence_dir"$' "$GATE" \
  | tail -1 | cut -d: -f1)
((gate_final_binding_line < gate_marker_line \
  && gate_marker_line < gate_marker_sync_line \
  && gate_marker_sync_line < gate_marker_dir_sync_line)) || {
  printf 'gate publishes success before revalidating the immutable release binding\n' >&2
  exit 1
}
cutover_binding_line=$(grep -n '^verify_release_binding "\$RELEASE_MANIFEST"' "$CUTOVER" \
  | head -1 | cut -d: -f1)
cutover_transition_line=$(grep -n '^transition_started=true$' "$CUTOVER" | cut -d: -f1)
((cutover_binding_line < cutover_transition_line)) || {
  printf 'cutover mutates runtime before revalidating the immutable release binding\n' >&2
  exit 1
}
rust_baseline_verify_line=$(grep -n '^  verify_rust_runtime "\$gate_baseline_release_path"' \
  "$CUTOVER" | head -1 | cut -d: -f1)
rust_snapshot_line=$(grep -n '^snapshot_legacy "\$rollback_dir" "\$baseline_mode"' \
  "$CUTOVER" | cut -d: -f1)
((rust_baseline_verify_line < rust_snapshot_line \
  && rust_snapshot_line < cutover_transition_line)) || {
  printf 'Rust cutover snapshots or mutates before proving the gated baseline\n' >&2
  exit 1
}
candidate_link_line=$(grep -n '^ln -s "\$candidate_binary" "\$temporary_link"$' "$CUTOVER" | cut -d: -f1)
candidate_controls_line=$(grep -n '^install_control_release "\$SCRIPT_DIR"$' "$CUTOVER" | cut -d: -f1)
candidate_units_line=$(grep -n '^for asset in "\${UNIT_ASSETS\[@\]}"; do$' "$CUTOVER" | tail -1 | cut -d: -f1)
((candidate_link_line < candidate_controls_line && candidate_controls_line < candidate_units_line)) || {
  printf 'candidate release, controls, and units are not installed in the governed order\n' >&2
  exit 1
}
grep -Fq '.control_modes[$asset]=$mode' "$CUTOVER"
grep -Fq '.active_symlink={target:$path,sha256:$sha}' "$CUTOVER"
grep -Fq 'verify_control_release "$CONTROL_DIR" "$rollback_sha" "$active_target"' "$CUTOVER"
cutover_final_binding_line=$(grep -n '^verify_release_binding "\$RELEASE_MANIFEST"' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_success_marker_line=$(grep -n '^success_marker="\$evidence_dir/PASSED.sha256"$' \
  "$CUTOVER" | cut -d: -f1)
((cutover_final_binding_line < cutover_success_marker_line)) || {
  printf 'cutover publishes success without final immutable release revalidation\n' >&2
  exit 1
}

gate_upload_env_chain_line=$(grep -n '^  "\$RELEASE_ROOT" /etc/monday ' "$GATE" \
  | head -1 | cut -d: -f1)
gate_upload_env_read_line=$(grep -n '^secure_control_file "\$UPLOAD_ENV"$' "$GATE" \
  | cut -d: -f1)
((gate_upload_env_chain_line < gate_upload_env_read_line)) || {
  printf 'gate reads the upload environment before validating its trusted parent chain\n' >&2
  exit 1
}

printf 'Polymarket raw-ops control-plane tests passed\n'
