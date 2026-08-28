#!/usr/bin/env bash

# Pure identity helpers shared by the five Control Plane V2 operations.
# Host scripts may add runtime checks, but no helper writes state.

monday_sha256_file() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{print $1}'
  else
    shasum -a 256 -- "$1" | awk '{print $1}'
  fi
}

monday_sha256_text() {
  [[ $# -eq 1 ]] || return 2
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | awk '{print $1}'
  else
    printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
  fi
}

monday_sha256_ok() {
  [[ $# -eq 1 && $1 =~ ^[a-f0-9]{64}$ ]]
}

monday_root_join() {
  [[ $# -eq 2 ]] || return 2
  local root=${1:-/} suffix=${2#/}
  root=${root%/}
  [[ -n $root ]] || root=/
  if [[ $root == / ]]; then
    printf '/%s\n' "$suffix"
  else
    printf '%s/%s\n' "$root" "$suffix"
  fi
}

monday_iso_epoch() {
  [[ $# -eq 1 ]] || return 2
  local value=$1 normalized tz
  [[ $value =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$ ]] || return 1
  if date -u -d "$value" +%s >/dev/null 2>&1; then
    date -u -d "$value" +%s
    return
  fi
  [[ $value =~ ^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$ ]] || return 1
  normalized=${BASH_REMATCH[1]}
  tz=${BASH_REMATCH[3]}
  [[ $tz == Z ]] && tz=+0000 || tz=${tz/:/}
  normalized+="$tz"
  date -u -j -f '%Y-%m-%dT%H:%M:%S%z' "$normalized" +%s
}

# Parse an RFC3339 timestamp without discarding its fractional component.  All
# control-plane receipts compare nanoseconds so a pre-cutover value in the
# same wall-clock second cannot pass a gate by rounding to epoch seconds.
monday_iso_epoch_ns() {
  [[ $# -eq 1 ]] || return 2
  local value=$1 seconds fraction normalized
  [[ $value =~ ^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(\.([0-9]{1,9}))?(Z|[+-][0-9]{2}:[0-9]{2})$ ]] || return 1
  seconds=$(monday_iso_epoch "$value") || return 1
  fraction=${BASH_REMATCH[3]:-}
  while ((${#fraction} < 9)); do fraction="${fraction}0"; done
  (( ${#fraction} == 9 )) || return 1
  [[ $seconds =~ ^-?[0-9]+$ && $fraction =~ ^[0-9]{9}$ ]] || return 1
  printf '%s\n' "$((seconds * 1000000000 + 10#$fraction))"
}

monday_epoch_ns_rfc3339() {
  [[ $# -eq 1 && $1 =~ ^[0-9]+$ ]] || return 2
  local value=$1 seconds fraction date_value
  seconds=$((value / 1000000000)); fraction=$((value % 1000000000))
  if date_value=$(date -u -d "@$seconds" +%Y-%m-%dT%H:%M:%S 2>/dev/null); then
    :
  else
    date_value=$(date -u -r "$seconds" +%Y-%m-%dT%H:%M:%S) || return 1
  fi
  printf '%s.%09dZ\n' "$date_value" "$fraction"
}

monday_validate_component() {
  [[ $# -eq 1 && $1 =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*$ && $1 != . && $1 != .. && $1 != *%* && $1 != *\\* ]]
}

monday_validate_oss_prefix() {
  [[ $# -eq 3 ]] || return 2
  local market=$1 dataset=$2 prefix=$3
  monday_validate_component "$dataset" || return 1
  [[ $market == spot || $market == usdm ]] || return 1
  [[ $prefix == "lake/raw/venue=binance/market=$market/dataset=$dataset/shard=all" ]]
}

monday_path_direct_or_absent() {
  [[ $# -eq 1 ]] || return 2
  [[ ! -e $1 && ! -L $1 ]] || monday_path_direct "$1"
}

# Host operations have exactly two modes.  Production is rooted at / and may
# not inherit a test root, timeout, fixture fault, or evidence redirect from a
# caller's shell.  Offline fixtures must opt in with a non-root root and an
# explicit sentinel; this prevents a copied test command from ever reaching
# the real host paths.
monday_control_plane_validate_mode() {
  [[ $# -eq 2 ]] || return 2
  local root=${1%/} test_only=$2 name
  [[ -n $root ]] || root=/
  if [[ $test_only == true ]]; then
    [[ $root != / ]] || return 1
    [[ ${MONDAY_CONTROL_PLANE_FIXTURE_SENTINEL:-} == monday-v2-fixture \
      || -f "$root/.monday-control-plane-fixture" ]] || return 1
    return 0
  fi
  [[ $root == / ]] || return 1
  # MONDAY_* is intentionally absent from the production environment.  The
  # local operator sends an env -i command, and a direct invocation with any
  # redirect/test knob is refused instead of silently changing the state root.
  while IFS= read -r name; do
    [[ -n $name ]] || continue
    return 1
  done < <(compgen -v MONDAY_)
}

monday_path_direct() {
  [[ $# -eq 1 ]] || return 2
  local path=$1 resolved
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

monday_file_direct() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]]
}

monday_file_uid() {
  [[ $# -eq 1 ]] || return 2
  stat -c %u -- "$1" 2>/dev/null || stat -f %u -- "$1"
}

monday_file_mode() {
  [[ $# -eq 1 ]] || return 2
  stat -c %a -- "$1" 2>/dev/null || stat -f %Lp -- "$1"
}

monday_runtime_assets() {
  printf '%s\n' \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-rust@.service \
    binance-lob-archiver-upload@.service \
    binance-lob-archiver-rust-upload@.service \
    binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env \
    binance-lob-archiver-rust-spot.env \
    binance-lob-archiver-rust-usdm.env
}

monday_controller_assets() {
  printf '%s\n' \
    binance-lob-archiver-recovery@.service \
    binance-lob-archiver-recovery@.timer \
    host-rust-lob-recovery-queue.sh \
    host-rust-lob-readback.sh \
    host-rust-lob-shadow-gate.sh \
    host-rust-lob-cutover.sh \
    host-rust-lob-restore.sh \
    host-rust-lob-controller-release.sh \
    monday-collector-health.sh \
    rust-lob-control-plane-lib.sh \
    rust-lob-runtime-health-policy.jq \
    rust-lob-shadow-gate-policy.jq
}

# These are the controller-owned programs that systemd invokes through a
# fixed /opt/monday/bin path.  They are stable projections of the active
# controller, not a second mutable controller state.
monday_controller_projection_assets() {
  printf '%s\n' \
    host-rust-lob-recovery-queue.sh \
    monday-collector-health.sh
}

monday_controller_projection_target() {
  [[ $# -eq 2 ]] || return 2
  local root=$1 asset=$2
  case "$asset" in
    host-rust-lob-recovery-queue.sh)
      monday_root_join "$root" opt/monday/bin/monday-rust-lob-recovery-queue ;;
    monday-collector-health.sh)
      monday_root_join "$root" opt/monday/bin/monday-collector-health.sh ;;
    *) return 1 ;;
  esac
}

# The only canonical writer names that may touch the Binance LOB spools.  The
# first four are pre-V2 units kept solely as rollback evidence; V2 never
# resumes them.  The production pair is the only long-running writer that a
# successful transition may start.  Upload units are oneshot writers and are
# still included so a stale/manual invocation cannot race a transition.
monday_rust_lob_legacy_writer_units() {
  printf '%s\n' \
    binance-lob-archiver@spot.service \
    binance-lob-archiver@usdm.service \
    binance-lob-archiver-upload@spot.service \
    binance-lob-archiver-upload@usdm.service
}

monday_rust_lob_production_writer_units() {
  printf '%s\n' \
    binance-lob-archiver-production@spot.service \
    binance-lob-archiver-production@usdm.service
}

monday_rust_lob_shadow_writer_units() {
  printf '%s\n' \
    binance-lob-archiver-rust@spot.service \
    binance-lob-archiver-rust@usdm.service \
    binance-lob-archiver-rust-upload@spot.service \
    binance-lob-archiver-rust-upload@usdm.service
}

monday_rust_lob_recovery_service_units() {
  printf '%s\n' \
    binance-lob-archiver-recovery@spot.service \
    binance-lob-archiver-recovery@usdm.service
}

monday_rust_lob_recovery_timer_units() {
  printf '%s\n' \
    binance-lob-archiver-recovery@spot.timer \
    binance-lob-archiver-recovery@usdm.timer
}

monday_rust_lob_recovery_scheduler_units() {
  monday_rust_lob_recovery_service_units
  monday_rust_lob_recovery_timer_units
}

monday_rust_lob_contain_recovery_schedulers() {
  local unit failed=0
  while IFS= read -r unit; do
    systemctl stop "$unit" >/dev/null 2>&1 || failed=1
    systemctl disable "$unit" >/dev/null 2>&1 || failed=1
    systemctl mask --runtime "$unit" >/dev/null 2>&1 || failed=1
  done < <(monday_rust_lob_recovery_scheduler_units)
  return "$failed"
}

monday_rust_lob_verify_recovery_schedulers_contained() {
  local unit load active enabled
  while IFS= read -r unit; do
    IFS=$'\t' read -r load active enabled \
      < <(monday_rust_lob_writer_state "$unit") || return 1
    [[ $active != active && $active != activating && $active != deactivating ]] || return 1
    [[ $enabled == masked || $enabled == masked-runtime || $enabled == masked-runtime* ]] || return 1
  done < <(monday_rust_lob_recovery_scheduler_units)
}

monday_rust_lob_enable_recovery_schedulers() {
  local unit failed=0
  while IFS= read -r unit; do
    systemctl unmask "$unit" >/dev/null 2>&1 || failed=1
  done < <(monday_rust_lob_recovery_scheduler_units)
  while IFS= read -r unit; do
    systemctl enable "$unit" >/dev/null 2>&1 || failed=1
  done < <(monday_rust_lob_recovery_timer_units)
  while IFS= read -r unit; do
    systemctl start "$unit" >/dev/null 2>&1 || failed=1
  done < <(monday_rust_lob_recovery_timer_units)
  monday_rust_lob_verify_recovery_schedulers_active || failed=1
  return "$failed"
}

monday_rust_lob_verify_recovery_schedulers_active() {
  local unit
  while IFS= read -r unit; do
    systemctl is-enabled --quiet "$unit" >/dev/null 2>&1 || return 1
    systemctl is-active --quiet "$unit" >/dev/null 2>&1 || return 1
  done < <(monday_rust_lob_recovery_timer_units)
}

monday_rust_lob_all_writer_units() {
  monday_rust_lob_legacy_writer_units
  monday_rust_lob_production_writer_units
  monday_rust_lob_shadow_writer_units
}

# Snapshot the exact systemd state needed for a direct bootstrap rollback.
# The output is tab-separated and intentionally in-memory at each caller; no
# mutable host state is used as a rollback source after the active rename.
# Fields: unit, load-state, active-state, unit-file-state.
monday_rust_lob_writer_state_snapshot() {
  local unit load active enabled
  while IFS= read -r unit; do
    load=$(systemctl show "$unit" --property=LoadState --value 2>/dev/null || true)
    [[ -n $load ]] || load=not-found
    active=$(systemctl show "$unit" --property=ActiveState --value 2>/dev/null || true)
    [[ -n $active ]] || active=inactive
    enabled=$(systemctl show "$unit" --property=UnitFileState --value 2>/dev/null || true)
    if [[ -z $enabled ]]; then
      enabled=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    fi
    [[ -n $enabled ]] || enabled=disabled
    printf '%s\t%s\t%s\t%s\n' "$unit" "$load" "$active" "$enabled"
  done < <(monday_rust_lob_all_writer_units)
}

# Stop, disable, and runtime-mask an allowlisted writer stream.  Missing
# legacy units are harmless (the runtime mask still prevents a late manual
# start), while failures for known units are returned to the caller.
monday_rust_lob_contain_writer_list() {
  [[ $# -eq 1 ]] || return 2
  local list_command=$1 unit failed=0 known
  while IFS= read -r unit; do
    known=false
    if systemctl show "$unit" --property=LoadState --value >/dev/null 2>&1; then
      known=true
    fi
    if [[ $known == true ]]; then
      systemctl stop "$unit" >/dev/null 2>&1 || failed=1
      systemctl disable "$unit" >/dev/null 2>&1 || failed=1
    fi
    systemctl mask --runtime "$unit" >/dev/null 2>&1 || failed=1
  done < <("$list_command")
  return "$failed"
}

monday_rust_lob_contain_writers() {
  monday_rust_lob_contain_writer_list monday_rust_lob_all_writer_units
}

monday_rust_lob_contain_legacy_writers() {
  monday_rust_lob_contain_writer_list monday_rust_lob_legacy_writer_units
}

monday_rust_lob_verify_contained() {
  local unit load active enabled
  while IFS= read -r unit; do
    IFS=$'\t' read -r load active enabled \
      < <(monday_rust_lob_writer_state "$unit") || return 1
    [[ $active != active && $active != activating && $active != deactivating ]] || return 1
    [[ $enabled == masked || $enabled == masked-runtime || $enabled == masked-runtime* ]] || return 1
  done < <(monday_rust_lob_all_writer_units)
}

monday_rust_lob_verify_legacy_contained() {
  local unit load active enabled
  while IFS= read -r unit; do
    IFS=$'\t' read -r load active enabled \
      < <(monday_rust_lob_writer_state "$unit") || return 1
    [[ $active != active && $active != activating && $active != deactivating ]] || return 1
    [[ $enabled == masked || $enabled == masked-runtime || $enabled == masked-runtime* ]] || return 1
  done < <(monday_rust_lob_legacy_writer_units)
}

monday_rust_lob_writer_state() {
  [[ $# -eq 1 ]] || return 2
  local unit=$1 load active enabled
  load=$(systemctl show "$unit" --property=LoadState --value 2>/dev/null || true)
  [[ -n $load ]] || load=not-found
  active=$(systemctl show "$unit" --property=ActiveState --value 2>/dev/null || true)
  [[ -n $active ]] || active=inactive
  enabled=$(systemctl show "$unit" --property=UnitFileState --value 2>/dev/null || true)
  if [[ -z $enabled ]]; then enabled=$(systemctl is-enabled "$unit" 2>/dev/null || true); fi
  [[ -n $enabled ]] || enabled=disabled
  printf '%s\t%s\t%s\n' "$load" "$active" "$enabled"
}

monday_rust_lob_verify_writer_state() {
  [[ $# -eq 4 ]] || return 2
  local unit=$1 expected_load=$2 expected_active=$3 expected_enabled=$4
  local observed_load observed_active observed_enabled
  IFS=$'\t' read -r observed_load observed_active observed_enabled \
    < <(monday_rust_lob_writer_state "$unit") || return 1
  [[ $observed_load == "$expected_load" \
    && $observed_active == "$expected_active" \
    && $observed_enabled == "$expected_enabled" ]]
}

# Restore a snapshot captured before direct bootstrap.  Passing `legacy` is
# allowed only for the direct migration; stable V2 rollback/restore must keep
# all legacy writers masked.  A failed restoration is deliberately reported so
# the caller can contain the complete allowlist instead of guessing a state.
monday_rust_lob_restore_writer_snapshot() {
  [[ $# -eq 2 ]] || return 2
  local snapshot=$1 restore_legacy=$2
  local unit load active enabled
  local failed=0
  while IFS=$'\t' read -r unit load active enabled; do
    [[ -n $unit ]] || continue
    if [[ $restore_legacy != legacy ]] &&
      monday_rust_lob_legacy_writer_units | grep -Fqx "$unit"; then
      continue
    fi
    # A not-found unit has no enable state to restore; remove the temporary
    # mask and leave it absent.  This also handles hosts without old units.
    if [[ $load == not-found ]]; then
      systemctl unmask "$unit" >/dev/null 2>&1 || failed=1
      continue
    fi
    systemctl stop "$unit" >/dev/null 2>&1 || failed=1
    case "$enabled" in
      masked|masked-runtime|masked-runtime*)
        systemctl mask --runtime "$unit" >/dev/null 2>&1 || failed=1 ;;
      enabled|enabled-runtime|enabled-presets|indirect|generated|linked|linked-runtime)
        systemctl unmask "$unit" >/dev/null 2>&1 || failed=1
        systemctl enable "$unit" >/dev/null 2>&1 || failed=1 ;;
      *)
        systemctl unmask "$unit" >/dev/null 2>&1 || failed=1
        systemctl disable "$unit" >/dev/null 2>&1 || failed=1 ;;
    esac
    if [[ $active == active || $active == activating ]]; then
      systemctl start "$unit" >/dev/null 2>&1 || failed=1
    else
      systemctl stop "$unit" >/dev/null 2>&1 || failed=1
    fi
    monday_rust_lob_verify_writer_state "$unit" "$load" "$active" "$enabled" || failed=1
  done <"$snapshot"
  return "$failed"
}

# Verify the fixed entrypoints used by systemd resolve to one exact active
# ControllerRelease.  This is read-only and deliberately accepts no regular
# file fallback once V2 has been bootstrapped.
monday_verify_controller_projections() {
  [[ $# -eq 2 ]] || return 2
  local root=$1 sha=$2 controller_root active release asset target expected resolved
  monday_sha256_ok "$sha" || return 1
  controller_root=$(monday_root_join "$root" opt/monday/releases/binance-lob-controller) || return 1
  active="$controller_root/active"; release="$controller_root/$sha"
  [[ -L $active && $(readlink -f -- "$active") == "$release" ]] || return 1
  while IFS= read -r asset; do
    target=$(monday_controller_projection_target "$root" "$asset") || return 1
    expected="$active/deployment/$asset"
    [[ -L $target && $(readlink -- "$target") == "$expected" ]] || return 1
    resolved=$(readlink -f -- "$target") || return 1
    monday_file_direct "$resolved" || return 1
    cmp -s "$release/deployment/$asset" "$resolved" || return 1
  done < <(monday_controller_projection_assets)
}

# Read-only verifier for the controller identity left by the pre-V2 apply
# path.  No v1 control bytes are executed; only the manifest, payload and
# runtime identity are used to establish an exact bootstrap rollback anchor.
monday_verify_legacy_controller_release() {
  [[ $# -eq 3 ]] || return 2
  local root=$1 sha=$2 production=$3 controller_root release manifest artifact runtime
  monday_sha256_ok "$sha" || return 1
  controller_root=$(monday_root_join "$root" opt/monday/releases/binance-lob-controller) || return 1
  release="$controller_root/$sha"; manifest="$release/release.json"
  monday_path_direct "$controller_root" || return 1
  monday_path_direct "$release" || return 1
  monday_path_direct "$release/deployment" || return 1
  monday_file_direct "$manifest" || return 1
  [[ $(monday_sha256_file "$manifest") == "$sha" ]] || return 1
  [[ -f "$release/release.json.sha256" && ! -L "$release/release.json.sha256" ]] || return 1
  (cd "$release" && sha256sum --check --strict release.json.sha256 >/dev/null) || return 1
  local legacy_schema
  legacy_schema=$(printf '%s%s' 'monday.rust_lob_controller_release.' 'v1')
  jq -e --arg schema "$legacy_schema" \
    '.schema == $schema
     and (.artifact_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
     and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
    "$manifest" >/dev/null 2>&1 || return 1
  artifact=$(jq -er '.artifact_sha256' "$manifest") || return 1
  runtime=$(jq -er '.runtime_contract_sha256' "$manifest") || return 1
  monday_sha256_ok "$runtime" || return 1
  local binary
  binary=$(monday_root_join "$root" "opt/monday/releases/binance-lob-archiver/$artifact/binance-lob-archiver") || return 1
  monday_file_direct "$binary" || return 1
  [[ $(monday_sha256_file "$binary") == "$artifact" ]] || return 1
  [[ -L $production && $(readlink -f -- "$production") == "$binary" ]] || return 1
  printf '%s %s %s\n' "$artifact" "$runtime" "$release"
}

monday_runtime_asset_target() {
  [[ $# -eq 2 ]] || return 2
  local root=$1 asset=$2
  case "$asset" in
    binance-lob-archiver-production@.service|binance-lob-archiver-rust@.service|\
    binance-lob-archiver-upload@.service|binance-lob-archiver-rust-upload@.service)
      monday_root_join "$root" "etc/systemd/system/$asset" ;;
    binance-lob-archiver-production-spot.env|binance-lob-archiver-production-usdm.env|\
    binance-lob-archiver-rust-spot.env|binance-lob-archiver-rust-usdm.env)
      monday_root_join "$root" "etc/monday/$asset" ;;
    *) return 1 ;;
  esac
}

monday_rust_lob_runtime_contract_sha256() {
  [[ $# -eq 1 ]] || return 2
  local directory=${1%/} asset digest
  local -a assets=()
  mapfile -t assets < <(monday_runtime_assets)
  for asset in "${assets[@]}"; do
    monday_file_direct "$directory/$asset" || return 1
  done
  {
    for asset in "${assets[@]}"; do
      digest=$(monday_sha256_file "$directory/$asset") || return 1
      printf '%s  %s\n' "$digest" "$asset"
    done
  } | if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

monday_rust_lob_live_runtime_contract_sha256() {
  [[ $# -eq 1 ]] || return 2
  local root=$1 scratch asset target resolved digest
  scratch=$(mktemp -d) || return 1
  while IFS= read -r asset; do
    target=$(monday_runtime_asset_target "$root" "$asset") || {
      rm -rf -- "$scratch"
      return 1
    }
    [[ -f $target ]] || {
      rm -rf -- "$scratch"
      return 1
    }
    resolved=$(readlink -f -- "$target") || {
      rm -rf -- "$scratch"
      return 1
    }
    [[ -f $resolved && ! -L $resolved ]] || {
      rm -rf -- "$scratch"
      return 1
    }
    cp -p -- "$resolved" "$scratch/$asset" || {
      rm -rf -- "$scratch"
      return 1
    }
  done < <(monday_runtime_assets)
  digest=$(monday_rust_lob_runtime_contract_sha256 "$scratch") || {
    rm -rf -- "$scratch"
    return 1
  }
  rm -rf -- "$scratch"
  printf '%s\n' "$digest"
}

# Strict, read-only verifier for the production runtime that a controller
# release would install.  Gate and Cutover both call this function so the
# bytes checked before authorization are the same bytes checked at commit.
# The unit templates intentionally keep one fixed production topology; adding
# another runtime mode would require a new reviewed contract, not a fallback.
monday_unit_exact_line() {
  [[ $# -eq 3 ]] || return 2
  local file=$1 key=$2 value=$3
  [[ $(grep -Fxc "$key=$value" "$file" || true) -eq 1 ]]
}

# Parse a systemd unit into a small, deterministic semantic form.  The
# production verifier below uses this allowlist before checking individual
# trust-critical values.  This matters because checking only expected lines
# would still accept an attacker-supplied ExecStartPost, extra EnvironmentFile,
# or a duplicate directive that changes systemd's effective configuration.
# Comments and blank lines are not semantics; section/directive order is kept
# because systemd treats repeated command directives as ordered operations.
monday_unit_normalized() {
  [[ $# -eq 2 ]] || return 2
  local file=$1 kind=$2 raw line section='' key value identity normalized=''
  local -A seen=() sections=()
  local -a expected=() required_sections=()
  case "$kind" in
    production)
      required_sections=(Unit Service Install)
      expected=(
        'Unit|Description|Rust Binance LOB archiver production (%i)'
        'Unit|After|network-online.target'
        'Unit|Wants|network-online.target'
        'Unit|RequiresMountsFor|/data'
        'Unit|AssertPathIsMountPoint|/data'
        'Unit|StartLimitIntervalSec|7200'
        'Unit|StartLimitBurst|5'
        'Service|Type|simple'
        'Service|User|hftcollector'
        'Service|Group|hftcollector'
        'Service|Environment|RUST_LOG=info'
        'Service|Environment|HOME=/var/lib/hft-collector'
        'Service|EnvironmentFile|/etc/monday/binance-lob-archiver-production-%i.env'
        'Service|ExecStartPre|/opt/monday/bin/binance-lob-archiver --self-test'
        'Service|ExecStartPre|+/opt/monday/bin/monday-rust-lob-recovery-queue isolate %i'
        'Service|ExecStart|/opt/monday/bin/binance-lob-archiver'
        'Service|Restart|always'
        'Service|RestartSec|5'
        'Service|RuntimeMaxSec|21600'
        'Service|KillMode|mixed'
        'Service|TimeoutStartSec|120'
        'Service|TimeoutStopSec|600'
        'Service|NoNewPrivileges|true'
        'Service|PrivateTmp|true'
        'Service|ProtectSystem|strict'
        'Service|ProtectHome|true'
        'Service|ProtectKernelTunables|true'
        'Service|ProtectKernelModules|true'
        'Service|ProtectControlGroups|true'
        'Service|LockPersonality|true'
        'Service|RestrictSUIDSGID|true'
        'Service|StateDirectory|hft-collector'
        'Service|ReadWritePaths|/data/monday/spool/binance-lob -/data/monday/spool/binance-lob-recovery'
        'Service|CPUQuota|80%'
        'Service|MemoryHigh|2048M'
        'Service|MemoryMax|2560M'
        'Install|WantedBy|multi-user.target'
      ) ;;
    upload)
      required_sections=(Unit Service)
      expected=(
        'Unit|Description|Rust Binance LOB archiver production pending-upload drain (%i)'
        'Unit|After|network-online.target'
        'Unit|Wants|network-online.target'
        'Unit|RequiresMountsFor|/data'
        'Unit|AssertPathIsMountPoint|/data'
        'Service|Type|oneshot'
        'Service|User|hftcollector'
        'Service|Group|hftcollector'
        'Service|Environment|RUST_LOG=info'
        'Service|Environment|HOME=/var/lib/hft-collector'
        'Service|EnvironmentFile|/etc/monday/binance-lob-archiver-production-%i.env'
        'Service|ExecStart|/opt/monday/bin/binance-lob-archiver --upload-only'
        'Service|TimeoutStartSec|0'
        'Service|NoNewPrivileges|true'
        'Service|PrivateTmp|true'
        'Service|ProtectSystem|strict'
        'Service|ProtectHome|true'
        'Service|ProtectKernelTunables|true'
        'Service|ProtectKernelModules|true'
        'Service|ProtectControlGroups|true'
        'Service|LockPersonality|true'
        'Service|RestrictSUIDSGID|true'
        'Service|StateDirectory|hft-collector'
        'Service|ReadWritePaths|/data/monday/spool/binance-lob'
        'Service|CPUQuota|80%'
        'Service|MemoryHigh|384M'
        'Service|MemoryMax|512M'
      ) ;;
    shadow)
      required_sections=(Unit Service Install)
      expected=(
        'Unit|Description|Rust Binance LOB archiver shadow (%i)'
        'Unit|After|network-online.target'
        'Unit|Wants|network-online.target'
        'Unit|RequiresMountsFor|/data'
        'Unit|AssertPathIsMountPoint|/data'
        'Service|Type|simple'
        'Service|User|hftcollector'
        'Service|Group|hftcollector'
        'Service|Environment|RUST_LOG=info'
        'Service|Environment|HOME=/var/lib/hft-collector'
        'Service|EnvironmentFile|/etc/monday/binance-lob-archiver-rust-%i.env'
        'Service|EnvironmentFile|-/run/monday/binance-lob-archiver-rust-%i-soak.env'
        'Service|ExecStartPre|/opt/monday/bin/binance-lob-archiver-shadow --self-test'
        'Service|ExecStart|/opt/monday/bin/binance-lob-archiver-shadow'
        'Service|Restart|always'
        'Service|RestartSec|5'
        'Service|RuntimeMaxSec|21600'
        'Service|KillMode|mixed'
        'Service|TimeoutStopSec|600'
        'Service|NoNewPrivileges|true'
        'Service|PrivateTmp|true'
        'Service|ProtectSystem|strict'
        'Service|ProtectHome|true'
        'Service|ProtectKernelTunables|true'
        'Service|ProtectKernelModules|true'
        'Service|ProtectControlGroups|true'
        'Service|LockPersonality|true'
        'Service|RestrictSUIDSGID|true'
        'Service|StateDirectory|hft-collector'
        'Service|ReadWritePaths|/data/monday/spool/binance-lob-rust-shadow'
        'Service|CPUQuota|80%'
        'Service|OOMScoreAdjust|500'
        'Service|MemoryHigh|1792M'
        'Service|MemoryMax|2048M'
        'Install|WantedBy|multi-user.target'
      ) ;;
    shadow_upload|shadow_upload_run)
      required_sections=(Unit Service)
      expected=(
        'Unit|Description|Rust Binance LOB archiver shadow pending-upload drain (%i)'
        'Unit|After|network-online.target'
        'Unit|Wants|network-online.target'
        'Unit|RequiresMountsFor|/data'
        'Unit|AssertPathIsMountPoint|/data'
        'Service|Type|oneshot'
        'Service|User|hftcollector'
        'Service|Group|hftcollector'
        'Service|Environment|RUST_LOG=info'
        'Service|Environment|HOME=/var/lib/hft-collector'
        'Service|EnvironmentFile|/etc/monday/binance-lob-archiver-rust-%i.env'
        'Service|ExecStart|/opt/monday/bin/binance-lob-archiver-shadow --upload-only'
        'Service|TimeoutStartSec|0'
        'Service|NoNewPrivileges|true'
        'Service|PrivateTmp|true'
        'Service|ProtectSystem|strict'
        'Service|ProtectHome|true'
        'Service|ProtectKernelTunables|true'
        'Service|ProtectKernelModules|true'
        'Service|ProtectControlGroups|true'
        'Service|LockPersonality|true'
        'Service|RestrictSUIDSGID|true'
        'Service|StateDirectory|hft-collector'
        'Service|ReadWritePaths|/data/monday/spool/binance-lob-rust-shadow'
        'Service|CPUQuota|80%'
        'Service|MemoryHigh|384M'
        'Service|MemoryMax|512M'
      )
      if [[ $kind == shadow_upload_run ]]; then
        expected+=(
          'Service|Restart|no'
          'Service|RuntimeMaxSec|1800'
        )
      fi ;;
    *) return 2 ;;
  esac
  monday_file_direct "$file" || return 1
  while IFS= read -r raw || [[ -n $raw ]]; do
    line=${raw%%#*}
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    [[ -n $line ]] || continue
    if [[ $line =~ ^\[([A-Za-z]+)\]$ ]]; then
      section=${BASH_REMATCH[1]}
      [[ ${sections[$section]:-0} == 0 ]] || return 1
      case " ${required_sections[*]} " in *" $section "*) ;; *) return 1 ;; esac
      sections[$section]=1; continue
    fi
    [[ -n $section && $line =~ ^([A-Za-z][A-Za-z0-9]*)=(.*)$ ]] || return 1
    key=${BASH_REMATCH[1]}; value=${BASH_REMATCH[2]}
    identity="$section|$key"
    # Environment carries its own key; command directives are distinct only
    # when their full command differs.  All other directives are singleton.
    [[ $key == Environment ]] && identity+="|${value%%=*}"
    [[ $key == EnvironmentFile ]] && identity+="|$value"
    [[ $key == ExecStartPre ]] && identity+="|$value"
    [[ ${seen[$identity]:-0} == 0 ]] || return 1
    seen[$identity]=1
    local semantic="$section|$key|$value" expected_line found=false
    for expected_line in "${expected[@]}"; do
      [[ $semantic == "$expected_line" ]] && found=true
    done
    [[ $found == true ]] || return 1
    normalized+="$semantic"$'\n'
  done <"$file"
  for section in "${required_sections[@]}"; do
    [[ ${sections[$section]:-0} == 1 ]] || return 1
  done
  # Every allowlisted semantic must be present exactly once (apart from the
  # two intentionally distinct ExecStartPre commands and Environment keys,
  # which are already keyed by their value/name above).
  for expected_line in "${expected[@]}"; do
    local count=0
    while IFS= read -r line; do [[ $line == "$expected_line" ]] && count=$((count + 1)); done <<<"$normalized"
    (( count == 1 )) || return 1
  done
  printf '%s' "$normalized"
}

monday_validate_unit_allowlist() {
  [[ $# -eq 2 ]] || return 2
  monday_unit_normalized "$1" "$2" >/dev/null
}

monday_unit_semantics_sha256() {
  [[ $# -eq 2 ]] || return 2
  local normalized
  normalized=$(monday_unit_normalized "$1" "$2") || return 1
  monday_sha256_text "$normalized"
}

monday_env_value() {
  [[ $# -eq 2 ]] || return 2
  local file=$1 key=$2 count value
  count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || return 1
  value=$(sed -n "s/^${key}=//p" "$file")
  [[ -n $value ]] || return 1
  printf '%s\n' "$value"
}

monday_validate_usdm_symbols() {
  [[ $# -eq 1 ]] || return 2
  local value=$1 unique
  [[ $value =~ ^[A-Z0-9]+(,[A-Z0-9]+)*$ ]] || return 1
  local -a symbols
  IFS=, read -r -a symbols <<<"$value"
  (( ${#symbols[@]} == 100 )) || return 1
  unique=$(printf '%s\n' "${symbols[@]}" | sort -u | wc -l)
  (( unique == 100 ))
}

monday_validate_production_env() {
  [[ $# -eq 3 ]] || return 2
  local file=$1 market=$2 dataset=$3 symbols spool
  monday_file_direct "$file" || return 1
  # EnvironmentFile is part of the signed runtime pair.  Accept comments and
  # blank lines, but reject unknown keys, duplicates, and missing projections;
  # otherwise an extra variable could silently alter collector behaviour while
  # the unit and the handful of checked identity fields still look valid.
  local raw line key value allowed_key found
  local -A seen=()
  local -a allowed_keys=(
    MARKET DATASET SHARD_ID SYMBOLS DEPTH_MODE WS_SHARD_SIZE SNAPSHOT_LIMIT
    SNAPSHOT_REQUESTS_PER_SECOND SNAPSHOT_RETRY_ATTEMPTS SYNC_TIMEOUT_SECONDS
    STALL_TIMEOUT_SECONDS PROCESS_WATCHDOG_SECONDS MAX_BUFFERED_DIFFS
    MAX_PENDING_DIFFS_TOTAL MIN_FREE_GB ZSTD_TIMEOUT_SECONDS
    OSS_COPY_TIMEOUT_SECONDS SEGMENT_SECONDS SPOOL_DIR OSS_BUCKET OSS_ENDPOINT
    OSS_REGION ALIYUN_PROFILE BINANCE_REST_BASE LOG_LEVEL
  )
  [[ $market == spot ]] && allowed_keys+=(SNAPSHOT_PRODUCERS)
  while IFS= read -r raw || [[ -n $raw ]]; do
    line="$raw"
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    [[ -z $line || $line == \#* ]] && continue
    [[ $line =~ ^([A-Za-z][A-Za-z0-9_]*)=(.+)$ ]] || return 1
    key=${BASH_REMATCH[1]}; value=${BASH_REMATCH[2]}
    found=false
    for allowed_key in "${allowed_keys[@]}"; do
      [[ $key == "$allowed_key" ]] && found=true
    done
    [[ $found == true && ${seen[$key]:-0} == 0 ]] || return 1
    [[ -n $value ]] || return 1
    seen[$key]=1
  done <"$file"
  for key in "${allowed_keys[@]}"; do
    [[ ${seen[$key]:-0} == 1 ]] || return 1
  done
  [[ $(monday_env_value "$file" MARKET) == "$market" ]] || return 1
  [[ $(monday_env_value "$file" DATASET) == "$dataset" ]] || return 1
  [[ $(monday_env_value "$file" SHARD_ID) == all ]] || return 1
  symbols=$(monday_env_value "$file" SYMBOLS) || return 1
  if [[ $market == spot ]]; then
    [[ $symbols == ALL ]] || return 1
  else
    monday_validate_usdm_symbols "$symbols" || return 1
    [[ $(monday_env_value "$file" WS_SHARD_SIZE) == 25 ]] || return 1
  fi
  [[ $(monday_env_value "$file" DEPTH_MODE) == diff ]] || return 1
  [[ $(monday_env_value "$file" SEGMENT_SECONDS) == 3600 ]] || return 1
  spool="/data/monday/spool/binance-lob/$market"
  [[ $(monday_env_value "$file" SPOOL_DIR) == "$spool" ]] || return 1
  [[ $(monday_env_value "$file" OSS_BUCKET) == monday-lob-apne1-1045353359 ]] || return 1
  [[ $(monday_env_value "$file" OSS_ENDPOINT) == oss-ap-northeast-1-internal.aliyuncs.com ]] || return 1
  [[ $(monday_env_value "$file" OSS_REGION) == ap-northeast-1 ]] || return 1
  [[ $(monday_env_value "$file" ALIYUN_PROFILE) == ecs-role ]] || return 1
}

monday_verify_production_runtime_assets() {
  [[ $# -eq 3 ]] || return 2
  local root=$1 deployment=$2 payload=$3 service upload spot_env usdm_env target
  local service_sha upload_sha spot_sha usdm_sha service_semantics_sha upload_semantics_sha production_json markets_json
  monday_sha256_ok "$payload" || return 1
  monday_path_direct "$deployment" || return 1
  service="$deployment/binance-lob-archiver-production@.service"
  upload="$deployment/binance-lob-archiver-upload@.service"
  spot_env="$deployment/binance-lob-archiver-production-spot.env"
  usdm_env="$deployment/binance-lob-archiver-production-usdm.env"
  for file in "$service" "$upload" "$spot_env" "$usdm_env"; do
    monday_file_direct "$file" || return 1
  done

  # Reject every unit directive outside the reviewed production topology before
  # checking individual values.  The raw SHA below binds bytes; these semantic
  # hashes bind the normalized directive/section contract and make comments or
  # formatting the only harmless source changes.
  service_semantics_sha=$(monday_unit_semantics_sha256 "$service" production) || return 1
  upload_semantics_sha=$(monday_unit_semantics_sha256 "$upload" upload) || return 1

  # Candidate ExecStart is the only stable production projection.  The
  # candidate payload itself is checked at its immutable digest path below.
  monday_unit_exact_line "$service" Type simple || return 1
  monday_unit_exact_line "$service" User hftcollector || return 1
  monday_unit_exact_line "$service" Group hftcollector || return 1
  monday_unit_exact_line "$service" EnvironmentFile /etc/monday/binance-lob-archiver-production-%i.env || return 1
  monday_unit_exact_line "$service" ExecStart /opt/monday/bin/binance-lob-archiver || return 1
  monday_unit_exact_line "$service" Restart always || return 1
  monday_unit_exact_line "$service" RestartSec 5 || return 1
  monday_unit_exact_line "$service" RuntimeMaxSec 21600 || return 1
  monday_unit_exact_line "$service" KillMode mixed || return 1
  monday_unit_exact_line "$service" TimeoutStartSec 120 || return 1
  monday_unit_exact_line "$service" TimeoutStopSec 600 || return 1
  monday_unit_exact_line "$service" NoNewPrivileges true || return 1
  monday_unit_exact_line "$service" PrivateTmp true || return 1
  monday_unit_exact_line "$service" ProtectSystem strict || return 1
  monday_unit_exact_line "$service" ProtectHome true || return 1
  monday_unit_exact_line "$service" ProtectKernelTunables true || return 1
  monday_unit_exact_line "$service" ProtectKernelModules true || return 1
  monday_unit_exact_line "$service" ProtectControlGroups true || return 1
  monday_unit_exact_line "$service" LockPersonality true || return 1
  monday_unit_exact_line "$service" RestrictSUIDSGID true || return 1
  monday_unit_exact_line "$service" StateDirectory hft-collector || return 1
  monday_unit_exact_line "$service" ReadWritePaths '/data/monday/spool/binance-lob -/data/monday/spool/binance-lob-recovery' || return 1
  monday_unit_exact_line "$service" CPUQuota '80%' || return 1
  monday_unit_exact_line "$service" MemoryHigh '2048M' || return 1
  monday_unit_exact_line "$service" MemoryMax '2560M' || return 1
  monday_unit_exact_line "$service" AssertPathIsMountPoint /data || return 1
  monday_unit_exact_line "$service" StartLimitIntervalSec 7200 || return 1
  monday_unit_exact_line "$service" StartLimitBurst 5 || return 1
  [[ $(grep -Fxc 'ExecStartPre=/opt/monday/bin/binance-lob-archiver --self-test' "$service" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'ExecStartPre=+/opt/monday/bin/monday-rust-lob-recovery-queue isolate %i' "$service" || true) -eq 1 ]] || return 1
  [[ $(grep -c '^EnvironmentFile=' "$service" || true) -eq 1 ]] || return 1
  [[ $(grep -c '^ExecStartPre=' "$service" || true) -eq 2 ]] || return 1
  [[ $(grep -c '^ExecStart=' "$service" || true) -eq 1 ]] || return 1

  monday_unit_exact_line "$upload" Type oneshot || return 1
  monday_unit_exact_line "$upload" User hftcollector || return 1
  monday_unit_exact_line "$upload" Group hftcollector || return 1
  monday_unit_exact_line "$upload" EnvironmentFile /etc/monday/binance-lob-archiver-production-%i.env || return 1
  monday_unit_exact_line "$upload" ExecStart '/opt/monday/bin/binance-lob-archiver --upload-only' || return 1
  monday_unit_exact_line "$upload" TimeoutStartSec 0 || return 1
  monday_unit_exact_line "$upload" NoNewPrivileges true || return 1
  monday_unit_exact_line "$upload" PrivateTmp true || return 1
  monday_unit_exact_line "$upload" ProtectSystem strict || return 1
  monday_unit_exact_line "$upload" ProtectHome true || return 1
  monday_unit_exact_line "$upload" ProtectKernelTunables true || return 1
  monday_unit_exact_line "$upload" ProtectKernelModules true || return 1
  monday_unit_exact_line "$upload" ProtectControlGroups true || return 1
  monday_unit_exact_line "$upload" LockPersonality true || return 1
  monday_unit_exact_line "$upload" RestrictSUIDSGID true || return 1
  monday_unit_exact_line "$upload" StateDirectory hft-collector || return 1
  monday_unit_exact_line "$upload" ReadWritePaths /data/monday/spool/binance-lob || return 1
  monday_unit_exact_line "$upload" CPUQuota '80%' || return 1
  monday_unit_exact_line "$upload" MemoryHigh '384M' || return 1
  monday_unit_exact_line "$upload" MemoryMax '512M' || return 1
  monday_unit_exact_line "$upload" AssertPathIsMountPoint /data || return 1
  [[ $(grep -c '^ExecStart=' "$upload" || true) -eq 1 ]] || return 1
  [[ $(grep -c '^EnvironmentFile=' "$upload" || true) -eq 1 ]] || return 1
  [[ $(grep -c '^ExecStartPre=' "$upload" || true) -eq 0 ]] || return 1
  [[ $(grep -c '^Restart=' "$upload" || true) -eq 0 ]] || return 1

  monday_validate_production_env "$spot_env" spot spot_all || return 1
  monday_validate_production_env "$usdm_env" usdm usdm_perpetual_top100_lob || return 1
  target=$(monday_root_join "$root" "opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver") || return 1
  monday_file_direct "$target" || return 1
  [[ -x $target && $(monday_sha256_file "$target") == "$payload" ]] || return 1

  service_sha=$(monday_sha256_file "$service") || return 1
  upload_sha=$(monday_sha256_file "$upload") || return 1
  spot_sha=$(monday_sha256_file "$spot_env") || return 1
  usdm_sha=$(monday_sha256_file "$usdm_env") || return 1
  markets_json=$(jq -cn \
    --arg spot_market spot --arg spot_dataset spot_all \
    --arg spot_symbols "$(monday_env_value "$spot_env" SYMBOLS)" \
    --arg spot_spool /data/monday/spool/binance-lob/spot \
    --arg usdm_market usdm --arg usdm_dataset usdm_perpetual_top100_lob \
    --arg usdm_symbols "$(monday_env_value "$usdm_env" SYMBOLS)" \
    --arg usdm_spool /data/monday/spool/binance-lob/usdm \
    '{spot:{market:$spot_market,dataset:$spot_dataset,symbols:$spot_symbols,shard_id:"all",spool_dir:$spot_spool,oss_bucket:"monday-lob-apne1-1045353359",oss_endpoint:"oss-ap-northeast-1-internal.aliyuncs.com",oss_region:"ap-northeast-1",aliyun_profile:"ecs-role"},usdm:{market:$usdm_market,dataset:$usdm_dataset,symbols:$usdm_symbols,shard_id:"all",spool_dir:$usdm_spool,oss_bucket:"monday-lob-apne1-1045353359",oss_endpoint:"oss-ap-northeast-1-internal.aliyuncs.com",oss_region:"ap-northeast-1",aliyun_profile:"ecs-role",ws_shard_size:25}}') || return 1
  production_json=$(jq -cnS \
    --arg service_sha "$service_sha" --arg upload_sha "$upload_sha" \
    --arg spot_sha "$spot_sha" --arg usdm_sha "$usdm_sha" \
    --argjson markets "$markets_json" \
    --arg service_semantics_sha "$service_semantics_sha" --arg upload_semantics_sha "$upload_semantics_sha" \
    '{schema:"monday.rust_lob_production_runtime.v1",exec_start:"/opt/monday/bin/binance-lob-archiver",environment_file:"/etc/monday/binance-lob-archiver-production-%i.env",user:"hftcollector",group:"hftcollector",restart:"always",restart_sec:5,runtime_max_sec:21600,kill_mode:"mixed",timeout_start_sec:120,timeout_stop_sec:600,type:"simple",cpu_quota:"80%",memory_high:"2048M",memory_max:"2560M",sandbox:{no_new_privileges:true,private_tmp:true,protect_system:"strict",protect_home:true,protect_kernel_tunables:true,protect_kernel_modules:true,protect_control_groups:true,lock_personality:true,restrict_suidsgid:true,state_directory:"hft-collector",read_write_paths:["/data/monday/spool/binance-lob","/data/monday/spool/binance-lob-recovery"]},upload:{type:"oneshot",exec_start:"/opt/monday/bin/binance-lob-archiver --upload-only",environment_file:"/etc/monday/binance-lob-archiver-production-%i.env",cpu_quota:"80%",memory_high:"384M",memory_max:"512M",timeout_start_sec:0},unit_sha256:{collector:$service_sha,upload:$upload_sha},unit_semantics_sha256:{collector:$service_semantics_sha,upload:$upload_semantics_sha},env_sha256:{spot:$spot_sha,usdm:$usdm_sha},markets:$markets}') || return 1
  printf '%s\n' "$production_json"
}

monday_controller_release_sha256() {
  [[ $# -eq 1 ]] || return 2
  monday_sha256_file "$1/release.json"
}

# Resolve the immutable V2 deployment currently installed as the active pair.
# This helper is intentionally strict: it never guesses another release and
# never accepts an unsupported manifest.
monday_rust_lob_active_controller_deployment() {
  [[ $# -eq 3 ]] || return 2
  local controller_root=$1 artifact_sha=$2 runtime_contract=$3
  local active="$controller_root/active" release sha manifest deployment
  [[ $artifact_sha =~ ^[a-f0-9]{64}$ && $runtime_contract =~ ^[a-f0-9]{64}$ ]] || return 1
  [[ -L $active ]] || return 1
  release=$(readlink -f -- "$active") || return 1
  [[ $release =~ ^${controller_root}/([a-f0-9]{64})$ ]] || return 1
  sha=${BASH_REMATCH[1]}
  manifest="$release/release.json"
  deployment="$release/deployment"
  [[ -d $release && ! -L $release && -d $deployment && ! -L $deployment ]] || return 1
  local root
  root=$(cd -- "${controller_root%/}/../../../.." 2>/dev/null && pwd -P) || return 1
  monday_verify_controller_release "$root" "$sha" 2>/dev/null || return 1
  jq -e --arg artifact "$artifact_sha" --arg runtime "$runtime_contract" \
    '.artifact_sha256 == $artifact and .runtime_contract_sha256 == $runtime' \
    "$manifest" >/dev/null || return 1
  printf '%s\n' "$deployment"
}

# Apply the exact health policy shipped by the active controller.  Restore and
# readback pass the active policy path, so a newer controller cannot silently
# widen the fields checked by an older host script.
monday_verify_rust_lob_runtime_health() {
  [[ $# -eq 6 ]] || return 2
  local policy=$1 health=$2 market=$3 dataset=$4 minimum_symbols=$5 minimum_updated_ns=$6
  monday_file_direct "$policy" && monday_file_direct "$health" || return 1
  [[ $market == spot || $market == usdm ]] || return 1
  [[ $dataset =~ ^[A-Za-z0-9_.-]+$ && $minimum_symbols =~ ^[1-9][0-9]*$ \
    && $minimum_updated_ns =~ ^[0-9]+$ ]] || return 1
  jq -e -f "$policy" \
    --arg expected_market "$market" \
    --arg expected_dataset "$dataset" \
    --argjson minimum_symbols "$minimum_symbols" \
    --argjson minimum_updated_ns "$minimum_updated_ns" \
    --arg old_session '' "$health" >/dev/null
}

# Pure monotonic health freshness transition used by the host gate and tests.
# Output: last_updated_ns last_advance_mono max_gap_seconds sample_increment
monday_observe_health_freshness() {
  [[ $# -eq 6 ]] || return 2
  local last_updated_ns=$1 last_advance_mono=$2 max_gap_seconds=$3
  local current_updated_ns=$4 current_mono=$5 allowed_gap_seconds=$6
  local gap_seconds sample_increment=0
  [[ $last_updated_ns =~ ^[0-9]+$ && $last_advance_mono =~ ^[0-9]+$ \
    && $max_gap_seconds =~ ^[0-9]+$ && $current_updated_ns =~ ^[0-9]+$ \
    && $current_mono =~ ^[0-9]+$ && $allowed_gap_seconds =~ ^[1-9][0-9]*$ ]] || return 2
  ((current_updated_ns >= last_updated_ns && current_mono >= last_advance_mono)) || return 1
  gap_seconds=$((current_mono - last_advance_mono))
  ((gap_seconds > max_gap_seconds)) && max_gap_seconds=$gap_seconds
  ((gap_seconds <= allowed_gap_seconds)) || return 1
  if ((current_updated_ns > last_updated_ns)); then
    last_updated_ns=$current_updated_ns
    last_advance_mono=$current_mono
    sample_increment=1
  fi
  printf '%s %s %s %s\n' "$last_updated_ns" "$last_advance_mono" \
    "$max_gap_seconds" "$sample_increment"
}

# Return the required bytes and accept only when the host has that headroom.
monday_shadow_memory_admission() {
  (($# >= 3)) || return 2
  local available=$1 total=0 value
  for value in "$@"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    (( value <= 9223372036854775807 )) || return 2
  done
  shift
  for value in "$@"; do
    ((value <= 9223372036854775807 - total)) || return 2
    total=$((total + value))
  done
  ((total > 0)) || return 2
  printf '%s\n' "$total"
  ((available >= total))
}

# Reserve measured production peak plus bounded growth, capped by unit limit.
monday_production_memory_growth_headroom() {
  [[ $# -eq 4 ]] || return 2
  local current=$1 peak=$2 maximum=$3 margin=$4 target
  local value
  for value in "$current" "$peak" "$maximum" "$margin"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    (( value <= 9223372036854775807 )) || return 2
  done
  ((current <= peak && peak <= maximum)) || return 2
  if ((margin >= maximum - peak)); then target=$maximum; else target=$((peak + margin)); fi
  printf '%s\n' "$((target - current))"
}

# Read cumulative I/O-full stall time from a Linux PSI source.
monday_io_full_psi_total_us() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 2
  awk '$1 == "full" { rows += 1; for (i = 2; i <= NF; i++) {
    if ($i ~ /^total=[0-9]+$/) { totals += 1; value = substr($i, 7) }
  }} END { if (rows != 1 || totals != 1 || value !~ /^(0|[1-9][0-9]*)$/) exit 1; print value }' "$1"
}

# Output: delta_us ratio hit consecutive_hits. Threshold is normalized to the
# reference window and any non-hit resets the consecutive count.
monday_io_full_psi_window() {
  [[ $# -eq 6 ]] || return 2
  local previous=$1 current=$2 window_us=$3 reference_window_us=$4
  local threshold_us=$5 consecutive=$6 value delta hit next ratio
  for value in "$previous" "$current" "$window_us" "$reference_window_us" "$threshold_us" "$consecutive"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    (( value <= 9223372036854775807 )) || return 2
  done
  ((current >= previous && window_us > 0 && reference_window_us > 0 && threshold_us > 0)) || return 2
  delta=$((current - previous))
  if awk -v delta="$delta" -v window="$window_us" -v threshold="$threshold_us" \
    -v reference="$reference_window_us" 'BEGIN { exit !((delta / window) >= (threshold / reference)) }'; then
    hit=true; next=$((consecutive + 1))
  else hit=false; next=0; fi
  ratio=$(awk -v delta="$delta" -v window="$window_us" 'BEGIN { printf "%.9f", delta / window }')
  printf '%s %s %s %s\n' "$delta" "$ratio" "$hit" "$next"
}

# Replay-unsafe manifests may only trail the safe observation window.
monday_validate_replay_safe_manifest_order() {
  [[ $# -eq 3 ]] || return 2
  local market=$1 candidates=$2 unsafe_candidates=$3
  local unsafe_start unsafe_end unsafe_uri
  [[ -f $candidates && -f $unsafe_candidates ]] || return 2
  [[ -s $unsafe_candidates ]] || return 0
  while IFS=$'\t' read -r unsafe_start unsafe_end unsafe_uri; do
    if awk -F '\t' -v start="$unsafe_start" -v end="$unsafe_end" \
      '$1 < end && start < $2 { overlap=1 } END { exit(overlap ? 0 : 1) }' "$candidates"; then
      printf '%s replay-unsafe manifest overlaps a replay-safe segment: %s\n' "$market" "$unsafe_uri" >&2
      return 1
    fi
    if awk -F '\t' -v start="$unsafe_start" '$1 > start { found=1 } END { exit(found ? 0 : 1) }' "$candidates"; then
      printf '%s has a replay-unsafe manifest before a later replay-safe manifest: %s\n' "$market" "$unsafe_uri" >&2
      return 1
    fi
  done < <(sort -n -k1,1 "$unsafe_candidates")
}

monday_validate_v2_manifest() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 2
  jq -e '
    type == "object"
    and (keys | sort) == [
      "artifact_sha256", "artifact_uri", "control_plane_version",
      "deployment_bundle_sha256", "deployment_bundle_uri",
      "deployment_source_revision", "runtime_contract_sha256", "schema",
      "topology"
    ]
    and .schema == "monday.rust_lob_controller_release.v2"
    and .control_plane_version == 2
    and (.artifact_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
    and (.artifact_uri | type == "string" and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.deployment_bundle_uri | type == "string" and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.topology | . == "stable")' \
    "$1" >/dev/null
}

monday_manifest_field() {
  [[ $# -eq 2 ]] || return 2
  jq -er --arg key "$2" '.[$key]' "$1"
}

monday_active_controller_sha() {
  [[ $# -eq 1 ]] || return 2
  local root=${1:-/} link controller_root
  controller_root=$(monday_root_join "$root" opt/monday/releases/binance-lob-controller) || return 1
  link="$controller_root/active"
  local target sha
  [[ -L $link ]] || return 1
  target=$(readlink -f -- "$link") || return 1
  sha=${target##*/}
  [[ $target == "$controller_root/$sha" \
    && $sha =~ ^[a-f0-9]{64}$ ]] || return 1
  printf '%s\n' "$sha"
}

monday_verify_controller_release() {
  [[ $# -eq 2 ]] || return 2
  local root=${1:-/} sha=$2 controller_root release
  controller_root=$(monday_root_join "$root" opt/monday/releases/binance-lob-controller) || return 1
  release="$controller_root/$sha"
  local manifest="$release/release.json" asset expected projection target payload
  monday_sha256_ok "$sha" || return 1
  monday_path_direct "$controller_root" || return 1
  monday_path_direct "$release" || return 1
  monday_path_direct "$release/deployment" || return 1
  monday_file_direct "$manifest" || return 1
  monday_file_direct "$release/release.json.sha256" || return 1
  monday_file_direct "$release/deployment.sha256" || return 1
  [[ $(monday_sha256_file "$manifest") == "$sha" ]] || return 1
  (cd "$release" && sha256sum --check --strict release.json.sha256 >/dev/null \
    && sha256sum --check --strict deployment.sha256 >/dev/null) || return 1
  monday_validate_v2_manifest "$manifest" || return 1
  expected=$(cd "$release" && sha256sum deployment/* | sort -k2)
  cmp -s <(printf '%s\n' "$expected") "$release/deployment.sha256" || return 1
  while IFS= read -r asset; do
    [[ -n $asset ]] || continue
    monday_file_direct "$release/deployment/$asset" || return 1
  done < <(monday_controller_assets)
  payload=$(monday_manifest_field "$manifest" artifact_sha256) || return 1
  projection="$release/binance-lob-archiver"
  target=$(monday_root_join "$root" "opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver")
  [[ -L $projection && $(readlink -- "$projection") == "$target" ]] || return 1
  [[ $(readlink -f -- "$projection") == "$target" ]] || return 1
  monday_file_direct "$target" || return 1
  [[ $(monday_sha256_file "$target") == "$payload" ]] || return 1
  [[ $(monday_rust_lob_runtime_contract_sha256 "$release/deployment") \
    == "$(monday_manifest_field "$manifest" runtime_contract_sha256)" ]] || return 1
}

monday_validate_v2_gate() {
  [[ $# -eq 4 ]] || return 2
  local gate=$1 from=$2 candidate=$3 gate_sha=$4
  local controller_asset_keys production_asset_keys shadow_asset_keys observed_now_ns market observed_at observed_at_ns parsed_observed
  controller_asset_keys=$(monday_controller_assets | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  production_asset_keys=$(printf '%s\n' \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-upload@.service \
    binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env \
    | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  shadow_asset_keys=$(printf '%s\n' \
    binance-lob-archiver-rust@.service \
    binance-lob-archiver-rust-upload@.service \
    binance-lob-archiver-rust-spot.env \
    binance-lob-archiver-rust-usdm.env \
    | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  monday_file_direct "$gate" || return 1
  [[ $(monday_sha256_file "$gate") == "$gate_sha" ]] || return 1
  jq -e \
    --arg from "$from" --arg candidate "$candidate" --arg gate_sha "$gate_sha" \
    --argjson controller_asset_keys "$controller_asset_keys" \
    --argjson production_asset_keys "$production_asset_keys" \
    --argjson shadow_asset_keys "$shadow_asset_keys" '
      .schema == "monday.rust_lob_shadow_gate.v5"
      and .control_plane_version == 2
      and .passed == true
      and (.production_eligible | type == "boolean")
      and (.test_only | type == "boolean")
      and (if .test_only then .production_eligible == false else .production_eligible == true end)
      and (.source_mode == "stable" or .source_mode == "direct")
      and (.from_controller_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (if $from == "direct" then
        .source_mode == "direct"
        and .transition.before == .from_controller_sha256
      else
        .source_mode == "stable"
        and .from_controller_sha256 == $from
        and .transition.before == $from
      end)
      and .transition.after == $candidate
      and (.transition.topology == "stable" or .transition.topology == "direct-bootstrap")
      and (.candidate_controller_sha256 == $candidate)
      and (.candidate_payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
      and (.candidate_control_bytes | type == "object"
        and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.assets | type == "object" and (keys | sort) == $controller_asset_keys
          and all(.[]; type == "string" and test("^[a-f0-9]{64}$"))))
      and (.before | type == "object")
      and (.before.controller == (if $from == "direct" then .from_controller_sha256 else $from end))
      and (.before.payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.before.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.before.production_projection | type == "string" and length > 0)
      and (.before.production_assets | type == "object" and (keys | sort) == $production_asset_keys
        and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
      and (.production_assets | type == "object" and (keys | sort) == $production_asset_keys
        and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
      and (.production_runtime | type == "object"
        and .schema == "monday.rust_lob_production_runtime.v1"
        and .type == "simple"
        and .exec_start == "/opt/monday/bin/binance-lob-archiver"
        and .environment_file == "/etc/monday/binance-lob-archiver-production-%i.env"
        and .user == "hftcollector"
        and .group == "hftcollector"
        and .restart == "always"
        and .restart_sec == 5
        and .runtime_max_sec == 21600
        and .kill_mode == "mixed"
        and .timeout_start_sec == 120
        and .timeout_stop_sec == 600
        and .cpu_quota == "80%"
        and .memory_high == "2048M"
        and .memory_max == "2560M"
        and (.sandbox | type == "object"
          and .no_new_privileges == true
          and .private_tmp == true
          and .protect_system == "strict"
          and .protect_home == true
          and .protect_kernel_tunables == true
          and .protect_kernel_modules == true
          and .protect_control_groups == true
          and .lock_personality == true
          and .restrict_suidsgid == true
          and .state_directory == "hft-collector"
          and .read_write_paths == ["/data/monday/spool/binance-lob", "/data/monday/spool/binance-lob-recovery"])
        and (.upload | type == "object"
          and .type == "oneshot"
          and .exec_start == "/opt/monday/bin/binance-lob-archiver --upload-only"
          and .environment_file == "/etc/monday/binance-lob-archiver-production-%i.env"
          and .cpu_quota == "80%"
          and .memory_high == "384M"
          and .memory_max == "512M"
          and .timeout_start_sec == 0)
        and (.unit_sha256 | type == "object"
          and (.collector | type == "string" and test("^[a-f0-9]{64}$"))
          and (.upload | type == "string" and test("^[a-f0-9]{64}$")))
        and (.unit_semantics_sha256 | type == "object"
          and (.collector | type == "string" and test("^[a-f0-9]{64}$"))
          and (.upload | type == "string" and test("^[a-f0-9]{64}$")))
        and (.env_sha256 | type == "object"
          and (.spot | type == "string" and test("^[a-f0-9]{64}$"))
          and (.usdm | type == "string" and test("^[a-f0-9]{64}$")))
        and (.markets | type == "object" and (keys | sort) == ["spot", "usdm"]
          and (.spot | type == "object"
            and .market == "spot" and .dataset == "spot_all" and .symbols == "ALL"
            and .shard_id == "all" and .spool_dir == "/data/monday/spool/binance-lob/spot"
            and .oss_bucket == "monday-lob-apne1-1045353359"
            and .oss_endpoint == "oss-ap-northeast-1-internal.aliyuncs.com"
            and .oss_region == "ap-northeast-1" and .aliyun_profile == "ecs-role")
          and (.usdm | type == "object"
            and .market == "usdm" and .dataset == "usdm_perpetual_top100_lob"
            and .shard_id == "all" and .ws_shard_size == 25
            and .spool_dir == "/data/monday/spool/binance-lob/usdm"
            and .oss_bucket == "monday-lob-apne1-1045353359"
            and .oss_endpoint == "oss-ap-northeast-1-internal.aliyuncs.com"
            and .oss_region == "ap-northeast-1" and .aliyun_profile == "ecs-role")
          and (.usdm.symbols | type == "string")
          and ((.usdm.symbols | split(",")) | length == 100)
          and ((.usdm.symbols | split(",") | unique) | length == 100)))
      and (.production_process | type == "object")
      and (if .test_only then true else
        (.production_process | ((keys | sort) == ["spot", "usdm"]
          and all(.[]; .active == true
            and (.main_pid | type == "number" and . >= 1)
            and (.process_exe_sha256 | type == "string" and test("^[a-f0-9]{64}$"))))) end)
      and (.resource_admission | type == "array" and length >= 3
        and ((["preflight","shadow-spot","strict-verifier-spot","upload-drain-spot","shadow-usdm","strict-verifier-usdm","upload-drain-usdm","oss-readback-spot","oss-readback-usdm"]
          - (map(.phase) | unique)) | length == 0)
        and all(.[]; . as $r
          | (.phase | type == "string" and length > 0)
          and (.started_at | type == "string" and length > 0)
          and (.ended_at | type == "string" and length > 0)
          and (.samples | type == "number" and . >= 1)
          and (.host_memory_available_bytes | type == "number" and . >= 0)
          and (.max_memory_available_bytes | type == "number" and . >= 0)
          and (.current_memory_available_bytes | type == "number" and . >= 0)
          and (.breach | type == "boolean" and . == false)
          and ($r.required_bytes | type == "number" and . > 0 and . <= $r.host_memory_available_bytes)
          and (.phase_memory_max_bytes | type == "number" and . > 0)))
      and (.io_full_psi_windows | type == "array" and length >= 3
        and all(.[]; . as $p
          | (.phase | type == "string" and length > 0)
          and (.stage | type == "string" and length > 0)
          and (.hit | type == "boolean")
          and ($p.consecutive_hits | type == "number" and . >= 0)
          and (if $p.stage == "calibration"
               then ($p.delta_us | type == "number" and . >= 0)
                 and ($p.ratio | type == "number" and . >= 0)
               else true end)))
      and (.shadow_staging | type == "object"
        and .mode == "run-scoped"
        and (.run_unit_root | type == "string" and test("/run/monday/rust-lob-gate/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$"))
        and (.spool_root | type == "string" and test("/data/monday/spool/binance-lob-rust-shadow/gate/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$"))
        and (.units | type == "object" and (keys | sort) == ["spot", "usdm"]
          and (.spot | type == "string" and test("^monday-rust-lob-gate-[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-spot\\.service$"))
          and (.usdm | type == "string" and test("^monday-rust-lob-gate-[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-usdm\\.service$")))
        and (.upload_units | type == "object" and (keys | sort) == ["spot", "usdm"]
          and all(.[]; type == "object"
            and (.unit | type == "string" and test("^monday-rust-lob-gate-[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-(spot|usdm)-upload\\.service$"))
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))))
        and (.candidate_assets | type == "object" and (keys | sort) == $shadow_asset_keys
          and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
        and (.restored_assets | type == "object" and (keys | sort) == $shadow_asset_keys)
        and (.before_assets | type == "object" and (keys | sort) == $shadow_asset_keys)
        and (.restored_assets == .before_assets)
        and all([.restored_assets, .before_assets][] | .[];
          ((.state == "present"
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
           or (.state == "absent" and .sha256 == null)
           or (.state == "projection"
             and (.target | type == "string" and length > 0)
             and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))))
        and (.binary | type == "object" and (.path | type == "string")
          and (.candidate_target | type == "string" and (contains("/opt/monday/bin/") | not))
          and (.restored_present | type == "boolean")
          and ((.restored_target_sha256 == null)
            or (.restored_target_sha256 | type == "string" and test("^[a-f0-9]{64}$")))))
        and (.binary.path == .run_unit_root)
      and (.checks | type == "object"
        and .before_pair_unchanged == true
        and .production_runtime_verified == true
        and .shadow_staging_verified == true
        and .shadow_assets_restored == true
        and .resource_preflight == true
        and .oss_triplets == true
        and .strict_segment_verifier == true
        and .final_identity == true
        and .controller_control_bytes == true
        and .shadow_link_restored == true
        and .health_freshness == true)
      and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"])
        and (to_entries | all(.[]; .value.market == .key))
        and all(.[]; . as $m
          | (.market | type == "string")
          and (.dataset | type == "string" and length > 0)
          and (.session_id | type == "string" and length > 0)
          and (.spool_dir | type == "string" and test("/data/monday/spool/binance-lob-rust-shadow/gate/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/(spot|usdm)$"))
          and (.shard_id == "all")
          and (.oss_bucket == "monday-lob-apne1-1045353359")
          and (.oss_endpoint == "oss-ap-northeast-1-internal.aliyuncs.com")
          and (.oss_region == "ap-northeast-1")
          and (.aliyun_profile == "ecs-role")
          and (.expected_oss_bucket | type == "string" and length > 0)
          and (.expected_oss_prefix | type == "string" and length > 0)
          and (.observed_at_ns | type == "number" and floor == . and . >= 0)
          and ($m.segment_count | type == "number" and . >= 2 and . == ($m.segments | length))
          and ($m.oss_triplet_count | type == "number" and . >= 2 and . == ($m.triplets | length))
          and (.n_restarts | type == "number" and . == 0)
          and (.process_identity_verified == true)
          and (.installed_shadow_assets_verified == true)
          and (.strict_lob_continuity_readback == true)
          and (.strict_aggregate_trade_continuity_readback | type == "boolean")
          and (.strict_raw_trade_continuity_readback | type == "boolean")
          and (if .market == "spot" then
            .strict_aggregate_trade_continuity_readback == true
            and .strict_raw_trade_continuity_readback == true
          else true end)
          and (.segments | type == "array" and length >= 2
            and all(.[];
              (.file | type == "string" and test("^[A-Za-z0-9._-]+\\.jsonl\\.zst$"))
              and (.path | type == "string" and length > 0)
              and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.start_received_at_ns | type == "number" and . >= 0)
              and (.end_received_at_ns | type == "number")
              and (.end_received_at_ns >= .start_received_at_ns)
              and (.end_received_at_ns <= $m.observed_at_ns)
              and (.session_id | type == "string" and . == $m.session_id)))
          and (.triplets | type == "array" and length >= 2
            and all(.[];
              (.market | type == "string" and . == $m.market)
              and (.dataset | type == "string" and . == $m.dataset)
              and (.data_uri | type == "string"
                and startswith(("oss://" + $m.expected_oss_bucket + "/" + $m.expected_oss_prefix + "/"))
                and test("^oss://[^/]+/.+\\.jsonl\\.zst$"))
              and (.manifest_uri | type == "string")
              and (.manifest_uri == (.data_uri + ".manifest.json"))
              and (.success_uri | type == "string")
              and (.success_uri == (.data_uri + "._SUCCESS"))
              and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.success_content == (.data_sha256 + "\n"))
              and (.start_received_at_ns | type == "number" and . >= 0)
              and (.end_received_at_ns | type == "number")
              and (.end_received_at_ns >= .start_received_at_ns)
              and (.end_received_at_ns <= $m.observed_at_ns)
              and (.observed_at | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?Z$"))
              and (.observed_at_ns | type == "number" and floor == . and . >= 0)
              and (.session_id | type == "string" and . == $m.session_id)
              and (.catalog_sha256 | type == "string" and . == $m.health.frozen_catalog_sha256)))
          and (.health | type == "object"
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
            and (.session_id | type == "string" and length > 0)
            and (.frozen_catalog_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
            and (.frozen_symbol_count | type == "number" and . >= 1)
            and (.max_health_silence_seconds | type == "number" and . >= 0 and . <= 120)
            and (.samples | type == "number" and . >= 1)
            and .session_id == $m.session_id)))' \
    "$gate" >/dev/null || return 1
  observed_now_ns=$(date +%s%N) || return 1
  [[ $observed_now_ns =~ ^[0-9]+$ ]] || return 1
  while IFS=$'\t' read -r market observed_at observed_at_ns market_observed_at_ns; do
    [[ $market == spot || $market == usdm ]] || return 1
    [[ $observed_at_ns =~ ^[0-9]+$ && $market_observed_at_ns =~ ^[0-9]+$ \
      && $observed_at_ns -le $market_observed_at_ns \
      && $market_observed_at_ns -le $observed_now_ns ]] || return 1
    parsed_observed=$(monday_iso_epoch_ns "$observed_at") || return 1
    [[ $parsed_observed == "$observed_at_ns" ]] || return 1
  done < <(jq -r '.markets | to_entries[] | .key as $market | .value as $value
    | $value.triplets[] | [$market, .observed_at, .observed_at_ns, $value.observed_at_ns] | @tsv' "$gate")
}

# Validate the transition receipt and its exact V2 Gate evidence.  A cutover
# receipt is not authoritative by itself: the immutable Gate receipt must be
# present, hash-identical, and pass the full evidence validator above.
monday_validate_v2_transition() {
  [[ $# -eq 5 ]] || return 2
  local receipt=$1 from=$2 to=$3 gate=$4 gate_sha=$5
  local gate_evidence gate_payload gate_runtime gate_from gate_production_runtime pair_asset_keys controller_projection_keys
  # The stable pair contains exactly the eight runtime unit/env assets
  # (production + shadow).  Recovery/health helpers remain controller assets
  # and are addressed through the immutable active controller, never copied
  # into a second live state projection.
  pair_asset_keys=$(monday_runtime_assets | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  monday_file_direct "$receipt" || return 1
  [[ $from == direct || $from =~ ^[a-f0-9]{64}$ ]] || return 1
  [[ $to =~ ^[a-f0-9]{64}$ && $gate_sha =~ ^[a-f0-9]{64}$ ]] || return 1
  monday_file_direct "$gate" || return 1
  monday_validate_v2_gate "$gate" "$from" "$to" "$gate_sha" || return 1
  gate_evidence=$(jq -ceS \
    '{candidate_control_bytes,resource_admission,io_full_psi_windows,shadow_staging,checks,markets}' \
    "$gate") || return 1
  gate_payload=$(jq -er '.candidate_payload_sha256' "$gate") || return 1
  gate_runtime=$(jq -er '.candidate_runtime_contract_sha256' "$gate") || return 1
  gate_from=$(jq -er '.from_controller_sha256' "$gate") || return 1
  gate_production_runtime=$(jq -ce '.production_runtime' "$gate") || return 1
  controller_projection_keys=$(monday_controller_projection_assets | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  jq -e --arg from "$from" --arg to "$to" --arg gate "$gate" --arg gate_sha "$gate_sha" \
    --arg gate_from "$gate_from" --arg payload "$gate_payload" --arg runtime "$gate_runtime" \
    --argjson controller_projection_keys "$controller_projection_keys" \
    --argjson pair_asset_keys "$pair_asset_keys" '
    .schema == "monday.rust_lob_pair_transition.v2"
    and .control_plane_version == 2
    and .operation == "cutover"
    and (.from_source_mode == (if $from == "direct" then "direct" else "stable" end))
    and (.source_mode == .from_source_mode)
    and ((if $from == "direct" then
      .source_mode == "direct" and .from_controller_sha256 == $gate_from
    else
      .source_mode == "stable" and .from_controller_sha256 == $from
    end))
    and .controller_sha256 == $to
    and .payload_sha256 == $payload
    and .runtime_contract_sha256 == $runtime
    and .gate_receipt == $gate
    and .gate_sha256 == $gate_sha
    and (.test_only | type == "boolean")
    and (if .test_only then .production_eligible == false else .production_eligible == true end)
    and (.production_runtime | type == "object")
    and (.production_process | type == "object")
    and (.recovery_schedulers | type == "object"
      and (if .test_only then true else (keys | sort) == ["spot", "usdm"] end)
      and (if .test_only then true else all(.[];
        .active == true and .enabled == true
        and (.unit | type == "string" and test("^binance-lob-archiver-recovery@(spot|usdm)\\.timer$"))) end))
    and (if .test_only then
      (.production_process == {} or
       ((.production_process | keys | sort) == ["spot", "usdm"]
        and all(.production_process[];
          .active == true
          and (.main_pid | type == "number" and . >= 1)
          and (.process_exe_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
          and (.n_restarts | type == "number" and . == 0)
          and (.session_id | type == "string" and length > 0)
          and (.observed_at_ns | type == "number" and . >= 0))))
    else
      ((.production_process | keys | sort) == ["spot", "usdm"]
       and all(.production_process[];
         .active == true
         and (.main_pid | type == "number" and . >= 1)
         and (.process_exe_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
         and (.n_restarts | type == "number" and . == 0)
         and (.session_id | type == "string" and length > 0)
         and (.observed_at_ns | type == "number" and . >= 0)))
    end)
    and .active_pair_committed == true
    and (.completed_at | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?Z$"))
    and (.completed_at_ns | type == "number" and floor == . and . >= 0)
    and (.stable_production_projection | type == "string"
      and . == "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver")
    and (.gate_evidence | type == "object"
      and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"]))
      and (.candidate_control_bytes | type == "object")
      and (.resource_admission | type == "array" and length >= 3)
      and (.io_full_psi_windows | type == "array" and length >= 3))
      and (.before | type == "object"
      and (.controller == (if $from == "direct" then $gate_from else $from end))
      and (.payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.production_projection | type == "string"
        and . == "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver")
      and (.assets | type == "object" and (keys | sort) == $pair_asset_keys
        and all(.[]; ((.state == "present"
          and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
          or (.state == "absent" and .sha256 == null)
          or (.state == "projection"
            and (.target | type == "string" and length > 0)
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))))))
    and (.installed_assets | type == "object" and (keys | sort) == $pair_asset_keys
      and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
    and (.installed_projections | type == "object" and (keys | sort) == $pair_asset_keys
      and all(.[]; type == "string" and length > 0))
    and (.installed_controller_projections | type == "object"
      and (keys | sort) == $controller_projection_keys
      and all(.[];
        (.target | type == "string" and test("^/opt/monday/releases/binance-lob-controller/active/deployment/[A-Za-z0-9._-]+$"))
        and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))))' \
    "$receipt" >/dev/null
  jq -e --argjson expected "$gate_evidence" \
    '.gate_evidence == $expected' "$receipt" >/dev/null
  jq -e --argjson expected "$gate_production_runtime" \
    '.production_runtime == $expected' "$receipt" >/dev/null
  local completed_at completed_at_ns parsed_completed_at now_ns
  completed_at=$(jq -er '.completed_at' "$receipt") || return 1
  completed_at_ns=$(jq -er '.completed_at_ns' "$receipt") || return 1
  [[ $completed_at_ns =~ ^[0-9]+$ ]] || return 1
  parsed_completed_at=$(monday_iso_epoch_ns "$completed_at") || return 1
  [[ $parsed_completed_at == "$completed_at_ns" ]] || return 1
  now_ns=$(date +%s%N) || return 1
  [[ $now_ns =~ ^[0-9]+$ && completed_at_ns -le now_ns ]] || return 1
}

# Verify one production upload-status triplet using an injected copy function.
# The caller owns the OSS credentials; this helper owns URI identity, triplet
# bytes, marker content, and manifest re-download consistency.
monday_verify_upload_triplet_readback() {
  [[ $# -eq 8 || $# -eq 9 ]] || return 2
  local status=$1 market=$2 dataset=$3 expected_bucket=$4 expected_prefix=$5
  local tmp_root=$6 minimum_success_at=$7 copy_fn=$8 expected_session=${9:-}
  local triplet data_uri manifest_uri success_uri status_session triplet_session
  local data_sha manifest_sha success_sha object_prefix first_manifest second_manifest uploaded_at uploaded_ns
  local data_file manifest_file success_file expected_success_file expected_prefix_norm data_file_name
  local success_at success_ns start_ns end_ns minimum_ns now_ns manifest_session
  monday_file_direct "$status" || return 1
  jq -e '
    type == "object"
    and ((.last_error? // null) == null)
    and ((.last_error_at? // null) == null)
    and (((.failure_count? // 0) | type == "number" and floor == . and . == 0))
    and (((.discovery_failed? // false) == false))
    and (((.pending? // false) as $pending
      | ($pending == false
        or ($pending | type == "number" and floor == . and . == 0))))
    and (((.pending_batches? // 0) | type == "number" and floor == . and . == 0))
    and (((.pending_segments? // 0) | type == "number" and floor == . and . == 0))
    and (((.failed_batches? // []) | type == "array" and length == 0))
    and (((.failed_segments? // []) | type == "array" and length == 0))
  ' "$status" >/dev/null || return 1
  status_session=$(jq -er '.session_id // empty' "$status") || true
  if [[ -n $expected_session && -n $status_session ]]; then
    [[ $status_session == "$expected_session" ]] || return 1
  fi
  [[ $market == spot || $market == usdm ]] || return 1
  monday_validate_component "$dataset" || return 1
  monday_validate_component "$expected_bucket" || return 1
  expected_prefix_norm=${expected_prefix%/}
  monday_validate_oss_prefix "$market" "$dataset" "$expected_prefix_norm" || return 1
  declare -F "$copy_fn" >/dev/null 2>&1 || return 2
  triplet=$(jq -cer '.last_uploaded_triplet | objects' "$status") || return 1
  triplet_session=$(jq -er '.session_id // empty' <<<"$triplet") || true
  if [[ -n $expected_session && -n $triplet_session ]]; then
    [[ $triplet_session == "$expected_session" ]] || return 1
  fi
  data_sha=$(jq -er '.data_sha256' <<<"$triplet") || return 1
  manifest_sha=$(jq -er '.manifest_sha256' <<<"$triplet") || return 1
  success_sha=$(jq -er '.success_sha256' <<<"$triplet") || return 1
  monday_sha256_ok "$data_sha" && monday_sha256_ok "$manifest_sha" \
    && monday_sha256_ok "$success_sha" || return 1
  object_prefix=$(jq -er '.object_prefix' <<<"$triplet") || return 1
  [[ $object_prefix == "$expected_prefix_norm" ]] || return 1
  data_uri=$(jq -er '.data_uri // .object // empty' <<<"$triplet") || true
  if [[ -z $data_uri ]]; then data_uri=$(jq -er '.last_uploaded_object' "$status") || return 1; fi
  data_file_name=${data_uri##*/}
  [[ $data_file_name =~ ^[A-Za-z0-9._-]+\.jsonl\.zst$ \
    && $data_uri == "oss://$expected_bucket/$expected_prefix_norm/$data_file_name" \
    && $data_uri != *'%'* && $data_uri != *'\\'* ]] || return 1
  manifest_uri=$(jq -er '.manifest_uri // empty' <<<"$triplet") || true
  [[ -n $manifest_uri ]] || manifest_uri="$data_uri.manifest.json"
  success_uri=$(jq -er '.success_uri // empty' <<<"$triplet") || true
  [[ -n $success_uri ]] || success_uri="$data_uri._SUCCESS"
  [[ $manifest_uri == "$data_uri.manifest.json" && $success_uri == "$data_uri._SUCCESS" \
    && $manifest_uri != *'%'* && $manifest_uri != *'\\'* \
    && $success_uri != *'%'* && $success_uri != *'\\'* ]] || return 1
  mkdir -p "$tmp_root" || return 1
  first_manifest="$tmp_root/$market.manifest.first"; second_manifest="$tmp_root/$market.manifest.second"
  data_file="$tmp_root/$market.data"; manifest_file="$tmp_root/$market.manifest"; success_file="$tmp_root/$market.success"
  expected_success_file="$tmp_root/$market.success.expected"
  "$copy_fn" "$manifest_uri" "$first_manifest" || return 1
  "$copy_fn" "$data_uri" "$data_file" || return 1
  "$copy_fn" "$success_uri" "$success_file" || return 1
  "$copy_fn" "$manifest_uri" "$second_manifest" || return 1
  monday_file_direct "$first_manifest" && monday_file_direct "$data_file" \
    && monday_file_direct "$success_file" && monday_file_direct "$second_manifest" \
    && cmp -s "$first_manifest" "$second_manifest" || return 1
  cp -p -- "$first_manifest" "$manifest_file" || return 1
  [[ $(monday_sha256_file "$data_file") == "$data_sha" \
    && $(monday_sha256_file "$manifest_file") == "$manifest_sha" ]] || return 1
  printf '%s\n' "$data_sha" >"$expected_success_file"
  cmp -s "$success_file" "$expected_success_file" || return 1
  if [[ $success_sha != "$data_sha" ]]; then
    [[ $(monday_sha256_file "$success_file") == "$success_sha" ]] || return 1
  fi
  jq -e --arg market "$market" --arg dataset "$dataset" --arg data_sha "$data_sha" \
    --arg data_file "${data_uri##*/}" '
      type == "object" and .market == $market and .dataset == $dataset
      and .file == $data_file and .sha256 == $data_sha
      and .shard_id == "all"
      and (.start_received_at_ns | type == "number" and floor == . and . >= 0)
      and (.end_received_at_ns | type == "number" and floor == . and . >= 0)
      and .end_received_at_ns >= .start_received_at_ns
      and ((.session_id // .lob_continuity.capture_session_id)
        | type == "string" and length > 0)
      and (.catalog_sha256? // "" | type == "string")' \
    "$manifest_file" >/dev/null || return 1
  success_at=$(jq -er '.last_success_at' "$status") || return 1
  success_ns=$(monday_iso_epoch_ns "$success_at") || return 1
  now_ns=$(date +%s%N) || return 1
  [[ $now_ns =~ ^[0-9]+$ && $success_ns -le $now_ns ]] || return 1
  minimum_ns=0
  if [[ -n $minimum_success_at ]]; then
    minimum_ns=$(monday_iso_epoch_ns "$minimum_success_at") || return 1
    [[ $success_ns -ge $minimum_ns ]] || return 1
  fi
  start_ns=$(jq -er '.start_received_at_ns' "$manifest_file") || return 1
  end_ns=$(jq -er '.end_received_at_ns' "$manifest_file") || return 1
  [[ $start_ns =~ ^[0-9]+$ && $end_ns =~ ^[0-9]+$ \
    && $start_ns -ge $minimum_ns && $end_ns -ge $start_ns && $end_ns -le $now_ns ]] || return 1
  uploaded_at=$(jq -er '.uploaded_at // empty' <<<"$triplet") || true
  if [[ -n $uploaded_at ]]; then
    uploaded_ns=$(monday_iso_epoch_ns "$uploaded_at") || return 1
    [[ $uploaded_ns -le $now_ns && $uploaded_ns -ge $minimum_ns ]] || return 1
  fi
  manifest_session=$(jq -er '.session_id // .lob_continuity.capture_session_id' "$manifest_file") || return 1
  if [[ -n $expected_session ]]; then
    [[ $manifest_session == "$expected_session" ]] || return 1
  fi
  jq -cn --arg market "$market" --arg data_uri "$data_uri" --arg manifest_uri "$manifest_uri" \
    --arg success_uri "$success_uri" --arg data_sha "$data_sha" --arg manifest_sha "$manifest_sha" \
    --arg success_sha "$success_sha" --arg object_prefix "$object_prefix" \
    --arg last_success_at "$(jq -er '.last_success_at' "$status")" \
    --arg session "$manifest_session" \
    --arg catalog "$(jq -er '.catalog_sha256 // ""' "$manifest_file")" \
    --argjson start "$start_ns" --argjson end "$end_ns" \
    '{market:$market,data_uri:$data_uri,manifest_uri:$manifest_uri,success_uri:$success_uri,
      data_sha256:$data_sha,manifest_sha256:$manifest_sha,success_sha256:$success_sha,
      success_content:($data_sha + "\n"),object_prefix:$object_prefix,last_success_at:$last_success_at,
      start_received_at_ns:$start,end_received_at_ns:$end,
      session_id:$session,catalog_sha256:$catalog}'
}

monday_atomic_symlink() {
  [[ $# -eq 2 ]] || return 2
  local target=$1 link=$2 temporary resolved
  resolved=$(readlink -f -- "$target") || return 1
  temporary="$link.new.$$"
  rm -f -- "$temporary"
  ln -s "$target" "$temporary"
  if [[ $(uname -s) == Darwin ]]; then
    # macOS mv follows a directory symlink; remove the link while the
    # operation lock is held, then rename the staged link into place.
    rm -f -- "$link"
    mv -f -- "$temporary" "$link"
  else
    mv -Tf -- "$temporary" "$link"
  fi
  [[ -L $link && $(readlink -f -- "$link") == "$resolved" ]]
}
