#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/runner"
cp "$root/.github/scripts/cargo-license-tool.sh" "$work/helper.sh"
export RUNNER_TEMP="$work/runner" RUNNER_OS=Linux RUNNER_ARCH=X64 ImageOS=ubuntu24
export GITHUB_OUTPUT="$work/output" GITHUB_PATH="$work/path" GITHUB_EVENT_NAME=push
export GITHUB_REF=refs/heads/main GITHUB_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
export INSTALL_CALLS="$work/installs"
cat >"$work/bin/rustc" <<'SH'
#!/usr/bin/env bash
printf 'rustc %s\nhost: x86_64-unknown-linux-gnu\n' "${FIXTURE_COMPILER:-1.98.1}"
SH
cat >"$work/bin/rustup" <<'SH'
#!/usr/bin/env bash
printf '1.98.1-x86_64-unknown-linux-gnu (default)\n'
SH
cat >"$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ $* == 'install --list --root '* ]]; then
  printf 'cargo-license v0.7.0:\n    cargo-license\n'; exit
fi
[[ $* == 'install --locked --version 0.7.0 --root '* && ${!#} == cargo-license ]]
printf 'install\n' >>"$INSTALL_CALLS"
while [[ $1 != --root ]]; do shift; done
mkdir -p "$2/bin"
printf '#!/usr/bin/env bash\n[[ $1 == --help ]]\n' >"$2/bin/cargo-license"
chmod +x "$2/bin/cargo-license"
SH
chmod +x "$work/bin/"*
export PATH="$work/bin:$PATH"
prepare() { : >"$GITHUB_OUTPUT"; bash "$work/helper.sh" prepare; }
key() { sed -n 's/^key=//p' "$GITHUB_OUTPUT"; }
prepare
normal=$(key)
grep -qx save_allowed=true "$GITHUB_OUTPUT"
ImageOS=ubuntu26 prepare; [[ $(key) != "$normal" ]]
RUNNER_ARCH=ARM64 prepare; [[ $(key) != "$normal" ]]
FIXTURE_COMPILER=1.99.0 prepare; [[ $(key) != "$normal" ]]
GITHUB_REF=refs/pull/1/merge GITHUB_EVENT_NAME=pull_request prepare
[[ $(key) == "$normal" ]]; grep -qx save_allowed=false "$GITHUB_OUTPUT"
GITHUB_EVENT_NAME=pull_request_target prepare
grep -qx save_allowed=false "$GITHUB_OUTPUT"
LICENSE_CACHE_PROBE=true GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/codex/probe prepare
[[ $(key) == cargo-license-v1-probe-* && $(key) != "$normal" ]]
grep -qx save_allowed=true "$GITHUB_OUTPUT"
LICENSE_CACHE_PROBE=true GITHUB_EVENT_NAME=workflow_dispatch prepare
[[ $(key) == "$normal" ]]; grep -qx save_allowed=true "$GITHUB_OUTPUT"
if LICENSE_CACHE_PROBE=true prepare; then echo 'push accepted diagnostic mode' >&2; exit 1; fi
if LICENSE_CACHE_PROBE=invalid prepare; then exit 1; fi
mkdir "$work/project"
(cd "$work/project"; printf 'lock-v1\n' >Cargo.lock; prepare)
[[ $(key) == "$normal" ]]
(cd "$work/project"; printf 'lock-v2\n' >Cargo.lock; prepare)
[[ $(key) == "$normal" ]]
# A cold install creates a receipt; a hit never invokes Cargo installation.
bash "$work/helper.sh" install false
[[ $(wc -l <"$INSTALL_CALLS") -eq 1 ]]
bash "$work/helper.sh" install true
[[ $(wc -l <"$INSTALL_CALLS") -eq 1 ]]
cp "$RUNNER_TEMP/cargo-license-tool/receipt.json" "$work/receipt.json"
jq '.version="unexpected"' "$work/receipt.json" >"$RUNNER_TEMP/cargo-license-tool/receipt.json"
if bash "$work/helper.sh" install true; then echo 'wrong tool version accepted' >&2; exit 1; fi
cp "$work/receipt.json" "$RUNNER_TEMP/cargo-license-tool/receipt.json"
printf '# corrupted\n' >>"$RUNNER_TEMP/cargo-license-tool/bin/cargo-license"
if bash "$work/helper.sh" install true; then echo 'corrupt cache accepted' >&2; exit 1; fi
[[ $(wc -l <"$INSTALL_CALLS") -eq 1 ]]
bash "$work/helper.sh" install false
[[ $(wc -l <"$INSTALL_CALLS") -eq 2 ]]
if FIXTURE_COMPILER=1.99.0 bash "$work/helper.sh" install true; then exit 1; fi
rm "$RUNNER_TEMP/cargo-license-tool/bin/cargo-license"
if bash "$work/helper.sh" install true; then echo 'missing executable accepted' >&2; exit 1; fi
# Execute the workflow's real selection block with a captured normal selector.
ruby -ryaml - "$root/.github/workflows/security-enabled.yml" >"$work/select.sh" <<'RUBY'
workflow = YAML.safe_load(File.read(ARGV.fetch(0)))
print workflow.fetch('jobs').fetch('security-selector').fetch('steps').find { |s| s['name'] == 'Select security jobs' }.fetch('run')
RUBY
mkdir -p "$work/project/.github/scripts"
cat >"$work/project/.github/scripts/select-rust-ci-scope.sh" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >"$SELECTOR_CALLS"
SH
chmod +x "$work/project/.github/scripts/select-rust-ci-scope.sh"
export SELECTOR_CALLS="$work/selector-calls" BASE_SHA=base HEAD_SHA=head
: >"$GITHUB_OUTPUT"
(cd "$work/project"; LICENSE_CACHE_PROBE=true GITHUB_EVENT_NAME=workflow_dispatch bash -eu "$work/select.sh")
grep -qx 'security_jobs=,security/license-check,' "$GITHUB_OUTPUT"
grep -qx clippy_loop=false "$GITHUB_OUTPUT"
[[ ! -e $SELECTOR_CALLS ]]
(cd "$work/project"; LICENSE_CACHE_PROBE=false bash -eu "$work/select.sh")
grep -qx -- '--event push --base base --head head' "$SELECTOR_CALLS"
if (cd "$work/project"; LICENSE_CACHE_PROBE=true bash -eu "$work/select.sh"); then exit 1; fi
printf '%s\n' .github/scripts/cargo-license-tool.sh .github/scripts/test-cargo-license-tool.sh >"$work/changed.txt"
bash "$root/.github/scripts/select-rust-ci-scope.sh" --event pull_request \
  --changed-files "$work/changed.txt" --metadata "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture" \
  --output "$work/mapping.txt"
grep -q '^security_jobs=.*\bsecurity/license-check,' "$work/mapping.txt"
if grep -qE '^jobs=.*(ci/rust,|ploy/research-image)' "$work/mapping.txt"; then exit 1; fi
printf 'cargo-license cache contracts: ok\n'
