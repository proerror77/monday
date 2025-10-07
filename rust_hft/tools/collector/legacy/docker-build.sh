#!/usr/bin/env bash
set -euo pipefail

IMAGE_NAME=${IMAGE_NAME:-hft-collector:latest}

docker build -t "$IMAGE_NAME" -f apps/collector/Dockerfile .
echo "Built image: $IMAGE_NAME"

