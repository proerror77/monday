#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: configure_research_cargo_target.sh --profile <name> [--features <name>]

Configures a persistent Cargo target directory for self-hosted PM5D research
workflows. The directory lives outside GITHUB_WORKSPACE so actions/checkout does
not remove it between jobs.

Set PLOY_RESEARCH_TARGET_KEEP_DAYS to control stale target cleanup. The default
is 14 days; set it to 0 to disable cleanup.
EOF
}

profile=""
features="default"

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --profile)
      profile="${2:-}"
      shift 2
      ;;
    --features)
      features="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${profile}" ]]; then
  echo "--profile is required" >&2
  usage >&2
  exit 2
fi

if [[ ! "${profile}" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "--profile must contain only letters, numbers, dot, underscore, or dash" >&2
  exit 2
fi

if [[ ! "${features}" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "--features must contain only letters, numbers, dot, underscore, or dash" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

target_base="${PLOY_RESEARCH_TARGET_BASE:-}"
if [[ -z "${target_base}" ]]; then
  if [[ -n "${RUNNER_WORKSPACE:-}" ]]; then
    target_base="${RUNNER_WORKSPACE}/_ploy-cargo-targets"
  else
    target_base="/tmp/ploy-research-cargo-targets"
  fi
fi

hash_inputs=(
  Cargo.lock
  Cargo.toml
  rust-toolchain.toml
  crates/ploy-research/Cargo.toml
  crates/ploy-feed-loaders/Cargo.toml
)

hash_material="$(mktemp)"
trap 'rm -f "${hash_material}"' EXIT

for path in "${hash_inputs[@]}"; do
  if [[ -f "${path}" ]]; then
    sha256sum "${path}" >> "${hash_material}"
  fi
done

{
  printf 'profile=%s\n' "${profile}"
  printf 'features=%s\n' "${features}"
  rustc -Vv 2>/dev/null || true
} >> "${hash_material}"

target_hash="$(sha256sum "${hash_material}" | awk '{print substr($1, 1, 16)}')"
target_dir="${target_base}/${profile}-${features}-${target_hash}"
mkdir -p "${target_dir}"

keep_days="${PLOY_RESEARCH_TARGET_KEEP_DAYS:-14}"
if [[ "${keep_days}" =~ ^[0-9]+$ ]] && [[ "${keep_days}" -gt 0 ]]; then
  mkdir -p "${target_base}"
  find "${target_base}" \
    -mindepth 1 \
    -maxdepth 1 \
    -type d \
    -name "${profile}-${features}-*" \
    -mtime "+${keep_days}" \
    ! -path "${target_dir}" \
    -print \
    -exec rm -rf {} + || true
fi

if [[ -n "${GITHUB_ENV:-}" ]]; then
  printf 'CARGO_TARGET_DIR=%s\n' "${target_dir}" >> "${GITHUB_ENV}"
fi

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "## Research Cargo Target"
    echo ""
    echo "- Profile: \`${profile}\`"
    echo "- Features: \`${features}\`"
    echo "- Target dir: \`${target_dir}\`"
  } >> "${GITHUB_STEP_SUMMARY}"
fi

echo "CARGO_TARGET_DIR=${target_dir}"
