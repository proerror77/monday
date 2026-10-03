#!/usr/bin/env bash
# Separately deploy the host-wide monitor unit. LOB controller releases own the
# health executable; they do not project this host-wide systemd unit.
set -Eeuo pipefail

health_unit_sha() { sha256sum -- "$1" | awk '{print $1}'; }
health_unit_state() { systemctl show "$1" -p ActiveState --value; }

deploy_health_unit() {
  local source=$1 source_sha=$2 before_sha=$3 revision=$4 target=$5 receipt_root=$6
  local timer=monday-collector-health.timer service=monday-collector-health.service
  local before_active service_active backup temporary actual completed=false modified=false
  [[ $source_sha =~ ^[a-f0-9]{64}$ && $before_sha =~ ^[a-f0-9]{64}$
    && $revision =~ ^[a-f0-9]{40}$ ]] || return 1
  [[ -f $source && ! -L $source && -f $target && ! -L $target
    && $(realpath -- "$source") == "$source" && $(realpath -- "$target") == "$target"
    && $(stat -c %u -- "$source") == 0 && $(stat -c %u -- "$target") == 0
    && $(stat -c %a -- "$target") == 644
    && $(stat -c %h -- "$source") == 1 && $(stat -c %h -- "$target") == 1
    && $(health_unit_sha "$source") == "$source_sha"
    && $(health_unit_sha "$target") == "$before_sha" ]] || return 1
  # Never claim a larger effective budget while a drop-in overrides this unit.
  [[ -z $(systemctl show "$service" -p DropInPaths --value)
    && $(systemctl show "$service" -p FragmentPath --value) == "$target" ]] || return 1
  [[ $(awk -F= '$1=="TimeoutStartSec" {print $2}' "$source") == 480 ]] || return 1
  before_active=$(health_unit_state "$timer")
  service_active=$(health_unit_state "$service")
  [[ $before_active == active || $before_active == inactive ]] || return 1
  [[ $service_active == active || $service_active == activating || $service_active == inactive || $service_active == failed ]] || return 1
  [[ ! -e $receipt_root && ! -L $receipt_root ]] || return 1
  [[ $(realpath -- "$(dirname -- "$receipt_root")") == "$(dirname -- "$receipt_root")" ]] || return 1
  mkdir -m 0750 -- "$receipt_root"
  backup="$receipt_root/before.service"
  cp -p -- "$target" "$backup"
  [[ $(health_unit_sha "$backup") == "$before_sha" ]] || return 1
  temporary="$target.health-release.$$"
  [[ ! -e $temporary && ! -L $temporary ]] || return 1

  # The trap is armed before stopping even the read-only monitor. It never
  # touches producer/recovery units or the persistent monitor's state directory.
  cleanup_health_unit() {
    local status=$? rollback=not_needed
    trap - EXIT HUP INT TERM
    if [[ $modified == true && $completed != true ]]; then
      rollback=failed
      if [[ -f $target && ! -L $target ]] \
        && { [[ $(health_unit_sha "$target") == "$source_sha" ]] || [[ $(health_unit_sha "$target") == "$before_sha" ]]; } \
        && cp -p -- "$backup" "$temporary" && mv -f -- "$temporary" "$target" \
        && systemctl daemon-reload \
        && [[ $(health_unit_sha "$target") == "$before_sha" ]]; then
        rollback=passed
        if [[ $before_active == active ]]; then systemctl start "$timer" || rollback=failed; fi
        if [[ $service_active == active || $service_active == activating ]]; then systemctl start "$service" || rollback=failed; fi
      fi
      jq -n --arg source "$revision" --arg before "$before_sha" --arg after "$source_sha" \
        --arg rollback "$rollback" --argjson exit "$status" \
        '{schema:"monday.collector_health_unit_release.v1",result:"failed",
          source_revision:$source,before_sha256:$before,target_sha256:$after,
          rollback:$rollback,exit_code:$exit}' >"$receipt_root/result.json"
    fi
    rm -f -- "$temporary"
    exit "$status"
  }
  trap cleanup_health_unit EXIT
  trap 'exit 143' HUP INT TERM
  modified=true
  systemctl stop "$timer" "$service"
  # Re-read after containment so drift cannot be overwritten by the install.
  [[ $(health_unit_sha "$target") == "$before_sha" && $(health_unit_sha "$source") == "$source_sha" ]]
  install -o root -g root -m 0644 -- "$source" "$temporary"
  mv -f -- "$temporary" "$target"
  sync -f -- "$target"
  systemctl daemon-reload
  actual=$(systemctl show "$service" -p TimeoutStartUSec --value)
  [[ $(health_unit_sha "$target") == "$source_sha" && $actual == '8min'
    && -z $(systemctl show "$service" -p DropInPaths --value)
    && $(systemctl show "$service" -p FragmentPath --value) == "$target" ]]
  if [[ $before_active == active ]]; then
    systemctl start "$timer"
    [[ $(health_unit_state "$timer") == active ]]
  fi
  jq -n --arg source "$revision" --arg before "$before_sha" --arg after "$source_sha" \
    --arg timeout "$actual" --arg timer "$before_active" \
    '{schema:"monday.collector_health_unit_release.v1",result:"success",
      source_revision:$source,before_sha256:$before,target_sha256:$after,
      effective_timeout:$timeout,timer_state:$timer,rollback_backup:"before.service"}' \
    >"$receipt_root/result.json"
  chmod 0440 -- "$backup" "$receipt_root/result.json"
  sync -f -- "$receipt_root"
  completed=true
  trap - EXIT HUP INT TERM
  cat -- "$receipt_root/result.json"
}

main() {
  [[ $EUID == 0 && $# == 4 ]] || { printf 'usage: %s ABSOLUTE_SOURCE SOURCE_SHA256 BEFORE_SHA256 SOURCE_REVISION\n' "$0" >&2; return 2; }
  local source=$1 source_sha=$2 before_sha=$3 revision=$4 parent
  parent=/data/monday/evidence/health-unit-releases
  [[ -d /data/monday/evidence && $(realpath /data/monday/evidence) == /data/monday/evidence ]]
  [[ ! -L $parent ]]
  install -d -o root -g root -m 0750 "$parent"
  exec 9>/run/lock/monday-collector-health-unit-release.lock
  flock -n 9 || { printf 'another health-unit release owns the transition\n' >&2; return 1; }
  deploy_health_unit "$source" "$source_sha" "$before_sha" "$revision" \
    /etc/systemd/system/monday-collector-health.service "$parent/$source_sha"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
