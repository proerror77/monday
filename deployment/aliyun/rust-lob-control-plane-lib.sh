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

monday_controller_release_sha256() {
  [[ $# -eq 1 ]] || return 2
  monday_sha256_file "$1/release.json"
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
  local root=${1%/} link
  link="$root/opt/monday/releases/binance-lob-controller/active"
  local target sha
  [[ -L $link ]] || return 1
  target=$(readlink -f -- "$link") || return 1
  sha=${target##*/}
  [[ $target == "$root/opt/monday/releases/binance-lob-controller/$sha" \
    && $sha =~ ^[a-f0-9]{64}$ ]] || return 1
  printf '%s\n' "$sha"
}

monday_verify_controller_release() {
  [[ $# -eq 2 ]] || return 2
  local root=${1%/} sha=$2
  local release="$root/opt/monday/releases/binance-lob-controller/$sha"
  local manifest="$release/release.json" asset expected projection target payload
  monday_sha256_ok "$sha" || return 1
  monday_path_direct "$root/opt/monday/releases/binance-lob-controller" || return 1
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
  target="$root/opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver"
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
  monday_file_direct "$gate" || return 1
  [[ $(monday_sha256_file "$gate") == "$gate_sha" ]] || return 1
  jq -e \
    --arg from "$from" --arg candidate "$candidate" --arg gate_sha "$gate_sha" '
      .schema == "monday.rust_lob_shadow_gate.v5"
      and .control_plane_version == 2
      and .passed == true
      and .transition.before == $from
      and .transition.after == $candidate
      and (.candidate_controller_sha256 == $candidate)
      and (.candidate_payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.checks | type == "object")' \
    "$gate" >/dev/null
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
