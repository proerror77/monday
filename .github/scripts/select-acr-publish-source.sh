#!/usr/bin/env bash
set -euo pipefail

event=
automation_state=ready
research_product=all
conclusion=
source_event=
head_branch=
head_sha=
run_id=
binaries_conclusion=
smoke_conclusion=
target=
rebuild=false
source_test_sha=
source_test_profile=
source_test_tag=
current_sha=
current_run_id=
current_ref=
main_sha=
monorepo_conclusion=
prediction_conclusion=
security_conclusion=
published_products=none
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output=${GITHUB_OUTPUT:-/dev/stdout}

while (($#)); do
  case "$1" in
    --product) research_product=$2; shift 2 ;;
    --event) event=$2; shift 2 ;;
    --automation-state) automation_state=${2:-ready}; shift 2 ;;
    --conclusion) conclusion=$2; shift 2 ;;
    --source-event) source_event=$2; shift 2 ;;
    --head-branch) head_branch=$2; shift 2 ;;
    --head-sha) head_sha=$2; shift 2 ;;
    --run-id) run_id=$2; shift 2 ;;
    --binaries-conclusion) binaries_conclusion=$2; shift 2 ;;
    --smoke-conclusion) smoke_conclusion=$2; shift 2 ;;
    --target) target=$2; shift 2 ;;
    --rebuild) rebuild=$2; shift 2 ;;
    --source-test-sha) source_test_sha=$2; shift 2 ;;
    --source-test-profile) source_test_profile=$2; shift 2 ;;
    --current-sha) current_sha=$2; shift 2 ;;
    --current-run-id) current_run_id=$2; shift 2 ;;
    --current-ref) current_ref=$2; shift 2 ;;
    --main-sha) main_sha=$2; shift 2 ;;
    --monorepo-conclusion) monorepo_conclusion=$2; shift 2 ;;
    --prediction-conclusion) prediction_conclusion=$2; shift 2 ;;
    --security-conclusion) security_conclusion=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

require_current_main() {
  local candidate=$1
  [[ $main_sha =~ ^[0-9a-f]{40}$ ]] || {
    printf 'release admission requires the current 40-hex main SHA\n' >&2
    exit 1
  }
  [[ $candidate == "$main_sha" ]] || {
    printf 'release source %s is not current main %s\n' "$candidate" "$main_sha" >&2
    exit 1
  }
}

require_green_main() {
  local candidate=$1 name state
  require_current_main "$candidate"
  for name in monorepo prediction security; do
    case "$name" in
      monorepo) state=$monorepo_conclusion ;;
      prediction) state=$prediction_conclusion ;;
      security) state=$security_conclusion ;;
    esac
    [[ $state == success ]] || {
      printf 'release admission requires %s check success, got %s\n' "$name" "${state:-missing}" >&2
      exit 1
    }
  done
}

