#!/usr/bin/env bash
# Static artifact definitions; Cargo impact flags come from the shared selector.
set -euo pipefail
live=${1:?} paper=${2:?} collector=${3:?} event=${4:?}
[[ $live,$paper,$collector =~ ^(true|false),(true|false),(true|false)$ ]] || exit 2
core=$live trading=$live deploy_live=$live deploy_paper=$paper deploy_collector=$collector
if [[ $event == workflow_dispatch ]]; then
  core=true trading=true deploy_live=true deploy_paper=true deploy_collector=true
fi
while IFS= read -r path; do
  case "$path" in
    rust_hft/docker/Dockerfile) core=true ;;
    rust_hft/deployment/docker/Dockerfile.trading) trading=true ;;
    deploy/Dockerfile.hft|.dockerignore) deploy_live=true deploy_paper=true deploy_collector=true ;;
    rust_hft/.dockerignore) core=true trading=true ;;
  esac
done
jq -cn --argjson core "$core" --argjson trading "$trading" --argjson live "$deploy_live" \
  --argjson paper "$deploy_paper" --argjson collector "$deploy_collector" '
  {include:[
    {name:"hft-core",context:"rust_hft",dockerfile:"rust_hft/docker/Dockerfile",build_args:"",selected:$core},
    {name:"hft-trading",context:"rust_hft",dockerfile:"rust_hft/deployment/docker/Dockerfile.trading",build_args:"",selected:$trading},
    {name:"deploy-live",context:".",dockerfile:"deploy/Dockerfile.hft",build_args:"TARGET=live",selected:$live},
    {name:"deploy-paper",context:".",dockerfile:"deploy/Dockerfile.hft",build_args:"TARGET=paper",selected:$paper},
    {name:"deploy-collector",context:".",dockerfile:"deploy/Dockerfile.hft",build_args:"TARGET=collector",selected:$collector}
  ]|map(select(.selected)|del(.selected))}'
