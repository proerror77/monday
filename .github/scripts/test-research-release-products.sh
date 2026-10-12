#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Match realpath guards when macOS exposes its temporary root through an alias.
work=$(cd "$work" && pwd -P)
export GITHUB_RUN_ATTEMPT=2 MONDAY_RELEASE_JOB_ID=7 MONDAY_BUILD_INPUTS_FILE="$work/inputs.json"
sha=1111111111111111111111111111111111111111
# Domain-only recipes must stay independently buildable. Shared CEX/controller
# executables appear once when a producer builds more than one product.
products="$root/.github/scripts/research-release-products.sh"
bash "$products" recipes prediction-runner >"$work/prediction-recipes"
jq -s -e 'length==3 and all(.[]; .manifest=="prediction-markets/Cargo.toml") and any(.[]; .package=="ploy-research" and (.binaries|length)==3) and any(.[]; .package=="hft-prediction-research-worker" and .binaries==["monday-prediction-worker"]) and any(.[]; .package=="hft-prediction-research-operator" and .binaries==["monday-prediction-operator"])' "$work/prediction-recipes" >/dev/null
bash "$products" recipes cex-runner >"$work/cex-recipes"
jq -s -e 'length==6 and any(.[]; .package=="hft-cex-data-preparation" and .manifest=="data-pipelines/Cargo.toml" and .binaries==["monday-cex-prepare"]) and any(.[]; .package=="hft-cex-research-worker" and .binaries==["monday-cex-worker"]) and all(.[]; .package!="ploy-research" and .manifest!="prediction-markets/Cargo.toml")' "$work/cex-recipes" >/dev/null
jq -s -e 'any(.[]; .package=="alpha-harness" and .features=="offline-diagnostic" and .binaries==["monday-offline-baseline"]) and any(.[]; .package=="alpha-harness" and .features=="" and .binaries==["alpha-harness"])' "$work/cex-recipes" >/dev/null
[[ $(bash "$products" binaries cex-runner | wc -l) -eq 9 ]]
[[ $(bash "$products" binaries all | wc -l) -eq 14 ]]
[[ $(bash "$products" binaries cex-runner,controller | wc -l) -eq 9 ]]
[[ $(bash "$products" merge controller prediction-runner) == controller,prediction-runner ]]
for invalid in runner paired '' 'cex-runner,cex-runner' 'prediction-runner,unknown' 'all,controller'; do
  if bash "$products" normalize "$invalid" >"$work/rejection" 2>&1; then
    echo "invalid product selection accepted: $invalid" >&2; exit 1
  fi
done
# The real prebuilt Docker targets must copy precisely their catalog subset.
ruby -rjson - "$root" <<'RUBY'
root=ARGV.fetch(0)
catalog=JSON.parse(File.read(File.join(root,'.github/scripts/research-release-products.json'))).fetch('products')
{'cex-runner'=>'Dockerfile.research','prediction-runner'=>'Dockerfile.prediction-research'}.each do |product,file|
  text=File.read(File.join(root,'rust_hft/deployment/docker',file))
  prebuilt=text.split(/^FROM runtime-base AS prebuilt\s*$/,2).fetch(1).split(/^FROM /,2).first
  copied=prebuilt.scan(/^COPY --chmod=0755 research-bin\/(\S+) \/usr\/local\/bin\/(\S+)$/)
  abort "unexpected prebuilt COPY: #{product}" unless copied.all? { |source,target| source==target } && copied.map(&:first).sort==catalog.fetch(product).sort
  foreign=catalog.fetch(product=='cex-runner' ? 'prediction-runner' : 'cex-runner')
  abort "source builder imports another domain: #{product}" if foreign.any? { |binary| text.include?(binary) }
end
RUBY
mkdir -p "$work/controller/research-bin"
bash "$root/.github/scripts/research-release-products.sh" binaries controller >"$work/binaries"
[[ $(wc -l <"$work/binaries") -eq 1 ]]
while IFS= read -r binary; do
  printf 'mock executable: %s\n' "$binary" >"$work/controller/research-bin/$binary"
  chmod 0755 "$work/controller/research-bin/$binary"
