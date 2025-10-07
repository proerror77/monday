# Code Style & Conventions
- Rust 2021 edition with workspace-wide dependency management; follow idiomatic Rust patterns and zero-cost abstractions.
- Prefer zero-copy, allocation-aware data handling; `tracing` crate provides structured logging—use span fields instead of string formatting on hot paths.
- Enforce linting via `cargo clippy -- -D warnings`; treat warnings as errors. Keep modules lean and feature-gated appropriately.
- Format code with `cargo fmt`; avoid introducing non-ASCII unless required by existing text.
- Tests rely on `cargo test`; integration tests live under `tests/` and `examples/` directories; strategies rely on trait-based interfaces defined in `crates/ports`.
- Comments used sparingly to clarify complex flows; prefer expressive naming and module structure documented in `ARCHITECTURE.md`.