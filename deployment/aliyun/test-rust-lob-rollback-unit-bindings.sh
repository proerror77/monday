#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
if [[ ${1:-} != --container-fixture ]]; then
  # Reuse an explicitly selected, already present Linux image; never pull or
  # install a toolchain just to run this unit-file regression.
  image=${MONDAY_SYSTEMD_FIXTURE_IMAGE:?set an existing image containing systemctl, bash, jq and coreutils}
  docker image inspect "$image" >/dev/null
  exec docker run --rm --pull never --network none --read-only \
    --tmpfs /tmp:rw,exec,mode=1777 --tmpfs /opt/monday:mode=755 \
    --tmpfs /etc/systemd/system:mode=755 --tmpfs /run/systemd/system:mode=755 \
    --mount "type=bind,source=$SCRIPT_DIR,target=/test,readonly" \
    --entrypoint /bin/bash "$image" /test/test-rust-lob-rollback-unit-bindings.sh --container-fixture
fi

# This test uses real canonical paths. Refuse any host or non-empty mount;
# all unit-file mutations below belong to the disposable tmpfs container.
[[ $(id -u) == 0 && -f /.dockerenv && $(uname -s) == Linux ]]
for directory in /opt/monday /etc/systemd/system /run/systemd/system; do
  [[ $(stat -f -c %T "$directory") == tmpfs ]]
  [[ -z $(find "$directory" -mindepth 1 -maxdepth 1 -print -quit) ]]
done
command systemctl --version
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
controller_root=/opt/monday/releases/binance-lob-controller
payload_root=/opt/monday/releases/binance-lob-archiver
mkdir -p "$controller_root" "$payload_root" "$work/source"
while IFS= read -r asset; do
  cp -- "$SCRIPT_DIR/$asset" "$work/source/$asset"