done <"$work/binaries"
bash "$root/.github/scripts/research-release-products.sh" recipes controller >"$work/recipes"
jq -s -e 'length==1 and all(.[]; .package!="hft-research-platform") and any(.[]; .package=="alpha-harness") and
  all(.[]; .package!="hft-collector" and .package!="hft-cex-data-preparation") and
  all(.[]; .package!="hft-backtest" and .package!="ploy-research") and
  all(.[]; .features=="" and .binaries==["alpha-harness"])' "$work/recipes" >/dev/null
locks=$(bash "$root/.github/scripts/research-workspace-locks.sh" "$root/rust_hft")
workspace_profiles=$(ruby -rjson -rdigest -e 'root=ARGV[0]; puts JSON.generate(JSON.parse(File.read("#{root}/workspaces.json")).fetch("workspaces").to_h{|owner| manifest=owner.fetch("manifest"); [manifest,Digest::SHA256.file("#{root}/#{manifest}").hexdigest]})' "$root/rust_hft")
jq -n --arg h "$(printf a%.0s {1..64})" --argjson locks "$locks" --argjson workspace_profiles "$workspace_profiles" --argjson recipes "$(jq -s . "$work/recipes")" \
  '{schema:"monday.compilation-inputs.v3",target:"x86_64-unknown-linux-gnu",profile:"release",compiler:$h,native:$h,flags:$h,profiles:$h,recipe:$h,locks:$locks,builder_image:("builder@sha256:"+$h),recipes:$recipes,workspace_profiles:$workspace_profiles}' >"$MONDAY_BUILD_INPUTS_FILE"
bash "$root/.github/scripts/research-image-release-artifact.sh" create "$work/controller" "$sha" 42 "$root/rust_hft" 2 7 controller
ruby "$root/.github/scripts/research-release-bundle.rb" pack "$work/controller.tar" "$work/controller" controller
ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$work/controller.tar" "$work/roundtrip" controller
bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 controller
if ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$work/controller.tar" "$work/wrong-product" all >"$work/rejection" 2>&1; then exit 1; fi
if bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 cex-runner >"$work/rejection" 2>&1; then exit 1; fi
printf 'unadmitted control executable\n' >"$work/roundtrip/research-bin/researchctl"
chmod 0755 "$work/roundtrip/research-bin/researchctl"
if bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 controller >"$work/rejection" 2>&1; then exit 1; fi
# Round-trip every nonempty combination; wrong-domain and extra bytes remain
# rejected even when the producer contains more than one image's programs.
for selection in cex-runner prediction-runner cex-runner,controller cex-runner,prediction-runner controller,prediction-runner all; do
  directory="$work/$selection"
  mkdir -p "$directory/research-bin"
  while IFS= read -r binary; do
    printf 'mock executable: %s\n' "$binary" >"$directory/research-bin/$binary"
    chmod 0755 "$directory/research-bin/$binary"
  done < <(bash "$products" binaries "$selection")
  jq --argjson recipes "$(bash "$products" recipes "$selection" | jq -s .)" '.recipes=$recipes' "$MONDAY_BUILD_INPUTS_FILE" >"$work/updated-inputs.json"
  cp "$work/updated-inputs.json" "$MONDAY_BUILD_INPUTS_FILE"
  bash "$root/.github/scripts/research-image-release-artifact.sh" create "$directory" "$sha" 42 "$root/rust_hft" 2 7 "$selection"
  ruby "$root/.github/scripts/research-release-bundle.rb" pack "$directory.tar" "$directory" "$selection"
  ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$directory.tar" "$directory-roundtrip" "$selection"
  bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$directory-roundtrip" "$sha" 42 "$root/rust_hft" 2 7 "$selection"
done
printf '%s\n' deployment/aliyun/research/scripts/campaign-job-watch.sh >"$work/controller-path"
GITHUB_REF=refs/heads/main bash "$root/.github/scripts/select-rust-ci-scope.sh" --event push --changed-files "$work/controller-path" \
  --metadata "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture" --output "$work/plan"
