---
name: monday-remote-build
description: Run a disposable remote Rust build or validation without using an ACK system node or its system disk.
---

# Monday remote build

Use this for a Cloud Assistant command that compiles Rust, installs a toolchain,
or materializes a source tree remotely.

## Inputs

- A lowercase task `contract` containing only letters, digits, dots, underscores,
  or hyphens.
- The reviewed build or validation command and its required durable result.

## Preconditions

1. Resolve the ECS target live. Require `project=monday`, `role=research-worker`,
   `Running`, and a healthy Cloud Assistant heartbeat. Require `workload=backtest`
   either directly or through ACK's `node-template/label/workload` tag.
2. On the target, require `/work` to be a mounted filesystem with at least 20 GiB
   free. Create `/work/monday-builds` mode `0700` if it is absent.
3. Resolve one named controller for the task.

## Stop conditions

Reject `role=ack-system`, a missing `/work`, insufficient free space, an active
conflicting controller, or every attempt to use `/tmp` as a fallback.

## Task contract

Create exactly one task root and keep all mutable build state inside it.

This disposable isolation contract is an exception to general build-cache reuse.
Do not redirect writable caches or toolchains into shared locations to satisfy
that general preference.

```bash
task_root=$(mktemp -d "/work/monday-builds/${contract}.XXXXXX")
cleanup() { rm -rf -- "$task_root"; }
trap cleanup EXIT
export TMPDIR="$task_root/tmp"
export CARGO_HOME="$task_root/cargo"
export RUSTUP_HOME="$task_root/rustup"
export CARGO_TARGET_DIR="$task_root/target"
export SCCACHE_DIR="$task_root/sccache"
mkdir -p "$TMPDIR" "$CARGO_HOME" "$RUSTUP_HOME" "$CARGO_TARGET_DIR" "$SCCACHE_DIR"
```

Upload or read back every required result before the command exits. A task root
is never a retention surface: any evidence that must survive belongs in a
reviewed durable location before the command exits.

## Output

Report the target instance ID and tags, `/work` free space before and after, the
task root, the build verdict, artifact readback, and `test ! -e "$task_root"`.
Also confirm that no new `/tmp/monday-*` directory was created.
