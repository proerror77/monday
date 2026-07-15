# Implementation Plan: [FEATURE]

**Branch**: `[###-feature-name]` | **Date**: [DATE] | **Spec**: [link]
**Input**: Feature specification from `/specs/[###-feature-name]/spec.md`

**Note**: This template is filled in by the `/speckit.plan` command. See `.specify/templates/commands/plan.md` for the execution workflow.

**Runtime boundary**: Generated plans MUST use Rust and Cargo only. Do not
introduce a second language runtime, foreign package manager, or foreign test
runner.

## Summary

[Extract from feature spec: primary requirement + technical approach from research]

## Technical Context

<!--
  ACTION REQUIRED: Replace the content in this section with the technical details
  for the project. The structure here is presented in advisory capacity to guide
  the iteration process.
-->

**Language/Version**: [Rust toolchain/MSRV from `rust-toolchain.toml` and workspace policy, or NEEDS CLARIFICATION]
**Primary Dependencies**: [Cargo crates and enabled features, or NEEDS CLARIFICATION]
**Storage**: [if applicable, e.g., PostgreSQL via a Rust crate, append-only files, or N/A]
**Testing**: [`cargo test --locked` plus the narrow package/feature lane, or NEEDS CLARIFICATION]
**Target Platform**: [e.g., Linux server, WASM, supported target triple, or NEEDS CLARIFICATION]
**Project Type**: [Cargo workspace package: library, binary, service, or tool]
**Performance Goals**: [domain-specific, e.g., 1000 req/s, 10k lines/sec, 60 fps or NEEDS CLARIFICATION]  
**Constraints**: [domain-specific, e.g., <200ms p95, <100MB memory, offline-capable or NEEDS CLARIFICATION]  
**Scale/Scope**: [domain-specific, e.g., 10k users, 1M LOC, 50 screens or NEEDS CLARIFICATION]

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

[Gates determined based on constitution file]

## Project Structure

### Documentation (this feature)

```
specs/[###-feature]/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (/speckit.plan command)
├── data-model.md        # Phase 1 output (/speckit.plan command)
├── quickstart.md        # Phase 1 output (/speckit.plan command)
├── contracts/           # Phase 1 output (/speckit.plan command)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created by /speckit.plan)
```

### Source Code (repository root)
<!--
  ACTION REQUIRED: Replace the placeholder tree below with the concrete layout
  for this feature. Delete unused options and expand the chosen structure with
  real paths (e.g., apps/admin, packages/something). The delivered plan must
  not include Option labels.
-->

```
# [REMOVE IF UNUSED] Option 1: Extend an existing Cargo package (DEFAULT)
[package]/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── [domain].rs
│   └── bin/
│       └── [tool].rs
└── tests/
    ├── [contract].rs
    └── [integration].rs

# [REMOVE IF UNUSED] Option 2: Add a Cargo workspace package
[workspace-domain]/[package]/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   └── [modules].rs
├── tests/
│   └── [behavior].rs
└── benches/
    └── [benchmark].rs

Cargo.toml              # Register the package only when a new member is required
Cargo.lock              # Update with locked Cargo resolution
```

**Structure Decision**: [Document the selected structure and reference the real
directories captured above]

## Complexity Tracking

*Fill ONLY if Constitution Check has violations that must be justified*

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| [e.g., new workspace package] | [current need] | [why an existing package is insufficient] |
| [e.g., Repository pattern] | [specific problem] | [why direct DB access insufficient] |
