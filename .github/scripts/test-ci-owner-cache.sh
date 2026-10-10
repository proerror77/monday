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
chmod +x "$work/bin/cargo"
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
  printf native >"$profile/build/libduckdb-sys-native/out/native.o"
  printf local >"$profile/deps/lib$rust_name-hash.rlib"
  printf hidden >"$profile/deps/libhidden_local-hash.rlib"
  printf outside >"$profile/deps/libexternal_local-hash.rlib"
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
  [[ ! -e $profile/build/$package-hash && ! -e $profile/deps/lib$rust_name-hash.rlib && ! -e $profile/deps/libhidden_local-hash.rlib ]]
  [[ ! -e $profile/deps/libexternal_local-hash.rlib && ! -e $profile/examples ]]
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
abort 'early action can save local bytes' unless restore.dig('with', 'save-if') == false
abort 'cache save precedes cleanup' unless cleanup_index < save_index
abort 'real cleanup is not validated on PRs' unless steps.fetch(cleanup_index).fetch('if') == "${{ success() && needs.scope.outputs.toolchain == 'true' }}"
abort 'save is not success/main bound' unless save.fetch('if').include?("github.ref == 'refs/heads/main' && success()")
abort 'restore/save key or owner drift' unless %w[key workspaces].all? { |k| restore.dig('with', k) == save.dig('with', k) }
abort 'cache omits the selected compilation scope' unless restore.dig('with', 'key').include?('steps.cache-info.outputs.coverage')
dimensions = steps.find { |step| step['name'] == 'Record cache dimensions' }
abort 'cache coverage plan is not wired from the selector' unless dimensions.dig('env', 'MONDAY_CI_CACHE_PLAN') == '${{ toJSON(needs.scope.outputs) }}'
abort 'cache coverage output is never computed' unless dimensions.fetch('run').include?('ci-owner-cache.sh" coverage-input')
# A changed admitted feature or package must permit a new dependency cache save.
# Keep compiler/native/manifest dimensions fixed in these coverage fixtures.
key_inputs = restore.dig('with', 'key').scan(/hashFiles\((.*?)\)/).flat_map { |group| group.first.scan(/'([^']+)'/).flatten }
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
dimension = lambda do |directory|
  Digest::SHA256.hexdigest(key_inputs.sort.map { |path| Digest::SHA256.file("#{directory}/#{path}").digest }.join)
end
Dir.mktmpdir('ci-cache-coverage') do |fixture|
  coverage_paths = %w[.github/workflows/ci.yml .github/scripts/select-rust-ci-scope.sh rust_hft/scripts/workspace-metadata.sh]
  (key_inputs + coverage_paths).uniq.each do |path|
    FileUtils.mkdir_p(File.dirname("#{fixture}/#{path}"))
    FileUtils.cp("#{root}/#{path}", "#{fixture}/#{path}")
  end
  baseline = dimension.call(fixture)
  workflow_path = "#{fixture}/.github/workflows/ci.yml"
  workflow = File.read(workflow_path)
  changed = workflow.sub('--features formula-strategy,binance ', '--features formula-strategy,binance,cache-fixture-feature ')
  abort 'feature coverage fixture did not change a Cargo recipe' if changed == workflow
  File.write(workflow_path, changed)
  abort 'changed feature coverage reuses the immutable cache key' if dimension.call(fixture) == baseline
  File.write(workflow_path, workflow)
  scope_path = "#{fixture}/.github/scripts/select-rust-ci-scope.sh"
  scope = File.read(scope_path)
  changed = scope.sub('focused_packages=hft-live,hft-paper,hft-all-in-one,alpha-harness,hft-harnessctl',
                      'focused_packages=hft-live,hft-paper,hft-all-in-one,alpha-harness,hft-harnessctl,hft-cex-research-worker')
  abort 'package coverage fixture did not change the selected set' if changed == scope
  File.write(scope_path, changed)
  abort 'changed package coverage reuses the immutable cache key' if dimension.call(fixture) == baseline
  File.write(scope_path, scope)
  metadata_path = "#{fixture}/rust_hft/scripts/workspace-metadata.sh"
  metadata = File.read(metadata_path)
  changed = metadata.sub('.workspace_members | index($id)', '(.workspace_members + ["cache-fixture-member"]) | index($id)')
  abort 'metadata coverage fixture did not change member selection' if changed == metadata
  File.write(metadata_path, changed)
  abort 'changed metadata coverage reuses the immutable cache key' if dimension.call(fixture) == baseline
  File.write(metadata_path, metadata)
  FileUtils.mkdir_p("#{fixture}/rust_hft/src")
  File.write("#{fixture}/rust_hft/src/cache-fixture.rs", 'changed local source')
  abort 'local source changes invalidate dependency reuse' unless dimension.call(fixture) == baseline
end
pairs = restore.dig('with', 'workspaces').lines.map { |line| line.strip.split(' -> ') }
paths = pairs.map { |source, target| Pathname.new("#{root}/#{source}/#{target}").cleanpath.to_s }
manifests = JSON.parse(File.read("#{root}/rust_hft/workspaces.json")).fetch('workspaces').map { |w| w.fetch('manifest') }
expected = manifests.map { |m| m == 'data-pipelines/Cargo.toml' ? "#{root}/rust_hft/target" : "#{root}/rust_hft/#{File.dirname(m)}/target" }
abort 'cache misses a registered owner' unless paths.sort == expected.sort
abort 'cache cleaners share/nest targets' unless paths.uniq == paths && paths.none? { |p| paths.any? { |q| p != q && p.start_with?("#{q}/") } }
RUBY
printf 'CI owner targets, unchanged commands, dependency cleanup and failure contracts passed\n'
