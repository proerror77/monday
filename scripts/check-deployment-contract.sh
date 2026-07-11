#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

require_text() {
  local file="$1"
  local text="$2"
  if ! rg --fixed-strings --quiet -- "$text" "$file"; then
    printf 'deployment contract missing in %s: %s\n' "$file" "$text" >&2
    exit 1
  fi
}

reject_text() {
  local file="$1"
  local text="$2"
  if rg --fixed-strings --quiet -- "$text" "$file"; then
    printf 'deployment contract forbids in %s: %s\n' "$file" "$text" >&2
    exit 1
  fi
}

for dockerfile in \
  rust_hft/docker/Dockerfile \
  rust_hft/deployment/docker/Dockerfile.trading; do
  require_text "$dockerfile" '-p hft-live'
  require_text "$dockerfile" 'target/release/hft-live'
  require_text "$dockerfile" 'curl'
  require_text "$dockerfile" '/readiness'
  require_text "$dockerfile" 'ENTRYPOINT ["/usr/local/bin/hft-live"]'
  reject_text "$dockerfile" 'hft-collector'
  reject_text "$dockerfile" '|| true'
done

require_text deploy/Dockerfile.hft 'curl'
require_text deploy/Dockerfile.hft 'http://localhost:9090/readiness'
require_text deploy/docker-compose.yml '9090:9090'
reject_text deploy/docker-compose.yml 'command: ["hft-live"'
reject_text deploy/docker-compose.yml 'command: ["hft-paper"'

k8s=rust_hft/deployment/k8s/trading-engine.yaml
require_text "$k8s" 'path: /readiness'
reject_text "$k8s" 'path: /ready'
for flag in \
  --deployment-envelope \
  --deployment-policy \
  --deployment-trusted-keys \
  --deployment-nonce-ledger \
  --deployment-audit-log \
  --deployment-feedback-log; do
  require_text "$k8s" "$flag"
done
for path in \
  /app/deployment/envelope.json \
  /app/deployment/policy.json \
  /app/deployment/trusted-keys.json \
  /app/state/nonces.jsonl \
  /app/state/audit.jsonl \
  /app/state/feedback.jsonl; do
  require_text "$k8s" "$path"
done
require_text "$k8s" 'claimName: runtime-state-pvc'
require_text rust_hft/deployment/k8s/configmaps.yaml 'system.yaml: |'
require_text rust_hft/deployment/k8s/configmaps.yaml 'quotes_only: true'
require_text rust_hft/deployment/k8s/configmaps.yaml 'simulate_execution: true'
reject_text "$k8s" 'BITGET_API_SECRET'
require_text rust_hft/deployment/scripts/deploy.sh 'kubectl apply -f "$K8S_DIR/configmaps.yaml"'
reject_text rust_hft/deployment/scripts/deploy.sh 'envsubst < "$K8S_DIR/configmaps.yaml"'

printf 'deployment contract check passed\n'
