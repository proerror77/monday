# Research Core

Rust-first shared contracts for the bounded Loop Engineer research plane.

This tree owns durable schemas and deterministic validation for:

- manifests
- factor DSL

Search proposals, evaluation state, and promotion decisions live in `alpha-harness`.

It does not own hot-path execution, exchange adapters, order routing, runtime authority, or LLM orchestration.
