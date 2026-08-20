#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

readonly SHADOW_BINARY=/opt/monday/bin/binance-lob-archiver-shadow
readonly PRODUCTION_BINARY=/opt/monday/bin/binance-lob-archiver
readonly RELEASE_ROOT=/opt/monday/releases/binance-lob-archiver
readonly LOCK_FILE=/run/lock/monday-rust-lob-release.lock
readonly SERVICE_USER=hftcollector
readonly SERVICE_GROUP=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
readonly SHADOW_SPOOL=/data/monday/spool/binance-lob-rust-shadow/spot
readonly PRODUCTION_SPOOL=/data/monday/spool/binance-lob/spot
readonly SHADOW_UNIT=binance-lob-archiver-rust@spot.service
readonly SHADOW_USDM_UNIT=binance-lob-archiver-rust@usdm.service
readonly SHADOW_UPLOAD_UNIT=binance-lob-archiver-rust-upload@spot.service
readonly SHADOW_USDM_UPLOAD_UNIT=binance-lob-archiver-rust-upload@usdm.service
readonly EVIDENCE_ROOT=/data/monday/evidence/recoveries

die() {
  printf 'shadow recovery failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: host-rust-lob-shadow-recover.sh <candidate-sha256> <expected-production-sha256>' \
    '' \
    'Recovers exactly one isolated Spot shadow spool with the same immutable candidate.' \
    'It never starts a collector or changes the production symlink.'
}

