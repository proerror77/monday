#!/usr/bin/env bash
set -euo pipefail

snapshot_dir=""
run_id="${GITHUB_RUN_ID:-}"
registry_dir="${PLOY_RESEARCH_SNAPSHOT_REGISTRY:-}"
retention_days="14"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --snapshot-dir)
      snapshot_dir="${2:?missing --snapshot-dir value}"
      shift 2
      ;;
    --run-id)
      run_id="${2:?missing --run-id value}"
      shift 2
      ;;
    --registry-dir)
      registry_dir="${2:?missing --registry-dir value}"
      shift 2
      ;;
    --retention-days)
      retention_days="${2:?missing --retention-days value}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ -z "${snapshot_dir}" ] || [ ! -d "${snapshot_dir}" ]; then
  echo "snapshot directory is required" >&2
  exit 2
fi
if [ -z "${run_id}" ]; then
  echo "run id is required" >&2
  exit 2
fi
if [ -z "${registry_dir}" ]; then
  registry_dir="${RUNNER_WORKSPACE:-/tmp}/_ploy-research-snapshots"
fi
if [ ! -f "${snapshot_dir}/manifest.json" ]; then
  echo "snapshot manifest not found: ${snapshot_dir}/manifest.json" >&2
  exit 2
fi

snapshot_hash="$(
  python3 - <<'PY' "${snapshot_dir}/manifest.json"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh)["snapshot_hash"])
PY
)"

mkdir -p "${registry_dir}/by-hash" "${registry_dir}/by-run"
hash_dir="${registry_dir}/by-hash/${snapshot_hash}"
tmp_dir="${registry_dir}/by-hash/.${snapshot_hash}.tmp-${GITHUB_RUN_ID:-$$}-${RANDOM}"

if [ ! -d "${hash_dir}" ]; then
  rm -rf "${tmp_dir}"
  mkdir -p "${tmp_dir}"
  cp -a "${snapshot_dir}/." "${tmp_dir}/"
  mv "${tmp_dir}" "${hash_dir}"
else
  rm -rf "${tmp_dir}"
fi

ln -sfn "../by-hash/${snapshot_hash}" "${registry_dir}/by-run/${run_id}"
cat > "${registry_dir}/by-run/${run_id}.txt" <<EOF
snapshot_hash=${snapshot_hash}
snapshot_path=${hash_dir}
published_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

if [ "${retention_days}" -gt 0 ] 2>/dev/null; then
  find "${registry_dir}/by-hash" -mindepth 1 -maxdepth 1 -type d -mtime "+${retention_days}" -exec rm -rf {} + || true
  find "${registry_dir}/by-run" -mindepth 1 -maxdepth 1 -mtime "+${retention_days}" -exec rm -f {} + || true
fi

echo "published research snapshot run_id=${run_id} hash=${snapshot_hash} path=${hash_dir}"