grep -Fqx research_product=controller "$work/plan"
grep -Fq ',ploy/research-image-smoke,' "$work/plan"
bash "$root/.github/scripts/test-research-product-image.sh"
# Exercise the real producer with a conflicting ambient target and stale host
# binaries. Only the target recorded by compiler inputs may reach the archive.
fixture="$work/producer"
mkdir -p "$fixture/.github/scripts" "$fixture/rust_hft" "$fixture/bin" "$fixture/output"
for script in research-cache-layout.sh build-research-release.sh build-research-recipes.sh research-release-source-sha.sh research-release-products.sh research-release-products.json research-workspace-locks.sh research-image-release-artifact.sh verify-research-runner-binaries.sh research-release-bundle.rb; do
  cp "$root/.github/scripts/$script" "$fixture/.github/scripts/$script"
done
cp "$root/rust_hft/workspaces.json" "$fixture/rust_hft/workspaces.json"
while IFS= read -r manifest; do
  directory=${manifest%/Cargo.toml}; mkdir -p "$fixture/rust_hft/$directory"
  cp "$root/rust_hft/$manifest" "$fixture/rust_hft/$manifest"
  cp "$root/rust_hft/$directory/Cargo.lock" "$fixture/rust_hft/$directory/Cargo.lock"
done < <(jq -r '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
printf '#!/usr/bin/env bash\nexit 0\n' >"$fixture/.github/scripts/verify-research-runtime-abi.sh"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "1111111111111111111111111111111111111111"\n' >"$fixture/bin/git"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" 7\n' >"$fixture/bin/gh"
# The producer targets Linux. Inject host metrics only in this test fixture.
export MONDAY_TEST_AWK MONDAY_TEST_CAT MONDAY_TEST_CPUINFO_FILE MONDAY_TEST_MEMINFO_FILE MONDAY_TEST_CARGO_BUILD_LOG
MONDAY_TEST_AWK=$(command -v awk)
MONDAY_TEST_CAT=$(command -v cat)
MONDAY_TEST_CPUINFO_FILE="$fixture/cpuinfo"
MONDAY_TEST_MEMINFO_FILE="$fixture/meminfo"
MONDAY_TEST_CARGO_BUILD_LOG="$fixture/cargo-builds"
printf 'processor : 0\nmodel name : Monday fixture CPU\nprocessor : 1\nmodel name : ignored second CPU\n' >"$MONDAY_TEST_CPUINFO_FILE"
printf 'MemAvailable: 16777216 kB\n' >"$MONDAY_TEST_MEMINFO_FILE"
cat >"$fixture/bin/awk" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
arguments=("$@")
for index in "${!arguments[@]}"; do
  if [[ ${arguments[index]} == /proc/cpuinfo ]]; then
    arguments[index]=${MONDAY_TEST_CPUINFO_FILE:?}
  elif [[ ${arguments[index]} == /proc/meminfo ]]; then
    arguments[index]=${MONDAY_TEST_MEMINFO_FILE:?}
  fi
done
exec "${MONDAY_TEST_AWK:?}" "${arguments[@]}"
MOCK
cat >"$fixture/bin/nproc" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $# == 0 ]] || exit 91
printf '8\n'
MOCK
cat >"$fixture/bin/cat" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
# Fixture capacity must not depend on the machine running this contract test.
if [[ $# == 1 && $1 == /sys/fs/cgroup/memory.max ]]; then printf 'max\n';
elif [[ $# == 1 && $1 == /sys/fs/cgroup/memory.current ]]; then printf '0\n';
else exec "${MONDAY_TEST_CAT:?}" "$@"; fi
MOCK
cat >"$fixture/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ $1 == metadata ]]; then
  jq -n --arg root "$PWD" '{packages:[{name:"alpha-harness",manifest_path:($root+"/alpha-harness/Cargo.toml"),targets:[{name:"alpha_harness"}]}]}'
  exit
fi
printf 'build\n' >>"${MONDAY_TEST_CARGO_BUILD_LOG:?}"
[[ ${FAIL_RECIPE:-0} != 1 ]] || exit 37
target='' manifest='' package='' features='' jobs='' binaries=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) target=$2; shift ;;
    --bin) binaries+=("$2"); shift ;;
    --manifest-path) manifest=$2; shift ;;
    --jobs) jobs=$2; shift ;;
    -p) package=$2; shift ;;
    --features) features=$2; shift ;;
  esac
  shift
