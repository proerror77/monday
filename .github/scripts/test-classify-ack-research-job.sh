#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
classifier="$script_dir/classify-ack-research-job.sh"
root="$script_dir/../.."
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

classify() {
  bash "$classifier" --repository proerror77/monday --event pull_request \
    --head-repository proerror77/monday "$@"
}
for job in ci/research-preflight ci/rust ci/rust-fast-gates security/clippy-strict; do
  result=$(classify --job "$job" --research true)
  [[ $result == backend=ack$'\n'profile=* ]] || { echo "research job used another backend: $job" >&2; exit 1; }
  result=$(classify --job "$job" --research false)
  [[ $result == backend=github$'\n'profile= ]] || { echo "runtime-only job moved to ACK: $job" >&2; exit 1; }
done
for job in ploy/research-image-binaries ploy/research-image-smoke ploy/rust-format \
  ploy/rust-research-heavy acr/research-runner-binaries acr/research-publish acr/research-source-test; do
  [[ $(classify --job "$job") == backend=ack$'\n'profile=* ]]
  if bash "$classifier" --repository proerror77/monday --event pull_request \
    --head-repository contributor/monday --job "$job" >"$temporary/output" 2>"$temporary/error"; then
    echo "fork research work was admitted: $job" >&2; exit 1
  fi
  [[ ! -s $temporary/output ]]
  grep -Fq 'cannot fall back to a GitHub-hosted compiler' "$temporary/error"
  [[ $(classify --job "$job" --selected false) == backend=unselected$'\n'profile= ]]
done
if classify --job ci/rust --research yes >"$temporary/output" 2>/dev/null; then
  echo 'invalid research routing flag was accepted' >&2; exit 1
fi
if classify --job caller/arbitrary-command >"$temporary/output" 2>/dev/null; then
  echo 'arbitrary execution profile was accepted' >&2; exit 1
fi
if bash "$classifier" --repository proerror77/monday --event pull_request \
  --job ci/rust --research true >"$temporary/output" 2>/dev/null; then
  echo 'missing fork source identity was admitted' >&2; exit 1
fi
# A public fork can modify workflow YAML. No public workflow may expose an ACK
# self-hosted label; dispatch goes through a separately admitted private runner.
if grep -Eq 'runs-on:.*(monday-ack-research|self-hosted)' \
  "$root/.github/workflows/ci.yml" "$root/.github/workflows/security-enabled.yml" \
  "$root/.github/workflows/ploy-ci.yml" "$root/.github/workflows/acr-publish.yml"; then
  echo 'public workflow can directly address an ACK self-hosted runner' >&2; exit 1
fi
# Detect changed collector research files without moving unrelated trading edits.
git init -q "$temporary/repo"
git -C "$temporary/repo" config user.email test@example.invalid
git -C "$temporary/repo" config user.name test
mkdir -p "$temporary/repo/rust_hft/tools/collector/src/bin"
printf 'base\n' > "$temporary/repo/base"
git -C "$temporary/repo" add .
git -C "$temporary/repo" commit -qm base
base=$(git -C "$temporary/repo" rev-parse HEAD)
printf 'research\n' > "$temporary/repo/rust_hft/tools/collector/src/bin/research_data_service.rs"
git -C "$temporary/repo" add .
git -C "$temporary/repo" commit -qm research
head=$(git -C "$temporary/repo" rev-parse HEAD)
detect() {
  : > "$temporary/detect-output"
  (cd "$temporary/repo"; GITHUB_OUTPUT="$temporary/detect-output" bash "$classifier" --detect-scope --repository proerror77/monday --event push "$@")
  cat "$temporary/detect-output"
}
result=$(detect --loop false --base "$base" --head "$head")
[[ $result == ack_research=true ]]
result=$(detect --loop false --base "$head" --head "$head")
[[ $result == ack_research=false ]]
result=$(detect --loop true --base "$head" --head "$head")
[[ $result == ack_research=true ]]
echo 'ACK research routing metadata contracts passed'
