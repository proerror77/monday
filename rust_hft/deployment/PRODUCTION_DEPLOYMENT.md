# Production Deployment Contract

This directory deploys the deterministic Rust runtime. It does not deploy an LLM into the order path and does not enable live-small activation.

## Supported Artifacts

- `docker/Dockerfile.trading`: release `hft-live` image with Formula strategy, Bitget, Polymarket, metrics, ClickHouse, Redis, and gRPC support.
- `k8s/trading-engine.yaml`: signed envelope, bundle, runtime policy, trusted keys, runtime feedback signer, durable nonce/audit/feedback state, readiness, and gRPC auth wiring.
- `k8s/configmaps.yaml`: canonical quotes-only, simulated-execution startup configuration.
- `k8s/deployment-authority.yaml.example`: schema-only runtime policy/public-key template; populate it outside Git.
- `scripts/deploy.sh`: applies only external secret and deployment-authority manifests supplied by the operator.

Market-data and sentinel manifests remain separate runtime services. The research LoopRun is invoked separately and cannot mutate these manifests or runtime secrets.

The Kubernetes manifests are not the Monday live-host target. The future Tokyo
bare-ECS image path is defined by
[`../../deployment/aliyun/TRADING_ECS_HOST.md`](../../deployment/aliyun/TRADING_ECS_HOST.md).
That path is digest-only, static/boot-disabled, and currently permits signed
Paper or Shadow activation only.

## Build

From `rust_hft/`:

```bash
docker build \
  -t hft-trading:<immutable-tag> \
  -f deployment/docker/Dockerfile.trading \
  .
```

The builder uses Rust 1.91, `--release --locked`, and the production `clickhouse,redis,grpc,polymarket` feature graph. BuildKit caches Cargo registry and target artifacts, but the final binary is copied out of the cache before the runtime stage. The runtime image runs as the unprivileged `hft` user and contains only the binary, CA certificates, health-check client, and required runtime libraries.

## Required External Inputs

Do not create populated copies in the repository.

- `HFT_K8S_SECRETS_FILE`: Kubernetes Secret manifest containing runtime infrastructure credentials, gRPC credentials, and a dedicated 32-byte Ed25519 feedback signing key encoded as 64 hex characters.
- `HFT_K8S_DEPLOYMENT_ENVELOPE_FILE`: ConfigMap containing `envelope.json` and the exact `bundle.json`.
- `HFT_K8S_DEPLOYMENT_AUTHORITY_FILE`: ConfigMap containing runtime-owned `policy.json` and `trusted-keys.json`.
- `ECR_REGISTRY` and immutable `IMAGE_TAG`.

The deployment-envelope signing private key is not a runtime input. The separate runtime feedback signing private key is mounted read-only from `hft-secrets`; its public key belongs in the research plane's `runtime-feedback-trusted-keys.json`. The runtime authority ConfigMap contains only deployment public keys plus approval evidence.

`policy.json` must include an `approvals` array. Every envelope approval id must resolve to an active record with the exact approval class, promotion id, and normalized deployment scope hash. Unknown, expired, revoked, or mismatched evidence is rejected even when the envelope signature is valid.

Create the runtime feedback key outside Git with restrictive permissions, then derive and register its public key for research ingestion:

```bash
umask 077
openssl rand -hex 32 > /secure/feedback-signing-key.hex
cargo run -p hft-harnessctl -- feedback-public-key \
  --signing-key /secure/feedback-signing-key.hex \
  --key-id runtime-feedback-1 \
  > /secure/runtime-feedback-trusted-keys.json
```

The mounted secret key is referenced by `--deployment-feedback-signing-key`; `--deployment-feedback-key-id` must match the key id in the research plane's trusted-key map. Feedback keys and deployment-envelope keys must be different.

## Offline Validation

From the repository root:

```bash
cargo test --manifest-path rust_hft/Cargo.toml -p hft-infra-secrets --test tracked_secrets_contract --locked -- --nocapture
cargo test --manifest-path rust_hft/Cargo.toml -p hft-live --no-default-features --test deployment_artifacts --locked
kubectl apply --dry-run=client -f rust_hft/deployment/k8s/
```

The dry-run validates Kubernetes objects without contacting a trading venue. It does not prove that external secrets, accounts, exchange permissions, DNS, storage classes, or node labels are correct.

## Deployment

After external inputs are provisioned and reviewed:

```bash
HFT_K8S_SECRETS_FILE=/secure/hft-secrets.yaml \
HFT_K8S_DEPLOYMENT_ENVELOPE_FILE=/secure/alpha-deployment-envelope.yaml \
HFT_K8S_DEPLOYMENT_AUTHORITY_FILE=/secure/runtime-deployment-authority.yaml \
ECR_REGISTRY=<registry> \
IMAGE_TAG=<immutable-tag> \
deployment/scripts/deploy.sh deploy
```

The script verifies required Secret/ConfigMap keys before deploying `hft-live`. Runtime state uses a persistent volume so nonce replay and audit evidence survive restarts.

## Allowed Modes

- **Paper**: signed Formula bundle, paper venue execution.
- **Shadow**: signed Formula bundle, simulated Paper fills with Shadow-scoped attribution; no real venue order is sent.
- **LiveSmall**: disabled by the runtime even when the research loop records eligibility.

## External Release Gates

The repository cannot complete these actions locally:

1. Rotate every credential previously exposed in Git history.
2. Decide and execute a coordinated public-history rewrite and hosting/cache purge.
3. Provision least-privilege venue/testnet credentials with withdrawals disabled.
4. Run real-venue reconciliation, disconnect/recovery, reduce-only exit, order-size, slippage, and shadow-soak acceptance tests.
5. Record a scoped, expiring human approval only after those tests pass.

Until all five are complete, production means research plus Paper/Shadow operation, not real-money autonomy.
