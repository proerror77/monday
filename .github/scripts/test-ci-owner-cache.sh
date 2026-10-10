#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/repo/.github/scripts" "$work/repo/rust_hft/scripts" "$work/bin"
cp "$root/.github/scripts/ci-owner-cache.sh" "$work/repo/.github/scripts/"
cp "$root/rust_hft/scripts/"{cargo-scoped,workspace-metadata}.sh "$work/repo/rust_hft/scripts/"
cp "$root/rust_hft/workspaces.json" "$work/repo/rust_hft/"
mkdir -p "$work/native-bin"
for command in cc c++ clang mold protoc ldd; do
  # Expand the executable name inside the fixture.
  # shellcheck disable=SC2016
  printf '#!/usr/bin/env bash\nprintf "%%s version fixture\\n" "${0##*/}"\n' >"$work/native-bin/$command"
  chmod +x "$work/native-bin/$command"
done
export REAL_NATIVE_CAT
REAL_NATIVE_CAT=$(command -v cat)
cat >"$work/native-bin/uname" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $# == 1 ]]
case "$1" in
  -s) printf 'Linux\n' ;;
  -ms) printf 'Linux x86_64\n' ;;
  *) exit 92 ;;
esac
MOCK
cat >"$work/native-bin/dpkg-query" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $# == 2 && $1 == -W && $2 == -f=* ]]
printf 'fixture-native-toolchain\t1\tamd64\n'
MOCK
cat >"$work/native-bin/cat" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ $# == 1 && $1 == /etc/os-release ]]; then
  printf 'NAME=Monday-native-fixture\nVERSION_ID=1\n'
else
  exec "$REAL_NATIVE_CAT" "$@"
fi
MOCK
chmod +x "$work/native-bin/"{uname,dpkg-query,cat}
if [[ $(uname -s) != Linux ]]; then
  if bash "$root/.github/scripts/ci-owner-cache.sh" native-input; then
    echo 'native cache input accepted a non-Linux host' >&2
    exit 1
  fi
fi
native_before=$(PATH="$work/native-bin:$PATH" bash "$root/.github/scripts/ci-owner-cache.sh" native-input)
native_after=$(PATH="$work/native-bin:$PATH" CXXFLAGS=-DFIXTURE_NATIVE_INPUT_CHANGED bash "$root/.github/scripts/ci-owner-cache.sh" native-input)
[[ $native_before != "$native_after" ]]
# A narrow first save must not become an exact hit for wider coverage.
plan=$(jq -cn '{handoff:"false",json:"false",ondo:"false",collector:"false",control:"false",focused:"true",loop:"false",owning_packages:",,",focused_packages:",hft-live,",loop_packages:""}')
coverage() { MONDAY_CI_CACHE_PLAN="$1" bash "$root/.github/scripts/ci-owner-cache.sh" coverage-input; }
narrow=$(coverage "$plan")
wide=$(coverage "$(jq -c '.handoff="true" | .focused_packages=",hft-live,hft-cex-research-worker,"' <<<"$plan")")
[[ $narrow != "$wide" ]]
[[ $(coverage "$(jq -c '.focused_packages=",hft-live,hft-cex-research-worker,"' <<<"$plan")") != "$narrow" ]]
owning=$(jq -c '.owning_packages=",hft-live,"' <<<"$plan")
[[ $(coverage "$owning") != "$narrow" ]]
[[ $(coverage "$(jq -c '.owning_packages=",hft-paper,"' <<<"$owning")") != "$(coverage "$owning")" ]]
[[ $(coverage "$(jq -c '.loop_packages="hft-live"' <<<"$owning")") != "$(coverage "$owning")" ]]
for flag in handoff json ondo collector control focused loop; do
  changed=$(jq -c --arg flag "$flag" '.[$flag]=(if .[$flag]=="true" then "false" else "true" end) | .loop_packages="alpha-harness"' <<<"$plan")
  [[ $(coverage "$changed") != "$narrow" ]]