done
test "$target" = "$(jq -r .target "$MONDAY_BUILD_INPUTS_FILE")"
if [[ -n ${MONDAY_TEST_CARGO_EVENTS:-} ]]; then
  owner_lock="${MONDAY_TEST_TARGET_LOCKS:?}/${manifest//\//--}"
  mkdir "$owner_lock" || { echo 'concurrent writes to one owner target' >&2; exit 92; }
  trap 'rmdir "$owner_lock"' EXIT
  event() {
    ruby -rjson - "$MONDAY_TEST_CARGO_EVENTS" "$1" "$manifest" "$package" "$features" "$jobs" "$CARGO_TARGET_DIR" "${binaries[@]}" <<'RUBY'
file, event, manifest, package, features, jobs, target, *binaries = ARGV
record = {event:event,at:Process.clock_gettime(Process::CLOCK_MONOTONIC),manifest:manifest,
          package:package,features:features,jobs:Integer(jobs),target:target,binaries:binaries}
File.open(file, 'a') { |out| out.flock(File::LOCK_EX); out.puts(JSON.generate(record)) }
RUBY
  }
  event start
  if [[ ${MONDAY_TEST_CARGO_MODE:-} == fail-and-cancel || ${MONDAY_TEST_CARGO_MODE:-} == interrupted ]]; then
    if [[ $package == hft-collector ]]; then
      ruby -e 'Signal.trap("TERM") {}; File.write(ARGV[0], Process.pid.to_s); sleep 30' "${MONDAY_TEST_COMPILER_PID:?}" &
      wait "$!"
    elif [[ $package == hft-backtest && $MONDAY_TEST_CARGO_MODE == fail-and-cancel ]]; then
      for ((wait_count=0; wait_count<100; wait_count++)); do
        if [[ -f ${MONDAY_TEST_COMPILER_PID:?} ]]; then
          ruby -e 'Signal.trap("TERM") {}; File.write(ARGV[0], Process.pid.to_s); sleep 30' "${MONDAY_TEST_FAILED_COMPILER_PID:?}" &
          for ((child_wait=0; child_wait<100; child_wait++)); do
            [[ ! -f $MONDAY_TEST_FAILED_COMPILER_PID ]] || exit 37
            sleep 0.05
          done
          exit 94
        fi
        sleep 0.05
      done
      echo 'parallel compiler did not start' >&2; exit 93
    fi
  fi
  sleep 0.2
  event finish
fi
mkdir -p "$CARGO_TARGET_DIR/$target/release"
for binary in "${binaries[@]}"; do
  printf 'fresh target executable: %s\n' "$binary" >"$CARGO_TARGET_DIR/$target/release/$binary"
  printf '%s\n' '{"reason":"compiler-artifact","package_id":"path+file:///fixture#local-recipe@0.1.0","target":{"name":"mock-binary"},"fresh":true}'
done
printf '%s\n' '{"reason":"build-finished","success":true}'
MOCK
chmod +x "$fixture/bin/"* "$fixture/.github/scripts/verify-research-runtime-abi.sh"
mkdir -p "$fixture/rust_hft/target/release"
while IFS= read -r binary; do
  printf 'stale host executable\n' >"$fixture/rust_hft/target/release/$binary"
done < <(bash "$products" binaries controller)
jq --argjson recipes "$(bash "$products" recipes controller | jq -s .)" '.recipes=$recipes' "$MONDAY_BUILD_INPUTS_FILE" >"$fixture/inputs.json"
MONDAY_RESEARCH_CACHE_PROBE=1 PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/output" GITHUB_REPOSITORY=fixture/monday GITHUB_RUN_ID=42 CARGO_BUILD_TARGET=aarch64-unknown-linux-gnu MONDAY_BUILD_INPUTS_FILE="$fixture/inputs.json" bash "$fixture/.github/scripts/build-research-release.sh" controller
while IFS= read -r binary; do
  test "$(cat "$fixture/output/research-release/research-bin/$binary")" = "fresh target executable: $binary"
