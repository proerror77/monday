#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/fixture"
export FIXTURE_ROOT="$scratch/fixture" GITHUB_REPOSITORY=proerror77/monday
export TEST_SOURCE TEST_IMAGE
TEST_SOURCE=$(printf a%.0s {1..40}); TEST_IMAGE=sha256:$(printf b%.0s {1..64})
printf 'small mocked image archive\n' >"$FIXTURE_ROOT/image.tar"
jq -n --arg sha "$TEST_SOURCE" '{workflow_runs:[{id:42,run_attempt:2,head_sha:$sha,head_branch:"main",event:"push",path:".github/workflows/security-enabled.yml",head_repository:{full_name:"proerror77/monday"},status:"completed",conclusion:"success"}]}' >"$FIXTURE_ROOT/runs.json"
jq -n --arg name "tested-image-hft-core-$TEST_SOURCE-2" '{artifacts:[{name:$name,expired:false}]}' >"$FIXTURE_ROOT/artifacts.json"
jq -n --arg sha "$TEST_SOURCE" --arg id "$TEST_IMAGE" --arg hash "$(sha256sum "$FIXTURE_ROOT/image.tar" | awk '{print $1}')" \
  '{schema_version:"monday.tested_image.v2",job_id:7,job_name:"Container Security - Image Scan (hft-core)",platform:"linux/amd64",source_sha:$sha,image:"hft-core",run_id:"42",run_attempt:"2",image_id:$id,archive_sha256:$hash}' >"$FIXTURE_ROOT/image.json"
printf '%s\n' '[{"jobs":[{"id":7,"name":"Container Security - Image Scan (hft-core)","run_id":42,"run_attempt":2,"status":"completed","conclusion":"success"}]}]' >"$FIXTURE_ROOT/jobs.json"
cat >"$scratch/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  api*security-enabled.yml/runs*|api*ci.yml/runs*) cat "$FIXTURE_ROOT/runs.json" ;;
  api*42/attempts/2/jobs*) cat "$FIXTURE_ROOT/jobs.json" ;;
  api*actions/runs/42) jq --argjson attempt "${FINAL_ATTEMPT:-2}" '.workflow_runs[0] | .run_attempt=$attempt' "$FIXTURE_ROOT/runs.json" ;;
  api*42/artifacts*) cat "$FIXTURE_ROOT/artifacts.json" ;;
  'run download 42 '*) destination=${!#}; mkdir -p "$destination"; cp "$FIXTURE_ROOT/image.tar" "$FIXTURE_ROOT/image.json" "$destination/" ;;
  *) exit 91 ;;
esac
MOCK
cat >"$scratch/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'load --input '*) ;;
  'image inspect --format {{.Os}}/{{.Architecture}} '*) printf '%s\n' "${BAD_PLATFORM:-linux/amd64}" ;;
  'image inspect --format {{.Id}} '*) printf '%s\n' "$TEST_IMAGE" ;;
  'image inspect --format '*revision*) printf '%s\n' "${BAD_REVISION:-$TEST_SOURCE}" ;;
  *) exit 92 ;;
esac
MOCK
chmod +x "$scratch/bin/"*
export PATH="$scratch/bin:$PATH"
bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/good"
[[ $(cat "$scratch/good/image-id.txt") == "$TEST_IMAGE" ]]
if BAD_REVISION=wrong bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/wrong-revision"; then exit 1; fi
printf 'corrupt\n' >>"$FIXTURE_ROOT/image.tar"
if bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/corrupt" optional; then exit 1; else [[ $? != 75 ]]; fi
jq '.workflow_runs[0].conclusion="failure"' "$FIXTURE_ROOT/runs.json" >"$scratch/failed"
mv "$scratch/failed" "$FIXTURE_ROOT/runs.json"
if bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/failed-run" optional; then exit 1; else [[ $? != 75 ]]; fi
printf '{"workflow_runs":[]}\n' >"$FIXTURE_ROOT/runs.json"
if bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/absent" optional; then exit 1; else [[ $? == 75 ]]; fi
if bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/required"; then exit 1; fi
# The collector uses its successful Monorepo producer and the same byte seam.
printf 'collector image archive\n' >"$FIXTURE_ROOT/image.tar"
jq -n --arg sha "$TEST_SOURCE" '{workflow_runs:[{id:42,run_attempt:2,head_sha:$sha,head_branch:"main",event:"push",path:".github/workflows/ci.yml",head_repository:{full_name:"proerror77/monday"},status:"completed",conclusion:"success"}]}' >"$FIXTURE_ROOT/runs.json"
jq -n --arg name "tested-image-binance-lob-archiver-$TEST_SOURCE-2" '{artifacts:[{name:$name,expired:false}]}' >"$FIXTURE_ROOT/artifacts.json"
jq -n --arg sha "$TEST_SOURCE" --arg id "$TEST_IMAGE" --arg hash "$(sha256sum "$FIXTURE_ROOT/image.tar" | awk '{print $1}')" \
  '{schema_version:"monday.tested_image.v2",source_sha:$sha,image:"binance-lob-archiver",run_id:"42",run_attempt:"2",job_id:9,job_name:"Production Image And Kubernetes Contract",platform:"linux/amd64",image_id:$id,archive_sha256:$hash}' >"$FIXTURE_ROOT/image.json"
