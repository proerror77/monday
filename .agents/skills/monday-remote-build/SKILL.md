---
name: monday-remote-build
description: Select scoped Monday Rust validation or an explicitly assigned remote build, following the current CI workflow and reusing verified Build artifacts. Does not dispatch research, create workers, or grant resources.
---

# Monday Remote Build

Choose the build path for the requested package and source. This skill supplies
routing, not a new executor, budget, deployment target, or mandatory audit.

## Choose the path

- **PR/CI validation:** follow the current workflow and its affected-package
  selection. Native CI performs the checks it declares. Historical private ACK
  receipts are not an alternate completion path for those checks.
- **Focused local validation:** use the package's owning manifest and an existing
  task-owned cache. Do not default to the old root workspace or all packages.
- **Explicit remote build:** use the executor/profile already assigned by the
  task. Read only its matching source, command, cache and cleanup contract.
  A legacy ACK profile or disposable Cloud Assistant build is an explicit
  exception, not a fallback when CI or a worker is unavailable.
- **Scientific compute:** consume the admitted, verified binary/image. A compute
  Job does not cold-build Rust. Coding Agent sessions and their workspaces are
  separate from scientific compute and its lifecycle.

See [workspace and Build contracts](../../../docs/architecture/RESEARCH_FOUNDATION.md#独立-cargo-构建边界).
Follow [Rust instructions](../../../rust_hft/AGENTS.md) and the owning manifest;
[workspaces.json](../../../rust_hft/workspaces.json) is the owner index. Inspect
existing manifests for command selection. Run Cargo metadata only when a graph
change or an unresolved owner actually requires it.

From the repository root, a focused command has this shape:

```bash
cargo test --manifest-path rust_hft/<owning-manifest> --locked -p <package> <filter>
cargo clippy --manifest-path rust_hft/<owning-manifest> --locked -p <package> --all-targets -- -D warnings
```

Use the existing scoped script for explicit package sets. Its dry-run still
reads Cargo metadata; it is not a pure static preview.

## Remote execution, when assigned

Before mutation, resolve the exact source/command, worker and controller,
technical admission, original deadline and remaining task budget. Unknown
placement or a conflicting writer stops the affected command. A stale Pod
Running status is not worker-health evidence. Do not create or renew resources
to complete a build-selection request.

Reuse the profile's admitted downloads, toolchain layers and writable cache
isolation. Cache hits do not prove build success or artifact identity. Keep
workspaces, toolchains and target directories off `ack-system` nodes.

For an explicitly assigned disposable Cloud Assistant task, use its reviewed
research-worker identity, mounted `/work` capacity and isolated task root.
Arm task-owned exit cleanup before writes; persist required results before
cleanup. Fresh task-local caches are that profile's exception. Never delete
a separately owned managed cache or use `/tmp` as a missing-mount fallback.

## Results and stops

Bind checks and artifacts to the exact source and command. Report execution,
required terminal evidence and requested artifact readback separately. CI job
timeouts do not become research grant deadlines. Reuse the same pending
operation after interruption; do not dispatch an unnamed replacement.

Missing or expired admission, unavailable execution, unverified Build artifacts
or ownership conflict remains a concrete blocker. Do not replace it with a local
pass, preparation receipt or speculative cloud deployment.
