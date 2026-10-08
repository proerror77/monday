#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
root="$fixture/repo"
config="$root/rust_hft/prediction-markets/config"
mkdir -p "$root/.github/scripts" "$config/strategies" "$fixture/bin"
cp "$script_dir/run-strategy-config-contracts.sh" "$root/.github/scripts/"
runner="$root/.github/scripts/run-strategy-config-contracts.sh"
printf '%s\n' '[dry_run]' 'enabled = true' >"$config/default.toml"
printf '%s\n' '[runtime]' 'mode = "dryrun"' '[strategy]' >"$config/strategies/new-parameters.toml"
cat >"$fixture/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"$CONTRACT_FIXTURE/cargo-args"
printf '%s\n' "$PWD" >"$CONTRACT_FIXTURE/cargo-workspace"
printf '%s\n' "$MONDAY_STRATEGY_CONFIG_FILES_JSON" >"$CONTRACT_FIXTURE/config-files"
exit "${CONTRACT_CARGO_EXIT:-0}"
SH
chmod +x "$fixture/bin/cargo"
export PATH="$fixture/bin:$PATH" CONTRACT_FIXTURE="$fixture"

printf '%s\n' \
  rust_hft/prediction-markets/config/strategies/new-parameters.toml \
  rust_hft/prediction-markets/crates/ploy-strategy-bundles/src/config.rs \
  rust_hft/prediction-markets/config/default.toml >"$fixture/changed"
bash "$runner" --changed-files "$fixture/changed" >"$fixture/output"
jq -e '. == ["config/default.toml", "config/strategies/new-parameters.toml"]' "$fixture/config-files" >/dev/null
printf '%s\n' test --locked -p ploy-strategy-bundles --no-default-features --lib 'config::tests::' >"$fixture/expected-args"
diff -u "$fixture/expected-args" "$fixture/cargo-args"
grep -Fqx "$root/rust_hft/prediction-markets" "$fixture/cargo-workspace"

for path in \
  rust_hft/prediction-markets/config/strategies/missing.toml \
  rust_hft/prediction-markets/config/strategies/../default.toml \
  rust_hft/prediction-markets/config/strategies/nested/parameters.toml; do
  printf '%s\n' "$path" >"$fixture/invalid"
  rm -f "$fixture/cargo-args"
  if bash "$runner" --changed-files "$fixture/invalid" >"$fixture/output" 2>&1; then
    printf 'accepted invalid configuration input: %s\n' "$path" >&2; exit 1
  fi
  test ! -e "$fixture/cargo-args"
done

result=0
CONTRACT_CARGO_EXIT=71 bash "$runner" --changed-files "$fixture/changed" >"$fixture/output" 2>&1 || result=$?
[[ $result == 71 ]] || { echo 'configuration test failure was swallowed' >&2; exit 1; }

git -C "$root" init -q
git -C "$root" add .
git -C "$root" -c user.name=CI -c user.email=ci@example.invalid commit -qm base
base=$(git -C "$root" rev-parse HEAD)
printf '%s\n' '# parameter change' >>"$config/strategies/new-parameters.toml"
printf '%s\n' '[runtime]' 'mode = "backtest"' '[strategy]' >"$config/strategies/added.toml"
git -C "$root" add .
git -C "$root" -c user.name=CI -c user.email=ci@example.invalid commit -qm parameters
bash "$runner" --event pull_request --base "$base" --head HEAD >"$fixture/output"
jq -e '. == ["config/strategies/added.toml", "config/strategies/new-parameters.toml"]' "$fixture/config-files" >/dev/null

base=$(git -C "$root" rev-parse HEAD)
git -C "$root" rm -q rust_hft/prediction-markets/config/strategies/added.toml
git -C "$root" -c user.name=CI -c user.email=ci@example.invalid commit -qm deletion
bash "$runner" --event push --base "$base" --head HEAD >"$fixture/output"
jq -e '. == []' "$fixture/config-files" >/dev/null

# A regular file becoming a symlink must reach the file-type rejection.
base=$(git -C "$root" rev-parse HEAD)
rm "$config/strategies/new-parameters.toml"
ln -s ../default.toml "$config/strategies/new-parameters.toml"
git -C "$root" add .
git -C "$root" -c user.name=CI -c user.email=ci@example.invalid commit -qm type-change
git -C "$root" diff --name-status "$base...HEAD" | grep -q '^T'
(cd "$root" && bash "$script_dir/select-rust-ci-scope.sh" --event pull_request \
  --base "$base" --head HEAD --output "$fixture/type-scope")
grep -q '^jobs=.*[,/]strategy-config-contracts,' "$fixture/type-scope"
rm -f "$fixture/cargo-args"
if bash "$runner" --event pull_request --base "$base" --head HEAD >"$fixture/output" 2>&1; then
  echo 'configuration validation accepted a symlink type change' >&2; exit 1
fi
grep -q 'not a regular file' "$fixture/output"
test ! -e "$fixture/cargo-args"

rm -f "$fixture/cargo-args"
if bash "$runner" --event pull_request --base missing-ref --head HEAD >"$fixture/output" 2>&1; then
  echo 'configuration validation accepted a failed diff' >&2; exit 1
fi
test ! -e "$fixture/cargo-args"
printf 'strategy configuration dispatch contracts passed\n'
