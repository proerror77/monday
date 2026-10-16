#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/runner"
export RUNNER_TEMP="$work/runner" RUNNER_OS=Linux RUNNER_ARCH=X64 ImageOS=ubuntu24
export GITHUB_OUTPUT="$work/output" GITHUB_PATH="$work/path" GITHUB_EVENT_NAME=push
export GITHUB_REF=refs/heads/main GITHUB_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
export INSTALL_CALLS="$work/installs"
helper=$root/.github/scripts/sqlx-cli-tool.sh
cat >"$work/bin/rustc" <<'SH'
#!/usr/bin/env bash
printf 'rustc %s\n' "${FIXTURE_COMPILER:-1.98.1}"
SH
printf '#!/usr/bin/env bash\necho "1.98.1-x86_64-unknown-linux-gnu (default)"\n' >"$work/bin/rustup"
cat >"$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ $* == 'install sqlx-cli --locked --version 0.8.6 --no-default-features --features rustls,postgres --root '* ]]
printf 'install\n' >>"$INSTALL_CALLS"
dest=${!#}
mkdir -p "$dest/bin"
for bin in sqlx cargo-sqlx; do
  printf '#!/usr/bin/env bash\necho "sqlx-cli 0.8.6"\n' >"$dest/bin/$bin"
  chmod +x "$dest/bin/$bin"
done
jq -n --argjson defaults "${BAD_DEFAULTS:-false}" \
  '{installs:{"sqlx-cli 0.8.6 (registry+fixture)":{features:["postgres","rustls"],all_features:false,
    no_default_features:($defaults|not),bins:["sqlx","cargo-sqlx"]}}}' >"$dest/.crates2.json"
SH
chmod +x "$work/bin/"*
export PATH="$work/bin:$PATH"
prepare() { : >"$GITHUB_OUTPUT"; bash "$helper" prepare; }
key() { sed -n 's/^key=//p' "$GITHUB_OUTPUT"; }
prepare; normal=$(key); grep -qx save_allowed=true "$GITHUB_OUTPUT"
GITHUB_REF=refs/pull/1/merge GITHUB_EVENT_NAME=pull_request prepare
[[ $(key) == "$normal" ]]; grep -qx save_allowed=false "$GITHUB_OUTPUT"
SQLX_CACHE_PROBE=true GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/probe prepare
[[ $(key) == sqlx-cli-v1-probe-* ]]; grep -qx save_allowed=true "$GITHUB_OUTPUT"
SQLX_CACHE_PROBE=true GITHUB_EVENT_NAME=workflow_dispatch prepare; [[ $(key) == "$normal" ]]
ImageOS=ubuntu26 prepare; [[ $(key) != "$normal" ]]
FIXTURE_COMPILER=1.99.0 prepare; [[ $(key) != "$normal" ]]
if SQLX_CACHE_PROBE=true prepare; then exit 1; fi
if SQLX_CLI_VERSION=0.9.0 prepare; then exit 1; fi
if BAD_DEFAULTS=true bash "$helper" install false; then echo 'default feature profile accepted' >&2; exit 1; fi
bash "$helper" install false
bash "$helper" install true
[[ $(wc -l <"$INSTALL_CALLS") -eq 2 ]]
printf 'corrupt\n' >>"$RUNNER_TEMP/sqlx-cli-tool/bin/cargo-sqlx"
if bash "$helper" install true; then echo 'corrupt cached cargo-sqlx accepted' >&2; exit 1; fi
[[ $(wc -l <"$INSTALL_CALLS") -eq 2 ]]
printf 'SQLx pinned profile cache contracts: ok\n'
