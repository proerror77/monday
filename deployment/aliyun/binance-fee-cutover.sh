#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <artifact-directory> <controller>\n' "${0##*/}" >&2
}

configure_paths() {
  local root=${1%/}
  RELEASE_ROOT="$root/opt/monday/releases/binance-fee"
  BIN_ROOT="$root/opt/monday/bin"
  SNAPSHOT_LINK="$BIN_ROOT/binance-fee-snapshot"
  UPLOAD_LINK="$BIN_ROOT/binance-fee-snapshot-upload"
  SYSTEMD_ROOT="$root/etc/systemd/system"
  TMPFILES_ROOT="$root/etc/tmpfiles.d"
  CONFIG_ROOT="$root/etc/monday"
  CREDENTIAL_PATH="$CONFIG_ROOT/credentials/binance-account.json"
  UPLOAD_ENV_PATH="$CONFIG_ROOT/binance-fee-upload.env"
  DATA_ROOT="$root/data"
  FEE_SPOOL_ROOT="$DATA_ROOT/monday/spool/binance-fee"
  EVIDENCE_ROOT="$DATA_ROOT/monday/evidence/binance-fee-cutovers"
  LOCK_ROOT="$root/run/lock"
}

die() {
  FAILURE_REASON=$*
  printf '%s\n' "$*" >&2
  exit 1
}

