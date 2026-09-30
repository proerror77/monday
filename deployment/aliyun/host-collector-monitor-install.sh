#!/usr/bin/env bash
# Installs only a digest-addressed read-only monitor; writer/controller identity
# stays fixed. The atomic unit installer owns stop/reload/failure rollback.
set -Eeuo pipefail
export LC_ALL=C
[[ $EUID == 0 && $# == 4 ]] || { echo 'usage: STAGE MONITOR_SHA UNIT_PREIMAGE CONTROLLER_SHA' >&2; exit 2; }
stage=$1 monitor=$2 preimage=$3 controller=$4
[[ $monitor =~ ^[a-f0-9]{64}$ && $preimage =~ ^[a-f0-9]{64}$ && $controller =~ ^[a-f0-9]{64}$ ]]
[[ -d $stage && ! -L $stage && $(realpath "$stage") == "$stage" ]]
[[ $(sha256sum "$stage/release.json" | awk '{print $1}') == "$monitor" ]]
jq -e '.schema=="monday.collector_monitor_release.v1" and
  (.source_revision|test("^[a-f0-9]{40}$")) and
  (.assets|keys|sort)==["collector-monitor-retained.sh","collector-monitor.service.template",
    "host-collector-health-unit-release.sh","host-collector-monitor-install.sh","monday-collector-health.sh"]' \
  "$stage/release.json" >/dev/null
for name in $(jq -r '.assets|keys[]' "$stage/release.json"); do
  [[ -f $stage/$name && ! -L $stage/$name && $(stat -c %u "$stage/$name") == 0
    && $(stat -c %h "$stage/$name") == 1 ]]
  [[ $(sha256sum "$stage/$name" | awk '{print $1}') == "$(jq -r --arg name "$name" '.assets[$name]' "$stage/release.json")" ]]
done
(cd "$stage"; sha256sum --check --strict assets.sha256 >/dev/null)
active=/opt/monday/releases/binance-lob-controller/active
[[ $(readlink -f "$active") == "/opt/monday/releases/binance-lob-controller/$controller" ]]
before=$(systemctl show binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
  -p Id -p MainPID -p InvocationID)
root=/opt/monday/monitor/releases
[[ ! -L /opt/monday/monitor && ! -L $root ]]
install -d -o root -g root -m 0755 "$root"
[[ $(realpath "$root") == "$root" ]]
destination="$root/$monitor"
if [[ -e $destination ]]; then
  [[ -d $destination && ! -L $destination ]]
  cmp "$stage/release.json" "$destination/release.json"
  (cd "$destination"; sha256sum --check --strict assets.sha256 >/dev/null)
else
  temporary=$(mktemp -d "$root/.stage.XXXXXX")
  for name in release.json assets.sha256 $(jq -r '.assets|keys[]' "$stage/release.json"); do
    install -o root -g root -m 0555 "$stage/$name" "$temporary/$name"
  done
  chmod 0555 "$temporary"
  mv -T -- "$temporary" "$destination"
  sync -f "$root"
fi
[[ $(readlink -f "$active") == "/opt/monday/releases/binance-lob-controller/$controller" ]]
unit="$stage/resolved.service"
sed "s|__MONITOR_ROOT__|$destination|g" "$destination/collector-monitor.service.template" >"$unit"
chmod 0644 "$unit"
unit_sha=$(sha256sum "$unit" | awk '{print $1}')
source_revision=$(jq -r .source_revision "$destination/release.json")
bash "$destination/host-collector-health-unit-release.sh" "$unit" "$unit_sha" "$preimage" "$source_revision"
after=$(systemctl show binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
  -p Id -p MainPID -p InvocationID)
[[ $before == "$after" && $(readlink -f "$active") == "/opt/monday/releases/binance-lob-controller/$controller" ]]
jq -n --arg monitor "$monitor" --arg controller "$controller" --arg unit "$unit_sha" --arg source "$source_revision" \
  '{schema:"monday.collector_monitor_install.v1",monitor_sha256:$monitor,controller_sha256:$controller,
    source_revision:$source,unit_sha256:$unit,producer_state_unchanged:true,result:"success"}'
