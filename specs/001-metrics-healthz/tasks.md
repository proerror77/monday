---
description: "Tasks for Health & Metrics Endpoints"
---

# Tasks: Health & Metrics Endpoints

**Input**: Design documents from `/specs/001-metrics-healthz/`
**Prerequisites**: plan.md (required), spec.md (required for user stories)

**Organization**: Tasks grouped by user story for independent delivery

## Format: `[ID] [P?] [Story] Description`
- **[P]**: Can run in parallel (no cross-file deps)
- **[Story]**: US1/US2/US3

---

## Phase 1: Setup (Shared Infrastructure)

- [ ] T001 [P] Add crate `infra-services/core/metrics` with registry/export skeleton
- [ ] T002 [P] Wire minimal HTTP endpoint helper for apps/* （重用現有 stack 或新增最小 server）
- [ ] T003 Configure feature flags/env for enabling metrics export per app
- [ ] T004 Ensure `cargo fmt/clippy` gates in Makefile cover new paths

---

## Phase 2: Foundational (Blocking Prerequisites)

- [ ] T010 Define metrics families and names in `infra-services/core/metrics/src/families.rs`
- [ ] T011 Implement lock-free counters/histograms aggregation (batch snapshot) in `registry.rs`
- [ ] T012 Implement Prometheus text export in `export.rs`
- [ ] T013 [P] Add health model and probes for engine/risk/network/storage in `infra-services/core/metrics`
- [ ] T014 Add integration test scaffolding in `tests/integration/metrics_endpoints.rs`

**Checkpoint**: Foundation ready – user stories can start

---

## Phase 3: User Story 1 - `/healthz` (P1)

- [ ] T101 [P] [US1] apps/live: expose `/healthz` and wire probes
- [ ] T102 [P] [US1] apps/paper: expose `/healthz`
- [ ] T103 [P] [US1] apps/replay: expose `/healthz`
- [ ] T104 [US1] Integration tests: degrade scenario returns non-200 + reason

---

## Phase 4: User Story 2 - `/metrics` (P1)

- [ ] T201 [P] [US2] apps/live: expose `/metrics` using registry snapshot
- [ ] T202 [P] [US2] apps/paper: expose `/metrics`
- [ ] T203 [P] [US2] apps/replay: expose `/metrics`
- [ ] T204 [US2] Validate families/names/labels match constitution
- [ ] T205 [US2] Integration tests: curl `/metrics` & parse key series

---

## Phase 5: User Story 3 - 零熱路徑負擔 (P2)

- [ ] T301 [US3] Replace hotpath metrics with atomic counters/ring buffers
- [ ] T302 [US3] Batch snapshot exporter with backpressure-safe design
- [ ] T303 [US3] Criterion microbench against mainline baseline (≤2% regression)
- [ ] T304 [US3] Document measurement method in `specs/001-metrics-healthz/quickstart.md`

---

## Phase N: Polish & Cross-Cutting

- [ ] TX01 Update Grafana docs and sample alerts referencing new metrics
- [ ] TX02 Security review: ensure no secrets exposed via `/metrics`
- [ ] TX03 CI: add `cargo audit` step (workspace)
- [ ] TX04 README/ops guide updates

---

## Dependencies & Execution Order

- Phase 1 → Phase 2 → US1/US2 (parallel) → US3 → Polish