direct_directory() {
  local path=$1 resolved
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

direct_regular_file() {
  local path=$1 resolved
  [[ -f $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

secure_readonly_file() {
  local path=$1 owner mode
  direct_regular_file "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == "$EXPECTED_ROOT_UID" ]] || return 1
  (( (8#$mode & 0022) == 0 ))
}

secure_secret_file() {
  local path=$1 owner mode
  direct_regular_file "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == "$EXPECTED_ROOT_UID" ]] || return 1
  (( (8#$mode & 0077) == 0 ))
}

env_value() {
  local file=$1 key=$2
  awk -F= -v key="$key" '
      $1 == key { count += 1; value = substr($0, length(key) + 2) }
      END { if (count != 1) exit 1; print value }
    ' "$file"
}

require_env_value() {
  local file=$1 key=$2 expected=$3 actual
  actual=$(env_value "$file" "$key") \
    || die "$file must contain exactly one $key setting"
  [[ $actual == "$expected" ]] \
    || die "$file has unsafe $key=$actual (expected $expected)"
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  install -m "$mode" "$source" "$temporary"
  mv -Tf "$temporary" "$destination"
}

immutable_install() {
  local mode=$1 source=$2 destination=$3 actual_mode expected_mode=${1#0}
  if [[ -e $destination || -L $destination ]]; then
    direct_regular_file "$destination" \
      || die "immutable release member is not a direct regular file: $destination"
    cmp -s "$source" "$destination" \
      || die "immutable release member drifted: $destination"
    actual_mode=$(stat -c %a -- "$destination")
    [[ $actual_mode == "$expected_mode" ]] \
      || die "immutable release member has mode $actual_mode: $destination"
    return 0
  fi
  atomic_install "$mode" "$source" "$destination"
}

atomic_symlink() {
  local target=$1 link=$2 temporary
  temporary="${link}.new.$$"
  rm -f "$temporary"
  ln -s "$target" "$temporary"
  mv -Tf "$temporary" "$link"
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

service_result() {
  systemctl show "$1" --property=Result --value
}

service_exit_status() {
  systemctl show "$1" --property=ExecMainStatus --value
}

timer_enabled_or_active() {
  local timer=$1 state
  state=$(systemctl is-enabled "$timer" 2>/dev/null || true)
  [[ $state == enabled ]] || systemctl is-active --quiet "$timer"
}

timer_enabled_and_active() {
  local timer=$1 state
  state=$(systemctl is-enabled "$timer" 2>/dev/null || true)
  [[ $state == enabled ]] && systemctl is-active --quiet "$timer"
}

validate_credential() {
  secure_secret_file "$CREDENTIAL_PATH" \
    || die "credential must be a direct root-owned 0600 regular file: $CREDENTIAL_PATH"
  jq -e '
    type == "object"
    and (keys | sort) == ["api_key", "runtime_account_id", "secret"]
    and (.runtime_account_id | type == "string" and (gsub("^\\s+|\\s+$"; "") | length) > 0)
    and (.api_key | type == "string" and (gsub("^\\s+|\\s+$"; "") | length) > 0)
    and (.secret | type == "string" and (gsub("^\\s+|\\s+$"; "") | length) > 0)
  ' "$CREDENTIAL_PATH" >/dev/null \
    || die 'credential JSON must contain exactly runtime_account_id/api_key/secret as non-empty strings'
}

validate_release_sidecar() {
  local file=$1 sidecar=$2 expected_name=$3 digest
  secure_readonly_file "$sidecar" || die "missing sidecar: $sidecar"
  [[ $(wc -l <"$sidecar") -eq 1 ]] || die "$sidecar must contain exactly one line"
  digest=$(awk 'NR == 1 {print $1}' "$sidecar")
  [[ $(awk 'NR == 1 {print $2}' "$sidecar") == "$expected_name" ]] \
    || die "$sidecar must name only $expected_name"
  [[ $digest =~ ^[a-f0-9]{64}$ ]] || die "$sidecar has an invalid digest"
  [[ $(sha256sum "$file" | awk '{print $1}') == "$digest" ]] \
    || die "$file does not match $sidecar"
  printf '%s\n' "$digest"
}

validate_artifact_bundle() {
  local artifact_dir=$1 source_artifact_dir archive_sidecar extracted_manifest member
  source_artifact_dir=$(readlink -f -- "$artifact_dir") \
    || die "artifact directory is not canonical: $artifact_dir"
  direct_directory "$source_artifact_dir" \
    || die "artifact directory is not a direct directory: $artifact_dir"
  VALIDATED_ARTIFACT_DIR=$(readlink -f -- "$(mktemp -d)")
  chmod 0700 "$VALIDATED_ARTIFACT_DIR"
  for member in \
    binance-fee-snapshot \
    binance-fee-snapshot.sha256 \
    binance-fee-snapshot-upload \
    binance-fee-snapshot-upload.sha256 \
    binance-fee-release.json \
    binance-fee-release.json.sha256 \
    binance-fee-production-control-assets.sha256 \
    binance-fee-production-control.tar.gz \
    binance-fee-production-control.tar.gz.sha256; do
    direct_regular_file "$source_artifact_dir/$member" \
      || die "artifact member is missing or not a direct regular file: $member"
    install -m 0600 "$source_artifact_dir/$member" "$VALIDATED_ARTIFACT_DIR/$member"
  done
  ARTIFACT_DIR=$VALIDATED_ARTIFACT_DIR

  ARTIFACT_SNAPSHOT="$ARTIFACT_DIR/binance-fee-snapshot"
  ARTIFACT_SNAPSHOT_SIDECAR="$ARTIFACT_DIR/binance-fee-snapshot.sha256"
  ARTIFACT_UPLOADER="$ARTIFACT_DIR/binance-fee-snapshot-upload"
  ARTIFACT_UPLOADER_SIDECAR="$ARTIFACT_DIR/binance-fee-snapshot-upload.sha256"
  RELEASE_MANIFEST="$ARTIFACT_DIR/binance-fee-release.json"
  RELEASE_MANIFEST_SIDECAR="$ARTIFACT_DIR/binance-fee-release.json.sha256"
  CONTROL_MANIFEST="$ARTIFACT_DIR/binance-fee-production-control-assets.sha256"
  CONTROL_ARCHIVE="$ARTIFACT_DIR/binance-fee-production-control.tar.gz"
  archive_sidecar="$ARTIFACT_DIR/binance-fee-production-control.tar.gz.sha256"

  secure_readonly_file "$ARTIFACT_SNAPSHOT" || die "missing candidate binary: $ARTIFACT_SNAPSHOT"
  secure_readonly_file "$ARTIFACT_UPLOADER" || die "missing uploader binary: $ARTIFACT_UPLOADER"
  secure_readonly_file "$RELEASE_MANIFEST" || die "missing release manifest: $RELEASE_MANIFEST"
  secure_readonly_file "$CONTROL_MANIFEST" || die "missing control manifest: $CONTROL_MANIFEST"
  secure_readonly_file "$CONTROL_ARCHIVE" || die "missing control archive: $CONTROL_ARCHIVE"

  CANDIDATE_SHA256=$(validate_release_sidecar \
    "$ARTIFACT_SNAPSHOT" "$ARTIFACT_SNAPSHOT_SIDECAR" binance-fee-snapshot)
  UPLOADER_SHA256=$(validate_release_sidecar \
    "$ARTIFACT_UPLOADER" "$ARTIFACT_UPLOADER_SIDECAR" binance-fee-snapshot-upload)
  RELEASE_MANIFEST_SHA256=$(validate_release_sidecar \
    "$RELEASE_MANIFEST" "$RELEASE_MANIFEST_SIDECAR" binance-fee-release.json)
  CONTROL_ARCHIVE_SHA256=$(validate_release_sidecar \
    "$CONTROL_ARCHIVE" "$archive_sidecar" binance-fee-production-control.tar.gz)
  CONTROL_MANIFEST_SHA256=$(sha256sum "$CONTROL_MANIFEST" | awk '{print $1}')

  jq -e \
    --arg candidate "$CANDIDATE_SHA256" \
    --arg uploader "$UPLOADER_SHA256" \
    --arg control_manifest "$CONTROL_MANIFEST_SHA256" \
    --arg control_archive "$CONTROL_ARCHIVE_SHA256" '
      .schema == "monday.binance_fee_release.v1"
      and (.source_revision | type == "string" and length > 0)
      and .candidate == {file:"binance-fee-snapshot", sha256:$candidate}
      and .uploader == {file:"binance-fee-snapshot-upload", sha256:$uploader}
      and .control_manifest == {
        file:"binance-fee-production-control-assets.sha256",
        sha256:$control_manifest
      }
      and .control_archive == {
        file:"binance-fee-production-control.tar.gz",
        sha256:$control_archive
      }
    ' "$RELEASE_MANIFEST" >/dev/null \
    || die 'release manifest does not bind the candidate, uploader, and fee control bundle'
  SOURCE_REVISION=$(jq -er \
    '.source_revision | select(type == "string" and test("^[a-f0-9]{40,64}$"))' \
    "$RELEASE_MANIFEST") || die 'release manifest has no valid source_revision'

  CONTROL_STAGE=$(readlink -f -- "$(mktemp -d)")
  printf '%s\n' \
    binance-fee-cutover.sh \
    binance-fee-snapshot-spot.service \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.service \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.service \
    binance-fee-upload.timer \
    binance-fee-upload.env \
    binance-fee.conf \
    monday-collector-health.sh | sort >"$CONTROL_STAGE/expected-members.lst"
  tar -tzf "$CONTROL_ARCHIVE" | sort >"$CONTROL_STAGE/archive-members.lst"
  cmp -s "$CONTROL_STAGE/expected-members.lst" "$CONTROL_STAGE/archive-members.lst" \
    || die 'control archive has unexpected members'
  tar -tvzf "$CONTROL_ARCHIVE" \
    | awk '$1 !~ /^-/ { bad = 1 } END { exit bad }' \
    || die 'control archive members must be regular files'
  tar -xzf "$CONTROL_ARCHIVE" -C "$CONTROL_STAGE"
  extracted_manifest="$CONTROL_STAGE/.control-assets.sha256"
  (
    cd "$CONTROL_STAGE"
    sha256sum \
      binance-fee-cutover.sh \
      binance-fee-snapshot-spot.service \
      binance-fee-snapshot-spot.timer \
      binance-fee-snapshot-usdm.service \
      binance-fee-snapshot-usdm.timer \
      binance-fee-upload.service \
      binance-fee-upload.timer \
      binance-fee-upload.env \
      binance-fee.conf \
      monday-collector-health.sh
  ) >"$extracted_manifest"
  cmp -s "$CONTROL_MANIFEST" "$extracted_manifest" \
    || die 'control archive content drifted from the signed control manifest'
  cmp -s "$CONTROL_STAGE/binance-fee-cutover.sh" "$0" \
    || die 'invoked cutover script does not match the signed control archive'

  require_env_value "$CONTROL_STAGE/binance-fee-upload.env" OSS_BUCKET monday-lob-apne1-1045353359
  require_env_value "$CONTROL_STAGE/binance-fee-upload.env" OSS_ENDPOINT oss-ap-northeast-1-internal.aliyuncs.com
  require_env_value "$CONTROL_STAGE/binance-fee-upload.env" OSS_REGION ap-northeast-1
  require_env_value "$CONTROL_STAGE/binance-fee-upload.env" ALIYUN_PROFILE ecs-role
}

snapshot_baseline() {
  BASELINE_MODE=absent
  if timer_enabled_or_active binance-fee-snapshot-spot.timer \
    || timer_enabled_or_active binance-fee-snapshot-usdm.timer \
    || timer_enabled_or_active binance-fee-upload.timer; then
    die 'fee production already active'
  fi
  if systemctl is-active --quiet binance-fee-snapshot-spot.service \
    || systemctl is-active --quiet binance-fee-snapshot-usdm.service \
    || systemctl is-active --quiet binance-fee-upload.service; then
    die 'fee production already active'
  fi

  BASELINE_ROOT="$EVIDENCE_DIR/baseline"
  install -d -m 0750 "$BASELINE_ROOT/files"
  : >"$BASELINE_ROOT/symlinks.tsv"
  : >"$BASELINE_ROOT/files.lst"
  local mode
  for path in \
    "$SNAPSHOT_LINK" \
    "$UPLOAD_LINK" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-spot.service" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-spot.timer" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-usdm.service" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-usdm.timer" \
    "$SYSTEMD_ROOT/binance-fee-upload.service" \
    "$SYSTEMD_ROOT/binance-fee-upload.timer" \
    "$UPLOAD_ENV_PATH" \
    "$TMPFILES_ROOT/binance-fee.conf"; do
    if [[ -L $path ]]; then
      BASELINE_MODE=partial-contained
      printf '%s\t%s\n' "$path" "$(readlink -- "$path")" >>"$BASELINE_ROOT/symlinks.tsv"
    elif [[ -e $path ]]; then
      BASELINE_MODE=partial-contained
      mode=$(stat -c %a -- "$path")
      mkdir -p -- "$(dirname -- "$BASELINE_ROOT/files$path")"
      cp -p "$path" "$BASELINE_ROOT/files$path"
      printf '%s\t%s\n' "$path" "$mode" >>"$BASELINE_ROOT/files.lst"
    fi
  done
}

restore_baseline() {
  local path mode unit timer state
  for unit in \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.timer \
    binance-fee-snapshot-spot.service \
    binance-fee-snapshot-usdm.service \
    binance-fee-upload.service; do
    state=$(systemctl is-active "$unit" 2>/dev/null || true)
    case $state in
      active|activating|deactivating|reloading)
        systemctl stop "$unit" >/dev/null 2>&1 || return 1
        ;;
      inactive|failed|unknown) ;;
      *) return 1 ;;
    esac
    state=$(systemctl is-active "$unit" 2>/dev/null || true)
    case $state in
      inactive|failed|unknown) ;;
      *) return 1 ;;
    esac
  done
  for timer in \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.timer; do
    state=$(systemctl is-enabled "$timer" 2>/dev/null || true)
    case $state in
      disabled|masked|masked-runtime|not-found|static) ;;
      *) systemctl disable "$timer" >/dev/null 2>&1 || return 1 ;;
    esac
    state=$(systemctl is-enabled "$timer" 2>/dev/null || true)
    case $state in
      disabled|masked|masked-runtime|not-found|static) ;;
      *) return 1 ;;
    esac
  done
  rm -f \
    "$SNAPSHOT_LINK" \
    "$UPLOAD_LINK" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-spot.service" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-spot.timer" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-usdm.service" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-usdm.timer" \
    "$SYSTEMD_ROOT/binance-fee-upload.service" \
    "$SYSTEMD_ROOT/binance-fee-upload.timer" \
    "$UPLOAD_ENV_PATH" \
    "$TMPFILES_ROOT/binance-fee.conf"
  if [[ $BASELINE_MODE == partial-contained ]]; then
    while IFS=$'\t' read -r path mode; do
      [[ -n $path ]] || continue
      mkdir -p -- "$(dirname -- "$path")"
      cp -p "$BASELINE_ROOT/files$path" "$path"
      chmod "$mode" "$path"
    done <"$BASELINE_ROOT/files.lst"
    while IFS=$'\t' read -r path target; do
      [[ -n $path ]] || continue
      install -d -m 0755 "${path%/*}"
      ln -s "$target" "$path"
    done <"$BASELINE_ROOT/symlinks.tsv"
  fi
  systemctl daemon-reload
}

stage_release() {
  local release deployment asset_mode
  release="$RELEASE_ROOT/$RELEASE_MANIFEST_SHA256"
  deployment="$release/deployment"
  install -d -m 0755 "$release" "$deployment" "$BIN_ROOT" "$SYSTEMD_ROOT" "$CONFIG_ROOT" "$TMPFILES_ROOT"
  immutable_install 0755 "$ARTIFACT_SNAPSHOT" "$release/binance-fee-snapshot"
  immutable_install 0644 "$ARTIFACT_SNAPSHOT_SIDECAR" "$release/binance-fee-snapshot.sha256"
  immutable_install 0755 "$ARTIFACT_UPLOADER" "$release/binance-fee-snapshot-upload"
  immutable_install 0644 "$ARTIFACT_UPLOADER_SIDECAR" "$release/binance-fee-snapshot-upload.sha256"
  immutable_install 0644 "$RELEASE_MANIFEST" "$release/binance-fee-release.json"
  immutable_install 0644 "$RELEASE_MANIFEST_SIDECAR" "$release/binance-fee-release.json.sha256"
  immutable_install 0644 "$CONTROL_MANIFEST" "$release/binance-fee-production-control-assets.sha256"
  immutable_install 0644 "$CONTROL_ARCHIVE" "$release/binance-fee-production-control.tar.gz"
  immutable_install 0644 "$ARTIFACT_DIR/binance-fee-production-control.tar.gz.sha256" \
    "$release/binance-fee-production-control.tar.gz.sha256"
  for asset in \
    binance-fee-cutover.sh \
    binance-fee-snapshot-spot.service \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.service \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.service \
    binance-fee-upload.timer \
    binance-fee-upload.env \
    binance-fee.conf \
    monday-collector-health.sh; do
    asset_mode=0644
    [[ $asset == binance-fee-cutover.sh ]] && asset_mode=0755
    immutable_install "$asset_mode" "$CONTROL_STAGE/$asset" "$deployment/$asset"
  done
  CANDIDATE_RELEASE="$release"
  CANDIDATE_DEPLOYMENT="$deployment"
}

install_candidate() {
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee.conf" "$TMPFILES_ROOT/binance-fee.conf"
  systemd-tmpfiles --create "$TMPFILES_ROOT/binance-fee.conf"
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee-snapshot-spot.service" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-spot.service"
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee-snapshot-spot.timer" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-spot.timer"
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee-snapshot-usdm.service" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-usdm.service"
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee-snapshot-usdm.timer" \
    "$SYSTEMD_ROOT/binance-fee-snapshot-usdm.timer"
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee-upload.service" \
    "$SYSTEMD_ROOT/binance-fee-upload.service"
  atomic_install 0644 "$CANDIDATE_DEPLOYMENT/binance-fee-upload.timer" \
    "$SYSTEMD_ROOT/binance-fee-upload.timer"
  atomic_install 0640 "$CANDIDATE_DEPLOYMENT/binance-fee-upload.env" "$UPLOAD_ENV_PATH"
  atomic_symlink "$CANDIDATE_RELEASE/binance-fee-snapshot" "$SNAPSHOT_LINK"
  atomic_symlink "$CANDIDATE_RELEASE/binance-fee-snapshot-upload" "$UPLOAD_LINK"
  systemctl daemon-reload
  systemctl disable \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.timer >/dev/null 2>&1 || true
}

verify_local_triplet() {
  local dir=$1 data manifest success data_sha manifest_sha bytes market prefix runtime_account_id
  data="$dir/fee.json"
  manifest="$dir/fee.json.manifest.json"
  success="$dir/fee.json._SUCCESS"
  direct_regular_file "$data" || die "missing local fee data: $data"
  direct_regular_file "$manifest" || die "missing local fee manifest: $manifest"
  direct_regular_file "$success" || die "missing local fee success marker: $success"
  data_sha=$(sha256sum "$data" | awk '{print $1}')
  manifest_sha=$(sha256sum "$manifest" | awk '{print $1}')
  [[ $(<"$success") == "$data_sha" ]] || [[ $(<"$success") == "$data_sha"$'\n' ]] \
    || die "local success marker does not match data SHA: $success"
  bytes=$(wc -c <"$data" | tr -d '[:space:]')
  market=$(jq -er '.market' "$data") || die "fee data is missing market: $data"
  runtime_account_id=$(jq -er '.runtime_account_id' "$CREDENTIAL_PATH") \
    || die 'credential is missing runtime_account_id'
  jq -e \
    --arg market "$market" \
    --arg runtime_account_id "$runtime_account_id" '
      .schema == "binance.fee-snapshot.v2"
      and .venue == "binance"
      and .market == $market
      and .symbol == "BTCUSDT"
      and .runtime_account_id == $runtime_account_id
      and (.account_fingerprint | type == "string" and test("^[a-f0-9]{64}$"))
    ' "$data" >/dev/null || die "local fee data has the wrong governed identity: $data"
  jq -e \
    --arg market "$market" \
    --arg runtime_account_id "$runtime_account_id" \
    --arg data_sha "$data_sha" \
    --argjson bytes "$bytes" '
      .schema == "binance.fee-artifact-manifest.v2"
      and .data_schema == "binance.fee-snapshot.v2"
      and .venue == "binance"
      and .file == "fee.json"
      and .market == $market
      and .symbol == "BTCUSDT"
      and .runtime_account_id == $runtime_account_id
      and .sha256 == $data_sha
      and .bytes == $bytes
    ' "$manifest" >/dev/null \
    || die "local fee manifest does not match the data triplet: $manifest"
  [[ $dir == "$FEE_SPOOL_ROOT/"* ]] || die "fee triplet escapes the spool root: $dir"
  prefix=${dir#"$FEE_SPOOL_ROOT/"}
  jq -cn \
    --arg dir "$dir" \
    --arg market "$market" \
    --arg object_prefix "$prefix" \
    --arg data_sha256 "$data_sha" \
    --arg manifest_sha256 "$manifest_sha" \
    '{dir:$dir,market:$market,object_prefix:$object_prefix,
      data_sha256:$data_sha256,manifest_sha256:$manifest_sha256}'
}

discover_new_triplets() {
  local marker=$1 data_file
  install -d -m 0750 "$EVIDENCE_DIR"
  : >"$LOCAL_TRIPLETS_JSONL"
  while IFS= read -r data_file; do
    verify_local_triplet "${data_file%/fee.json}" >>"$LOCAL_TRIPLETS_JSONL"
  done < <(find "$FEE_SPOOL_ROOT/lake/raw" -type f -name fee.json -newer "$marker" | sort)
  [[ $(wc -l <"$LOCAL_TRIPLETS_JSONL") -eq 2 ]] || die 'fee cutover expected exactly two new local triplets'
  jq -s -e '
    map(.market) | sort == ["spot", "usdm"]
  ' "$LOCAL_TRIPLETS_JSONL" >/dev/null \
    || die 'fee cutover did not produce exactly one spot and one USD-M triplet'
}

verify_pending_triplet_scope() {
  local expected actual data_file dir
  expected="$EVIDENCE_DIR/current-triplets.lst"
  actual="$EVIDENCE_DIR/pending-triplets.lst"
  jq -r '.object_prefix' "$LOCAL_TRIPLETS_JSONL" | sort >"$expected"
  : >"$actual"
  if [[ -d $FEE_SPOOL_ROOT/lake/raw ]]; then
    while IFS= read -r data_file; do
      dir=${data_file%/fee.json}
      [[ $dir == "$FEE_SPOOL_ROOT/"* ]] \
        || die "fee triplet escapes the spool root: $dir"
      printf '%s\n' "${dir#"$FEE_SPOOL_ROOT/"}"
    done < <(find "$FEE_SPOOL_ROOT/lake/raw" -type f -name fee.json | sort) \
      | sort >"$actual"
  fi
  cmp -s "$expected" "$actual" \
    || die 'canonical fee spool contains triplets outside the current cutover'
}

oss_cp() {
  local src=$1 dst=$2
  aliyun ossutil cp "$src" "$dst" \
    --profile "$OSS_PROFILE" \
    --region "$OSS_REGION" \
    --endpoint "$OSS_ENDPOINT" \
    -f >/dev/null
}

verify_remote_triplets() {
  local status_path verify_root line dir prefix data_sha manifest_sha remote_dir
  status_path="$FEE_SPOOL_ROOT/upload-status.json"
  direct_regular_file "$status_path" || die "missing fee upload status: $status_path"
  jq -e '
    (.pending_batches | type == "number" and . == 0)
    and (.last_success_at | type == "string" and length > 0)
    and (.last_uploaded_triplet.object_prefix | type == "string" and length > 0)
    and (.last_uploaded_triplet.data_sha256 | type == "string" and length == 64)
    and (.last_uploaded_triplet.manifest_sha256 | type == "string" and length == 64)
  ' "$status_path" >/dev/null || die 'fee upload status did not report a complete readback-safe upload'

  verify_root="$EVIDENCE_DIR/remote-readback"
  install -d -m 0750 "$verify_root"
  : >"$REMOTE_TRIPLETS_JSONL"
  while IFS= read -r line; do
    dir=$(jq -er '.dir' <<<"$line")
    prefix=$(jq -er '.object_prefix' <<<"$line")
    data_sha=$(jq -er '.data_sha256' <<<"$line")
    manifest_sha=$(jq -er '.manifest_sha256' <<<"$line")
    remote_dir="$verify_root/$(jq -er '.market' <<<"$line")"
    install -d -m 0750 "$remote_dir"
    oss_cp "oss://$OSS_BUCKET/$prefix/fee.json" "$remote_dir/fee.json"
    oss_cp "oss://$OSS_BUCKET/$prefix/fee.json.manifest.json" "$remote_dir/fee.json.manifest.json"
    oss_cp "oss://$OSS_BUCKET/$prefix/fee.json._SUCCESS" "$remote_dir/fee.json._SUCCESS"
    [[ $(sha256sum "$remote_dir/fee.json" | awk '{print $1}') == "$data_sha" ]] \
      || die "remote fee data digest drifted for $prefix"
    [[ $(sha256sum "$remote_dir/fee.json.manifest.json" | awk '{print $1}') == "$manifest_sha" ]] \
      || die "remote fee manifest digest drifted for $prefix"
    [[ $(<"$remote_dir/fee.json._SUCCESS") == "$data_sha" ]] \
      || [[ $(<"$remote_dir/fee.json._SUCCESS") == "$data_sha"$'\n' ]] \
      || die "remote fee success marker drifted for $prefix"
    jq -cn \
      --arg object_prefix "$prefix" \
      --arg data_sha256 "$data_sha" \
      --arg manifest_sha256 "$manifest_sha" \
      '{object_prefix:$object_prefix,data_sha256:$data_sha256,manifest_sha256:$manifest_sha256,
        downloaded:true}' >>"$REMOTE_TRIPLETS_JSONL"
  done <"$LOCAL_TRIPLETS_JSONL"
}

write_receipt() {
  local receipt=$1 marker=$2 success=$3 rollback_result=$4 failure_reason=$5
  local receipt_tmp="${receipt}.new.$$" marker_tmp="${marker}.new.$$" digest
  rm -f "$receipt_tmp" "$marker_tmp"
  jq -n \
    --arg schema monday.binance_fee_cutover.v1 \
    --arg started_at "$STARTED_AT" \
    --arg finished_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg controller "$CONTROLLER" \
    --arg baseline_mode "$BASELINE_MODE" \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg uploader_sha256 "$UPLOADER_SHA256" \
    --arg source_revision "$SOURCE_REVISION" \
    --arg release_manifest_sha256 "$RELEASE_MANIFEST_SHA256" \
    --arg control_manifest_sha256 "$CONTROL_MANIFEST_SHA256" \
    --arg control_archive_sha256 "$CONTROL_ARCHIVE_SHA256" \
    --arg rollback_result "$rollback_result" \
    --arg failure_reason "$failure_reason" \
    --argjson success "$success" \
    --slurpfile local_triplets "$LOCAL_TRIPLETS_JSONL" \
    --slurpfile remote_triplets "$REMOTE_TRIPLETS_JSONL" \
    '{
      schema:$schema,
      started_at:$started_at,
      finished_at:$finished_at,
      controller:$controller,
      success:$success,
      baseline_mode:$baseline_mode,
      candidate_sha256:$candidate_sha256,
      uploader_sha256:$uploader_sha256,
      source_revision:$source_revision,
      release_manifest_sha256:$release_manifest_sha256,
      control_manifest_sha256:$control_manifest_sha256,
      control_archive_sha256:$control_archive_sha256,
      rollback_result:$rollback_result,
      failure_reason:(if $failure_reason == "" then null else $failure_reason end),
      local_triplets:$local_triplets,
      remote_triplets:$remote_triplets
    }' >"$receipt_tmp" || { rm -f "$receipt_tmp" "$marker_tmp"; return 1; }
  digest=$(sha256sum "$receipt_tmp" | awk '{print $1}') \
    || { rm -f "$receipt_tmp" "$marker_tmp"; return 1; }
  printf '%s  %s\n' "$digest" "${receipt##*/}" >"$marker_tmp" \
    || { rm -f "$receipt_tmp" "$marker_tmp"; return 1; }
  mv -Tf "$receipt_tmp" "$receipt" \
    || { rm -f "$receipt_tmp" "$marker_tmp"; return 1; }
  mv -Tf "$marker_tmp" "$marker" \
    || { rm -f "$receipt_tmp" "$marker_tmp"; return 1; }
}

cleanup_temporary() {
  rm -rf "$CONTROL_STAGE" "$VALIDATED_ARTIFACT_DIR" 2>/dev/null || true
  rm -f "$LOCAL_TRIPLETS_JSONL" "$REMOTE_TRIPLETS_JSONL" 2>/dev/null || true
}

on_error() {
  local status=$1 line=$2 command=$3
  if [[ -z ${FAILURE_REASON:-} ]]; then
    FAILURE_REASON="unhandled command failed at line $line with status $status: $command"
  fi
  return "$status"
}

on_exit() {
  local status=$? rollback_result=$ROLLBACK_RESULT failure_reason=${FAILURE_REASON:-}
  trap - ERR EXIT
  if (( status != 0 )) && (( TRANSITION_STARTED == 1 )); then
    if restore_baseline; then
      rollback_result=baseline-restored
    else
      rollback_result=restore-failed
    fi
  fi
  if [[ -n $EVIDENCE_DIR ]]; then
    if ! write_receipt "$EVIDENCE_DIR/cutover.json" \
      "$EVIDENCE_DIR/$([[ $status -eq 0 ]] && printf 'PASSED.sha256' || printf 'FAILED.sha256')" \
      "$([[ $status -eq 0 ]] && printf true || printf false)" \
      "$rollback_result" \
      "$failure_reason"; then
      printf 'failed to persist terminal fee cutover receipt\n' >&2
    fi
  fi
  cleanup_temporary
  exit "$status"
}

if [[ $# -ne 2 || ! $2 =~ ^[A-Za-z0-9._@-]{1,128}$ ]]; then
  usage
  exit 2
fi
if [[ $(id -u) -ne 0 ]]; then
  printf 'must run as root\n' >&2
  exit 2
fi
for command in awk chmod cmp cp date dirname find flock id install jq ln mkdir mktemp mountpoint mv readlink rm sha256sum sort stat systemctl systemd-tmpfiles tar tr wc; do
  command_exists "$command" || die "missing required command: $command"
done
configure_paths "${MONDAY_ROOT_PREFIX:-}"
EXPECTED_ROOT_UID=${MONDAY_EXPECTED_ROOT_UID:-0}
CONTROLLER=$2
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
FAILURE_REASON=
ROLLBACK_RESULT=not-needed
TRANSITION_STARTED=0
BASELINE_MODE=absent
CANDIDATE_SHA256=
UPLOADER_SHA256=
SOURCE_REVISION=
RELEASE_MANIFEST_SHA256=
CONTROL_MANIFEST_SHA256=
CONTROL_ARCHIVE_SHA256=
CONTROL_STAGE=
VALIDATED_ARTIFACT_DIR=
EVIDENCE_DIR=
LOCAL_TRIPLETS_JSONL=$(mktemp)
REMOTE_TRIPLETS_JSONL=$(mktemp)
MARKER=
trap 'on_error "$?" "$LINENO" "$BASH_COMMAND"' ERR
trap on_exit EXIT

install -d -m 0755 "$LOCK_ROOT"
exec 9>"$LOCK_ROOT/monday-binance-fee-cutover.lock"
flock -n 9 || die 'another Binance fee cutover is already running'

mountpoint -q "$DATA_ROOT" || die "$DATA_ROOT must be a mounted filesystem"
install -d -m 0750 "$EVIDENCE_ROOT"
EVIDENCE_DIR="$EVIDENCE_ROOT/$(date -u +%Y%m%dT%H%M%SZ)-${CONTROLLER}-${$}"
mkdir -m 0750 "$EVIDENCE_DIR"

validate_credential
validate_artifact_bundle "$1"
snapshot_baseline
stage_release
TRANSITION_STARTED=1
install_candidate

MARKER="$EVIDENCE_DIR/pre-snapshot.marker"
: >"$MARKER"
systemctl start binance-fee-snapshot-spot.service \
  || { FAILURE_REASON='spot fee snapshot start failed'; exit 1; }
[[ $(service_result binance-fee-snapshot-spot.service) == success \
  && $(service_exit_status binance-fee-snapshot-spot.service) == 0 ]] \
  || { FAILURE_REASON='spot fee snapshot did not finish successfully'; exit 1; }
systemctl start binance-fee-snapshot-usdm.service \
  || { FAILURE_REASON='usdm fee snapshot start failed'; exit 1; }
[[ $(service_result binance-fee-snapshot-usdm.service) == success \
  && $(service_exit_status binance-fee-snapshot-usdm.service) == 0 ]] \
  || { FAILURE_REASON='usdm fee snapshot did not finish successfully'; exit 1; }

discover_new_triplets "$MARKER"
verify_pending_triplet_scope
OSS_BUCKET=$(env_value "$UPLOAD_ENV_PATH" OSS_BUCKET) || die 'missing OSS_BUCKET'
OSS_ENDPOINT=$(env_value "$UPLOAD_ENV_PATH" OSS_ENDPOINT) || die 'missing OSS_ENDPOINT'
OSS_REGION=$(env_value "$UPLOAD_ENV_PATH" OSS_REGION) || die 'missing OSS_REGION'
OSS_PROFILE=$(env_value "$UPLOAD_ENV_PATH" ALIYUN_PROFILE) || die 'missing ALIYUN_PROFILE'

systemctl start binance-fee-upload.service \
  || { FAILURE_REASON='fee upload start failed'; exit 1; }
[[ $(service_result binance-fee-upload.service) == success \
  && $(service_exit_status binance-fee-upload.service) == 0 ]] \
  || { FAILURE_REASON='fee upload did not finish successfully'; exit 1; }

verify_remote_triplets
systemctl enable --now \
  binance-fee-snapshot-spot.timer \
  binance-fee-snapshot-usdm.timer \
  binance-fee-upload.timer >/dev/null
timer_enabled_and_active binance-fee-snapshot-spot.timer \
  || { FAILURE_REASON='spot timer is not enabled and active'; exit 1; }
timer_enabled_and_active binance-fee-snapshot-usdm.timer \
  || { FAILURE_REASON='usdm timer is not enabled and active'; exit 1; }
timer_enabled_and_active binance-fee-upload.timer \
  || { FAILURE_REASON='upload timer is not enabled and active'; exit 1; }

if ! write_receipt "$EVIDENCE_DIR/cutover.json" "$EVIDENCE_DIR/PASSED.sha256" \
  true not-needed ''; then
  FAILURE_REASON='terminal success receipt persistence failed'
  exit 1
fi
TRANSITION_STARTED=0
trap - ERR EXIT
cleanup_temporary
