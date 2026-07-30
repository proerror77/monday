#!/usr/bin/env bash
set -euo pipefail

expected_jobs=',,'
job_prefix=
while (($#)); do
  case "$1" in
    --expected-jobs) expected_jobs=$2; shift 2 ;;
    --job-prefix) job_prefix=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

[[ $expected_jobs == ,*, ]] || { printf 'invalid expected job list: %s\n' "$expected_jobs" >&2; exit 2; }
[[ -z $job_prefix || $job_prefix =~ ^(ci|ploy|security)$ ]] || { printf 'invalid job prefix: %s\n' "$job_prefix" >&2; exit 2; }

needs=$(cat)
expected=${expected_jobs#,}
expected=${expected%,}
failed=$(jq -r '
  to_entries[]
  | select(
      (.value.result != "success" and .value.result != "skipped") or
      (.value.result == "skipped" and (.key == "selector" or .key == "scope" or (.key | endswith("-selector")) or (.key | endswith("-scope"))))
    )
  | "\(.key)=\(.value.result)"
' <<<"$needs")

if [[ -n $expected ]]; then
  IFS=',' read -ra selected_jobs <<<"$expected"
  for selected_job in "${selected_jobs[@]}"; do
    [[ $selected_job =~ ^(ci|ploy|security)/[a-z0-9/-]+$ ]] || {
      printf 'invalid selected job: %s\n' "$selected_job" >&2
      exit 2
    }
    [[ -z $job_prefix || $selected_job == "$job_prefix/"* ]] || continue
    need=${selected_job#*/}
    [[ $selected_job == ci/* ]] && need=${need//-/_}
    result=$(jq -r --arg need "$need" '.[$need].result // "missing"' <<<"$needs")
    [[ $result == success ]] || failed+="${failed:+$'\n'}$need=$result"
  done
fi

if [[ -n $failed ]]; then
  printf 'CI gate rejected:\n%s\n' "$failed" >&2
  exit 1
fi
