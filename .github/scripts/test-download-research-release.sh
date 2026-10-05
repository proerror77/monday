#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export MOCK_WORK=$work GITHUB_REPOSITORY=fixture/monday GITHUB_RUN_ATTEMPT=2 MONDAY_RELEASE_JOB_ID=567
source_sha=1111111111111111111111111111111111111111
mkdir -p "$work/release/research-bin" "$work/bin"
while IFS= read -r binary; do
  printf 'fixture %s\n' "$binary" >"$work/release/research-bin/$binary"
  chmod 0755 "$work/release/research-bin/$binary"
done < <(bash "$root/.github/scripts/research-release-products.sh" binaries all)
export MONDAY_BUILD_INPUTS_FILE="$work/build-inputs.json"
locks=$("$root/.github/scripts/research-workspace-locks.sh" "$root/rust_hft")
jq -n --arg h "$(printf a%.0s {1..64})" --argjson locks "$locks" '{schema:"monday.compilation-inputs.v2",target:"x86_64-unknown-linux-gnu",profile:"release",compiler:$h,native:$h,flags:$h,profiles:$h,recipe:$h,locks:$locks}' >"$MONDAY_BUILD_INPUTS_FILE"
"$root/.github/scripts/research-image-release-artifact.sh" create "$work/release" "$source_sha" 1234 "$root/rust_hft"
ruby "$root/.github/scripts/research-release-bundle.rb" pack "$work/research-image-release.tar" "$work/release"
(cd "$work" && zip -q "$work/release.zip" research-image-release.tar)
cp "$work/release.zip" "$work/clean.zip"
cat >"$work/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
endpoint=${!#}
mode=${MOCK_MODE:-ok}
sha=1111111111111111111111111111111111111111
case "$endpoint" in
  */actions/runs/1234)
    count=0; if [[ -f $MOCK_WORK/count ]]; then count=$(cat "$MOCK_WORK/count"); fi
    printf '%s\n' "$((count+1))" >"$MOCK_WORK/count"
    attempt=2; if [[ $mode == rerun && $count -gt 0 ]]; then attempt=3; fi
    path=.github/workflows/ploy-ci.yml; if [[ $mode == wrong-workflow ]]; then path=.github/workflows/ci.yml; fi
    if [[ $mode == wrong-source ]]; then sha=2222222222222222222222222222222222222222; fi
    jq -n --arg sha "$sha" --arg path "$path" --argjson attempt "$attempt" '{id:1234,head_sha:$sha,head_branch:"main",head_repository:{full_name:"fixture/monday"},path:$path,event:"push",status:"completed",conclusion:"success",run_attempt:$attempt}' ;;
  */attempts/2/jobs\?*)
    conclusion=success; if [[ $mode == failed-job ]]; then conclusion=failure; fi
    jq -n --arg conclusion "$conclusion" '[{jobs:[{id:567,name:"Research image binaries",run_id:1234,run_attempt:2,status:"completed",conclusion:$conclusion}]}]' ;;
  */runs/1234/artifacts\?*)
    expired=false; if [[ $mode == expired ]]; then expired=true; fi
    jq -n --arg sha "$sha" --argjson expired "$expired" '[{artifacts:[{id:987,name:("research-image-release-"+$sha+"-cex-runner,controller,prediction-runner"),expired:$expired,workflow_run:{id:1234,head_sha:$sha},size_in_bytes:4096}]}]' ;;
  */artifacts/987/zip) cat "$MOCK_WORK/release.zip" ;;
  */jobs/567) printf '%s\n' '{run_id:1234,run_attempt:2,status:"completed",conclusion:"success"}' | jq -R 'fromjson' ;;
  *) echo 'unexpected mock API endpoint' >&2; exit 1 ;;
esac
SH
# The final read uses JSON, never trusted strings or a receipt branch.
sed -i.bak 's/{run_id:1234,run_attempt:2,status:"completed",conclusion:"success"}/{"run_id":1234,"run_attempt":2,"status":"completed","conclusion":"success"}/' "$work/bin/gh"
chmod 0755 "$work/bin/gh"
export PATH="$work/bin:$PATH"
"$root/.github/scripts/download-research-release.sh" 1234 "$source_sha" "$work/verified"
cmp "$work/release/research-bin/hft-backtest" "$work/verified/research-bin/hft-backtest"
for mode in wrong-workflow wrong-source failed-job expired rerun extra digest attempt; do
  export MOCK_MODE=$mode
  rm -f "$work/count"
  cp "$work/clean.zip" "$work/release.zip"
  case "$mode" in
    extra) (cd "$work/release" && touch unexpected && zip -q "$work/release.zip" unexpected); rm "$work/release/unexpected" ;;
    digest) cp "$work/release/research-bin/alpha-harness" "$work/original"; printf 'tampered\n' >>"$work/release/research-bin/alpha-harness"; rm "$work/research-image-release.tar"; ruby "$root/.github/scripts/research-release-bundle.rb" pack "$work/research-image-release.tar" "$work/release"; (cd "$work" && zip -q "$work/release.zip" research-image-release.tar); cp "$work/original" "$work/release/research-bin/alpha-harness" ;;
    attempt) cp "$work/release/research-image-release.json" "$work/original.json"; jq '.workflow_run_attempt=1' "$work/original.json" >"$work/release/research-image-release.json"; rm "$work/research-image-release.tar"; ruby "$root/.github/scripts/research-release-bundle.rb" pack "$work/research-image-release.tar" "$work/release"; (cd "$work" && zip -q "$work/release.zip" research-image-release.tar); cp "$work/original.json" "$work/release/research-image-release.json" ;;
  esac
  if "$root/.github/scripts/download-research-release.sh" 1234 "$source_sha" "$work/rejected-$mode" >"$work/$mode.log" 2>&1; then
    echo "invalid software provenance accepted: $mode" >&2; exit 1
  fi
done
printf 'cross-run software bytes, attempt, job and producer readback contracts passed\n'