done < <(bash "$products" binaries controller)
printf 'PASS: controller-only release excludes data preparation and release tools; product, archive and unadmitted control bytes fail closed\n'

# The opt-in probe compares both schedules after identical dependency-only cleanup.
jq --argjson count "$(bash "$products" recipes controller | jq -s length)" -es '
  length==($count*4) and all(group_by(.phase)[]; length==$count)
  and ([.[].phase]|unique)==["after-cache-lookup","dependency-warm-parallel","dependency-warm-serial","warm-local"]
  and ([.[].compilation_inputs_sha256]|unique|length)==1
  and ([.[].build_inputs]|unique|length)==1
  and all(.[]; .build_inputs.profile=="release" and .build_inputs.target=="x86_64-unknown-linux-gnu" and .phase_seconds>=0)
  and all(.[]|select(.phase=="dependency-warm-serial"); .scheduler.requested_owner_concurrency==1 and .scheduler.owner_concurrency==1 and .scheduler.cargo_jobs==8)
  and all(.[]|select(.phase=="dependency-warm-parallel"); .scheduler.requested_owner_concurrency==2 and .scheduler.owner_concurrency==1 and .scheduler.cargo_jobs==8)
  and all(.[]; .source_sha=="1111111111111111111111111111111111111111" and .compiler_artifacts>0
    and .runner.cpu_model=="Monday fixture CPU" and .runner.logical_cpus==8)' \
  "$fixture/output/research-recipe-probe/timings.jsonl" >/dev/null
# A missing metrics input must fail before Cargo, timings, or release creation.
mkdir -p "$fixture/cpuinfo-failed-output"
if MONDAY_TEST_CPUINFO_FILE="$fixture/missing-cpuinfo" MONDAY_TEST_CARGO_BUILD_LOG="$fixture/cpuinfo-failed-builds" \
  PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/cpuinfo-failed-output" GITHUB_REPOSITORY=fixture/monday \
  GITHUB_RUN_ID=42 MONDAY_BUILD_INPUTS_FILE="$fixture/inputs.json" \
  bash "$fixture/.github/scripts/build-research-release.sh" controller >"$work/rejection" 2>&1; then
  echo 'missing CPU information accepted by the producer' >&2; exit 1
fi
grep -Fq "$fixture/missing-cpuinfo" "$work/rejection"
[[ ! -e $fixture/cpuinfo-failed-builds ]]
[[ ! -e $fixture/cpuinfo-failed-output/research-recipe-probe/timings.jsonl ]]
[[ ! -e $fixture/cpuinfo-failed-output/research-image-release.tar ]]
# A compiler failure must block the producer before artifact creation.
mkdir -p "$fixture/failed-output"
if FAIL_RECIPE=1 PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/failed-output" GITHUB_REPOSITORY=fixture/monday \
  GITHUB_RUN_ID=42 MONDAY_BUILD_INPUTS_FILE="$fixture/inputs.json" bash "$fixture/.github/scripts/build-research-release.sh" controller; then
  echo 'compiler failure accepted by the producer' >&2; exit 1
fi
[[ ! -e $fixture/failed-output/research-image-release.tar ]]
jq '.recipes=[]' "$fixture/inputs.json" >"$fixture/wrong-inputs.json"
if PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/failed-output" MONDAY_SOURCE_REVISION="$sha" \
  MONDAY_BUILD_INPUTS_FILE="$fixture/wrong-inputs.json" bash "$fixture/.github/scripts/build-research-recipes.sh" controller after-cache-lookup; then
  echo 'wrong admitted recipes accepted by the probe' >&2; exit 1
fi
printf 'PASS: recipe probe preserves exact inputs, compiler failures and all selected recipes\n'

