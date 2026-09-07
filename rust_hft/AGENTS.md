# Rust Workspace

Use the repository-root AGENTS.md for authority, delivery, evidence and validation.
This is the primary Cargo workspace; prediction-markets currently has a nested
workspace with its own instructions and pinned toolchain.

For module ownership and dependency changes, read ARCHITECTURE.md and
../docs/architecture/REPOSITORY_LAYOUT.md. Select packages from Cargo manifests;
use the root validation policy rather than a fixed list of all research crates.