research_product=$(bash "$script_dir/research-release-products.sh" normalize "$research_product")
case "$event" in
  workflow_run)
    [[ -z $source_test_sha ]] || {
      printf 'automated ACR publication does not accept a source-test SHA\n' >&2
      exit 1
    }
    [[ $conclusion == success && $source_event == push && $head_branch == main ]] || {
      printf 'automated ACR publication requires a successful main push run\n' >&2
      exit 1
    }
    source_sha=$head_sha
    case "$automation_state" in
      ready) ;;
      deferred|stale|out_of_scope|already_published)
        publish_target=none
        research_mode=none
        ;;
      *) echo 'invalid automated ACR admission state' >&2; exit 1 ;;
    esac
    if [[ $automation_state == ready ]]; then
      case "$binaries_conclusion/$smoke_conclusion" in
        success/success)
          require_green_main "$source_sha"
          publish_target=research-products
          published_products=$research_product
          research_mode=artifact
          ;;
        skipped/skipped) publish_target=none; research_mode=none ;;
        *) printf 'incomplete research image validation: binaries=%s smoke=%s\n' \
             "$binaries_conclusion" "$smoke_conclusion" >&2; exit 1 ;;
      esac
    fi
    artifact_run_id=$run_id
    ;;
  workflow_dispatch)
    [[ $current_ref == refs/heads/main ]] || {
      printf 'manual publication must dispatch the trusted main workflow\n' >&2
      exit 1
    }
    case "$target" in
      polymarket-raw-ops) target=binance-lob-archiver ;;
      all|research-runner|prediction-research-runner|campaign-cycle-controller|hft-trading|binance-lob-archiver|polymarket-evidence-compiler|polymarket-market-recorder|research-source-test) ;;
      *) printf 'unsupported publish target: %s\n' "$target" >&2; exit 1 ;;
    esac
    if [[ $target == research-source-test ]]; then
      [[ $rebuild == false ]] || {
        printf 'source-test publication does not rebuild research-runner binaries\n' >&2
        exit 1
      }
      [[ $source_test_sha =~ ^[0-9a-f]{40}$ ]] || {
        printf 'source-test publication requires an exact 40-hex source SHA\n' >&2
        exit 1
      }
      [[ $source_test_sha == "$current_sha" ]] || {
        printf 'source-test publication requires the current trusted main SHA\n' >&2
        exit 1
      }
      require_current_main "$current_sha"
      case "$source_test_profile" in
        binance-bstocks-attestation|bybit-spot) ;;
        *) printf 'unsupported source-test profile: %s\n' "$source_test_profile" >&2; exit 1 ;;
      esac
      research_mode=none
      source_sha=$source_test_sha
      source_test_tag="source-test-${source_sha}-${source_test_profile}"
    else
      [[ -z $source_test_sha ]] || {
        printf 'source-test SHA is only valid for research-source-test\n' >&2
        exit 1
      }
      if [[ $target == all || $target == research-runner || $target == prediction-research-runner || $target == campaign-cycle-controller ]]; then
        case "$target" in
          all) published_products=$(bash "$script_dir/research-release-products.sh" normalize all) ;;
          research-runner) published_products=cex-runner ;;
          prediction-research-runner) published_products=prediction-runner ;;
          campaign-cycle-controller) published_products=controller ;;
        esac
        if [[ $rebuild == true ]]; then
          research_mode=rebuild
          research_product=$published_products
        else
          [[ $rebuild == false && $automation_state == ready && $binaries_conclusion == success && $smoke_conclusion == success && $run_id =~ ^[1-9][0-9]*$ ]] || {
            printf 'manual research publication needs a verified exact-source binary/smoke artifact; missing or expired artifacts require an explicitly planned producer\n' >&2
            exit 1
          }
          combined=$(bash "$script_dir/research-release-products.sh" merge "$research_product" "$published_products")
          [[ $combined == "$research_product" ]] || {
            echo 'publication requires every selected product in the verified producer artifact' >&2; exit 1;
          }
          research_mode=artifact
          artifact_run_id=$run_id
        fi
      else
        research_mode=none
      fi
      source_sha=$current_sha
      require_green_main "$source_sha"
    fi
    publish_target=$target
    [[ $research_mode == artifact ]] || artifact_run_id=$current_run_id
    ;;
  *) printf 'unsupported event: %s\n' "$event" >&2; exit 1 ;;
esac

[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { printf 'invalid source SHA: %s\n' "$source_sha" >&2; exit 1; }
[[ $artifact_run_id =~ ^[1-9][0-9]*$ ]] || { printf 'invalid artifact run id: %s\n' "$artifact_run_id" >&2; exit 1; }
printf '%s\n' \
  "publish_target=$publish_target" \
  "research_mode=$research_mode" \
  "source_sha=$source_sha" \
  "artifact_run_id=$artifact_run_id" >>"$output"
if [[ $publish_target == research-source-test ]]; then
  printf '%s\n' \
    "source_test_profile=$source_test_profile" \
    "source_test_tag=$source_test_tag" >>"$output"
fi

printf 'research_product=%s\npublished_products=%s\n' "$research_product" "$published_products" >>"$output"