# Exercise actual process overlap and target ownership, without Rust compilation.
export MONDAY_TEST_CARGO_EVENTS="$fixture/parallel-events" MONDAY_TEST_TARGET_LOCKS="$fixture/owner-locks"
mkdir -p "$MONDAY_TEST_TARGET_LOCKS" "$fixture/parallel-output"
jq --argjson recipes "$(bash "$products" recipes all | jq -s .)" '.recipes=$recipes' "$MONDAY_BUILD_INPUTS_FILE" >"$fixture/all-inputs.json"
PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/parallel-output" MONDAY_SOURCE_REVISION="$sha" \
  MONDAY_BUILD_INPUTS_FILE="$fixture/all-inputs.json" CARGO_BUILD_JOBS=4 \
  bash "$fixture/.github/scripts/build-research-recipes.sh" all after-cache-lookup >"$work/parallel.log"
ruby -rjson - "$MONDAY_TEST_CARGO_EVENTS" "$fixture/all-inputs.json" <<'RUBY'
events = File.readlines(ARGV[0]).map { |line| JSON.parse(line) }
recipes = JSON.parse(File.read(ARGV[1])).fetch('recipes')
starts = events.select { |event| event.fetch('event') == 'start' }
actual = starts.map { |event| event.slice('manifest', 'package', 'features', 'binaries') }
abort 'selected recipes changed or repeated' unless actual.sort_by { |recipe| recipe.fetch('package') } == recipes.sort_by { |recipe| recipe.fetch('package') }
recipes.map { |recipe| recipe.fetch('manifest') }.uniq.each do |owner|
  expected = recipes.select { |recipe| recipe.fetch('manifest') == owner }.map { |recipe| recipe.fetch('package') }
  observed = starts.select { |event| event.fetch('manifest') == owner }.map { |event| event.fetch('package') }
  abort 'owner recipe ordering changed' unless expected == observed
end
active = {}
maximum = 0
events.sort_by { |event| event.fetch('at') }.each do |event|
  owner = event.fetch('manifest')
  if event.fetch('event') == 'start'
    abort 'overlapping owner target writes' if active.key?(owner)
    active[owner] = event.fetch('jobs')
  else
    abort 'completion without owning writer' unless active.delete(owner)
  end
  maximum = [maximum, active.length].max
  abort 'CPU or owner budget exceeded' unless active.length <= 2 && active.values.sum <= 4
end
abort 'independent owners never overlapped' unless maximum == 2 && active.empty?
RUBY
jq --argjson count "$(bash "$products" recipes all | jq -s length)" -es 'length==$count and all(.[]; .scheduler.owner_concurrency==2 and .scheduler.cargo_jobs==2)' \
  "$fixture/parallel-output/research-recipe-probe/timings.jsonl" >/dev/null

# Low available memory reduces overlap. Explicit serial mode uses the same recipes.
printf 'MemAvailable: 4194304 kB\n' >"$MONDAY_TEST_MEMINFO_FILE"
for mode in memory serial; do
  if [[ $mode == serial ]]; then printf 'MemAvailable: 16777216 kB\n' >"$MONDAY_TEST_MEMINFO_FILE"; fi
  mkdir -p "$fixture/$mode-output"
  PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/$mode-output" MONDAY_SOURCE_REVISION="$sha" \
    MONDAY_BUILD_INPUTS_FILE="$fixture/inputs.json" MONDAY_RESEARCH_BUILD_CONCURRENCY=$([[ $mode == serial ]] && echo 1 || echo 2) \
    bash "$fixture/.github/scripts/build-research-recipes.sh" controller after-cache-lookup >"$work/$mode.log"
  jq --argjson count "$(bash "$products" recipes controller | jq -s length)" -es 'length==$count and all(.[]; .scheduler.owner_concurrency==1 and .scheduler.cargo_jobs==8)' \
    "$fixture/$mode-output/research-recipe-probe/timings.jsonl" >/dev/null
done

# A failed owner terminates sibling compilers, including TERM-ignoring children.
export MONDAY_TEST_COMPILER_PID="$fixture/compiler-pid" MONDAY_TEST_FAILED_COMPILER_PID="$fixture/failed-compiler-pid"
mkdir -p "$fixture/cancel-output"
code=0
MONDAY_TEST_CARGO_MODE=fail-and-cancel PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/cancel-output" \
  GITHUB_REPOSITORY=fixture/monday GITHUB_RUN_ID=42 MONDAY_BUILD_INPUTS_FILE="$fixture/all-inputs.json" \
  bash "$fixture/.github/scripts/build-research-release.sh" all >"$work/cancel.log" 2>&1 || code=$?
