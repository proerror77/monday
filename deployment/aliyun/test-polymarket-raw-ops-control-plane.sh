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

# Exercise the installed immutable release manifest parser. The manifest is the
# sole binding for source, candidate, control-manifest, and control-archive IDs.
(
release_manifest_verifier="$tmp_dir/verify-release-manifest.sh"
sed -n \
  -e '/^verify_release_manifest() {$/,/^}$/p' \
  -e '/^verify_release_binding() {$/,/^}$/p' "$GATE" \
  >"$release_manifest_verifier"
readonly RELEASE_MANIFEST_SCHEMA=monday.polymarket_raw_ops_release.v1
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
candidate_sha=$(sha256sum "$candidate_file" | awk '{print $1}')
source_revision=$(printf 'b%.0s' {1..40})
bundle_fixture_sha=$(printf 'c%.0s' {1..64})
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
bundle_fixture_sha=$(printf 'c%.0s' {1..64})
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
    deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,
    release_manifest_sha256:$release_manifest_sha,
    control_archive_sha256:$control_archive_sha,
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
      invocation_id:$shadow_invocation_id
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
  cycle_started_at:"2026-07-15T00:00:00Z",cycle_duration_ms:1000,
  target_markets:14,missing_target_symbols:[],api_errors:[],malformed_trade_rows:0,
  trade_poll_budget:112,trade_poll_concurrency:4,trade_request_spacing_ms:100,
  eligible_trade_markets:14,priority_trade_markets:8,
  selected_trade_markets:14,deferred_trade_markets:0,priority_trade_backlog:0,
  trade_polls:14,successful_trade_polls:14,
  truncated_trade_markets:[],non_object_trade_markets:[],invalid_settlement_markets:[],
  invalid_end_time_markets:[],stale_trade_markets:[],stale_settlement_markets:[],
  overdue_unresolved_markets:[]
}' >"$tmp_dir/rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/rust-health.json" >/dev/null
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
  'del(.trade_request_spacing_ms)' \
  '.trade_request_spacing_ms = 99' \
  '.priority_trade_backlog = 1' \
  '.selected_trade_markets = 13' \
  '.deferred_trade_markets = 1' \
  '.eligible_trade_markets = 13' \
  '.successful_trade_polls = 0' \
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
grep -Fq 'readonly MAX_ACCEPTED_CYCLE_SECONDS=180' "$GATE"
grep -Fq 'readonly INITIAL_HEALTH_GRACE_SECONDS=60' "$GATE"
grep -Fq 'readonly HEALTH_SETTLE_SECONDS=$((MAX_ACCEPTED_CYCLE_SECONDS + INITIAL_HEALTH_GRACE_SECONDS))' "$GATE"
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
grep -Fxq 'MemoryHigh=512M' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryMax=768M' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryHigh=512M' "$SCRIPT_DIR/polymarket-reference-collector.service"
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
grep -Fq 'actual_control_archive_sha=$(sha256sum polymarket-raw-ops-control.tar.gz' \
  "$README"
grep -Fq 'sha256sum --check --strict' "$README"
grep -Fq 'sha256sum polymarket-raw-ops-control-assets.sha256' "$README"
grep -Fq 'sha256sum -c "$artifact_dir/polymarket-raw-ops-control-assets.sha256"' "$README"
grep -Fq '"${control_assets[@]}" | LC_ALL=C sort' "$README"
grep -Fq 'tar -tzf polymarket-raw-ops-control.tar.gz | LC_ALL=C sort' "$README"
grep -Fq '"$control_dir"/polymarket-reference-{collector,upload}.service' "$README"
grep -Fq 'flock -n /run/monday/polymarket-raw-ops.lock' "$README"
grep -Fq 'sync -f /opt/monday/control/polymarket-raw-ops' "$README"
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
gate_final_binding_line=$(grep -n '^  verify_release_binding "\$RELEASE_MANIFEST"' "$GATE" \
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
