#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
ruby -ryaml -e '
  jobs = YAML.load_file(".github/workflows/ci.yml")["jobs"]
  abort "mutable producer checkout" unless jobs.fetch("research_preflight").fetch("steps").first.fetch("uses") == "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
  abort "producer self dependency" unless jobs.fetch("research_preflight").fetch("needs") == "scope"
  abort "missing Rust edge" unless jobs.fetch("rust").fetch("needs").include?("research_preflight")
  abort "missing aggregate edge" unless jobs.fetch("ci-gate").fetch("needs").include?("research_preflight")
  condition = jobs.fetch("rust").fetch("if")
  abort "missing non-ACK bypass" unless condition.include?("needs.scope.outputs.ack_research !=")
  abort "missing successful prerequisite" unless condition.include?("needs.research_preflight.result ==")
'
for prerequisite in success skipped failure cancelled; do
  rust=success
  [[ $prerequisite == failure || $prerequisite == cancelled ]] && rust=skipped
  fixture=$(jq -n --arg pre "$prerequisite" --arg rust "$rust" '{scope:{result:"success"},research_preflight:{result:$pre},rust:{result:$rust}}')
  if printf '%s' "$fixture" | bash .github/scripts/verify-ci-gate.sh --job-prefix ci --expected-jobs ',ci/rust,' >/dev/null 2>&1; then
    [[ $prerequisite == success || $prerequisite == skipped ]] || exit 1
  else
    [[ $prerequisite == failure || $prerequisite == cancelled ]] || exit 1
  fi
done
# A selected full gate cannot be replaced by quick success.
if printf '%s' '{"scope":{"result":"success"},"research_preflight":{"result":"success"},"rust":{"result":"skipped"}}' |
  bash .github/scripts/verify-ci-gate.sh --job-prefix ci --expected-jobs ',ci/rust,' >/dev/null 2>&1; then exit 1; fi
printf '%s\n' 'PASS: workflow prerequisite edges, non-ACK skip, failure/cancel rejection and mandatory full gate'