done < <({ monday_runtime_assets; monday_controller_assets; } | sort -u)
publish_fixture() {
  local runtime manifest sha asset payload
  runtime=$(monday_rust_lob_runtime_contract_sha256 "$work/source")
  printf '#!/bin/sh\n# %s\nexit 0\n' "$runtime" >"$work/binary"
  payload=$(monday_sha256_file "$work/binary")
  mkdir "$payload_root/$payload"
  cp -- "$work/binary" "$payload_root/$payload/binance-lob-archiver"
  manifest="$work/manifest.json"
  jq -cnS --arg payload "$payload" --arg runtime "$runtime" \
    --arg source "$(printf 'a%.0s' {1..40})" --arg bundle "$(printf 'b%.0s' {1..64})" '
    {schema:"monday.rust_lob_controller_release.v2",control_plane_version:2,
     topology:"stable",artifact_uri:"oss://fixture/payload",artifact_sha256:$payload,
     runtime_contract_sha256:$runtime,deployment_source_revision:$source,
     deployment_bundle_uri:"oss://fixture/controller",deployment_bundle_sha256:$bundle}' >"$manifest"
  sha=$(monday_sha256_file "$manifest")
  mkdir -p "$controller_root/$sha/deployment"
  cp -- "$manifest" "$controller_root/$sha/release.json"
  cp -- "$work/source/"* "$controller_root/$sha/deployment/"
  (
    cd "$controller_root/$sha"
    monday_sha256_checksum_line release.json >release.json.sha256
    for asset in deployment/*; do monday_sha256_checksum_line "$asset"; done | sort -k2 >deployment.sha256
  )
  ln -s "$payload_root/$payload/binance-lob-archiver" "$controller_root/$sha/binance-lob-archiver"
  monday_verify_controller_release / "$sha"
  printf '%s\n' "$sha"
}
c0=$(publish_fixture)
sed -i 's/^RuntimeMaxSec=infinity$/RuntimeMaxSec=6h/' "$work/source/binance-lob-archiver-production@.service"
c1=$(publish_fixture)
[[ $c0 != "$c1" ]]
asset=binance-lob-archiver-production@.service
unit=binance-lob-archiver-production@spot.service
template="/etc/systemd/system/$asset"
instance="/etc/systemd/system/$unit"
ln -s "$controller_root/c0" "$controller_root/active"
ln -s "$controller_root/active/deployment/$asset" "$template"
printf '[Unit]\nDescription=Fixture target\n' >/etc/systemd/system/multi-user.target
printf '%s\tloaded\tactive\tenabled\n' "$unit" >"$work/snapshot"

select_controller() { ln -sfn -- "$controller_root/$1" "$controller_root/active"; }
prepare_candidate_instance() {
  command systemctl --root=/ disable "$unit" >/dev/null 2>&1
  command systemctl --root=/ --runtime unmask "$unit" >/dev/null 2>&1
  select_controller "$c1"
  command systemctl --root=/ enable "$unit" >/dev/null 2>&1
  [[ $(readlink -- "$instance") == "$controller_root/$c1/deployment/$asset" ]]
  select_controller "$c0"
}

# First reproduce the old behavior with the real Linux unit-file installer.
prepare_candidate_instance
command systemctl --root=/ --runtime unmask "$unit" >/dev/null 2>&1
command systemctl --root=/ enable "$unit" >/dev/null 2>&1
[[ $(readlink -- "$instance") == "$controller_root/$c1/deployment/$asset" ]]
[[ $(command systemctl --root=/ is-enabled "$unit") == enabled ]]
printf 'reproduced: enable preserves the C1 instance after active=C0\n'

# There is no service manager in the container. Only service start/stop/show
# are modelled; disable/enable/mask/unmask use real systemctl and real files.
declare -A activity=()
starts=0 stops=0 fail_stop=false bad_fragment=false bad_lifetime=false
selected_fragment() {
  local requested=$1 path="/etc/systemd/system/$1"
  if [[ -e $path || -L $path ]]; then printf '%s\n' "$path"
  else printf '/etc/systemd/system/%s@.service\n' "${requested%@*}"; fi
}
# shellcheck disable=SC2329 # Called indirectly by the production restore function.
systemctl() {
  local action=$1 requested=${2:-} property fragment
  case "$action" in
    stop)
      stops=$((stops + 1))
      [[ $fail_stop == false ]] || return 1
      activity[$requested]=inactive ;;
    start)
      [[ $(readlink -f -- "$(selected_fragment "$requested")") == "$controller_root/$c0/deployment/$asset" ]] || return 1
      starts=$((starts + 1)); activity[$requested]=active ;;
    daemon-reload) : ;;
    show)
      property=${3#--property=}
      case "$property" in
        FragmentPath)
          if [[ $bad_fragment == true ]]; then printf '%s\n' "$controller_root/$c1/deployment/$asset"
          else selected_fragment "$requested"; fi ;;
        RuntimeMaxUSec)
          [[ $bad_lifetime == false ]] || { printf '6h\n'; return; }
          fragment=$(selected_fragment "$requested")
          sed -n 's/^RuntimeMaxSec=//p' "$fragment" ;;
        ActiveState) printf '%s\n' "${activity[$requested]:-inactive}" ;;
        LoadState)
          if [[ -L /run/systemd/system/$requested && ! -e /etc/systemd/system/$requested ]]; then printf 'masked\n'
          else printf 'loaded\n'; fi ;;
        UnitFileState) command systemctl --root=/ is-enabled "$requested" || : ;;
        *) return 1 ;;
      esac ;;
    disable|enable|mask|unmask) command systemctl --root=/ "$@" ;;
    *) return 1 ;;
  esac
}

restore() { monday_rust_lob_restore_writer_snapshot "$work/snapshot" v2 / "$c0" "$c1"; }
restore
[[ $starts == 1 && $(readlink -- "$instance") == "$controller_root/$c0/deployment/$asset" ]]
[[ $(command systemctl --root=/ is-enabled "$unit") == enabled ]]
printf 'passed: restore rebuilt C0 before starting production\n'

# A corrupted candidate executable is a reason to restore C0, not a reason to
# refuse unlinking the verified candidate unit configuration.
prepare_candidate_instance
candidate_binary=$(readlink -- "$controller_root/$c1/binance-lob-archiver")
cp -- "$candidate_binary" "$work/candidate-backup"
printf 'candidate payload failure\n' >"$candidate_binary"
restore
[[ $starts == 2 && $(readlink -- "$instance") == "$controller_root/$c0/deployment/$asset" ]]
cp -- "$work/candidate-backup" "$candidate_binary"

# No candidate enable happened yet: the exact instance is absent under a
# runtime mask, and rollback must still recreate C0 from the stable template.
command systemctl --root=/ disable "$unit" >/dev/null 2>&1
command systemctl --root=/ --runtime mask "$unit" >/dev/null 2>&1
restore
[[ $starts == 3 && $(readlink -- "$instance") == "$controller_root/$c0/deployment/$asset" ]]

for failure in regular unknown-link template-drift wrong-active failed-stop bad-fragment bad-lifetime; do
  prepare_candidate_instance
  before_starts=$starts; before_stops=$stops
  case "$failure" in
    regular) rm -- "$instance"; cp "$controller_root/$c1/deployment/$asset" "$instance" ;;
    unknown-link) rm -- "$instance"; ln -s /tmp/not-owned "$instance" ;;
    template-drift) rm -- "$template"; ln -s "$controller_root/$c1/deployment/$asset" "$template" ;;
    wrong-active) select_controller "$c1" ;;
    failed-stop) fail_stop=true ;;
    bad-fragment) bad_fragment=true ;;
    bad-lifetime) bad_lifetime=true ;;
  esac
  if restore; then printf 'accepted unsafe rollback: %s\n' "$failure" >&2; exit 1; fi
  [[ $starts == "$before_starts" ]]
  case "$failure" in
    regular|unknown-link|template-drift|wrong-active) [[ $stops == "$before_stops" ]] ;;
  esac
  [[ ! -e $instance && ! -L $instance ]] || rm -- "$instance"
  rm -- "$template"; ln -s "$controller_root/active/deployment/$asset" "$template"
  select_controller "$c0"
  fail_stop=false bad_fragment=false bad_lifetime=false
done

# Existing masked, disabled, and static snapshots must not become enabled or
# start a service. These branches retain their previous restore behavior.
for state in masked-runtime disabled static; do
  command systemctl --root=/ --runtime unmask "$unit" >/dev/null 2>&1
  command systemctl --root=/ disable "$unit" >/dev/null 2>&1
  if [[ $state == static ]]; then
    # A separate static allowlisted shadow instance, not a production release.
    snapshot_unit=binance-lob-archiver-rust-upload@spot.service
    printf '[Service]\nType=oneshot\nExecStart=/bin/true\n' >/etc/systemd/system/binance-lob-archiver-rust-upload@.service
  else snapshot_unit=$unit; fi
  load=loaded; [[ $state != masked-runtime ]] || load=masked
  printf '%s\t%s\tinactive\t%s\n' "$snapshot_unit" "$load" "$state" >"$work/snapshot"
  before_starts=$starts
  restore
  [[ $starts == "$before_starts" ]]
  [[ $(command systemctl --root=/ is-enabled "$snapshot_unit" || :) == "$state" ]]
done
printf 'passed: 7 unsafe boundaries and masked/disabled/static state preservation\n'
