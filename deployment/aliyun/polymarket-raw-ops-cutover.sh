#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly POLICY="$SCRIPT_DIR/polymarket-shadow-gate-policy.jq"
readonly LEGACY_HEALTH_POLICY="$SCRIPT_DIR/polymarket-legacy-health-policy.jq"
readonly RUST_HEALTH_POLICY="$SCRIPT_DIR/polymarket-rust-health-policy.jq"
readonly RELEASE_MANIFEST_SCHEMA=monday.polymarket_raw_ops_release.v1
readonly RELEASE_MANIFEST="$SCRIPT_DIR/polymarket-raw-ops-release.json"
readonly RELEASE_ROOT=/opt/monday/releases/polymarket-raw-ops
readonly CANDIDATE_ROOT=/opt/monday/candidates/polymarket-raw-ops
readonly ACTIVE_BINARY=/opt/monday/bin/polymarket-raw-ops
readonly CONTROL_DIR=/opt/monday/control/polymarket-raw-ops
readonly EVIDENCE_ROOT=/data/monday/evidence/polymarket-cutovers
readonly GATE_RECEIPT_ROOT=/data/monday/evidence/polymarket-gate-jobs
readonly GATE_EVIDENCE_ROOT=/data/monday/evidence/polymarket-shadow-gates
readonly MAX_GATE_AGE_SECONDS=86400
readonly LOCK_FILE=/run/monday/polymarket-raw-ops.lock
readonly COLLECTOR_UNIT=polymarket-reference-collector.service
readonly REFERENCE_UPLOAD_UNIT=polymarket-reference-upload.service
readonly REFERENCE_UPLOAD_TIMER=polymarket-reference-upload.timer
readonly MARKET_UPLOAD_UNIT=polymarket-market-tape-upload.service
readonly MARKET_UPLOAD_TIMER=polymarket-market-tape-upload.timer
readonly HEALTH=/data/monday/spool/polymarket-reference/health.json
readonly LEGACY_COLLECTOR=/opt/monday/bin/polymarket_reference_collector.py
readonly LEGACY_UPLOADER=/opt/monday/bin/polymarket_market_tape_upload.py
readonly LEGACY_EXEC="/usr/bin/python3 $LEGACY_COLLECTOR"
readonly RUST_EXEC="$ACTIVE_BINARY collect-reference"
readonly COLLECTOR_FRAGMENT="/etc/systemd/system/$COLLECTOR_UNIT"
readonly LEGACY_REFERENCE_UPLOAD_EXEC="/usr/bin/python3 $LEGACY_UPLOADER --spool-dir /data/monday/spool/polymarket-reference --dataset crypto_expiry_reference --quote-depth-levels 0 --quote-sample-ms 0"
readonly REFERENCE_UPLOAD_EXEC="$ACTIVE_BINARY upload --spool-dir /data/monday/spool/polymarket-reference --dataset crypto_expiry_reference --quote-depth-levels 0 --quote-sample-ms 0"
readonly MARKET_UPLOAD_EXEC="$ACTIVE_BINARY upload --quote-depth-levels 0 --quote-sample-ms 1000"
readonly UPLOAD_ENV=/etc/monday/polymarket-market-tape-upload.env
readonly MAX_HEALTH_SILENCE_SECONDS=240
readonly -a UNIT_ASSETS=(
  polymarket-reference-collector.service
  polymarket-reference-upload.service
  polymarket-reference-upload.timer
  polymarket-market-tape-upload.service
  polymarket-market-tape-upload.timer
)
readonly -a PYTHON_ASSETS=(
  polymarket_reference_collector.py
  polymarket_market_tape_upload.py
)
readonly -a BUNDLE_ASSETS=(
  polymarket-raw-ops-gate-control.sh
  polymarket-raw-ops-gate@.service
  polymarket-raw-ops-shadow-gate.sh
  polymarket-raw-ops-cutover.sh
  polymarket-shadow-gate-policy.jq
  polymarket-legacy-health-policy.jq
  polymarket-rust-health-policy.jq
  polymarket-reference-collector-shadow@.service
  polymarket-reference-collector.service
  polymarket-reference-upload.service
  polymarket-reference-upload.timer
  polymarket-market-tape-upload.service
  polymarket-market-tape-upload.timer
)
readonly -a STAGE_ARTIFACT_ASSETS=(
  polymarket-raw-ops
  polymarket-raw-ops.sha256
  source-revision.txt
  deployment-bundle.sha256
  polymarket-raw-ops-release.json
  polymarket-raw-ops-release.json.sha256
  polymarket-raw-ops-control-assets.sha256
  polymarket-raw-ops-control.tar.gz
  polymarket-raw-ops-control.tar.gz.sha256
)

die() {
  printf 'Polymarket cutover failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage:' \
    '  polymarket-raw-ops-cutover.sh stage <artifact-directory> <expected-source-revision>' \
    '  polymarket-raw-ops-cutover.sh cutover <candidate-sha256> <receipt.json>' \
    '  polymarket-raw-ops-cutover.sh rollback <cutover-evidence-directory>'
}

bundle_sha256() {
  local directory=${1:-$SCRIPT_DIR}
  (
    cd "$directory"
    sha256sum "${BUNDLE_ASSETS[@]}" | sha256sum | awk '{print $1}'
  )
}

release_control_assets() {
  local control_dir=$1 gate=$1/polymarket-raw-ops-shadow-gate.sh
  [[ -f $gate && ! -L $gate ]] || return 1
  awk '
    $0 == "readonly -a BUNDLE_ASSETS=(" {
      if (found || inside) exit 2
      found = 1
      inside = 1
      next
    }
    inside && $0 == ")" {
      inside = 0
      closed = 1
      next
    }
    inside {
      if ($0 !~ /^  [A-Za-z0-9@._][A-Za-z0-9@._-]*$/) exit 2
      sub(/^  /, "")
      if ($0 == "." || $0 == ".." || seen[$0]++) exit 2
      if ($0 == "polymarket-raw-ops-shadow-gate.sh") has_gate = 1
      print
      count += 1
    }
    END {
      if (!found || !closed || inside || count == 0 || !has_gate) exit 2
    }
  ' "$gate"
}