[[ $code == 37 && ! -e $fixture/cancel-output/research-image-release.tar ]]
for pid_file in "$MONDAY_TEST_COMPILER_PID" "$MONDAY_TEST_FAILED_COMPILER_PID"; do
  compiler_pid=$(cat "$pid_file")
  # PID 1 may retain an exited orphan as a zombie; it cannot execute anymore.
  compiler_state=$(ps -o stat= -p "$compiler_pid" 2>/dev/null || true)
  if kill -0 "$compiler_pid" 2>/dev/null && [[ ${compiler_state//[[:space:]]/} != Z* ]]; then
    echo 'failed or cancelled owner left a compiler alive' >&2; exit 1
  fi
done
grep -Fq 'research recipe owner failed: research-core/Cargo.toml status=37' "$work/cancel.log"

# Signal only the outer release Bash PID; both supervisors must forward cancellation.
rm -f "$MONDAY_TEST_COMPILER_PID"
rm -rf "$MONDAY_TEST_TARGET_LOCKS"
mkdir -p "$MONDAY_TEST_TARGET_LOCKS" "$fixture/interrupted-output"
MONDAY_TEST_CARGO_MODE=interrupted PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/interrupted-output" \
  GITHUB_REPOSITORY=fixture/monday GITHUB_RUN_ID=42 MONDAY_BUILD_INPUTS_FILE="$fixture/all-inputs.json" \
  ruby - "$fixture/.github/scripts/build-research-release.sh" "$MONDAY_TEST_COMPILER_PID" "$work/interrupted.log" <<'RUBY'
script, compiler_file, log = ARGV
pid = Process.spawn('bash', script, 'all', pgroup:true, out:log, err:log)
begin
  deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 10
  until File.exist?(compiler_file)
    abort 'interrupted compiler did not start' if Process.clock_gettime(Process::CLOCK_MONOTONIC) >= deadline
    sleep 0.05
  end
  Process.kill('TERM', pid)
  _, status = Process.wait2(pid)
  abort 'interrupted producer succeeded' if status.success?
  compiler = Integer(File.read(compiler_file))
  deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 3
  loop do
    begin
      Process.kill(0, compiler)
    rescue Errno::ESRCH
      break
    end
    state = IO.popen(['ps', '-o', 'stat=', '-p', compiler.to_s]) { |process| process.read(64).to_s.strip }
    break if state.start_with?('Z')
    abort 'interrupted compiler remained alive' if Process.clock_gettime(Process::CLOCK_MONOTONIC) >= deadline
    sleep 0.05
  end
ensure
  if File.exist?(compiler_file)
    Process.kill('KILL', Integer(File.read(compiler_file))) rescue Errno::ESRCH
  end
  Process.kill('KILL', -pid) rescue Errno::ESRCH
  Process.wait(pid) rescue Errno::ECHILD
end
RUBY
[[ ! -e $fixture/interrupted-output/research-image-release.tar ]]

# Invalid resource requests fail before starting a compiler.
for request in cpu concurrency; do
  mkdir -p "$fixture/invalid-$request-output"
  code=0
  PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/invalid-$request-output" MONDAY_SOURCE_REVISION="$sha" \
    MONDAY_BUILD_INPUTS_FILE="$fixture/all-inputs.json" MONDAY_TEST_CARGO_BUILD_LOG="$fixture/invalid-$request-builds" \
    CARGO_BUILD_JOBS=$([[ $request == cpu ]] && echo 9 || echo 8) \
    MONDAY_RESEARCH_BUILD_CONCURRENCY=$([[ $request == concurrency ]] && echo 3 || echo 2) \
    bash "$fixture/.github/scripts/build-research-recipes.sh" all after-cache-lookup >"$work/invalid-$request.log" 2>&1 || code=$?
  [[ $code != 0 && ! -e $fixture/invalid-$request-builds ]]
done
printf 'PASS: owner overlap, exact recipes, CPU/memory admission, serial mode, cancellation and compiler-tree failure cleanup\n'
bash "$root/.github/scripts/test-research-release-tools.sh"