done
[[ $(coverage "$(jq -c '.focused_packages="hft-live,hft-live" | .source_sha="other-source" | .event="pull_request"' <<<"$plan")") == "$narrow" ]]
[[ $(coverage "$(jq -c '.focused="false" | .focused_packages=""' <<<"$plan")") == \
   "$(coverage "$(jq -c '.focused="false" | .focused_packages="ignored-package"' <<<"$plan")")" ]]
for invalid in 'del(.handoff)' 'del(.owning_packages)' '.focused="yes"' '.focused_packages=""' '.focused_packages="hft-live --features unexpected"'; do
  if coverage "$(jq -c "$invalid" <<<"$plan")"; then
    echo 'invalid cache coverage accepted' >&2; exit 1
  fi
done
cat >"$work/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ $1 == -V ]]; then
  printf 'cargo 1.98.1 fixture\n'
  exit 0
fi
if [[ $1 == metadata ]]; then
  manifest=''
  args=("$@")
  for ((i=0;i<${#args[@]};i++)); do
    [[ ${args[i]} != --manifest-path ]] || manifest=${args[i+1]}
  done
  owner=${manifest#"$FIXTURE/rust_hft/"}; owner=${owner%/Cargo.toml}
  package=package-${owner//\//-}
  extra=false
  [[ " $* " != *" --all-features "* ]] || extra=true
  jq -n --arg manifest "$manifest" --arg package "$package" --arg root "$FIXTURE" --argjson extra "$extra" '
    {workspace_root:($manifest|sub("/Cargo.toml$";"")),workspace_members:[$package],packages:(
     [{id:$package,name:$package,manifest_path:$manifest,targets:[{name:($package|gsub("-";"_"))}]},
      {id:"hft-data",name:"hft-data",manifest_path:($root+"/rust_hft/data-pipelines/core/Cargo.toml"),targets:[{name:"data"}]}] +
     if $extra then [{id:"vendor",name:"hidden-local",manifest_path:($root+"/vendor/hidden/Cargo.toml"),targets:[{name:"hidden_local"}]},
       {id:"outside",name:"external-local",source:null,manifest_path:"/external/path/Cargo.toml",targets:[{name:"external_local"}]},
       {id:"registry",name:"libduckdb-sys",source:"registry+https://github.com/rust-lang/crates.io-index",manifest_path:"/registry/libduckdb/Cargo.toml",targets:[{name:"libduckdb_sys"}]},
       {id:"encoding",name:"data-encoding",source:"registry+https://github.com/rust-lang/crates.io-index",manifest_path:"/registry/data-encoding/Cargo.toml",targets:[{name:"data_encoding"}]}] else [] end)}'
else
  [[ ${FAIL_CARGO:-0} != 1 ]] || exit 19
  jq -cn --arg target "${CARGO_TARGET_DIR:-unset}" --args \
    '{target:$target,argv:$ARGS.positional}' -- "$@" >>"$CAPTURE"
fi
MOCK
cat >"$work/bin/rustc" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  -Vv) printf 'rustc 1.98.1\ncommit-hash: %s\nhost: %s\n' "${FIXTURE_COMPILER:-fixture}" "${FIXTURE_TARGET:-x86_64-unknown-linux-gnu}" ;;
  '--print sysroot') printf '%s\n' "$FIXTURE_SYSROOT" ;;
  *) exit 92 ;;
esac
MOCK
cat >"$work/bin/rustup" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == which && ( $2 == rustc || $2 == cargo ) ]]
printf '%s\n' "$COMPILER_BINARY"
MOCK
printf 'compiler fixture bytes' >"$work/compiler-binary"
export COMPILER_BINARY="$work/compiler-binary"
export FIXTURE_SYSROOT="$work/sysroot"
mkdir -p "$FIXTURE_SYSROOT/lib/rustlib/x86_64-unknown-linux-gnu/lib"
printf 'standard library fixture bytes' >"$FIXTURE_SYSROOT/lib/rustlib/x86_64-unknown-linux-gnu/lib/libstd-fixture.rlib"
chmod +x "$work/bin/"{cargo,rustc,rustup}
export FIXTURE="$work/repo" CAPTURE="$work/calls" PATH="$work/bin:$PATH"
scoped="$FIXTURE/rust_hft/scripts/cargo-scoped.sh"
# The opt-in changes directories, preserving each original Cargo argv.
for layout in default owning-workspace-v1; do
  : >"$CAPTURE"
  MONDAY_CARGO_TARGET_LAYOUT="$layout" CARGO_TARGET_DIR="$work/legacy" bash "$scoped" \
    build -p package-runtime -p package-research-core -p package-data-pipelines --locked
  cp "$CAPTURE" "$work/$layout.calls"
