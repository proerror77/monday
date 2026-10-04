#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
ruby -ryaml -e '
  jobs=YAML.safe_load(File.read(".github/workflows/ci.yml")).fetch("jobs")
  rust=jobs.fetch("rust")
  abort "research resource prerequisite remains" if jobs.key?("research_preflight") || rust.to_s.include?("wait-ack")
  collector=rust.fetch("steps").find{|s|s["id"]=="collector"}.fetch("run")
  abort "missing exact nonignored-test check" unless collector.include?("check-collector-test-presence.sh")
  abort "missing full owning suite" unless collector.include?("cargo-scoped.sh\" test -p hft-collector --features collector-binance --locked")
  abort "missing full aggregate edge" unless jobs.fetch("ci-gate").fetch("needs").include?("rust")
'
for result in success skipped failure cancelled; do
  fixture=$(jq -n --arg result "$result" '{scope:{result:"success"},rust:{result:$result}}')
  if printf '%s' "$fixture" | bash .github/scripts/verify-ci-gate.sh --job-prefix ci --expected-jobs ',ci/rust,' >/dev/null 2>&1; then
    [[ $result == success ]] || exit 1
  else
    [[ $result != success ]] || exit 1
  fi
done
printf '%s\n' 'PASS: native full collector coverage and selected success/skipped/failure/cancel admission'
