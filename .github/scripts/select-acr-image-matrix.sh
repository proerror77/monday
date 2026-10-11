#!/usr/bin/env bash
set -euo pipefail
: "${TARGET:?}" "${GITHUB_OUTPUT:?}"
PUBLISHED_PRODUCTS=${PUBLISHED_PRODUCTS:-none}
matrix=$(jq -cn --arg target "$TARGET" --arg products "$PUBLISHED_PRODUCTS" '
  [{repository:"research-runner",product:"cex-runner",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.research",target:"prebuilt",research_artifact:true},
   {repository:"prediction-research-runner",product:"prediction-runner",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.prediction-research",target:"prebuilt",research_artifact:true},
   {repository:"campaign-cycle-controller",product:"controller",context:".",file:"deployment/aliyun/research/Dockerfile.campaign-cycle-controller",target:"prebuilt",research_artifact:true},
   {repository:"hft-trading",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.trading",target:"runtime",research_artifact:false},
   {repository:"binance-lob-archiver",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.binance-lob-archiver",target:"runtime",research_artifact:false},
   {repository:"polymarket-evidence-compiler",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.polymarket-evidence-compiler",target:"runtime",research_artifact:false},
   {repository:"polymarket-market-recorder",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.polymarket-market-recorder",target:"runtime",research_artifact:false}]
  | {include: map(select(
      $target == "all" or .repository == $target
      or ($target == "research-products" and .research_artifact and (.product as $product | $products | split(",") | index($product)))
    ))}')
echo "matrix=$matrix" >> "$GITHUB_OUTPUT"
ordinary=$(jq -c '{include:[.include[] | select(.research_artifact == false)]}' <<< "$matrix")
products=$(jq -r '[.include[] | select(.research_artifact == true) | .product] | unique | sort | join(",")' <<< "$matrix")
echo "ordinary_matrix=$ordinary" >> "$GITHUB_OUTPUT"
research_images=$(jq -c '{include:[.include[] | select(.research_artifact == true)]}' <<< "$matrix")
echo "research_image_matrix=$research_images" >> "$GITHUB_OUTPUT"
echo "research_products=$products" >> "$GITHUB_OUTPUT"
