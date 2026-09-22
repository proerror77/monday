#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$script_dir/../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

ruby -ryaml - "$root" "$fixture" <<'RUBY'
root, fixture = ARGV
prediction = YAML.safe_load(File.read("#{root}/.github/workflows/ploy-ci.yml")).fetch('jobs')
architecture = prediction.fetch('architecture-contracts')
abort 'architecture checks must not need a database service' if architecture.key?('services')
abort 'architecture failures must fail their lane' if architecture['continue-on-error']
steps = architecture.fetch('steps')
command = steps.find { |step| step['run']&.include?('--test workspace_runtime_retirement') }
abort 'architecture suite must run as the standalone ploy test' unless command &&
  command['run'].strip == 'cargo test --locked -p ploy --test workspace_runtime_retirement' &&
  !command['continue-on-error']
abort 'architecture lane missing from required gate' unless
  prediction.fetch('prediction-markets-gate').fetch('needs').include?('architecture-contracts')
integration = prediction.fetch('integration-regressions').fetch('steps')
abort 'architecture suite still duplicated in the database lane' if
  integration.any? { |step| step['run']&.include?('workspace_runtime_retirement') }

ci = YAML.safe_load(File.read("#{root}/.github/workflows/ci.yml")).fetch('jobs')
engine = ci.fetch('rust_hft_engine_fast_lane')
abort 'engine failures must fail their lane' if engine['continue-on-error']
benchmark = engine.fetch('steps').find { |step| step['name'] == 'Enforce quote-to-worker latency budgets' }
abort 'benchmark failures must fail the step' unless benchmark && !benchmark['continue-on-error']
abort 'engine lane missing from required gate' unless
  ci.fetch('ci-gate').fetch('needs').include?('rust_hft_engine_fast_lane')
File.write("#{fixture}/benchmark.sh", benchmark.fetch('run'))
RUBY

mkdir "$fixture/bin"
cat >"$fixture/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"$RUNNER_TEMP/cargo-args"
printf 'quote_to_worker_queue samples=20000 warmup=1000 p50_ns=10 p99_ns=20 p999_ns=30 p99_budget_ns=500000 p999_budget_ns=1000000\n'
exit "$BENCHMARK_EXIT"
SH
chmod +x "$fixture/bin/cargo"
printf '%s\n' test -p hft-engine --bench hotpath_latency_p99 --release --locked -- --nocapture \
  >"$fixture/expected-args"

for result in 0 101; do
  actual=0
  PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture" BENCHMARK_EXIT=$result \
    bash "$fixture/benchmark.sh" >"$fixture/command-output" 2>&1 || actual=$?
  if [[ $actual != "$result" ]]; then
    printf 'benchmark step swallowed failure: expected %s, got %s\n' "$result" "$actual" >&2
    exit 1
  fi
  diff -u "$fixture/expected-args" "$fixture/cargo-args"
  grep -q '^quote_to_worker_queue samples=20000 ' "$fixture/engine-latency.log"
done

test ! -e "$root/rust_hft/benches/hotpath_latency_p99.rs"
printf 'agent validation gate tests passed\n'