[[ ${EUID} -eq 0 ]] || die 'must run as root'
[[ $# -eq 2 ]] || {
  usage >&2
  exit 2
}

for command in awk cat chown date find flock install jq mountpoint readlink runuser sha256sum stat \
  systemctl tr sort; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done
mountpoint -q /data || die '/data must be a mount point'
id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
install -d -m 0755 "$(dirname "$LOCK_FILE")"
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Rust collector release operation is running'

candidate_sha=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
expected_production_sha=$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 must be 64 hexadecimal characters'
[[ $expected_production_sha =~ ^[a-f0-9]{64}$ ]] \
  || die 'expected production SHA-256 must be 64 hexadecimal characters'

candidate_release="$RELEASE_ROOT/$candidate_sha"
candidate_binary="$candidate_release/binance-lob-archiver"
candidate_deployment="$candidate_release/deployment"
release_json="$candidate_release/release.json"
for path in /opt/monday /opt/monday/releases "$RELEASE_ROOT" "$candidate_release" \
  "$candidate_deployment"; do
  [[ -d $path && ! -L $path && $(readlink -f "$path") == "$path" ]] \
    || die "release path is missing, indirect, or a symlink: $path"
done
[[ -f $release_json && ! -L $release_json ]] || die 'release metadata is missing or a symlink'
deployment_bundle_sha=$(jq -er '.deployment_bundle_sha256' "$release_json")
source_revision=$(jq -er '.deployment_source_revision' "$release_json")
[[ $deployment_bundle_sha =~ ^[a-f0-9]{64}$ ]] \
  || die 'release metadata has an invalid deployment bundle SHA-256'
[[ $source_revision =~ ^[a-f0-9]{40,64}$ ]] \
  || die 'release metadata has an invalid deployment source revision'
[[ -f $candidate_binary && ! -L $candidate_binary && -x $candidate_binary ]] \
  || die "candidate binary is missing or not executable: $candidate_binary"
printf '%s  %s\n' "$candidate_sha" "$candidate_binary" | sha256sum --check --strict >/dev/null \
  || die 'candidate binary digest mismatch'
[[ -L $SHADOW_BINARY && $(readlink -f "$SHADOW_BINARY") == "$candidate_binary" ]] \
  || die 'shadow symlink does not point to the requested candidate'
printf '%s  %s\n' "$candidate_sha" "$SHADOW_BINARY" | sha256sum --check --strict >/dev/null \
  || die 'shadow symlink digest mismatch'
[[ -L $PRODUCTION_BINARY ]] || die 'production symlink is not direct'
production_resolved=$(readlink -f "$PRODUCTION_BINARY")
production_sha=$(sha256sum "$production_resolved" | awk '{print $1}')
[[ $production_sha == "$expected_production_sha" ]] \
  || die 'production binary identity changed before recovery'

for unit in "$SHADOW_UNIT" "$SHADOW_USDM_UNIT" "$SHADOW_UPLOAD_UNIT" "$SHADOW_USDM_UPLOAD_UNIT"; do
  state=$(systemctl show "$unit" -p ActiveState --value 2>/dev/null || true)
  [[ $state == inactive || $state == failed || -z $state ]] \
    || die "shadow writer or uploader is active: $unit ($state)"
  pid=$(systemctl show "$unit" -p MainPID --value 2>/dev/null || true)
  [[ -z $pid || $pid == 0 ]] || die "shadow unit has a managed process: $unit ($pid)"
done

[[ -d $SHADOW_SPOOL && ! -L $SHADOW_SPOOL && $(readlink -f "$SHADOW_SPOOL") == "$SHADOW_SPOOL" ]] \
  || die 'Spot shadow spool is missing, indirect, or a symlink'
[[ -d $PRODUCTION_SPOOL && ! -L $PRODUCTION_SPOOL && $(readlink -f "$PRODUCTION_SPOOL") == "$PRODUCTION_SPOOL" ]] \
  || die 'production Spot spool is missing, indirect, or a symlink'

mapfile -t parts < <(find "$SHADOW_SPOOL" -type f -name '*.jsonl.part' -print | sort)
mapfile -t temporaries < <(find "$SHADOW_SPOOL" -type f -name '*.jsonl.zst.tmp' -print | sort)
mapfile -t corrupt < <(find "$SHADOW_SPOOL" -type f -name '*.part.corrupt' -print | sort)
[[ ${#parts[@]} -eq 1 && ${#temporaries[@]} -eq 1 && ${#corrupt[@]} -eq 0 ]] \
  || die 'recovery requires exactly one Spot .jsonl.part plus one same-stem .zst.tmp and no corrupt artifact'
part=${parts[0]}
temporary=${temporaries[0]}
expected_temporary=${part%.jsonl.part}.jsonl.zst.tmp
[[ $temporary == "$expected_temporary" ]] \
  || die 'the Spot temporary is not the exact same stem as the part'

mapfile -t manifests < <(find "$SHADOW_SPOOL" -type f -name '*.manifest.json' -print | sort)
(( ${#manifests[@]} > 0 )) || die 'recovery requires a prior same-dataset catalog manifest'
catalog_manifest=${manifests[${#manifests[@]}-1]}
[[ -f $catalog_manifest && ! -L $catalog_manifest ]] \
  || die 'catalog manifest is not a direct regular file'
catalog_manifest_sha=$(sha256sum "$catalog_manifest" | awk '{print $1}')

stat_line() {
  stat -c '%n inode=%i dev=%d bytes=%s mtime_ns=%Y uid=%u gid=%g mode=%a' -- "$1"
}
part_before=$(stat_line "$part")
temporary_before=$(stat_line "$temporary")
production_orphan_before=$(find "$PRODUCTION_SPOOL" -type f -name '*.jsonl.part' -printf '%p inode=%i dev=%D bytes=%s mtime_ns=%T@\n' | sort)

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
evidence_dir="$EVIDENCE_ROOT/$candidate_sha/$run_id"
install -d -o "$SERVICE_USER" -g "$SERVICE_GROUP" -m 0750 \
  /data/monday /data/monday/evidence "$EVIDENCE_ROOT" \
  "$EVIDENCE_ROOT/$candidate_sha" "$evidence_dir"
chown "$SERVICE_USER:$SERVICE_GROUP" "$evidence_dir"
chmod 0750 "$evidence_dir"
cat >"$evidence_dir/host-input.txt" <<EOF
candidate_sha256=$candidate_sha
deployment_bundle_sha256=$deployment_bundle_sha
source_revision=$source_revision
production_sha256=$production_sha
shadow_spool=$SHADOW_SPOOL
part=$part
temporary=$temporary
catalog_manifest=$catalog_manifest
catalog_manifest_sha256=$catalog_manifest_sha
part_before=$part_before
temporary_before=$temporary_before
production_orphan_before=$production_orphan_before
EOF
chown "$SERVICE_USER:$SERVICE_GROUP" "$evidence_dir/host-input.txt"
chmod 0640 "$evidence_dir/host-input.txt"

env -i \
  HOME="$SERVICE_HOME" \
  PATH="$SAFE_PATH" \
  RUST_LOG=info \
  SPOOL_DIR="$SHADOW_SPOOL" \
  MARKET=spot \
  DATASET=spot_all_rust_shadow \
  SHARD_ID=all \
  SNAPSHOT_LIMIT=100 \
  ZSTD_TIMEOUT_SECONDS=300 \
  OSS_BUCKET=monday-lob-apne1-1045353359 \
  OSS_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com \
  OSS_REGION=ap-northeast-1 \
  ALIYUN_PROFILE=ecs-role \
  OSS_COPY_TIMEOUT_SECONDS=300 \
  RECOVERY_ARTIFACT_SHA256="$candidate_sha" \
  RECOVERY_SOURCE_REVISION="$source_revision" \
  RECOVERY_BUNDLE_SHA256="$deployment_bundle_sha" \
  RECOVERY_CATALOG_MANIFEST="$catalog_manifest" \
  RECOVERY_EVIDENCE_DIR="$evidence_dir" \
  runuser --user "$SERVICE_USER" -- "$candidate_binary" --recover-only \
  >"$evidence_dir/command.log" 2>&1 || die "candidate recovery failed; evidence: $evidence_dir"
chown "$SERVICE_USER:$SERVICE_GROUP" "$evidence_dir/command.log"
chmod 0640 "$evidence_dir/command.log"

mapfile -t remaining_parts < <(find "$SHADOW_SPOOL" -type f -name '*.jsonl.part' -print | sort)
mapfile -t remaining_temporaries < <(find "$SHADOW_SPOOL" -type f -name '*.jsonl.zst.tmp' -print | sort)
mapfile -t remaining_corrupt < <(find "$SHADOW_SPOOL" -type f -name '*.part.corrupt' -print | sort)
[[ ${#remaining_parts[@]} -eq 0 && ${#remaining_temporaries[@]} -eq 0 && ${#remaining_corrupt[@]} -eq 0 ]] \
  || die "recovery left incomplete artifacts; evidence: $evidence_dir"
[[ $(readlink -f "$PRODUCTION_BINARY") == "$production_resolved" ]] \
  || die 'production symlink changed during recovery'
production_after=$(sha256sum "$production_resolved" | awk '{print $1}')
[[ $production_after == "$production_sha" ]] || die 'production binary changed during recovery'
production_orphan_after=$(find "$PRODUCTION_SPOOL" -type f -name '*.jsonl.part' -printf '%p inode=%i dev=%D bytes=%s mtime_ns=%T@\n' | sort)
[[ $production_orphan_after == "$production_orphan_before" ]] \
  || die 'production Spot orphan identity changed during recovery'

printf 'shadow recovery passed: candidate=%s source=%s bundle=%s evidence=%s\n' \
  "$candidate_sha" "$source_revision" "$deployment_bundle_sha" "$evidence_dir"
