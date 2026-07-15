# ECS 自动部署说明（已退役）

> 历史说明：旧版 `create-ecs-instance.sh` 与
> `deploy-collector-auto.sh` 已从活跃代码树移除。本页不再是可执行的部署
> runbook，也不代表任何 ECS 实例、凭据、网络或远端服务已经验证。

当前 Monday 部署边界以以下文档为准：

- [Rust 构建与发布](../guides/RUST_BUILD_RELEASE.md)
- [生产部署契约](../../deployment/PRODUCTION_DEPLOYMENT.md)

仓库内只支持受治理的 Rust 构建、离线部署制品验证，以及 Paper/Shadow
启动。`LiveSmall` 仍然 fail closed；创建或修改云资源、写入外部 secrets、
部署远端服务和启用真实交易都需要独立审查与外部授权。

从 Monday 仓库根目录可以运行当前的非变更型验证：

```bash
cargo test --manifest-path rust_hft/Cargo.toml -p hft-infra-secrets --test tracked_secrets_contract --locked -- --nocapture
cargo test --manifest-path rust_hft/Cargo.toml -p hft-live --no-default-features --test deployment_artifacts --locked
kubectl apply --dry-run=client -f rust_hft/deployment/k8s/
```

这些命令只验证本地契约和 Kubernetes 对象结构，不创建 ECS 实例、不部署
collector，也不证明远端运行状态。
