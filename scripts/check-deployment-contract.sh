#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

require_text() {
  local file="$1"
  local text="$2"
  if ! grep -Fq -- "$text" "$file"; then
    printf 'deployment contract missing in %s: %s\n' "$file" "$text" >&2
    exit 1
  fi
}

reject_text() {
  local file="$1"
  local text="$2"
  if grep -Fq -- "$text" "$file"; then
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
  require_text "$dockerfile" 'protobuf-compiler'
  require_text "$dockerfile" '--mount=type=cache,target=/usr/local/cargo/registry'
  require_text "$dockerfile" '/readiness'
  require_text "$dockerfile" 'ENTRYPOINT ["/usr/local/bin/hft-live"]'
  require_text "$dockerfile" 'EXPOSE 9090 9092'
  require_text "$dockerfile" 'USER hft'
  require_text "$dockerfile" 'clickhouse,redis,grpc'
  reject_text "$dockerfile" 'hft-collector'
  reject_text "$dockerfile" 'EXPOSE 9090 9091 9092'
  reject_text "$dockerfile" '|| true'
done

require_text deploy/Dockerfile.hft 'curl'
require_text deploy/Dockerfile.hft 'http://localhost:9090/readiness'
require_text deploy/Dockerfile.hft 'EXPOSE 9090 9092'
require_text deploy/Dockerfile.hft 'USER hft'
reject_text deploy/Dockerfile.hft 'EXPOSE 9090 9091 9092'
require_text deploy/docker-compose.yml '19090:9090'
require_text deploy/prometheus/prometheus.yml 'trader:9090'
reject_text deploy/prometheus/prometheus.yml 'trader:8080'
reject_text deploy/docker-compose.yml 'command: ["hft-live"'
reject_text deploy/docker-compose.yml 'command: ["hft-paper"'
for compose_file in deploy/docker-compose.yml rust_hft/deployment/docker/docker-compose.yml; do
  require_text "$compose_file" '--strategy-bundle'
  require_text "$compose_file" '--deployment-feedback-signing-key'
  require_text "$compose_file" '--deployment-feedback-key-id'
  require_text "$compose_file" '/run/secrets/hft/feedback-signing-key.hex'
done

k8s=rust_hft/deployment/k8s/trading-engine.yaml
require_text "$k8s" 'path: /readiness'
reject_text "$k8s" 'path: /ready'
for flag in \
  --deployment-envelope \
  --strategy-bundle \
  --deployment-policy \
  --deployment-trusted-keys \
  --deployment-nonce-ledger \
  --deployment-audit-log \
  --deployment-feedback-log \
  --deployment-feedback-signing-key \
  --deployment-feedback-key-id; do
  require_text "$k8s" "$flag"
done
for path in \
  /app/deployment/envelope.json \
  /app/deployment/bundle.json \
  /app/deployment/policy.json \
  /app/deployment/trusted-keys.json \
  /app/state/nonces.jsonl \
  /app/state/audit.jsonl \
  /app/state/feedback.jsonl \
  /app/secrets/feedback-signing-key.hex; do
  require_text "$k8s" "$path"
done
require_text "$k8s" 'claimName: runtime-state-pvc'
require_text "$k8s" 'prometheus.io/port: "9090"'
require_text "$k8s" 'HFT_GRPC_AUTH_TOKEN'
require_text "$k8s" 'key: grpc-auth-token'
require_text "$k8s" 'key: feedback-signing-key-hex'
reject_text "$k8s" 'containerPort: 9091'
require_text rust_hft/deployment/k8s/configmaps.yaml 'system.yaml: |'
require_text rust_hft/deployment/k8s/configmaps.yaml 'quotes_only: true'
require_text rust_hft/deployment/k8s/configmaps.yaml 'simulate_execution: true'
reject_text "$k8s" 'BITGET_API_SECRET'
require_text rust_hft/deployment/scripts/deploy.sh 'kubectl apply -f "$K8S_DIR/configmaps.yaml"'
require_text rust_hft/deployment/scripts/deploy.sh 'HFT_K8S_DEPLOYMENT_ENVELOPE_FILE'
require_text rust_hft/deployment/scripts/deploy.sh 'HFT_K8S_DEPLOYMENT_AUTHORITY_FILE'
require_text rust_hft/deployment/scripts/deploy.sh 'require_configmap_key alpha-deployment-envelope envelope.json'
require_text rust_hft/deployment/scripts/deploy.sh 'require_configmap_key alpha-deployment-envelope bundle.json'
require_text rust_hft/deployment/scripts/deploy.sh 'require_secret_key hft-secrets grpc-auth-token'
require_text rust_hft/deployment/scripts/deploy.sh 'require_secret_key hft-secrets feedback-signing-key-hex'
reject_text rust_hft/deployment/scripts/deploy.sh 'envsubst < "$K8S_DIR/configmaps.yaml"'

check_unique_host_ports() {
  local compose_file="$1"
  local duplicates
  duplicates="$({
    GRAFANA_ADMIN_PASSWORD=CHANGE_ME_CONTRACT_ONLY \
      HFT_GRPC_AUTH_TOKEN=CHANGE_ME_CONTRACT_ONLY_32_CHARS \
      HFT_DEPLOYMENT_DIR=/tmp/hft-deployment \
      HFT_RUNTIME_STATE_DIR=/tmp/hft-state \
      docker compose -f "$compose_file" config --format json
  } | jq -r '
    [.services[].ports[]? | select(.published != null) |
      ((.host_ip // "") + ":" + (.published | tostring))]
    | group_by(.)[] | select(length > 1) | .[0]
  ' )"
  if [[ -n "$duplicates" ]]; then
    printf 'duplicate published host ports in %s:\n%s\n' "$compose_file" "$duplicates" >&2
    exit 1
  fi
}

check_unique_host_ports deploy/docker-compose.yml
check_unique_host_ports rust_hft/deployment/docker/docker-compose.yml

printf 'deployment contract check passed\n'
