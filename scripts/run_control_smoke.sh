#!/usr/bin/env bash
set -euo pipefail

DC="deployment/docker-compose.yml"

echo "[1/3] Building control-ws and smoke images..."
docker compose -f "$DC" build control-ws e2e-smoke

echo "[2/3] Starting infra and control-ws..."
docker compose -f "$DC" up -d redis clickhouse rust-hft-engine control-ws

echo "[3/3] Running smoke test..."
docker compose -f "$DC" run --rm e2e-smoke

echo "Done. Check logs with: docker compose -f $DC logs -f control-ws"

