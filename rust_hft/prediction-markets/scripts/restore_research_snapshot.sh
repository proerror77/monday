#!/usr/bin/env bash
set -euo pipefail

run_id="${SNAPSHOT_RUN_ID:-}"
output_dir=""
registry_dir="${PLOY_RESEARCH_SNAPSHOT_REGISTRY:-}"
required_paths=()

while [ "$#" -gt 0 ]; do
  case "$1" in
    --run-id)
      run_id="${2:?missing --run-id value}"
      shift 2
      ;;
    --output-dir)
      output_dir="${2:?missing --output-dir value}"
      shift 2
      ;;
    --registry-dir)
      registry_dir="${2:?missing --registry-dir value}"
      shift 2
      ;;
    --require)
      required_paths+=("${2:?missing --require value}")
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ -z "${run_id}" ]; then
  echo "run id is required" >&2
  exit 2
fi
if [ -z "${output_dir}" ]; then
  echo "output directory is required" >&2
  exit 2
fi
if [ -z "${registry_dir}" ]; then
  registry_dir="${RUNNER_WORKSPACE:-/tmp}/_ploy-research-snapshots"
fi

source_dir="${registry_dir}/by-run/${run_id}"
if [ ! -d "${source_dir}" ]; then
  echo "local research snapshot not found: ${source_dir}" >&2
  exit 1
fi

for path in "${required_paths[@]}"; do
  if [ ! -e "${source_dir}/${path}" ]; then
    echo "local research snapshot missing required path: ${path}" >&2
    exit 1
  fi
done

rm -rf "${output_dir}"
mkdir -p "${output_dir}"
cp -a "${source_dir}/." "${output_dir}/"
echo "restored local research snapshot run_id=${run_id} from=${source_dir} to=${output_dir}"
