# Rust Workspace

Use the repository-root AGENTS.md for authority, delivery, evidence and validation.
This directory contains six independent Cargo workspaces, registered in
`workspaces.json`. Each workspace owns its lockfile. Source packages declare one
owner; the root package belongs to `runtime/Cargo.toml`.

Select packages with `scripts/cargo-scoped.sh` or an explicit owning manifest.
Keep feature matrices within one owner. The control workspace must not depend
on acquisition, training, or columnar data conversion. Preserve cross-domain
contract tests and exact source/attempt/release evidence.

For module ownership and dependency changes, read ARCHITECTURE.md and
../docs/architecture/REPOSITORY_LAYOUT.md. Select packages from Cargo manifests;
use the root validation policy rather than a fixed list of all research crates.

ACK CEX Campaign Jobs follow
[ACK research accelerator](../docs/research/ACK_RESEARCH_ACCELERATOR.md).
The admitted trainer is Burn `ndarray` on CPU; do not open a GPU node pool or
request `nvidia.com/gpu` unless `admit_research_accelerator` returns `cuda_gpu`.
