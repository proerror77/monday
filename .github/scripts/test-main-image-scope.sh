#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/repo/rust_hft/apps/"{live,paper}/src
cat >"$scratch/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $* == 'buildx imagetools inspect ghcr.io/proerror77/hft:main --format {{json .}}' ]] || exit 91
jq -n --arg source "$PUBLISHED_SOURCE" '{manifest:{digest:("sha256:"+("b"*64))},image:{config:{Labels:{"org.opencontainers.image.revision":$source}}}}'
MOCK
chmod +x "$scratch/bin/docker"
export PATH="$scratch/bin:$PATH" PUBLISHED_SOURCE
cd "$scratch/repo"
git init -q
git config user.name 'CI fixture'
git config user.email ci@example.invalid
git config core.hooksPath /dev/null
printf 'initial live\n' >rust_hft/apps/live/src/main.rs
printf 'initial paper\n' >rust_hft/apps/paper/src/main.rs
git add .
git commit -qm initial
base=$(git rev-parse HEAD)
printf 'live change\n' >>rust_hft/apps/live/src/main.rs
git commit -qam 'unpublished live change'
live=$(git rev-parse HEAD)
printf 'paper change\n' >>rust_hft/apps/paper/src/main.rs
git commit -qam 'superseding paper change'
head=$(git rev-parse HEAD)
PUBLISHED_SOURCE=$base
export SELECTED_IMAGE_MATRIX
GITHUB_REF=refs/heads/main bash "$root/select-rust-ci-scope.sh" --event push --base "$live" --head "$head" --metadata "$root/fixtures/rust-ci-scope/metadata.fixture" --output "$scratch/current"
SELECTED_IMAGE_MATRIX=$(sed -n 's/^image_matrix=//p' "$scratch/current")
bash "$root/select-main-image-scope.sh" "$head" "$scratch/pending" "$root/fixtures/rust-ci-scope/metadata.fixture"
sed -n 's/^image_matrix=//p' "$scratch/pending" | jq -e 'any(.include[]; .name=="hft-core") and any(.include[]; .name=="deploy-paper")'
PUBLISHED_SOURCE=$live
bash "$root/select-main-image-scope.sh" "$head" "$scratch/core-published" "$root/fixtures/rust-ci-scope/metadata.fixture"
[[ $(sed -n 's/^image_matrix=//p' "$scratch/core-published" | jq -r '.include[].name') == deploy-paper ]]
PUBLISHED_SOURCE=$head
SELECTED_IMAGE_MATRIX='{"include":[]}' SELECTED_SECURITY_JOBS=,security/sast-semgrep,security/container-scan, bash "$root/select-main-image-scope.sh" "$head" "$scratch/all-published" "$root/fixtures/rust-ci-scope/metadata.fixture"
[[ $(sed -n 's/^image_matrix=//p' "$scratch/all-published" | jq '.include|length') == 0 ]]
grep -qx 'security_jobs=,security/sast-semgrep,' "$scratch/all-published"
# Later policy changes do not replay already checked, unpublished paper images.
PUBLISHED_SOURCE=$live
SELECTED_IMAGE_MATRIX='{"include":[]}' bash "$root/select-main-image-scope.sh" "$head" "$scratch/policy-after-paper" "$root/fixtures/rust-ci-scope/metadata.fixture"
[[ $(sed -n 's/^image_matrix=//p' "$scratch/policy-after-paper" | jq '.include|length') == 0 ]]
PUBLISHED_SOURCE=unknown
if bash "$root/select-main-image-scope.sh" "$head" "$scratch/invalid" "$root/fixtures/rust-ci-scope/metadata.fixture"; then exit 1; fi
[[ ! -s $scratch/invalid ]]
printf 'PASS: unpublished live impact survives a later paper commit; verified publication clears only delivered impact\n'