direct_directory() {
  local path=$1
  [[ -d $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]
}

secure_root_directory() {
  local path=$1 owner mode
  direct_directory "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == 0 ]] && (( (8#$mode & 0022) == 0 ))
}

valid_absolute_path() {
  local path=$1
  [[ $path == /* && $path != *//* && $path != */./* && $path != */../* \
    && $path != */. && $path != */.. ]]
}

secure_root_chain() {
  local path=$1 remainder component current=
  valid_absolute_path "$path" || return 1
  if [[ $path == / ]]; then
    secure_root_directory /
    return
  fi
  remainder=${path#/}
  while [[ -n $remainder ]]; do
    component=${remainder%%/*}
    [[ -n $component ]] || return 1
    current="$current/$component"
    secure_root_directory "$current" || return 1
    [[ $remainder == "$component" ]] && break
    remainder=${remainder#*/}
  done
}

secure_root_chain_or_absent() {
  local path=$1 ancestor=$1 parent
  valid_absolute_path "$path" || return 1
  [[ ! -L $path ]] || return 1
  if [[ -e $path ]]; then
    secure_root_chain "$path"
    return
  fi
  while [[ ! -e $ancestor && ! -L $ancestor ]]; do
    parent=${ancestor%/*}
    [[ -n $parent ]] || parent=/
    [[ $parent != "$ancestor" ]] || return 1
    ancestor=$parent
  done
  [[ ! -L $ancestor ]] || return 1
  secure_root_chain "$ancestor"
}

secure_collector_directory() {
  local path=$1 owner group mode parent
  direct_directory "$path" || return 1
  owner=$(stat -c %U -- "$path") || return 1
  group=$(stat -c %G -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == hftcollector && $group == hftcollector && $mode == 750 ]] || return 1
  parent=${path%/*}
  secure_root_chain "$parent"
}

secure_release_directory() {
  local path=$1 owner mode
  secure_root_chain "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == 0 && $mode == 755 ]]
}

secure_regular_file() {
  local path=$1 mode owner
  [[ -f $path && ! -L $path ]] || die "required direct regular file is missing: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == 0 ]] || die "required file is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) \
    || die "required file is group/world writable: $path"
}

verify_release_manifest() {
  local manifest=$1
  secure_regular_file "$manifest" || return 1
  jq -e -s --arg schema "$RELEASE_MANIFEST_SCHEMA" '
    length == 1 and (.[0] |
      .schema == $schema
      and (keys | sort) == (["candidate","control_archive","control_manifest",
        "schema","source_revision"] | sort)
      and (.source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
      and .candidate.file == "polymarket-raw-ops"
      and (.candidate | keys | sort) == ["file","sha256"]
      and (.candidate.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and .control_manifest.file == "polymarket-raw-ops-control-assets.sha256"
      and (.control_manifest | keys | sort) == ["file","sha256"]
      and (.control_manifest.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and .control_archive.file == "polymarket-raw-ops-control.tar.gz"
      and (.control_archive | keys | sort) == ["file","sha256"]
      and (.control_archive.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    )
  ' "$manifest" >/dev/null
}

verify_release_binding() {
  local manifest=$1 expected_manifest_sha=$2 expected_candidate_sha=$3
  local expected_source_revision=$4 expected_bundle_sha=$5 expected_archive_sha=$6
  local candidate=$7 control_dir=${8:-$SCRIPT_DIR}
  verify_release_manifest "$manifest" || return 1
  [[ $(sha256sum "$manifest" | awk '{print $1}') == "$expected_manifest_sha" ]] \
    || return 1
  [[ $(jq -er -s '.[0].candidate.sha256' "$manifest") \
    == "$expected_candidate_sha" ]] || return 1
  [[ $(jq -er -s '.[0].source_revision' "$manifest") \
    == "$expected_source_revision" ]] || return 1
  [[ $(jq -er -s '.[0].control_manifest.sha256' "$manifest") \
    == "$expected_bundle_sha" ]] || return 1
  [[ $(jq -er -s '.[0].control_archive.sha256' "$manifest") \
    == "$expected_archive_sha" ]] || return 1
  [[ $(bundle_sha256 "$control_dir") == "$expected_bundle_sha" ]] || return 1
  printf '%s  %s\n' "$expected_candidate_sha" "$candidate" \
    | sha256sum --check --strict >/dev/null
}

stage_release() (
  local artifact_dir=$1 candidate_root=$2 expected_source_revision=$3
  local manifest manifest_sha candidate_sha
  local source_revision bundle_sha archive_sha destination staging='' published=''
  local control_extract expected_entries actual_entries expected_control_manifest
  local asset mode
  artifact_dir=$(readlink -f -- "$artifact_dir")
  secure_root_chain "$artifact_dir" || die 'artifact directory is not trusted'
  secure_root_chain "$candidate_root" || die 'candidate root is not trusted'
  for asset in "${STAGE_ARTIFACT_ASSETS[@]}"; do
    secure_regular_file "$artifact_dir/$asset"
  done

  manifest="$artifact_dir/polymarket-raw-ops-release.json"
  verify_release_manifest "$manifest" || die 'release manifest is invalid'
  manifest_sha=$(sha256sum "$manifest" | awk '{print $1}')
  [[ $(wc -l <"$artifact_dir/polymarket-raw-ops-release.json.sha256") -eq 1 \
    && $(<"$artifact_dir/polymarket-raw-ops-release.json.sha256") \
      == "$manifest_sha  polymarket-raw-ops-release.json" ]] \
    || die 'release manifest checksum sidecar is invalid'
  candidate_sha=$(jq -er '.candidate.sha256' "$manifest")
  source_revision=$(jq -er '.source_revision' "$manifest")
  [[ $expected_source_revision =~ ^[a-f0-9]{40,64}$ \
    && $source_revision == "$expected_source_revision" ]] \
    || die 'release manifest differs from the trusted source revision'
  bundle_sha=$(jq -er '.control_manifest.sha256' "$manifest")
  archive_sha=$(jq -er '.control_archive.sha256' "$manifest")
  [[ $(wc -l <"$artifact_dir/polymarket-raw-ops.sha256") -eq 1 \
    && $(<"$artifact_dir/polymarket-raw-ops.sha256") \
      == "$candidate_sha  polymarket-raw-ops" ]] \
    || die 'candidate checksum sidecar is invalid'
  printf '%s  %s\n' "$candidate_sha" "$artifact_dir/polymarket-raw-ops" \
    | sha256sum --check --strict >/dev/null || die 'candidate checksum mismatch'
  [[ -x $artifact_dir/polymarket-raw-ops ]] || die 'candidate is not executable'
  [[ $(wc -l <"$artifact_dir/source-revision.txt") -eq 1 \
    && $(<"$artifact_dir/source-revision.txt") == "$source_revision" ]] \
    || die 'source revision sidecar differs from the release manifest'
  [[ $(wc -l <"$artifact_dir/deployment-bundle.sha256") -eq 1 \
    && $(<"$artifact_dir/deployment-bundle.sha256") == "$bundle_sha" ]] \
    || die 'deployment bundle sidecar differs from the release manifest'
  [[ $(sha256sum "$artifact_dir/polymarket-raw-ops-control-assets.sha256" \
      | awk '{print $1}') == "$bundle_sha" ]] \
    || die 'control manifest checksum differs from the release manifest'
  [[ $(wc -l <"$artifact_dir/polymarket-raw-ops-control.tar.gz.sha256") -eq 1 \
    && $(<"$artifact_dir/polymarket-raw-ops-control.tar.gz.sha256") \
      == "$archive_sha  polymarket-raw-ops-control.tar.gz" ]] \
    || die 'control archive checksum sidecar is invalid'
  printf '%s  %s\n' "$archive_sha" "$artifact_dir/polymarket-raw-ops-control.tar.gz" \
    | sha256sum --check --strict >/dev/null || die 'control archive checksum mismatch'

  destination="$candidate_root/$manifest_sha"
  [[ ! -e $destination && ! -L $destination ]] \
    || die 'immutable candidate destination already exists'
  staging=$(mktemp -d "$candidate_root/.${manifest_sha}.new.XXXXXX")
  published=
  trap '[[ -z ${staging:-} ]] || rm -rf -- "$staging"; \
    [[ -z ${published:-} ]] || rm -rf -- "$published"' EXIT
  control_extract="$staging/.controls"
  mkdir -m 0700 "$control_extract"
  expected_entries="$staging/.expected-entries"
  actual_entries="$staging/.actual-entries"
  printf '%s\n' "${BUNDLE_ASSETS[@]}" | sort >"$expected_entries"
  tar -tzf "$artifact_dir/polymarket-raw-ops-control.tar.gz" | sort >"$actual_entries"
  cmp -s "$expected_entries" "$actual_entries" \
    || die 'control archive entries differ from the governed bundle'
  tar --no-same-owner --no-same-permissions \
    -xzf "$artifact_dir/polymarket-raw-ops-control.tar.gz" -C "$control_extract"
  expected_control_manifest="$staging/.control-assets.sha256"
  for asset in "${BUNDLE_ASSETS[@]}"; do
    [[ -f $control_extract/$asset && ! -L $control_extract/$asset ]] \
      || die "control archive entry is not a direct regular file: $asset"
    secure_regular_file "$SCRIPT_DIR/$asset"
    cmp -s "$SCRIPT_DIR/$asset" "$control_extract/$asset" \
      || die "control archive entry differs from the trusted source: $asset"
  done
  (
    cd "$control_extract"
    sha256sum "${BUNDLE_ASSETS[@]}"
  ) >"$expected_control_manifest"
  cmp -s "$expected_control_manifest" \
    "$artifact_dir/polymarket-raw-ops-control-assets.sha256" \
    || die 'extracted controls differ from the signed control manifest'
  for asset in "${STAGE_ARTIFACT_ASSETS[@]}"; do
    mode=0444; [[ $asset == polymarket-raw-ops ]] && mode=0755
    install -m "$mode" "$artifact_dir/$asset" "$staging/$asset"
  done
  for asset in "${BUNDLE_ASSETS[@]}"; do
    mode=0644; [[ $asset == *.sh ]] && mode=0755
    install -m "$mode" "$control_extract/$asset" "$staging/$asset"
  done
  rm -rf -- "$control_extract" "$expected_entries" "$actual_entries" \
    "$expected_control_manifest"
  chmod 0755 "$staging"
  mv -T -n "$staging" "$destination"
  [[ ! -e $staging && -d $destination && ! -L $destination ]] \
    || die 'immutable candidate destination appeared during atomic publication'
  staging=
  published=$destination
  sync -f "$candidate_root"
  published=
  printf '%s\n' "$destination"
)

verify_control_release() {
  local control_dir=$1 expected_sha=$2 expected_binary=$3 manifest asset assets
  local actual_bundle_sha expected_bundle_sha
  local -a release_assets=()
  manifest="$control_dir/${RELEASE_MANIFEST##*/}"
  secure_regular_file "$manifest" || return 1
  verify_release_manifest "$manifest" || return 1
  assets=$(release_control_assets "$control_dir") || return 1
  while IFS= read -r asset; do
    [[ $asset != "${RELEASE_MANIFEST##*/}" ]] || return 1
    release_assets+=("$asset")
    secure_regular_file "$control_dir/$asset" || return 1
  done <<<"$assets"
  actual_bundle_sha=$(
    cd "$control_dir"
    sha256sum -- "${release_assets[@]}" | sha256sum | awk '{print $1}'
  ) || return 1
  expected_bundle_sha=$(jq -er '.control_manifest.sha256' "$manifest") || return 1
  [[ $actual_bundle_sha == "$expected_bundle_sha" ]] || return 1
  [[ $(jq -er '.candidate.sha256' "$manifest") == "$expected_sha" ]] || return 1
  printf '%s  %s\n' "$expected_sha" "$expected_binary" \
    | sha256sum --check --strict >/dev/null
}

rollback_control_files() {
  local state=$1
  jq -er --arg gate polymarket-raw-ops-shadow-gate.sh \
    --arg manifest "${RELEASE_MANIFEST##*/}" '
      .control_files
      | select(type == "array" and length >= 2)
      | . as $files
      | select(
          all(.[];
            type == "string"
            and test("^[A-Za-z0-9@._][A-Za-z0-9@._-]*$")
            and . != "." and . != ".."
          )
          and (unique | length) == ($files | length)
          and index($gate) != null
          and index($manifest) != null
        )
      | .[]
    ' "$state"
}

remove_snapshotted_control_files() {
  local state=$1 control_dir_present files asset
  control_dir_present=$(
    jq -er '.control_dir_present | select(type == "boolean") | tostring' "$state"
  ) || return 1
  [[ $control_dir_present == true ]] || return 0
  files=$(rollback_control_files "$state") || return 1
  while IFS= read -r asset; do
    rm -f -- "$CONTROL_DIR/$asset" || return 1
  done <<<"$files"
}

install_control_release() {
  local source=$1 asset mode
  [[ -d $CONTROL_DIR ]] || install -d -m 0755 "$CONTROL_DIR"
  for asset in "${BUNDLE_ASSETS[@]}"; do
    mode=0644; [[ $asset == *.sh ]] && mode=0755
    atomic_install "$mode" "$source/$asset" "$CONTROL_DIR/$asset"
  done
  atomic_install 0444 "$source/${RELEASE_MANIFEST##*/}" \
    "$CONTROL_DIR/${RELEASE_MANIFEST##*/}"
}

verify_gate_marker() {
  local gate_dir=$1 marker expected actual line_count
  marker="$gate_dir/PASSED.sha256"
  [[ -f $marker && ! -L $marker ]] || return 1
  line_count=$(wc -l <"$marker") || return 1
  [[ $line_count -eq 1 ]] || return 1
  expected=$(cd "$gate_dir" && sha256sum gate.json) || return 1
  actual=$(<"$marker") || return 1
  [[ $actual == "$expected" ]] || return 1
  (
    cd "$gate_dir"
    sha256sum --check --strict PASSED.sha256 >/dev/null
  )
}

verify_gate_terminal_receipt() {
  local receipt=$1 candidate_sha=$2 invocation source_revision receipt_dir
  local expected_receipt gate_dir gate_json gate_json_sha receipt_sha
  [[ -f $receipt && ! -L $receipt ]] || return 1
  receipt=$(readlink -f -- "$receipt") || return 1
  secure_regular_file "$receipt" || return 1
  receipt_sha=$(sha256sum "$receipt" | awk '{print $1}') || return 1
  jq -e -s --arg candidate "$candidate_sha" '
    length == 1 and (.[0] |
      keys == ["candidate_sha256","phase","schema","shadow",
        "source_revision","systemd","systemd_invocation_id","terminal_state",
        "unit"]
      and (.systemd | keys == ["exit_code","exit_status","result"])
      and (.shadow | keys == ["active_state","containment","main_pid",
        "stop_result","unit"])
      and .schema == "monday.polymarket_gate_receipt.v1"
      and .candidate_sha256 == $candidate
      and (.source_revision | type == "string"
        and test("^[a-f0-9]{40,64}$"))
      and (.systemd_invocation_id | type == "string"
        and test("^[a-f0-9]{32}$"))
      and .unit == ("polymarket-raw-ops-gate@" + $candidate + ".service")
      and .phase == "terminal" and .terminal_state == "passed"
      and .systemd.result == "success" and .systemd.exit_code == "exited"
      and .systemd.exit_status == "0"
      and .shadow.unit ==
        ("polymarket-reference-collector-shadow@" + $candidate + ".service")
      and .shadow.stop_result == "success"
      and .shadow.containment == "contained"
      and (.shadow.active_state == "inactive" or .shadow.active_state == "failed")
      and .shadow.main_pid == "0")' "$receipt" >/dev/null || return 1
  invocation=$(jq -er .systemd_invocation_id "$receipt") || return 1
  source_revision=$(jq -er .source_revision "$receipt") || return 1
  receipt_dir="$GATE_RECEIPT_ROOT/$candidate_sha/$invocation"
  expected_receipt="$receipt_dir/receipt.json"
  [[ $receipt == "$expected_receipt" ]] || return 1
  secure_root_chain "$receipt_dir" || return 1
  gate_dir="$GATE_EVIDENCE_ROOT/$candidate_sha/$invocation"
  gate_json="$gate_dir/gate.json"
  secure_root_chain "$gate_dir" || return 1
  secure_regular_file "$gate_json" || return 1
  secure_regular_file "$gate_dir/PASSED.sha256" || return 1
  verify_gate_marker "$gate_dir" || return 1
  jq -e --arg candidate "$candidate_sha" --arg source "$source_revision" \
    --arg invocation "$invocation" '
    .candidate_sha256 == $candidate
    and .deployment_source_revision == $source
    and .shadow_run_id == $invocation
    and .production_eligible == true and .passed == true
  ' "$gate_json" >/dev/null || return 1
  gate_json_sha=$(sha256sum "$gate_json" | awk '{print $1}') || return 1
  [[ $(sha256sum "$receipt" | awk '{print $1}') == "$receipt_sha" ]] || return 1
  printf '%s|%s|%s|%s|%s|%s\n' "$invocation" "$source_revision" \
    "$receipt_sha" "$gate_json_sha" "$gate_json" "$receipt"
}

verify_named_marker() {
  local evidence_dir=$1 json_name=$2 marker_name=$3 expected actual line_count
  [[ -f $evidence_dir/$json_name && ! -L $evidence_dir/$json_name ]] || return 1
  [[ -f $evidence_dir/$marker_name && ! -L $evidence_dir/$marker_name ]] || return 1
  line_count=$(wc -l <"$evidence_dir/$marker_name") || return 1
  [[ $line_count -eq 1 ]] || return 1
  expected=$(cd "$evidence_dir" && sha256sum "$json_name") || return 1
  actual=$(<"$evidence_dir/$marker_name") || return 1
  [[ $actual == "$expected" ]] || return 1
  (
    cd "$evidence_dir"
    sha256sum --check --strict "$marker_name" >/dev/null
  )
}

prepare_rollback_evidence() {
  local evidence_dir=$1
  local marker=$evidence_dir/PASSED.sha256
  local pending=$evidence_dir/PASSED.rollback-pending.sha256
  local cutover=$evidence_dir/cutover.json
  secure_root_chain "$evidence_dir" || return 1
  if [[ -e $marker || -L $marker ]]; then
    secure_regular_file "$marker"
    secure_regular_file "$cutover"
    verify_named_marker "$evidence_dir" cutover.json PASSED.sha256 \
      || die 'cutover success marker does not verify the exact cutover evidence'
    [[ ! -e $pending && ! -L $pending ]] \
      || die 'rollback-pending marker path already exists'
    mv -Tf "$marker" "$pending" || return 1
    sync "$pending" || return 1
    sync -f "$evidence_dir" || return 1
  elif [[ -e $pending || -L $pending ]]; then
    secure_regular_file "$pending"
    secure_regular_file "$cutover"
    verify_named_marker "$evidence_dir" cutover.json PASSED.rollback-pending.sha256 \
      || die 'rollback-pending marker does not verify the exact cutover evidence'
  fi
}

finalize_rollback_evidence() {
  local evidence_dir=$1 label=$2
  local pending=$evidence_dir/PASSED.rollback-pending.sha256
  local final=$evidence_dir/PASSED.$label.sha256
  [[ $label == invalid || $label == rolled-back ]] || return 1
  secure_root_chain "$evidence_dir" || return 1
  if [[ -e $pending || -L $pending ]]; then
    verify_named_marker "$evidence_dir" cutover.json PASSED.rollback-pending.sha256 \
      || return 1
    [[ ! -e $final && ! -L $final ]] || return 1
    mv -Tf "$pending" "$final" || return 1
    sync "$final" || return 1
  fi
  sync -f "$evidence_dir"
}

effective_exec_argv() {
  local unit=$1 raw argv
  raw=$(systemctl show --property=ExecStart --value "$unit") || return 1
  argv=$(sed -nE 's/^.*argv\[\]=([^;]+);.*$/\1/p' <<<"$raw" \
    | sed -E 's/[[:space:]]+$//')
  [[ -n $argv ]] || return 1
  printf '%s\n' "$argv"
}

proc_cmdline() {
  local pid=$1
  [[ $pid =~ ^[1-9][0-9]*$ && -r /proc/$pid/cmdline ]] || return 1
  tr '\0' ' ' <"/proc/$pid/cmdline"
}

journal_cursor() {
  local unit=$1 cursor
  journalctl --sync || return 1
  cursor=$(journalctl --unit "$unit" --lines=0 --show-cursor --no-pager \
    | sed -n 's/^-- cursor: //p') || return 1
  [[ -n $cursor ]] || return 1
  printf '%s\n' "$cursor"
}

verify_no_restart_after_cursor() {
  local unit=$1 cursor=$2 expected_invocation_id=$3
  journalctl --sync || return 1
  journalctl --unit "$unit" --after-cursor "$cursor" --output=json --no-pager \
    | jq -s -e --arg expected "$expected_invocation_id" '
      all(.[];
        ((.MESSAGE_ID // "") != "5eb03494b6584870a536b337290809b3")
        and ((.INVOCATION_ID // "") | length == 0 or . == $expected)
        and ((._SYSTEMD_INVOCATION_ID // "") | length == 0 or . == $expected)
      )
    ' >/dev/null || return 1
}

verify_effective_unit() {
  local unit=$1 expected_fragment=$2 expected_exec=$3 fragment drop_ins exec_argv
  fragment=$(systemctl show --property=FragmentPath --value "$unit") || return 1
  [[ $fragment == "$expected_fragment" ]] || return 1
  drop_ins=$(systemctl show --property=DropInPaths --value "$unit") || return 1
  [[ -z $drop_ins ]] || return 1
  exec_argv=$(effective_exec_argv "$unit") || return 1
  [[ $exec_argv == "$expected_exec" ]]
}

env_value() {
  local key=$1 file=${2:-$UPLOAD_ENV} count value
  count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one $key"
  value=$(sed -n "s/^${key}=//p" "$file")
  [[ -n $value ]] || die "$file has an empty $key"
  printf '%s\n' "$value"
}

oss_config_sha256() {
  local file=${1:-$UPLOAD_ENV} key
  for key in OSS_BUCKET OSS_ENDPOINT OSS_REGION ALIYUN_PROFILE \
    ZSTD_TIMEOUT_SECONDS OSS_COPY_TIMEOUT_SECONDS; do
    printf '%s=%s\n' "$key" "$(env_value "$key" "$file")"
  done | sha256sum | awk '{print $1}'
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  install -m "$mode" "$source" "$temporary"
  mv -Tf "$temporary" "$destination"
}

unit_active() {
  systemctl is-active --quiet "$1"
}

unit_enabled() {
  systemctl is-enabled --quiet "$1"
}

verify_oneshot_success() {
  local unit=$1 result status
  result=$(systemctl show --property=Result --value "$unit")
  status=$(systemctl show --property=ExecMainStatus --value "$unit")
  [[ $result == success && $status == 0 ]]
}

verify_deferred_market_upload() {
  local expected_binary=$1 previous_invocation=$2 state pid proc_exe invocation
  for _ in $(seq 1 10); do
    systemctl is-failed --quiet "$MARKET_UPLOAD_UNIT" && return 1
    invocation=$(systemctl show --property=InvocationID --value "$MARKET_UPLOAD_UNIT")
    if [[ $invocation =~ ^[a-f0-9]{32}$ && $invocation != "$previous_invocation" ]]; then
      state=$(systemctl show --property=ActiveState --value "$MARKET_UPLOAD_UNIT")
      if [[ $state == inactive ]]; then
        verify_oneshot_success "$MARKET_UPLOAD_UNIT"
        return
      fi
      if [[ $state == active || $state == activating ]]; then
        pid=$(systemctl show --property=MainPID --value "$MARKET_UPLOAD_UNIT")
        if [[ ! $pid =~ ^[1-9][0-9]*$ ]]; then
          sleep 1
          continue
        fi
        if ! proc_exe=$(readlink -f -- "/proc/$pid/exe"); then
          sleep 1
          continue
        fi
        [[ $proc_exe == "$expected_binary" ]]
        return
      fi
    fi
    sleep 1
  done
  return 1
}

verify_upload_units() {
  local pinned_upload_env=$1 unit_file
  verify_effective_unit "$REFERENCE_UPLOAD_UNIT" \
    "/etc/systemd/system/$REFERENCE_UPLOAD_UNIT" "$REFERENCE_UPLOAD_EXEC" || return 1
  verify_effective_unit "$MARKET_UPLOAD_UNIT" \
    "/etc/systemd/system/$MARKET_UPLOAD_UNIT" "$MARKET_UPLOAD_EXEC" || return 1
  for unit_file in "/etc/systemd/system/$REFERENCE_UPLOAD_UNIT" \
    "/etc/systemd/system/$MARKET_UPLOAD_UNIT"; do
    secure_regular_file "$unit_file"
    [[ $(grep -c '^EnvironmentFile=' "$unit_file" || true) -eq 1 ]] || return 1
    grep -Fxq "EnvironmentFile=$pinned_upload_env" "$unit_file" || return 1
  done
  local timer drop_ins fragment
  for timer in "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"; do
    fragment=$(systemctl show --property=FragmentPath --value "$timer") || return 1
    [[ $fragment == "/etc/systemd/system/$timer" ]] || return 1
    drop_ins=$(systemctl show --property=DropInPaths --value "$timer") || return 1
    [[ -z $drop_ins ]] || return 1
  done
}

render_upload_unit() {
  local source=$1 destination=$2 pinned_upload_env=$3 temporary
  temporary="${destination}.new.$$"
  sed "s|^EnvironmentFile=.*$|EnvironmentFile=$pinned_upload_env|" \
    "$source" >"$temporary"
  chmod 0644 "$temporary"
  mv -Tf "$temporary" "$destination"
}

verify_saved_unit_state() {
  local state_json=$1 unit expected_enabled expected_active
  for unit in "$COLLECTOR_UNIT" "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"; do
    expected_enabled=$(jq -er --arg unit "$unit" '.units[$unit].enabled' "$state_json") \
      || return 1
    expected_active=$(jq -er --arg unit "$unit" '.units[$unit].active' "$state_json") \
      || return 1
    if [[ $expected_enabled == true ]]; then
      unit_enabled "$unit" || return 1
    elif unit_enabled "$unit"; then
      return 1
    fi
    if [[ $expected_active == true ]]; then
      unit_active "$unit" || return 1
    elif unit_active "$unit"; then
      return 1
    fi
  done
}

verify_legacy_runtime() {
  local expected_pid=$1 expected_restarts=$2 expected_invocation_id=$3
  local pid cmdline restarts invocation_id
  unit_active "$COLLECTOR_UNIT" || return 1
  verify_effective_unit "$COLLECTOR_UNIT" "$COLLECTOR_FRAGMENT" "$LEGACY_EXEC" || return 1
  pid=$(systemctl show --property=MainPID --value "$COLLECTOR_UNIT")
  [[ $pid == "$expected_pid" ]] || return 1
  restarts=$(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT") || return 1
  [[ $restarts == "$expected_restarts" ]] || return 1
  invocation_id=$(systemctl show --property=InvocationID --value "$COLLECTOR_UNIT") || return 1
  [[ $invocation_id == "$expected_invocation_id" ]] || return 1
  cmdline=$(proc_cmdline "$pid") || return 1
  [[ $cmdline == "$LEGACY_EXEC " ]]
}

verify_legacy_health() {
  local started_epoch=$1 health_policy=${2:-$LEGACY_HEALTH_POLICY}
  local updated_at last_success_at updated_epoch success_epoch now_epoch
  [[ -f $HEALTH && ! -L $HEALTH ]] || return 1
  (( $(stat -c %Y "$HEALTH") >= started_epoch )) || return 1
  jq -e -f "$health_policy" "$HEALTH" >/dev/null || return 1
  updated_at=$(jq -er '.updated_at' "$HEALTH") || return 1
  last_success_at=$(jq -er '.last_success_at' "$HEALTH") || return 1
  updated_epoch=$(date -u -d "$updated_at" +%s) || return 1
  success_epoch=$(date -u -d "$last_success_at" +%s) || return 1
  now_epoch=$(date -u +%s)
  ((updated_epoch >= started_epoch && updated_epoch <= now_epoch)) || return 1
  ((success_epoch >= started_epoch && success_epoch <= now_epoch)) || return 1
  ((now_epoch - updated_epoch <= MAX_HEALTH_SILENCE_SECONDS)) || return 1
  ((now_epoch - success_epoch <= MAX_HEALTH_SILENCE_SECONDS))
}

verify_cutover_target_preflight() {
  local baseline_mode=$1 active_binary=$2 control_dir=$3 release_manifest_name=$4
  local file_verifier=$5 unit fragment expected_fragment drop_ins asset assets
  [[ $baseline_mode == legacy_python || $baseline_mode == rust_release ]] || return 1
  [[ $baseline_mode != legacy_python || ( ! -e $active_binary && ! -L $active_binary ) ]] \
    || return 1
  secure_root_chain_or_absent "$control_dir" || return 1
  for unit in polymarket-reference-collector.service \
    polymarket-reference-upload.service polymarket-reference-upload.timer \
    polymarket-market-tape-upload.service polymarket-market-tape-upload.timer; do
    expected_fragment="/etc/systemd/system/$unit"
    fragment=$(systemctl show --property=FragmentPath --value "$unit") || return 1
    [[ $fragment == "$expected_fragment" ]] || return 1
    "$file_verifier" "$expected_fragment" || return 1
    drop_ins=$(systemctl show --property=DropInPaths --value "$unit") || return 1
    [[ -z $drop_ins ]] || return 1
  done
  if [[ -e $control_dir || -L $control_dir ]]; then
    direct_directory "$control_dir" && secure_root_chain "$control_dir" || return 1
    assets=$(release_control_assets "$control_dir") || return 1
    while IFS= read -r asset; do
      [[ $asset != "$release_manifest_name" ]] || return 1
      "$file_verifier" "$control_dir/$asset" || return 1
    done <<<"$assets"
    "$file_verifier" "$control_dir/$release_manifest_name" || return 1
  fi
}

verify_fresh_legacy_runtime() {
  local started_epoch=$1 expected_pid=$2 expected_restarts=$3 expected_invocation_id=$4
  local health_policy=${5:-$LEGACY_HEALTH_POLICY}
  verify_legacy_runtime "$expected_pid" "$expected_restarts" "$expected_invocation_id" \
    || return 1
  verify_legacy_health "$started_epoch" "$health_policy"
}

clear_health_before_restart() {
  local evidence_dir=$1 label=$2 snapshot
  [[ ! -e $HEALTH && ! -L $HEALTH ]] && return 0
  [[ -f $HEALTH && ! -L $HEALTH ]] || die 'health path is not a direct regular file'
  snapshot="$evidence_dir/${label}-health.json"
  [[ ! -e $snapshot && ! -L $snapshot ]] || die 'health snapshot path already exists'
  install -m 0640 "$HEALTH" "$snapshot"
  rm -f "$HEALTH"
  sync "$snapshot"
}

verify_rust_health_file() {
  local health_file=$1 started_epoch=$2 health_mtime updated_at last_success_at
  local health_policy=${3:-$RUST_HEALTH_POLICY} updated_epoch success_epoch now_epoch
  [[ -f $health_file && ! -L $health_file ]] || return 1
  health_mtime=$(stat -c %Y "$health_file")
  ((health_mtime >= started_epoch)) || return 1
  jq -e -f "$health_policy" "$health_file" >/dev/null || return 1
  updated_at=$(jq -er '.updated_at' "$health_file") || return 1
  last_success_at=$(jq -er '.last_success_at' "$health_file") || return 1
  updated_epoch=$(date -u -d "$updated_at" +%s) || return 1
  success_epoch=$(date -u -d "$last_success_at" +%s) || return 1
  now_epoch=$(date -u +%s)
  ((updated_epoch >= started_epoch && updated_epoch <= now_epoch)) || return 1
  ((success_epoch >= started_epoch && success_epoch <= now_epoch)) || return 1
  ((now_epoch - updated_epoch <= MAX_HEALTH_SILENCE_SECONDS)) || return 1
  ((now_epoch - success_epoch <= MAX_HEALTH_SILENCE_SECONDS))
}

verify_rust_runtime() {
  local expected_binary=$1 started_epoch=$2 expected_pid=$3 expected_invocation_id=$4
  local expected_restarts=${5:-0} health_policy=${6:-$RUST_HEALTH_POLICY}
  local pid cmdline restarts invocation_id
  unit_active "$COLLECTOR_UNIT" || return 1
  verify_effective_unit "$COLLECTOR_UNIT" "$COLLECTOR_FRAGMENT" "$RUST_EXEC" || return 1
  pid=$(systemctl show --property=MainPID --value "$COLLECTOR_UNIT")
  [[ $pid == "$expected_pid" ]] || return 1
  restarts=$(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT") || return 1
  [[ $restarts == "$expected_restarts" ]] || return 1
  invocation_id=$(systemctl show --property=InvocationID --value "$COLLECTOR_UNIT") || return 1
  [[ $invocation_id == "$expected_invocation_id" ]] || return 1
  [[ $(readlink -f "/proc/$pid/exe") == "$expected_binary" ]] || return 1
  [[ -L $ACTIVE_BINARY && $(readlink -f -- "$ACTIVE_BINARY") == "$expected_binary" ]] \
    || return 1
  cmdline=$(proc_cmdline "$pid") || return 1
  [[ $cmdline == "$RUST_EXEC " ]] || return 1
  verify_rust_health_file "$HEALTH" "$started_epoch" "$health_policy"
}

snapshot_legacy() {
  local rollback_dir=$1 baseline_mode=${2:-legacy_python}
  local baseline_release_path=${3:-} baseline_release_sha=${4:-} candidate_sha=${5:-}
  local state_json=$rollback_dir/state.json asset enabled active mode snapshot_asset
  local control_present=false control_assets control_files
  install -d -m 0750 "$rollback_dir/systemd" "$rollback_dir/bin" \
    "$rollback_dir/config" "$rollback_dir/control"
  secure_root_chain "$rollback_dir" \
    || die 'rollback snapshot directory chain is not trusted'
  jq -n --arg baseline_mode "$baseline_mode" --arg candidate_sha "$candidate_sha" \
    '{baseline_mode:$baseline_mode,candidate_sha256:$candidate_sha}' >"$state_json"
  for asset in "${UNIT_ASSETS[@]}"; do
    secure_regular_file "/etc/systemd/system/$asset"
    mode=$(stat -c %a -- "/etc/systemd/system/$asset")
    install -m "$mode" "/etc/systemd/system/$asset" "$rollback_dir/systemd/$asset"
    jq --arg asset "$asset" --arg mode "$mode" '.unit_modes[$asset]=$mode' \
      "$state_json" >"$state_json.tmp"; mv "$state_json.tmp" "$state_json"
  done
  secure_regular_file "$UPLOAD_ENV"
  mode=$(stat -c %a -- "$UPLOAD_ENV")
  install -m "$mode" "$UPLOAD_ENV" "$rollback_dir/config/polymarket-market-tape-upload.env"
  jq --arg mode "$mode" '.upload_env_mode=$mode' "$state_json" >"$state_json.tmp"
  mv "$state_json.tmp" "$state_json"
  if [[ $baseline_mode == rust_release ]]; then
    [[ -L $ACTIVE_BINARY && $(readlink -f -- "$ACTIVE_BINARY") == "$baseline_release_path" ]] \
      || die 'active Rust symlink changed before rollback snapshot'
    verify_control_release "$CONTROL_DIR" "$baseline_release_sha" "$baseline_release_path" \
      || die 'global controls changed before rollback snapshot'
  fi
  if [[ -d $CONTROL_DIR && ! -L $CONTROL_DIR ]]; then
    secure_root_chain "$CONTROL_DIR" || die 'global control directory is not trusted'
    control_present=true
    control_assets=$(release_control_assets "$CONTROL_DIR") \
      || die 'global control directory has no valid release-specific asset list'
    control_files="$control_assets"$'\n'"${RELEASE_MANIFEST##*/}"
    while IFS= read -r asset; do
      secure_regular_file "$CONTROL_DIR/$asset"
      mode=$(stat -c %a -- "$CONTROL_DIR/$asset")
      snapshot_asset=$asset
      [[ $baseline_mode == legacy_python ]] && snapshot_asset="global-$asset"
      install -m "$mode" "$CONTROL_DIR/$asset" "$rollback_dir/control/$snapshot_asset"
      jq --arg asset "$asset" --arg mode "$mode" \
        '.control_modes[$asset]=$mode
          | .control_files=((.control_files // []) + [$asset])' \
        "$state_json" >"$state_json.tmp"; mv "$state_json.tmp" "$state_json"
    done <<<"$control_files"
  fi
  jq --argjson present "$control_present" '.control_dir_present=$present' \
    "$state_json" >"$state_json.tmp"; mv "$state_json.tmp" "$state_json"
  if [[ $baseline_mode == legacy_python ]]; then
    secure_regular_file "$LEGACY_COLLECTOR"; secure_regular_file "$LEGACY_UPLOADER"
    secure_regular_file "$LEGACY_HEALTH_POLICY"
    install -m 0755 "$LEGACY_COLLECTOR" "$rollback_dir/bin/${PYTHON_ASSETS[0]}"
    install -m 0755 "$LEGACY_UPLOADER" "$rollback_dir/bin/${PYTHON_ASSETS[1]}"
    install -m 0644 "$LEGACY_HEALTH_POLICY" \
      "$rollback_dir/control/polymarket-legacy-health-policy.jq"
  else
    printf '%s\n' "$baseline_release_path" >"$rollback_dir/bin/release-path"
    printf '%s\n' "$baseline_release_sha" >"$rollback_dir/bin/release-sha256"
    jq --arg path "$baseline_release_path" --arg sha "$baseline_release_sha" \
      '.active_symlink={target:$path,sha256:$sha}' "$state_json" >"$state_json.tmp"
    mv "$state_json.tmp" "$state_json"
  fi
  for asset in "$COLLECTOR_UNIT" "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"; do
    enabled=false
    active=false
    unit_enabled "$asset" && enabled=true
    unit_active "$asset" && active=true
    jq --arg unit "$asset" --argjson enabled "$enabled" --argjson active "$active" \
      '.units[$unit] = {enabled:$enabled,active:$active}' "$state_json" \
      >"$state_json.tmp"
    mv "$state_json.tmp" "$state_json"
  done
  (
    cd "$rollback_dir"
    sha256sum state.json systemd/* bin/* config/* control/* >manifest.sha256
  )
  sync -f "$rollback_dir"
}

restore_legacy() (
  set -e
  local evidence_dir=$1
  local rollback_dir=$evidence_dir/rollback active_target actual_manifest_sha asset mode
  local expected_manifest_sha started_epoch rollback_pid current_pid restarts rollback_mode
  local rollback_sha temporary_link previous_health_sha current_health_sha
  local control_dir_present control_files=
  local rollback_health_policy=$rollback_dir/control/polymarket-legacy-health-policy.jq
  secure_root_chain "$evidence_dir" || die 'rollback evidence directory is not trusted'
  secure_root_chain "$rollback_dir" || die 'rollback payload directory is not trusted'
  secure_regular_file "$rollback_dir/manifest.sha256"
  (
    cd "$rollback_dir"
    sha256sum --check --strict manifest.sha256 >/dev/null
  ) || die 'rollback snapshot checksum failed'
  rollback_mode=$(jq -er '.baseline_mode // "legacy_python" | select(. == "legacy_python" or . == "rust_release")' \
    "$rollback_dir/state.json") || die 'rollback snapshot has no valid baseline mode'
  [[ $rollback_mode == rust_release ]] \
    && rollback_health_policy=$rollback_dir/control/polymarket-rust-health-policy.jq
  secure_regular_file "$rollback_health_policy"
  if [[ -e $evidence_dir/cutover.json || -L $evidence_dir/cutover.json ]]; then
    secure_regular_file "$evidence_dir/cutover.json"
    expected_manifest_sha=$(jq -er '.rollback_manifest_sha256' "$evidence_dir/cutover.json") \
      || die 'cutover evidence is missing the rollback manifest identity'
    actual_manifest_sha=$(sha256sum "$rollback_dir/manifest.sha256" | awk '{print $1}')
    [[ $actual_manifest_sha == "$expected_manifest_sha" ]] \
      || die 'rollback manifest differs from the completed cutover evidence'
  fi
  control_dir_present=$(
    jq -er '.control_dir_present | select(type == "boolean") | tostring' \
      "$rollback_dir/state.json"
  ) || die 'rollback snapshot has no valid control directory state'
  if [[ $control_dir_present == true ]]; then
    control_files=$(rollback_control_files "$rollback_dir/state.json") \
      || die 'rollback snapshot has no valid release-specific control list'
  elif [[ $rollback_mode == rust_release ]]; then
    die 'Rust rollback snapshot has no control release'
  fi

  systemctl stop "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"
  systemctl stop "$REFERENCE_UPLOAD_UNIT" "$MARKET_UPLOAD_UNIT"
  systemctl stop "$COLLECTOR_UNIT"
  previous_health_sha=
  current_health_sha=
  [[ $rollback_mode == legacy_python || ! -f $HEALTH || -L $HEALTH ]] \
    || previous_health_sha=$(sha256sum "$HEALTH" | awk '{print $1}')
  [[ $rollback_mode == rust_release ]] \
    || clear_health_before_restart "$evidence_dir" "pre-rollback-$(date -u +%Y%m%dT%H%M%SZ)-$$"
  for asset in "${UNIT_ASSETS[@]}"; do
    mode=$(jq -r --arg asset "$asset" '.unit_modes[$asset] // "0644"' \
      "$rollback_dir/state.json")
    atomic_install "$mode" "$rollback_dir/systemd/$asset" "/etc/systemd/system/$asset"
  done
  mode=$(jq -r '.upload_env_mode // "0640"' "$rollback_dir/state.json")
  atomic_install "$mode" "$rollback_dir/config/polymarket-market-tape-upload.env" "$UPLOAD_ENV"
  for asset in "${BUNDLE_ASSETS[@]}" "${RELEASE_MANIFEST##*/}"; do
    rm -f -- "$CONTROL_DIR/$asset"
  done
  if [[ $rollback_mode == legacy_python ]]; then
    if [[ $control_dir_present == true ]]; then
      while IFS= read -r asset; do
        mode=$(jq -er --arg asset "$asset" '.control_modes[$asset]' "$rollback_dir/state.json")
        atomic_install "$mode" "$rollback_dir/control/global-$asset" "$CONTROL_DIR/$asset"
      done <<<"$control_files"
    fi
    atomic_install 0755 "$rollback_dir/bin/${PYTHON_ASSETS[0]}" "$LEGACY_COLLECTOR"
    atomic_install 0755 "$rollback_dir/bin/${PYTHON_ASSETS[1]}" "$LEGACY_UPLOADER"
  elif [[ ! -e $ACTIVE_BINARY || -L $ACTIVE_BINARY ]]; then
    active_target=$(jq -er '.active_symlink.target' "$rollback_dir/state.json")
    rollback_sha=$(jq -er '.active_symlink.sha256' "$rollback_dir/state.json")
    [[ $active_target == "$RELEASE_ROOT/$rollback_sha/polymarket-raw-ops" ]] \
      || die 'rollback release lineage is invalid'
    secure_release_directory "${active_target%/*}" \
      || die 'rollback release directory is not trusted'
    secure_regular_file "$active_target"; [[ -x $active_target ]] \
      || die 'rollback release is not executable'
    printf '%s  %s\n' "$rollback_sha" "$active_target" \
      | sha256sum --check --strict >/dev/null || die 'rollback release checksum failed'
    while IFS= read -r asset; do
      mode=$(jq -er --arg asset "$asset" '.control_modes[$asset]' "$rollback_dir/state.json")
      atomic_install "$mode" "$rollback_dir/control/$asset" "$CONTROL_DIR/$asset"
    done <<<"$control_files"
    verify_control_release "$CONTROL_DIR" "$rollback_sha" "$active_target" \
      || die 'restored controls do not bind the rollback release'
    temporary_link="${ACTIVE_BINARY}.new.$$"; rm -f "$temporary_link"
    ln -s "$active_target" "$temporary_link"
    mv -Tf "$temporary_link" "$ACTIVE_BINARY"
  elif [[ -e $ACTIVE_BINARY ]]; then
    die 'refusing to replace a non-symlink active Rust path'
  fi
  if [[ $rollback_mode == legacy_python && ( -e $ACTIVE_BINARY || -L $ACTIVE_BINARY ) ]]; then
    [[ -L $ACTIVE_BINARY ]] || die 'refusing to remove a non-symlink active Rust path'
    active_target=$(readlink -f -- "$ACTIVE_BINARY")
    [[ $active_target == "$RELEASE_ROOT"/*/polymarket-raw-ops ]] \
      || die 'active Rust symlink points outside the immutable release root'
    rm -f "$ACTIVE_BINARY"
  fi
  sync -f /etc/monday
  sync -f /etc/systemd/system
  sync -f /opt/monday
  systemctl daemon-reload
  systemctl reset-failed "$COLLECTOR_UNIT"
  [[ $(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT") == 0 ]] \
    || die 'legacy restart counter did not reset before rollback verification'
  started_epoch=$(date -u +%s)
  systemctl restart "$COLLECTOR_UNIT"
  rollback_pid=
  rollback_invocation_id=
  for _ in $(seq 1 36); do
    restarts=$(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT")
    [[ $restarts == 0 ]] || die 'collector restarted while rollback was being verified'
    current_pid=$(systemctl show --property=MainPID --value "$COLLECTOR_UNIT")
    if [[ $current_pid =~ ^[1-9][0-9]*$ ]]; then
      if [[ -z $rollback_pid ]]; then
        rollback_pid=$current_pid
        rollback_invocation_id=$(systemctl show --property=InvocationID --value "$COLLECTOR_UNIT")
        [[ $rollback_invocation_id =~ ^[a-f0-9]{32}$ ]] \
          || die 'rollback collector has no verifiable systemd invocation ID'
      else
        [[ $current_pid == "$rollback_pid" ]] \
          || die 'collector PID changed while rollback was being verified'
      fi
      if [[ $rollback_mode == legacy_python ]]; then
        verify_legacy_runtime "$rollback_pid" 0 "$rollback_invocation_id" && break
      else
        if verify_rust_runtime "$active_target" "$started_epoch" "$rollback_pid" \
          "$rollback_invocation_id" 0 "$rollback_health_policy"; then
          current_health_sha=$(sha256sum "$HEALTH" | awk '{print $1}')
          [[ $current_health_sha != "$previous_health_sha" ]] && break
        fi
      fi
    fi
    sleep 5
  done
  [[ -n $rollback_pid ]] || die 'collector never produced a verifiable MainPID'
  [[ $rollback_mode == legacy_python \
    || ( -n $current_health_sha && $current_health_sha != "$previous_health_sha" ) ]] \
    || die 'Rust collector health did not advance after rollback restart'
  if [[ $rollback_mode == legacy_python ]]; then
  verify_legacy_runtime "$rollback_pid" 0 "$rollback_invocation_id" \
    || die 'Python collector identity did not recover during rollback'
  else
    verify_rust_runtime "$active_target" "$started_epoch" "$rollback_pid" \
      "$rollback_invocation_id" 0 "$rollback_health_policy" \
      || die 'Rust collector identity or health did not recover during rollback'
  fi

  for asset in "$COLLECTOR_UNIT" "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"; do
    if jq -e --arg unit "$asset" '.units[$unit].enabled == true' \
      "$rollback_dir/state.json" >/dev/null; then
      systemctl enable "$asset"
    else
      systemctl disable "$asset"
    fi
    if jq -e --arg unit "$asset" '.units[$unit].active == true' \
      "$rollback_dir/state.json" >/dev/null; then
      systemctl start "$asset"
    fi
  done
  if [[ $rollback_mode == legacy_python ]]; then
  verify_legacy_runtime "$rollback_pid" 0 "$rollback_invocation_id" \
    || die 'rollback did not preserve legacy runtime identity'
  else
    verify_rust_runtime "$active_target" "$started_epoch" "$rollback_pid" \
      "$rollback_invocation_id" 0 "$rollback_health_policy" || die 'restored Rust runtime changed'
  fi
  verify_saved_unit_state "$rollback_dir/state.json" \
    || die 'rollback did not restore the saved collector/timer state'
  if [[ $rollback_mode == legacy_python ]]; then
  verify_legacy_runtime "$rollback_pid" 0 "$rollback_invocation_id" \
    || die 'legacy runtime changed before rollback completion'
  else
    verify_rust_runtime "$active_target" "$started_epoch" "$rollback_pid" \
      "$rollback_invocation_id" 0 "$rollback_health_policy" || die 'Rust rollback did not hold'
  fi
  printf '%s\n' "$evidence_dir"
)

[[ ${EUID} -eq 0 ]] || die 'must run as root'
for command in awk cmp date dirname flock grep install journalctl jq ln mkdir mktemp mountpoint \
  mv readlink rm sed seq sha256sum sleep sort stat sync systemctl tar tr wc; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done
mode=${1:-}
case "$mode" in
  stage)
    [[ $# -eq 3 ]] || {
      usage >&2
      exit 2
    }
    ;;
  rollback)
    [[ $# -eq 2 ]] || {
      usage >&2
      exit 2
    }
    ;;
  cutover)
    [[ $# -eq 3 ]] || {
      usage >&2
      exit 2
    }
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
if [[ $mode == stage ]]; then
  [[ -d $2 && ! -L $2 ]] || die 'artifact must be a direct directory'
  artifact_dir=$(readlink -f -- "$2")
  stage_script=$(readlink -f -- "$0")
  [[ -f $0 && ! -L $0 \
    && $stage_script == "$SCRIPT_DIR/polymarket-raw-ops-cutover.sh" ]] \
    || die 'stage command must be the direct script from a trusted source tree'
  for path in "$SCRIPT_DIR" "$artifact_dir" /opt/monday /opt/monday/candidates \
    "$CANDIDATE_ROOT" /run/monday; do
    secure_root_chain_or_absent "$path" \
      || die "trusted path chain is not root-owned and non-writable: $path"
  done
  secure_regular_file "$stage_script"
  install -d -m 0755 /opt/monday/candidates "$CANDIDATE_ROOT" /run/monday
  secure_root_chain "$CANDIDATE_ROOT" || die 'candidate root is not trusted'
  secure_root_chain /run/monday || die 'runtime control directory is not trusted'
  exec 9>"$LOCK_FILE"
  flock -n 9 || die 'another Polymarket release operation is running'
  stage_release "$artifact_dir" "$CANDIDATE_ROOT" "$3"
  exit
fi
mountpoint -q /data || die '/data must be a mount point'
for path in "$SCRIPT_DIR" /etc/monday /etc/systemd/system /opt/monday \
  /opt/monday/bin /opt/monday/control "$CONTROL_DIR" /opt/monday/releases "$RELEASE_ROOT" \
  /data /data/monday \
  /data/monday/spool /data/monday/evidence "$EVIDENCE_ROOT" \
  "$GATE_RECEIPT_ROOT" "$GATE_EVIDENCE_ROOT" /run/monday; do
  secure_root_chain_or_absent "$path" \
    || die "trusted path chain is not root-owned and non-writable: $path"
done
secure_collector_directory /data/monday/spool/polymarket-reference \
  || die 'production spool is not an exact hftcollector-owned 0750 directory'
install -d -m 0755 /run/monday
secure_root_chain /run/monday || die 'runtime control directory is not trusted'
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Polymarket release operation is running'

if [[ $mode == rollback ]]; then
  [[ -d $2 && ! -L $2 ]] || die 'rollback evidence must be a direct directory'
  rollback_evidence=$(readlink -f -- "$2")
  [[ $rollback_evidence == "$EVIDENCE_ROOT"/* ]] \
    || die 'rollback evidence is outside the fixed cutover evidence root'
  secure_root_chain "$rollback_evidence" \
    || die 'manual rollback evidence directory is not trusted'
  rollback_dir="$rollback_evidence/rollback"
  secure_root_chain "$rollback_dir" || die 'rollback payload directory is not trusted'
  secure_regular_file "$rollback_dir/manifest.sha256"
  (cd "$rollback_dir" && sha256sum --check --strict manifest.sha256 >/dev/null) \
    || die 'rollback snapshot checksum failed'
  rollback_mode=$(jq -er '.baseline_mode // "legacy_python" | select(. == "legacy_python" or . == "rust_release")' \
    "$rollback_dir/state.json") || die 'rollback snapshot has no valid baseline mode'
  snapshot_candidate=$(jq -er '.candidate_sha256 // empty | select(test("^[a-f0-9]{64}$"))' \
    "$rollback_dir/state.json" 2>/dev/null || true)
  rollback_marker_state=none
  if [[ -e $rollback_evidence/cutover.json || -L $rollback_evidence/cutover.json ]]; then
    secure_regular_file "$rollback_evidence/cutover.json"
    for marker in PASSED.invalid.sha256 PASSED.rolled-back.sha256; do
      [[ ! -e $rollback_evidence/$marker && ! -L $rollback_evidence/$marker ]] \
        || die 'rollback evidence is already finalized'
    done
    if [[ -e $rollback_evidence/PASSED.sha256 || -L $rollback_evidence/PASSED.sha256 ]]; then
      verify_named_marker "$rollback_evidence" cutover.json PASSED.sha256 \
        || die 'cutover success marker does not verify the requested evidence'
      rollback_marker_state=passed
      [[ ! -e $rollback_evidence/PASSED.rollback-pending.sha256 \
        && ! -L $rollback_evidence/PASSED.rollback-pending.sha256 ]] \
        || die 'rollback evidence has conflicting success and pending markers'
    elif [[ -e $rollback_evidence/PASSED.rollback-pending.sha256 \
      || -L $rollback_evidence/PASSED.rollback-pending.sha256 ]]; then
      verify_named_marker "$rollback_evidence" cutover.json PASSED.rollback-pending.sha256 \
        || die 'rollback-pending marker does not verify the requested evidence'
      rollback_marker_state=pending
    fi
    rollback_candidate=$(jq -er '.candidate_sha256 | select(test("^[a-f0-9]{64}$"))' \
      "$rollback_evidence/cutover.json") || die 'cutover lineage has no candidate identity'
    [[ -z $snapshot_candidate || $snapshot_candidate == "$rollback_candidate" ]] \
      || die 'cutover candidate differs from the rollback snapshot'
    manual_manifest_identity=$(jq -er '.rollback_manifest_sha256' \
      "$rollback_evidence/cutover.json") || die 'cutover evidence has no rollback identity'
    [[ $(sha256sum "$rollback_dir/manifest.sha256" | awk '{print $1}') \
      == "$manual_manifest_identity" ]] || die 'cutover rollback manifest identity changed'
  else
    for marker in PASSED.sha256 PASSED.rollback-pending.sha256 PASSED.invalid.sha256 \
      PASSED.rolled-back.sha256; do
      [[ ! -e $rollback_evidence/$marker && ! -L $rollback_evidence/$marker ]] \
        || die 'rollback marker exists without cutover evidence'
    done
    rollback_candidate=$snapshot_candidate
  fi
  [[ $rollback_candidate =~ ^[a-f0-9]{64}$ ]] \
    || die 'rollback snapshot has no candidate identity'
  rollback_candidate_path="$RELEASE_ROOT/$rollback_candidate/polymarket-raw-ops"
  rollback_saved_path=
  if [[ $rollback_mode == rust_release ]]; then
    rollback_saved_path=$(jq -er '.active_symlink.target' "$rollback_dir/state.json")
    rollback_saved_sha=$(jq -er '.active_symlink.sha256' "$rollback_dir/state.json")
    [[ $rollback_saved_path == "$RELEASE_ROOT/$rollback_saved_sha/polymarket-raw-ops" ]] \
      || die 'saved baseline release lineage is invalid'
    secure_release_directory "${rollback_saved_path%/*}" \
      || die 'saved baseline release directory is not trusted'
    secure_regular_file "$rollback_saved_path"; [[ -x $rollback_saved_path ]] \
      || die 'saved baseline release is not executable'
    printf '%s  %s\n' "$rollback_saved_sha" "$rollback_saved_path" \
      | sha256sum --check --strict >/dev/null || die 'saved baseline release checksum failed'
  fi
  if [[ -L $ACTIVE_BINARY ]]; then
    rollback_active_path=$(readlink -f -- "$ACTIVE_BINARY")
  elif [[ -e $ACTIVE_BINARY ]]; then
    die 'active release lineage is not a symlink'
  else
    rollback_active_path=
  fi
  if [[ $rollback_marker_state == passed ]]; then
    [[ $rollback_active_path == "$rollback_candidate_path" ]] \
      || die 'completed cutover candidate is not the active release'
  else
    [[ $rollback_active_path == "$rollback_candidate_path" \
      || $rollback_active_path == "$rollback_saved_path" ]] \
      || die 'active release is neither the candidate nor saved baseline'
  fi
  if [[ $rollback_active_path == "$rollback_candidate_path" ]]; then
    secure_release_directory "${rollback_candidate_path%/*}" \
      || die 'active candidate release directory is not trusted'
    secure_regular_file "$rollback_candidate_path"; [[ -x $rollback_candidate_path ]] \
      || die 'active candidate release is not executable'
    printf '%s  %s\n' "$rollback_candidate" "$rollback_candidate_path" \
      | sha256sum --check --strict >/dev/null || die 'active candidate checksum failed'
  fi
  prepare_rollback_evidence "$rollback_evidence" \
    || die 'could not invalidate cutover success before manual rollback'
  restore_legacy "$rollback_evidence" >/dev/null
  finalize_rollback_evidence "$rollback_evidence" rolled-back \
    || die 'could not finalize rolled-back cutover evidence'
  printf '%s\n' "$rollback_evidence"
  exit 0
fi

# Cutover depends on the current gate bundle and live uploader configuration.
# Manual rollback intentionally branches before these checks and uses only the
# checksum-protected snapshot embedded in the cutover evidence directory.
secure_regular_file "$POLICY"
for asset in "${BUNDLE_ASSETS[@]}"; do
  secure_regular_file "$SCRIPT_DIR/$asset"
done
secure_regular_file "$UPLOAD_ENV"
deployment_bundle_sha=$(bundle_sha256)
current_oss_config_sha=$(oss_config_sha256)

candidate_sha=$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 is invalid'
terminal_binding=$(verify_gate_terminal_receipt "$3" "$candidate_sha") \
  || die 'Gate terminal receipt is invalid or not passed'
IFS='|' read -r gate_systemd_invocation_id gate_receipt_source_revision \
  gate_terminal_receipt_sha256 gate_json_sha256 gate_json \
  gate_terminal_receipt extra_binding \
  <<<"$terminal_binding"
[[ -n $gate_systemd_invocation_id && -n $gate_receipt_source_revision \
  && -n $gate_terminal_receipt_sha256 && -n $gate_json_sha256 \
  && -n $gate_json \
  && -n $gate_terminal_receipt && -z $extra_binding ]] \
  || die 'Gate terminal receipt binding is malformed'
secure_regular_file "$gate_json"
gate_dir=$(dirname "$gate_json")
secure_root_chain "$gate_dir" || die 'gate evidence directory is not trusted'
secure_regular_file "$gate_dir/PASSED.sha256"
verify_gate_marker "$gate_dir" \
  || die 'shadow gate marker is not the exact single gate.json checksum'
jq -e -f "$POLICY" "$gate_json" >/dev/null || die 'shadow gate is not production eligible'
[[ $(jq -r '.candidate_sha256' "$gate_json") == "$candidate_sha" ]] \
  || die 'gate belongs to a different candidate'
[[ $(jq -r '.deployment_bundle_sha256' "$gate_json") == "$deployment_bundle_sha" ]] \
  || die 'control-plane bundle changed after the shadow gate'
gate_release_manifest_sha=$(jq -er \
  '.release_manifest_sha256 | select(type == "string" and test("^[a-f0-9]{64}$"))' \
  "$gate_json") || die 'shadow gate is missing the release manifest identity'
gate_control_archive_sha=$(jq -er \
  '.control_archive_sha256 | select(type == "string" and test("^[a-f0-9]{64}$"))' \
  "$gate_json") || die 'shadow gate is missing the control archive identity'
gate_source_revision=$(jq -er \
  '.deployment_source_revision | select(type == "string" and test("^[a-f0-9]{40,64}$"))' \
  "$gate_json") || die 'shadow gate is missing the source revision identity'
[[ $gate_source_revision == "$gate_receipt_source_revision" ]] \
  || die 'Gate receipt source differs from shadow evidence'
[[ $(jq -er .shadow_run_id "$gate_json") == "$gate_systemd_invocation_id" ]] \
  || die 'Gate receipt invocation differs from shadow evidence'
gate_oss_config_sha=$(jq -er '.oss_config_sha256 | select(type == "string")' "$gate_json") \
  || die 'shadow gate is missing the OSS configuration identity'
[[ $gate_oss_config_sha == "$current_oss_config_sha" ]] \
  || die 'OSS configuration changed after the shadow gate'
gate_completed_at=$(jq -er '.completed_at | select(type == "string")' "$gate_json") \
  || die 'shadow gate completion timestamp is missing'
gate_completed_epoch=$(date -u -d "$gate_completed_at" +%s) \
  || die 'shadow gate completion timestamp is invalid'
now_epoch=$(date -u +%s)
gate_age=$((now_epoch - gate_completed_epoch))
((gate_age >= 0 && gate_age <= MAX_GATE_AGE_SECONDS)) \
  || die 'shadow gate evidence is stale or from the future'

candidate_release_dir="$RELEASE_ROOT/$candidate_sha"
secure_release_directory "$candidate_release_dir" \
  || die 'candidate release directory is not root-owned mode 0755'
[[ $(readlink -f -- "$SCRIPT_DIR") == "$candidate_release_dir/control" ]] \
  || die 'cutover requires the gate-pinned candidate control directory'
candidate_binary="$candidate_release_dir/polymarket-raw-ops"
secure_regular_file "$candidate_binary"
[[ -x $candidate_binary ]] || die 'candidate release is not executable'
printf '%s  %s\n' "$candidate_sha" "$candidate_binary" \
  | sha256sum --check --strict >/dev/null || die 'candidate release checksum mismatch'
verify_release_binding "$RELEASE_MANIFEST" "$gate_release_manifest_sha" \
  "$candidate_sha" "$gate_source_revision" "$deployment_bundle_sha" \
  "$gate_control_archive_sha" "$candidate_binary" \
  || die 'release manifest no longer binds the gated candidate and control bundle'
pinned_upload_env="$RELEASE_ROOT/$candidate_sha/polymarket-upload-env-$gate_oss_config_sha.env"
secure_regular_file "$pinned_upload_env"
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned uploader environment differs from the shadow gate'
for asset in "$COLLECTOR_UNIT" "$REFERENCE_UPLOAD_UNIT" "$REFERENCE_UPLOAD_TIMER" \
  "$MARKET_UPLOAD_UNIT" "$MARKET_UPLOAD_TIMER"; do
  secure_regular_file "$SCRIPT_DIR/$asset"
done
baseline_mode=$(jq -er '.baseline_mode | select(. == "legacy_python" or . == "rust_release")' \
  "$gate_json") || die 'shadow gate has no valid baseline mode'
baseline_runtime_stability_required=$(jq -er \
  '.baseline_runtime_stability_required | select(type == "boolean") | tostring' \
  "$gate_json") \
  || die 'shadow gate has no valid baseline runtime stability contract'
legacy_pid=$(systemctl show --property=MainPID --value "$COLLECTOR_UNIT")
[[ $legacy_pid =~ ^[1-9][0-9]*$ ]] \
  || die 'cutover requires a verifiable active legacy reference collector PID'
gate_legacy_pid=$(jq -er '.legacy_runtime.main_pid | select(type == "number" and floor == . and . > 0)' \
  "$gate_json") || die 'shadow gate has no valid legacy MainPID'
gate_legacy_restarts=$(jq -er \
  '.legacy_runtime.restarts | select(type == "number" and floor == . and . >= 0)' \
  "$gate_json") || die 'shadow gate has no valid legacy restart counter'
gate_legacy_invocation_id=$(jq -er \
  '.legacy_runtime.invocation_id | select(type == "string" and test("^[a-f0-9]{32}$"))' \
  "$gate_json") || die 'shadow gate has no valid legacy systemd invocation ID'
if [[ $baseline_mode == legacy_python ]]; then
  if [[ $baseline_runtime_stability_required == true ]]; then
    [[ $legacy_pid == "$gate_legacy_pid" ]] \
      || die 'legacy collector MainPID changed after the shadow gate'
  else
    gate_legacy_pid=$legacy_pid
    gate_legacy_restarts=$(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT")
    gate_legacy_invocation_id=$(systemctl show \
      --property=InvocationID --value "$COLLECTOR_UNIT")
  fi
  verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" \
    "$gate_legacy_invocation_id" \
    || die 'cutover requires an exact canonical legacy rollback runtime'
  # The immutable Gate policy above already admitted the atomic legacy health
  # snapshot. Promotion requires the same runtime identity, not a new cycle.
else
  [[ $legacy_pid == "$gate_legacy_pid" ]] \
    || die 'Rust baseline MainPID changed after the shadow gate'
  gate_baseline_release_path=$(jq -er '.legacy_runtime.release_path' "$gate_json")
  gate_baseline_release_sha=$(jq -er '.legacy_runtime.release_sha256' "$gate_json")
  gate_baseline_proc_exe=$(jq -er '.legacy_runtime.proc_exe' "$gate_json")
  [[ $gate_baseline_release_path == \
      "$RELEASE_ROOT/$gate_baseline_release_sha/polymarket-raw-ops" \
    && $gate_baseline_proc_exe == "$gate_baseline_release_path" ]] \
    || die 'gated Rust baseline release lineage is invalid'
  secure_release_directory "${gate_baseline_release_path%/*}" \
    || die 'gated Rust baseline release directory is not trusted'
  printf '%s  %s\n' "$gate_baseline_release_sha" "$gate_baseline_release_path" \
    | sha256sum --check --strict >/dev/null || die 'gated Rust baseline digest changed'
  legacy_health_not_before=$(($(date -u +%s) - MAX_HEALTH_SILENCE_SECONDS))
  verify_rust_runtime "$gate_baseline_release_path" "$legacy_health_not_before" \
    "$legacy_pid" "$gate_legacy_invocation_id" "$gate_legacy_restarts" \
    "$LEGACY_HEALTH_POLICY" \
    || die 'Rust baseline identity, restart counter, or health changed after the shadow gate'
  verify_control_release "$CONTROL_DIR" "$gate_baseline_release_sha" \
    "$gate_baseline_release_path" || die 'global controls do not bind the active Rust baseline'
  baseline_pinned_upload_env="$RELEASE_ROOT/$gate_baseline_release_sha/polymarket-upload-env-$gate_oss_config_sha.env"
  secure_regular_file "$baseline_pinned_upload_env"
  [[ $(oss_config_sha256 "$baseline_pinned_upload_env") == "$gate_oss_config_sha" ]] \
    || die 'active Rust uploader environment differs from the shadow gate'
  verify_upload_units "$baseline_pinned_upload_env" \
    || die 'active Rust upload units do not bind the baseline release'
fi
verify_cutover_target_preflight "$baseline_mode" "$ACTIVE_BINARY" \
  "$CONTROL_DIR" "${RELEASE_MANIFEST##*/}" secure_regular_file \
  || die 'production cutover target state would reject promotion'

install -d -m 0755 /data/monday /data/monday/evidence "$EVIDENCE_ROOT"
secure_root_chain "$EVIDENCE_ROOT" || die 'cutover evidence root is not trusted'
run_id="$(date -u +%Y%m%dT%H%M%SZ)-${candidate_sha:0:12}-$$"
evidence_dir="$EVIDENCE_ROOT/$run_id"
mkdir -m 0750 "$evidence_dir" || die 'cutover evidence directory already exists'
secure_root_chain "$evidence_dir" || die 'cutover evidence directory is not trusted'
rollback_dir="$evidence_dir/rollback"
snapshot_legacy "$rollback_dir" "$baseline_mode" \
  "${gate_baseline_release_path:-}" "${gate_baseline_release_sha:-}" "$candidate_sha"

transition_started=false
cutover_succeeded=false
on_exit() {
  local status=$? restore_status=0
  if [[ $cutover_succeeded == false && $transition_started == true ]]; then
    printf 'cutover failed; restoring snapshotted legacy runtime\n' >&2
    trap - EXIT
    prepare_rollback_evidence "$evidence_dir" || {
      printf 'refusing automatic rollback because success evidence could not be invalidated\n' >&2
      exit 1
    }
    set +e
    restore_legacy "$evidence_dir" >/dev/null
    restore_status=$?
    set -e
    if ((restore_status != 0)); then
      printf 'automatic rollback failed; collector and upload timers require operator recovery\n' >&2
      status=1
    else
      finalize_rollback_evidence "$evidence_dir" invalid || status=1
    fi
  fi
  exit "$status"
}
trap on_exit EXIT

# Drain with the still-installed legacy uploader before changing any unit.
[[ $(sha256sum "$gate_terminal_receipt" | awk '{print $1}') \
  == "$gate_terminal_receipt_sha256" ]] \
  || die 'Gate terminal receipt changed before cutover transition'
[[ $(sha256sum "$gate_json" | awk '{print $1}') == "$gate_json_sha256" ]] \
  || die 'Gate evidence changed before cutover transition'
[[ $(oss_config_sha256) == "$gate_oss_config_sha" ]] \
  || die 'OSS configuration changed before the cutover transition'
transition_started=true
systemctl stop "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"
systemctl stop "$REFERENCE_UPLOAD_UNIT" "$MARKET_UPLOAD_UNIT"
if [[ $baseline_mode == legacy_python ]]; then
render_upload_unit "/etc/systemd/system/$REFERENCE_UPLOAD_UNIT" \
  "/etc/systemd/system/$REFERENCE_UPLOAD_UNIT" "$pinned_upload_env"
systemctl daemon-reload
verify_effective_unit "$REFERENCE_UPLOAD_UNIT" \
  "/etc/systemd/system/$REFERENCE_UPLOAD_UNIT" "$LEGACY_REFERENCE_UPLOAD_EXEC" \
  || die 'legacy reference uploader effective unit identity is not exact'
grep -Fxq "EnvironmentFile=$pinned_upload_env" \
  "/etc/systemd/system/$REFERENCE_UPLOAD_UNIT" \
  || die 'legacy reference uploader is not pinned to the gated OSS configuration'
else
  verify_upload_units "$baseline_pinned_upload_env" \
    || die 'Rust baseline upload units changed before drain'
fi
systemctl start "$REFERENCE_UPLOAD_UNIT"
verify_oneshot_success "$REFERENCE_UPLOAD_UNIT" \
  || die 'legacy reference uploader drain did not complete successfully'
if [[ $baseline_mode == rust_release ]]; then
  systemctl start "$MARKET_UPLOAD_UNIT"
  verify_oneshot_success "$MARKET_UPLOAD_UNIT" \
    || die 'Rust market uploader drain did not complete successfully'
fi
legacy_stop_cursor=$(journal_cursor "$COLLECTOR_UNIT") \
  || die 'could not capture the legacy collector journal cursor before stop'
[[ $(oss_config_sha256) == "$gate_oss_config_sha" ]] \
  || die 'OSS configuration changed during the legacy uploader drain'
[[ $baseline_mode == legacy_python \
  || $(oss_config_sha256 "$baseline_pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'active Rust uploader configuration changed during drain'
if [[ $baseline_mode == legacy_python ]]; then
verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" "$gate_legacy_invocation_id" \
  || die 'legacy collector identity or restart counter changed during uploader drain'
else
  pre_stop_health_not_before=$(($(date -u +%s) - MAX_HEALTH_SILENCE_SECONDS))
  verify_rust_runtime "$gate_baseline_release_path" "$pre_stop_health_not_before" \
    "$legacy_pid" "$gate_legacy_invocation_id" "$gate_legacy_restarts" \
    "$LEGACY_HEALTH_POLICY" \
    || die 'Rust baseline identity or health changed during uploader drain'
fi

systemctl stop "$COLLECTOR_UNIT"
verify_no_restart_after_cursor \
  "$COLLECTOR_UNIT" "$legacy_stop_cursor" "$gate_legacy_invocation_id" \
  || die 'legacy collector journal recorded a restart during final stop'
stopped_legacy_restarts=$(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT")
[[ $stopped_legacy_restarts == "$gate_legacy_restarts" ]] \
  || die 'legacy collector restarted between final verification and stop'
if [[ $baseline_mode == legacy_python ]]; then
clear_health_before_restart "$evidence_dir" pre-cutover
fi
install -d -m 0755 /opt/monday/bin
temporary_link="${ACTIVE_BINARY}.new.$$"
rm -f "$temporary_link"
ln -s "$candidate_binary" "$temporary_link"
mv -Tf "$temporary_link" "$ACTIVE_BINARY"
remove_snapshotted_control_files "$rollback_dir/state.json" \
  || die 'could not remove snapshotted baseline controls before promotion'
install_control_release "$SCRIPT_DIR"
for asset in "${UNIT_ASSETS[@]}"; do
  case "$asset" in
    "$REFERENCE_UPLOAD_UNIT"|"$MARKET_UPLOAD_UNIT")
      render_upload_unit "$SCRIPT_DIR/$asset" "/etc/systemd/system/$asset" \
        "$pinned_upload_env"
      ;;
    *) atomic_install 0644 "$SCRIPT_DIR/$asset" "/etc/systemd/system/$asset" ;;
  esac
done
systemctl daemon-reload
verify_upload_units "$pinned_upload_env" \
  || die 'Rust upload unit or timer identity differs from the gated configuration'

systemctl reset-failed "$COLLECTOR_UNIT"
[[ $(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT") == 0 ]] \
  || die 'collector restart counter did not reset before Rust verification'
started_epoch=$(date -u +%s)
systemctl restart "$COLLECTOR_UNIT"
rust_pid=
rust_invocation_id=
first_health_updated_at=
health_advanced=false
for _ in $(seq 1 36); do
  rust_restarts=$(systemctl show --property=NRestarts --value "$COLLECTOR_UNIT")
  [[ $rust_restarts == 0 ]] || die 'Rust collector restarted during cutover verification'
  current_pid=$(systemctl show --property=MainPID --value "$COLLECTOR_UNIT")
  if [[ $current_pid =~ ^[1-9][0-9]*$ ]]; then
    if [[ -z $rust_pid ]]; then
      rust_pid=$current_pid
      rust_invocation_id=$(systemctl show --property=InvocationID --value "$COLLECTOR_UNIT")
      [[ $rust_invocation_id =~ ^[a-f0-9]{32}$ ]] \
        || die 'Rust collector has no verifiable systemd invocation ID'
    else
      [[ $current_pid == "$rust_pid" ]] \
        || die 'Rust collector PID changed during cutover verification'
    fi
    if verify_rust_runtime \
      "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id"; then
      current_health_updated_at=$(jq -er '.updated_at' "$HEALTH")
      if [[ -z $first_health_updated_at ]]; then
        first_health_updated_at=$current_health_updated_at
      elif [[ $current_health_updated_at != "$first_health_updated_at" ]]; then
        health_advanced=true
        break
      fi
    fi
  fi
  sleep 5
done
[[ -n $rust_pid ]] || die 'Rust collector never produced a verifiable MainPID'
[[ $health_advanced == true ]] \
  || die 'Rust collector health did not advance across two clean polls'
verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id" \
  || die 'Rust collector failed post-restart identity or health checks'

# The Gate's real-segment OSS readback proves market-uploader compatibility.
# Start its historical backlog asynchronously so promotion remains bounded.
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned OSS configuration changed before Rust uploader verification'
verify_upload_units "$pinned_upload_env" \
  || die 'Rust upload unit or timer identity changed before execution'
systemctl start "$REFERENCE_UPLOAD_UNIT"
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned OSS configuration changed during reference upload'
verify_oneshot_success "$REFERENCE_UPLOAD_UNIT" \
  || die 'Rust reference uploader did not complete successfully'
systemctl reset-failed "$MARKET_UPLOAD_UNIT"
market_upload_invocation_before=$(systemctl show \
  --property=InvocationID --value "$MARKET_UPLOAD_UNIT")
systemctl start --no-block "$MARKET_UPLOAD_UNIT"
verify_deferred_market_upload "$candidate_binary" "$market_upload_invocation_before" \
  || die 'Rust market uploader did not start cleanly for deferred backlog processing'
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned OSS configuration changed during market upload startup'
systemctl enable "$COLLECTOR_UNIT" "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"
systemctl start "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"
unit_active "$REFERENCE_UPLOAD_TIMER" \
  || die 'Rust reference upload timer is not active'
unit_active "$MARKET_UPLOAD_TIMER" \
  || die 'Rust market upload timer is not active'
verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id" \
  || die 'Rust collector identity changed while enabling upload timers'
verify_upload_units "$pinned_upload_env" \
  || die 'Rust upload unit or timer identity changed while enabling timers'
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned OSS configuration changed while enabling upload timers'

journal_file="$evidence_dir/post-start.journal"
journalctl --unit "$COLLECTOR_UNIT" --since "@$started_epoch" --no-pager \
  >"$journal_file"
[[ -s $journal_file ]] || die 'post-start journal evidence is empty'
if grep -Eiq 'panic|fatal|segmentation fault|core dumped' "$journal_file"; then
  die 'post-start journal contains a fatal runtime signal'
fi

main_pid=$(systemctl show --property=MainPID --value "$COLLECTOR_UNIT")
[[ $main_pid == "$rust_pid" ]] || die 'Rust collector PID changed before evidence publication'
! systemctl is-failed --quiet "$MARKET_UPLOAD_UNIT" \
  || die 'Rust market uploader failed during deferred backlog startup'
verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id" \
  || die 'Rust collector identity or health changed before evidence publication'
health_file="$evidence_dir/post-start-health.json"
install -m 0640 "$HEALTH" "$health_file"
sync "$health_file"
verify_rust_health_file "$health_file" "$started_epoch" \
  || die 'snapshotted Rust health is not current and fail-closed clean'
health_sha=$(sha256sum "$health_file" | awk '{print $1}')
journal_sha=$(sha256sum "$journal_file" | awk '{print $1}')
rollback_sha=$(sha256sum "$rollback_dir/manifest.sha256" | awk '{print $1}')
[[ $(sha256sum "$gate_terminal_receipt" | awk '{print $1}') \
  == "$gate_terminal_receipt_sha256" ]] \
  || die 'Gate terminal receipt changed before cutover evidence publication'
[[ $(sha256sum "$gate_json" | awk '{print $1}') == "$gate_json_sha256" ]] \
  || die 'Gate evidence changed before cutover evidence publication'
jq -n \
  --arg schema monday.polymarket_cutover.v1 \
  --arg baseline_mode "$baseline_mode" \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha" \
  --arg deployment_source_revision "$gate_source_revision" \
  --arg release_manifest_sha256 "$gate_release_manifest_sha" \
  --arg control_archive_sha256 "$gate_control_archive_sha" \
  --arg oss_config_sha256 "$gate_oss_config_sha" \
  --arg gate_json_sha256 "$gate_json_sha256" \
  --arg gate_terminal_receipt_sha256 "$gate_terminal_receipt_sha256" \
  --arg gate_systemd_invocation_id "$gate_systemd_invocation_id" \
  --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg health_sha256 "$health_sha" \
  --arg journal_sha256 "$journal_sha" \
  --arg rollback_manifest_sha256 "$rollback_sha" \
  --arg rust_invocation_id "$rust_invocation_id" \
  --argjson main_pid "$main_pid" \
  '{schema:$schema,baseline_mode:$baseline_mode,candidate_sha256:$candidate_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    release_manifest_sha256:$release_manifest_sha256,
    control_archive_sha256:$control_archive_sha256,
    oss_config_sha256:$oss_config_sha256,
    gate_json_sha256:$gate_json_sha256,
    gate_terminal_receipt_sha256:$gate_terminal_receipt_sha256,
    gate_systemd_invocation_id:$gate_systemd_invocation_id,
    completed_at:$completed_at,
    collector:{main_pid:$main_pid,restarts:0,invocation_id:$rust_invocation_id,
      health_sha256:$health_sha256,
      journal_sha256:$journal_sha256},
    rollback_manifest_sha256:$rollback_manifest_sha256,
    explicit_restart:true,post_start_identity_verified:true,
    upload_services_verified:true,
    market_upload_gate_verified:true,
    market_upload_terminal_success_required:false,
    market_backlog_deferred_to_timer:true,
    upload_timers_verified:true,rollback_ready:true}' \
  >"$evidence_dir/cutover.json.tmp"
verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id" \
  || die 'Rust collector identity changed while cutover evidence was being prepared'
verify_upload_units "$pinned_upload_env" \
  || die 'Rust upload unit identity changed while cutover evidence was being prepared'
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned OSS configuration changed while cutover evidence was being prepared'
mv "$evidence_dir/cutover.json.tmp" "$evidence_dir/cutover.json"
sync "$evidence_dir/cutover.json"
sync -f "$rollback_dir"
sync -f "$candidate_binary"
verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id" \
  || die 'Rust collector identity changed before cutover completion'
verify_upload_units "$pinned_upload_env" \
  || die 'Rust upload unit identity changed before cutover completion'
! systemctl is-failed --quiet "$MARKET_UPLOAD_UNIT" \
  || die 'Rust market uploader failed before cutover completion'
[[ $(oss_config_sha256 "$pinned_upload_env") == "$gate_oss_config_sha" ]] \
  || die 'pinned OSS configuration changed before cutover completion'
verify_release_binding "$RELEASE_MANIFEST" "$gate_release_manifest_sha" \
  "$candidate_sha" "$gate_source_revision" "$deployment_bundle_sha" \
  "$gate_control_archive_sha" "$candidate_binary" \
  || die 'release manifest or installed control bundle changed before cutover completion'
sync -f /etc/systemd/system
sync -f /opt/monday
verify_control_release "$CONTROL_DIR" "$candidate_sha" "$candidate_binary" \
  || die 'installed global controls changed before cutover completion'
success_marker="$evidence_dir/PASSED.sha256"
success_marker_tmp="$evidence_dir/.PASSED.sha256.tmp"
[[ ! -e $success_marker && ! -L $success_marker \
  && ! -e $success_marker_tmp && ! -L $success_marker_tmp ]] \
  || die 'cutover success marker path already exists'
secure_root_chain "$evidence_dir" \
  || die 'cutover evidence directory trust changed before success publication'
(
  cd "$evidence_dir"
  sha256sum cutover.json >"${success_marker_tmp##*/}"
)
mv -Tf "$success_marker_tmp" "$success_marker"
sync "$success_marker"
sync -f "$evidence_dir"

cutover_succeeded=true
trap - EXIT
printf '%s\n' "$evidence_dir/cutover.json"