printf '%s\n' '[{"jobs":[{"id":9,"name":"Production Image And Kubernetes Contract","run_id":42,"run_attempt":2,"status":"completed","conclusion":"success"}]}]' >"$FIXTURE_ROOT/jobs.json"
bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/collector" required binance-lob-archiver
[[ $(cat "$scratch/collector/image-id.txt") == "$TEST_IMAGE" ]]
cp "$FIXTURE_ROOT/image.json" "$scratch/clean-manifest"
cp "$FIXTURE_ROOT/runs.json" "$scratch/clean-run"
for bad in source job attempt platform archive fork final-rerun; do
  cp "$scratch/clean-manifest" "$FIXTURE_ROOT/image.json"
  cp "$scratch/clean-run" "$FIXTURE_ROOT/runs.json"
  case "$bad" in
    source) jq '.source_sha="wrong"' "$scratch/clean-manifest" >"$FIXTURE_ROOT/image.json" ;;
    job) jq '.job_id=8' "$scratch/clean-manifest" >"$FIXTURE_ROOT/image.json" ;;
    attempt) jq '.run_attempt="1"' "$scratch/clean-manifest" >"$FIXTURE_ROOT/image.json" ;;
    platform) jq '.platform="linux/arm64"' "$scratch/clean-manifest" >"$FIXTURE_ROOT/image.json" ;;
    archive) jq '.archive_sha256=("a"*64)' "$scratch/clean-manifest" >"$FIXTURE_ROOT/image.json" ;;
    fork) jq '.workflow_runs[0].head_repository.full_name="foreign/repo"' "$scratch/clean-run" >"$FIXTURE_ROOT/runs.json" ;;
    final-rerun) ;;
  esac
  status=0
  FINAL_ATTEMPT=$([[ $bad == final-rerun ]] && echo 3 || echo 2) bash "$root/read-tested-image.sh" "$TEST_SOURCE" "$scratch/reject-$bad" required binance-lob-archiver >"$scratch/reject-$bad.log" 2>&1 || status=$?
  [[ $status != 0 ]] || { echo "invalid collector provenance admitted: $bad" >&2; exit 1; }
done
printf 'PASS: exact-source producer, archive hash and loaded revision are required; automatic publication never falls back to a rebuild\n'
# Exercise the actual automatic/manual branch with an unavailable artifact.
ruby -ryaml - "$root/../workflows/docker-publish.yml" "$scratch/admission-step.sh" <<'RUBY'
workflow=YAML.safe_load(File.read(ARGV[0]))
step=workflow.fetch('jobs').fetch('build-and-push').fetch('steps').find { |s| s['id']=='tested' }
File.write(ARGV[1],step.fetch('run'))
RUBY
mkdir -p "$scratch/control/.github/scripts"
# shellcheck disable=SC2016
printf '#!/usr/bin/env bash\nexit "$ARTIFACT_STATUS"\n' >"$scratch/control/.github/scripts/read-tested-image.sh"
export SOURCE_SHA="$TEST_SOURCE" RUNNER_TEMP="$scratch/runner" GITHUB_OUTPUT="$scratch/step-output"
for event in workflow_run workflow_dispatch push; do
  : >"$GITHUB_OUTPUT"
  status=0
  (cd "$scratch/control"; ARTIFACT_STATUS=75 EVENT_NAME="$event" bash -e "$scratch/admission-step.sh") || status=$?
  if [[ $event == workflow_run ]]; then
    [[ $status == 75 && ! -s $GITHUB_OUTPUT ]]
  else
    [[ $status == 0 ]]
    grep -qx 'build=true' "$GITHUB_OUTPUT"
  fi
done
printf 'PASS: actual automatic publisher refuses missing artifacts; explicit manual/tag paths retain a single build\n'
