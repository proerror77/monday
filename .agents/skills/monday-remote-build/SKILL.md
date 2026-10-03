---
name: monday-remote-build
description: Select and verify Monday managed ACK CI or a disposable Cloud Assistant build, preserving each path's source, cache, and cleanup contract.
---

# Monday remote build

Use this before remote Rust compilation, validation, toolchain installation, or
source materialization. Select the execution path from the current task; this
skill does not create a new approval, resource budget, or deployment target.

## Choose the execution path

- **Managed ACK CI:** the task already has a reviewed private executor profile,
  committed request, named controller, and signed public-receipt contract. Use
  that executor and its pinned control source. Read the matching private
  executor README and admission/cache scripts; do not substitute a one-off
  Cloud Assistant compiler or an empty temporary cache for the managed recipe.
- **Disposable Cloud Assistant build:** no managed execution is assigned and
  the task calls for a bounded standalone build command. Use the disposable
  task contract below.

For either path, verify the actual worker, source identity, remaining budget,
deadline, and task-owned cleanup before mutation. Keep existing authorization;
an expired technical grant or conflicting writer still blocks its affected work.
Do not infer a healthy worker from a stale Pod Running status.

## Managed ACK CI

Keep the current private executor as the sole computation writer. The public
runner only performs the checks permitted by its workflow; a local diagnostic
or cached artifact is not a signed CI pass.

Reuse admitted registry/Git downloads and immutable toolchain image layers.
Resolve the writable target isolation from the reviewed task contract and
actual cache implementation, rather than inferring it from directory presence
or stale prose. If a task requires per-SHA targets, test/clippy stages for that
SHA may share the target; another SHA may not. Keep compiler-state reuse,
successful-checkpoint reuse, and artifact publication as separate decisions.

Verify the research-worker identity and /work placement, the admitted cache
volume UID and owner, available space, source/command bindings, lease, and
public consumer before dispatch. Use the executor's cleanup and terminal
persistence mechanisms. Retain a separately owned cache volume according to its
contract; do not delete it as if it belonged to a disposable command.

Report execution, terminal receipt, public consumption, and any artifact
readback separately. A private failure or cancellation needs a terminal public
outcome; it is not a reason to launch a fresh unnamed attempt.

The remaining sections apply only to a disposable Cloud Assistant command.

## Disposable inputs

- A lowercase task `contract` containing only letters, digits, dots, underscores,
  or hyphens.
- The reviewed build or validation command and its required durable result.

## Disposable preconditions

1. Resolve the ECS target live. Require `project=monday`, `role=research-worker`,
   `Running`, and a healthy Cloud Assistant heartbeat. Require `workload=backtest`
   either directly or through ACK's `node-template/label/workload` tag.
2. On the target, require `/work` to be a mounted filesystem with at least 20 GiB
   free. Create `/work/monday-builds` mode `0700` if it is absent.
3. Resolve one named controller for the task.

## Disposable stop conditions

Reject `role=ack-system`, a missing `/work`, insufficient free space, an active
conflicting controller, or every attempt to use `/tmp` as a fallback.

## Disposable task contract

Create exactly one task root and keep all mutable build state inside it.

This one-off isolation contract is an exception to general build-cache reuse.
Do not redirect its writable caches or toolchains into shared locations. This
exception does not apply to the managed ACK execution path above.

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

## Disposable output

Report the target instance ID and tags, `/work` free space before and after, the
task root, the build verdict, artifact readback, and `test ! -e "$task_root"`.
Also confirm that no new `/tmp/monday-*` directory was created.