done
jq -s 'map(.argv)' "$work/default.calls" >"$work/default.argv"
jq -s 'map(.argv)' "$work/owning-workspace-v1.calls" >"$work/owners.argv"
cmp "$work/default.argv" "$work/owners.argv"
jq -se --arg legacy "$work/legacy" 'all(.[];.target==$legacy)' "$work/default.calls" >/dev/null
jq -se --arg root "$FIXTURE/rust_hft" '
  ([.[].target]|sort)==[$root+"/research-core/target",$root+"/runtime/target",$root+"/target"]' \
  "$work/owning-workspace-v1.calls" >/dev/null
: >"$CAPTURE"
MONDAY_CARGO_TARGET_LAYOUT=owning-workspace-v1 bash "$scoped" \
  test -p package-runtime --no-default-features --features feature-a --test contract --locked
jq -se 'length==1 and .[0].argv==["test","--manifest-path",env.FIXTURE+"/rust_hft/runtime/Cargo.toml", "-p","package-runtime","--no-default-features","--features","feature-a","--test","contract","--locked"]' "$CAPTURE" >/dev/null
if MONDAY_CARGO_TARGET_LAYOUT=invalid bash "$scoped" build -p package-runtime --locked; then
  echo 'unknown layout accepted' >&2; exit 1
fi
if MONDAY_CARGO_TARGET_LAYOUT=owning-workspace-v1 bash "$scoped" \
  test -p package-runtime -p package-research-core --features feature-a --locked; then
  echo 'cross-owner feature union accepted' >&2; exit 1
fi
if FAIL_CARGO=1 MONDAY_CARGO_TARGET_LAYOUT=owning-workspace-v1 bash "$scoped" build -p package-runtime --locked; then
  echo 'Cargo failure was swallowed' >&2; exit 1
fi

# Remove local/shared/vendor bytes, retaining native external outputs.
while IFS= read -r manifest; do
  owner=${manifest%/Cargo.toml}
  target="$FIXTURE/rust_hft/$owner/target"
  [[ $owner != data-pipelines ]] || target="$FIXTURE/rust_hft/target"
  profile="$target/debug"
  mkdir -p "$profile/"{build,.fingerprint,deps,incremental,examples}
  package=package-${owner//\//-}; rust_name=${package//-/_}
  mkdir -p "$profile/build/$package-hash/out" "$profile/build/libduckdb-sys-native/out"
  printf local >"$profile/build/$package-hash/out/local.o"
  printf executable >"$profile/build/$package-hash/build-script-build"
  chmod +x "$profile/build/$package-hash/build-script-build"
  printf native >"$profile/build/libduckdb-sys-native/out/native.o"
  printf executable >"$profile/build/libduckdb-sys-native/build-script-build"
  printf executable >"$profile/deps/external-test-hash"
  chmod +x "$profile/build/libduckdb-sys-native/build-script-build" "$profile/deps/external-test-hash"
  printf local >"$profile/deps/lib$rust_name-hash.rlib"
  printf hidden >"$profile/deps/libhidden_local-hash.rlib"
  printf outside >"$profile/deps/libexternal_local-hash.rlib"
  printf stale-local >"$profile/deps/libdeleted_local-hash.rlib"
  mkdir -p "$profile/build/deleted-local-hash/out" "$profile/.fingerprint/deleted-local-hash"
  printf stale-local >"$profile/build/deleted-local-hash/out/local.o"
  printf stale-local >"$profile/.fingerprint/deleted-local-hash/lib-deleted_local"
  printf local >"$profile/.fingerprint/$package-hash"
  printf native >"$profile/deps/liblibduckdb_sys-hash.rlib"
  printf executable >"$profile/$package"
  printf incremental >"$profile/incremental/data"
  printf executable >"$profile/examples/example"
  # The local target data must not remove the external package data-encoding.
  mkdir -p "$profile/build/hft-data-hash/out" "$profile/.fingerprint/hft-data-hash" \
    "$profile/.fingerprint/data-encoding-hash" "$profile/build/data-encoding-hash/out"
  printf 'local data object' >"$profile/build/hft-data-hash/out/local.o"
  printf 'local data fingerprint' >"$profile/.fingerprint/hft-data-hash/lib-data"
  for artifact in data-hash.d libdata-hash.rlib libdata-hash.rmeta libdata-hash.so hft-data-hash; do
    printf 'local data' >"$profile/deps/$artifact"
  done
  printf 'external dependency fingerprint' >"$profile/.fingerprint/data-encoding-hash/lib-data_encoding"
  printf 'external dependency bytes' >"$profile/deps/libdata_encoding-hash.rlib"
  printf 'external build output' >"$profile/build/data-encoding-hash/out/dependency.o"
done < <(jq -r '.workspaces[].manifest' "$FIXTURE/rust_hft/workspaces.json")
MONDAY_CARGO_TARGET_LAYOUT=owning-workspace-v1 bash "$FIXTURE/.github/scripts/ci-owner-cache.sh" cleanup
while IFS= read -r manifest; do
  owner=${manifest%/Cargo.toml}; target="$FIXTURE/rust_hft/$owner/target"
  [[ $owner != data-pipelines ]] || target="$FIXTURE/rust_hft/target"
  profile="$target/debug"; package=package-${owner//\//-}; rust_name=${package//-/_}
  [[ -f $profile/build/libduckdb-sys-native/out/native.o && -f $profile/deps/liblibduckdb_sys-hash.rlib ]]
  [[ -x $profile/build/libduckdb-sys-native/build-script-build && ! -e $profile/deps/external-test-hash ]]
  [[ ! -e $profile/build/$package-hash && ! -e $profile/deps/lib$rust_name-hash.rlib && ! -e $profile/deps/libhidden_local-hash.rlib ]]
  [[ ! -e $profile/deps/libexternal_local-hash.rlib && ! -e $profile/examples ]]
  [[ ! -e $profile/deps/libdeleted_local-hash.rlib && ! -e $profile/build/deleted-local-hash && ! -e $profile/.fingerprint/deleted-local-hash ]]
  [[ ! -e $profile/.fingerprint/$package-hash && ! -e $profile/$package && ! -e $profile/incremental ]]
  [[ ! -e $profile/build/hft-data-hash && ! -e $profile/.fingerprint/hft-data-hash ]]
  for artifact in data-hash.d libdata-hash.rlib libdata-hash.rmeta libdata-hash.so hft-data-hash; do
    [[ ! -e $profile/deps/$artifact ]]
  done
  [[ $(cat "$profile/.fingerprint/data-encoding-hash/lib-data_encoding") == 'external dependency fingerprint' ]]
  [[ $(cat "$profile/deps/libdata_encoding-hash.rlib") == 'external dependency bytes' ]]
  [[ $(cat "$profile/build/data-encoding-hash/out/dependency.o") == 'external build output' ]]
done < <(jq -r '.workspaces[].manifest' "$FIXTURE/rust_hft/workspaces.json")
printf 'PASS: exact local artifact names removed; external data-encoding fingerprint and bytes retained in every CI owner target\n'
profile="$FIXTURE/rust_hft/research-core/target/debug"
rm -rf "$profile/deps"
mkdir -p "$work/outside"; printf untouched >"$work/outside/marker"
ln -s "$work/outside" "$profile/deps"
if MONDAY_CARGO_TARGET_LAYOUT=owning-workspace-v1 bash "$FIXTURE/.github/scripts/ci-owner-cache.sh" cleanup; then
  echo 'cache cleanup traversed a symlink' >&2; exit 1
fi
[[ $(<"$work/outside/marker") == untouched ]]

# The workflow uses the same disjoint paths and saves only after cleanup.
ruby -ryaml -rpathname -rjson -rdigest -rtmpdir -rfileutils -ropen3 - "$root" <<'RUBY'
root = ARGV.fetch(0)
job = YAML.safe_load(File.read("#{root}/.github/workflows/ci.yml")).fetch('jobs').fetch('rust')
abort 'owner layout missing' unless job.fetch('env')['MONDAY_CARGO_TARGET_LAYOUT'] == 'owning-workspace-v1'
steps = job.fetch('steps')
restore = steps.find { |s| s['name'] == 'Cache Rust' }
cleanup_index = steps.index { |s| s['name'] == 'Retain only external dependencies for trusted workspace cache saves' }
save_index = steps.index { |s| s['name'] == 'Save trusted workspace dependency cache' }
save = steps.fetch(save_index)
abort 'restore action can save local bytes' unless restore.fetch('uses') == 'actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830'
abort 'cache save precedes cleanup' unless cleanup_index < save_index
abort 'real cleanup is not validated on PRs' unless steps.fetch(cleanup_index).fetch('if') == "${{ success() && needs.scope.outputs.toolchain == 'true' }}"
abort 'save is not success/main bound' unless save.fetch('if').include?("github.ref == 'refs/heads/main' && success()")
abort 'restore has no stable cache state' unless restore.fetch('id') == 'owner-cache'
abort 'exact hit still compresses before save conflict' unless save.fetch('if').include?("steps.owner-cache.outputs.cache-hit != 'true'")
abort 'save action drift' unless save.fetch('uses') == 'actions/cache/save@0057852bfaa89a56745cba8c7296529d2fc39830'
abort 'restore/save key or owner drift' unless %w[key path].all? { |k| restore.dig('with', k) == save.dig('with', k) }
abort 'restore has no compatible prefix' unless restore.dig('with', 'restore-keys') == '${{ steps.cache-info.outputs.compat_prefix }}'
abort 'cache exact key is not explicit' unless restore.dig('with', 'key') == '${{ steps.cache-info.outputs.key }}'
abort 'PR token permissions changed' unless job.fetch('permissions') == {'contents' => 'read', 'actions' => 'read'}
dimensions = steps.find { |step| step['name'] == 'Record cache dimensions' }
abort 'cache coverage plan is not wired from the selector' unless dimensions.dig('env', 'MONDAY_CI_CACHE_PLAN') == '${{ toJSON(needs.scope.outputs) }}'
abort 'cache coverage output is never computed' unless dimensions.fetch('run').include?('ci-owner-cache.sh" coverage-input') && dimensions.fetch('run').include?('ci-owner-cache.sh" cache-input')
# A changed admitted feature or package must permit a new dependency cache save.
# Keep compiler/native/manifest dimensions fixed in these coverage fixtures.
script = File.read("#{root}/.github/scripts/ci-owner-cache.sh")
key_inputs = script.match(/helpers = \[(.*?)\]/m)[1].scan(/"([^"]+)"/).flatten
abort 'cache key has no compilation inputs' if key_inputs.empty?
# Audit actual workflow Cargo callers and every selector input they consume.
compile_steps = steps.select do |step|
  run = step['run'].to_s
  helpers = run.scan(%r{((?:rust_hft|\.github|deployment)/[a-zA-Z0-9_./-]+\.sh)}).flatten.uniq
  callers = helpers.select do |path|
    File.read("#{root}/#{path}").match?(/\bcargo[ \t]+(?:build|test|check|clippy)\b/) || path.end_with?('/cargo-scoped.sh')
  end
  abort 'direct Cargo helper is absent from cache inputs' unless (callers - key_inputs).empty?
  !callers.empty? || run.match?(/\bcargo[ \t]+(?:build|test|check|clippy)\b/)
end
fields = compile_steps.flat_map do |step|
  [step['if'], step['run'], *step.fetch('env', {}).values].join.scan(/needs\.scope\.outputs\.([a-z_]+)/).flatten
end.uniq
plan = %w[handoff json ondo collector control focused loop].to_h { |name| [name, 'true'] }
plan.merge!('owning_packages' => 'hft-paper', 'focused_packages' => 'hft-live', 'loop_packages' => 'alpha-harness')
fields.each { |name| plan[name] ||= 'false' }
coverage_digest = lambda do |selected|
  output, _, status = Open3.capture3({'MONDAY_CI_CACHE_PLAN' => JSON.generate(selected)},
    'bash', "#{root}/.github/scripts/ci-owner-cache.sh", 'coverage-input')
  abort 'valid workflow coverage could not be hashed' unless status.success?
  output
end
baseline_coverage = coverage_digest.call(plan)
fields.each do |name|
  changed = plan.dup
  changed[name] = name.end_with?('_packages') ? "#{plan.fetch(name)},hft-cex-research-worker" : 'false'
  changed[name] = 'true' if plan.fetch(name) == 'false'
  abort "compiled workflow input #{name} is absent from coverage digest" if coverage_digest.call(changed) == baseline_coverage
end
cache_inputs = lambda do |directory, selected = plan, overrides = {}|
  coverage = coverage_digest.call(selected).strip.delete_prefix('coverage=')
  env = {'MONDAY_CARGO_TARGET_LAYOUT' => 'owning-workspace-v1',
         'MONDAY_CI_CACHE_NATIVE' => 'a' * 64, 'MONDAY_CI_CACHE_COVERAGE' => coverage}.merge(overrides)
  output, error, status = Open3.capture3(env, 'bash', "#{directory}/.github/scripts/ci-owner-cache.sh", 'cache-input')
  abort "valid cache inputs failed: #{error}" unless status.success?
  output.lines.grep(/^(key|compat_prefix)=/).to_h { |line| line.strip.split('=', 2) }
end
tracked, _, status = Open3.capture3('git', '-C', root, 'ls-files', '-z', '--',
  ':(glob)**/Cargo.toml', ':(glob)**/Cargo.lock', ':(glob)**/.cargo/config', ':(glob)**/.cargo/config.toml',
  'Cargo.toml', 'Cargo.lock', '.cargo/config', '.cargo/config.toml', 'rust-toolchain.toml')
abort 'could not read Cargo inputs' unless status.success?
Dir.mktmpdir('ci-cache-coverage') do |fixture|
  FileUtils.cp_r("#{root}/.github/scripts/vendor", "#{fixture}/vendor")
  FileUtils.mkdir_p("#{fixture}/.github/scripts")
  FileUtils.mv("#{fixture}/vendor", "#{fixture}/.github/scripts/vendor")
  (key_inputs + tracked.split("\0")).uniq.each do |path|
    FileUtils.mkdir_p(File.dirname("#{fixture}/#{path}"))
    FileUtils.cp("#{root}/#{path}", "#{fixture}/#{path}")
  end
  _, _, status = Open3.capture3('git', '-C', fixture, 'init', '-q')
  abort 'fixture git init failed' unless status.success?
  _, _, status = Open3.capture3('git', '-C', fixture, 'add', '.')
  abort 'fixture git add failed' unless status.success?
  baseline = cache_inputs.call(fixture)
  wide = cache_inputs.call(fixture, plan.merge('focused_packages' => 'hft-live,hft-cex-research-worker'))
  abort 'coverage changed compatible restore prefix' unless wide['compat_prefix'] == baseline['compat_prefix']
  abort 'coverage changed no exact key' if wide['key'] == baseline['key']
  abort 'exact key is not inside compatible prefix' unless baseline['key'].start_with?(baseline['compat_prefix'])
  {'FIXTURE_COMPILER' => 'changed-compiler', 'MONDAY_CI_CACHE_NATIVE' => 'b' * 64,
   'RUSTFLAGS' => '-C target-cpu=native', 'CARGO_PROFILE_TEST_DEBUG' => '1'}.each do |name, value|
    changed = cache_inputs.call(fixture, plan, name => value)
    abort "#{name} crossed compatibility boundary" if changed['compat_prefix'] == baseline['compat_prefix']
  end
  compiler_binary = ENV.fetch('COMPILER_BINARY')
  original_binary = File.read(compiler_binary)
  File.write(compiler_binary, original_binary + 'changed compiler bytes')
  abort 'compiler bytes crossed compatibility boundary' if cache_inputs.call(fixture)['compat_prefix'] == baseline['compat_prefix']
  File.write(compiler_binary, original_binary)
  stdlib = "#{ENV.fetch('FIXTURE_SYSROOT')}/lib/rustlib/x86_64-unknown-linux-gnu/lib/libstd-fixture.rlib"
  original_stdlib = File.read(stdlib)
  File.write(stdlib, original_stdlib + 'changed standard library bytes')
  abort 'standard library bytes crossed compatibility boundary' if cache_inputs.call(fixture)['compat_prefix'] == baseline['compat_prefix']
  File.write(stdlib, original_stdlib)
  change = lambda do |path, old, replacement, compatible|
    original = File.read("#{fixture}/#{path}")
    revised = original.sub(old, replacement)
    abort "fixture did not change #{path}" if revised == original
    File.write("#{fixture}/#{path}", revised)
    changed = cache_inputs.call(fixture)
    abort "changed #{path} reused exact key" if changed['key'] == baseline['key']
    abort "wrong compatibility for #{path}" unless (changed['compat_prefix'] == baseline['compat_prefix']) == compatible
    File.write("#{fixture}/#{path}", original)
  end
  change.call('.github/workflows/ci.yml', '--features formula-strategy,binance ',
    '--features formula-strategy,binance,cache-fixture-feature ', true)
  change.call('.github/scripts/select-rust-ci-scope.sh',
    'focused_packages=hft-live,hft-paper,hft-all-in-one,alpha-harness,hft-harnessctl',
    'focused_packages=hft-live,hft-paper,hft-all-in-one,alpha-harness,hft-harnessctl,hft-cex-research-worker', true)
  change.call('rust_hft/scripts/workspace-metadata.sh', '.workspace_members | index($id)',
    '(.workspace_members + ["cache-fixture-member"]) | index($id)', true)
  change.call('rust_hft/shared/Cargo.toml', '[workspace.dependencies]', "# changed dependency declaration\n[workspace.dependencies]", true)
  change.call('rust_hft/shared/Cargo.lock', '# This file', "# Changed dependency lock\n# This file", true)
  change.call('rust_hft/shared/Cargo.toml', '[profile.bench]', "[profile.dev.package.cache_fixture]\nopt-level = 1\n[profile.bench]", false)
  change.call('rust_hft/.cargo/config.toml', '[build]', "# changed Cargo configuration\n[build]", false)
  FileUtils.mkdir_p("#{fixture}/rust_hft/src")
  File.write("#{fixture}/rust_hft/src/cache-fixture.rs", 'changed local source')
  abort 'local source changed dependency identity' unless cache_inputs.call(fixture) == baseline
  reject = lambda do |overrides|
    _, _, status = Open3.capture3({'MONDAY_CARGO_TARGET_LAYOUT' => 'owning-workspace-v1',
      'MONDAY_CI_CACHE_NATIVE' => 'a' * 64, 'MONDAY_CI_CACHE_COVERAGE' => 'c' * 64}.merge(overrides),
      'bash', "#{fixture}/.github/scripts/ci-owner-cache.sh", 'cache-input')
    abort 'unadmitted cache inputs accepted' if status.success?
  end
  reject.call('FIXTURE_TARGET' => 'aarch64-unknown-linux-gnu')
  reject.call('MONDAY_CARGO_TARGET_LAYOUT' => 'unknown')
  reject.call('MONDAY_CI_CACHE_COVERAGE' => 'invalid')
  reject.call('CARGO_PROFILE_UNKNOWN_DEBUG' => '1')
  reject.call('CARGO_BUILD_TARGET' => 'x86_64-unknown-linux-gnu')
  reject.call('FIXTURE_SYSROOT' => "#{fixture}/missing-sysroot")
  empty_sysroot = "#{fixture}/empty-sysroot"
  FileUtils.mkdir_p("#{empty_sysroot}/lib/rustlib/x86_64-unknown-linux-gnu/lib")
  reject.call('FIXTURE_SYSROOT' => empty_sysroot)
  manifest_path = "#{fixture}/rust_hft/shared/Cargo.toml"
  original = File.read(manifest_path)
  File.write(manifest_path, original + "\n[invalid TOML\n")
  reject.call({})
  File.write(manifest_path, original.sub('[profile.dev]', "[profile]\ndev = \"invalid\""))
  reject.call({})
  File.write(manifest_path, original.sub('inherits = "release"', 'inherits = "unknown"'))
  reject.call({})
  File.write(manifest_path, original)
  parser_path = "#{fixture}/.github/scripts/vendor/tomlrb/lib"
  FileUtils.mv(parser_path, "#{fixture}/parser-backup")
  reject.call({})
  FileUtils.mv("#{fixture}/parser-backup", parser_path)
  registry_path = "#{fixture}/rust_hft/workspaces.json"
  registry = File.read(registry_path)
  File.write(registry_path, registry.sub('"id": "shared"', '"id": "unknown"'))
  reject.call({})
end
# Only dependency source and debug artifact directories enter the archive.
output, error, status = Open3.capture3({'MONDAY_CARGO_TARGET_LAYOUT' => 'owning-workspace-v1',
  'MONDAY_CI_CACHE_NATIVE' => 'a' * 64, 'MONDAY_CI_CACHE_COVERAGE' => 'c' * 64},
  'bash', "#{root}/.github/scripts/ci-owner-cache.sh", 'cache-input')
abort error unless status.success?
paths = output.split("cache_paths<<MONDAY_CI_CACHE_PATHS\n", 2)[1].lines.map(&:strip)[0...-1]
abort 'cache admits installed executable tools' if paths.any? { |path| path.include?('/bin') }
expected = JSON.parse(File.read("#{root}/rust_hft/workspaces.json")).fetch('workspaces').map do |owner|
  manifest = owner.fetch('manifest')
  manifest == 'data-pipelines/Cargo.toml' ? "#{root}/rust_hft/target/debug" : "#{root}/rust_hft/#{File.dirname(manifest)}/target/debug"
end
abort 'cache misses a registered owner' unless paths.drop(2).sort == expected.sort
abort 'cache cleaners share targets' unless paths.uniq == paths
RUBY
printf 'CI owner targets, unchanged commands, dependency cleanup and failure contracts passed\n'
