# Phase 4: Best Practices Remediation Action Plan

**Project**: HFT Agent-Driven Trading System
**Created**: 2025-12-13
**Priority**: CRITICAL
**Estimated Timeline**: 12 weeks (3 sprints)

---

## Sprint 1: Critical Stability (Weeks 1-4)

### Week 1: Error Handling Foundation

**Task 1.1: Hot Path Unwrap Elimination**
```bash
Target files:
- rust_hft/market-core/engine/src/lib.rs
- rust_hft/market-core/core/src/types.rs
- rust_hft/market-core/aggregation/

Action:
1. Identify all unwrap() in tick() and run() functions
2. Replace with ? operator or proper error handling
3. Add HftError variants for each failure mode

Acceptance:
- Zero unwrap() in engine hot path
- All parsing returns Result<T, HftError>
- Tests pass with invalid input handling
```

**Task 1.2: Python Version Upgrade**
```bash
Action:
1. Install Python 3.11.x on development machines
2. Update virtual environments
3. Test all scripts for compatibility
4. Update CI/CD to use Python 3.11

Acceptance:
- python3 --version shows 3.11+
- All ml_workspace scripts run successfully
- pyproject.toml requirements met
```

**Task 1.3: Security Dependency Updates**
```bash
cargo update:
- rustls: 0.21.12 → 0.23.35
- redis: 0.24.0 → 1.0.1
- clickhouse: 0.11.6 → 0.14.1

Action:
1. Update Cargo.toml workspace dependencies
2. Fix breaking changes (if any)
3. Run cargo audit --fix
4. Test integration with all adapters

Acceptance:
- Zero high/critical vulnerabilities
- All tests pass
- WebSocket/Redis/ClickHouse connections work
```

### Week 2: Logging and Observability

**Task 2.1: Remove Debug Prints**
```bash
Target: 161 println!/dbg! statements

Action:
1. Replace println! → tracing::info!/debug!
2. Replace dbg! → tracing::trace!
3. Add module-level log filtering
4. Configure RUST_LOG for different environments

Pattern:
// Before
println!("Processing order {}", id);

// After
tracing::debug!(order_id = %id, "Processing order");

Acceptance:
- Zero println!/dbg! in market-core/apps
- Tests still output to console
- Production logs use JSON format
```

**Task 2.2: Log Level Configuration**
```bash
Create: config/logging.yaml

dev:
  default: debug
  hft_engine: trace
  hft_execution: debug

prod:
  default: info
  hft_engine: info
  hft_execution: warn

Acceptance:
- Configurable log levels per module
- Hot path uses trace! level
- Production defaults to info
```

### Week 3-4: Code Quality Automation

**Task 3.1: Pre-commit Hooks**
```bash
File: .pre-commit-config.yaml

repos:
  - repo: https://github.com/psf/black
    hooks: [black]
  - repo: https://github.com/astral-sh/ruff-pre-commit
    hooks: [ruff, ruff-format]
  - repo: local
    hooks:
      - id: cargo-clippy
        name: Clippy
        entry: cargo clippy
        args: [--all-targets, --, -D, warnings]

Acceptance:
- Hooks run on every commit
- Formatting auto-applied
- Commits rejected on lint errors
```

**Task 3.2: CI Quality Gates**
```bash
File: .github/workflows/quality.yml

Checks:
1. Rust: clippy, cargo fmt --check
2. Python: mypy, ruff, bandit
3. Hot path audit: no unwrap() in engine
4. Dependency audit: cargo audit

Acceptance:
- All checks run on PR
- Failures block merge
- Status badges on README
```

---

## Sprint 2: Performance Optimization (Weeks 5-8)

### Week 5: HashMap Standardization

**Task 5.1: FxHashMap Migration**
```bash
Files to update:
- rust_hft/market-core/engine/src/lib.rs (line 28)
- rust_hft/strategy-framework/strategies/arbitrage/src/lib.rs
- All HashMap<String, T> → FxHashMap<InstrumentId, T>

Pattern:
// Before
use std::collections::HashMap;
let map: HashMap<String, Data> = HashMap::new();

// After
use rustc_hash::FxHashMap;
let map: FxHashMap<InstrumentId, Data> = FxHashMap::default();

Acceptance:
- Zero std::HashMap in hot paths
- All venue/symbol maps use newtype keys
- Benchmark shows 10-20ns improvement
```

**Task 5.2: Parking Lot Adoption**
```bash
Action:
1. Replace std::sync::Mutex → parking_lot::Mutex
2. Audit RwLock usage (prefer ArcSwap for reads)
3. Document lock-free snapshot strategy

Acceptance:
- Consistent parking_lot usage
- Lock contention reduced in profiling
```

### Week 6: Type Safety Enhancement

**Task 6.1: Python Type Hints**
```bash
Target: 12 files without type hints

Action:
1. Add full type hints to all functions
2. Enable mypy strict mode
3. Fix all type errors

Pattern:
# Before
def process(data):
    return data.mean()

# After
def process(data: pd.DataFrame) -> float:
    return float(data.mean())

Acceptance:
- 100% type hint coverage
- mypy --strict passes
- No Any types in public APIs
```

**Task 6.2: Configuration Validation**
```bash
File: ml_workspace/utils/config.py

Action:
1. Replace os.getenv() → pydantic-settings
2. Add validation for all secrets
3. Consolidate .env files

Pattern:
from pydantic_settings import BaseSettings

class ClickHouseSettings(BaseSettings):
    host: str
    password: str  # Validated, required

Acceptance:
- Startup fails on missing config
- Type-safe config access
- Single .env file
```

### Week 7-8: Dependency Modernization

**Task 7.1: Rust Crate Updates**
```bash
Non-breaking updates:
- simd-json: 0.13 → 0.17 (performance)
- tokio-tungstenite: 0.20 → 0.28 (latency)
- metrics-exporter-prometheus: 0.17 → 0.18

Breaking updates (Phase 2):
- axum: 0.7 → 0.8 (HTTP changes)
- thiserror: 1.0 → 2.0 (macro changes)

Acceptance:
- All crates <6 months old
- Benchmark shows improvements
```

**Task 7.2: Python Package Updates**
```bash
Updates:
- numpy: 1.24 → 2.0 (major changes)
- transformers: 4.21 → 4.47 (security)
- clickhouse-connect: 0.7 → 0.8

Action:
1. Update requirements.txt
2. Add upper bounds (>=X,<Y)
3. Generate requirements.lock

Acceptance:
- No breaking changes
- Security vulnerabilities patched
```

---

## Sprint 3: Testing and Documentation (Weeks 9-12)

### Week 9-10: Test Coverage Expansion

**Task 9.1: Property-Based Tests**
```bash
Add: proptest for numeric types

File: rust_hft/market-core/core/tests/proptest.rs

use proptest::prelude::*;

proptest! {
    #[test]
    fn price_roundtrip(value in 0.0..1_000_000.0) {
        let price = Price::from_f64(value)?;
        let back = price.as_f64();
        assert!((value - back).abs() < 1e-6);
    }
}

Acceptance:
- 1000+ property tests pass
- Edge cases discovered and fixed
```

**Task 9.2: Benchmark Regression Tests**
```bash
File: rust_hft/benches/regression.yml

Benchmarks:
- tick_latency_p99: <25us
- aggregation_latency: <5us
- strategy_latency: <10us

Action:
1. Run criterion benchmarks in CI
2. Fail if >10% regression
3. Track trends over time

Acceptance:
- Automated performance tracking
- Historical data visualization
```

### Week 11: Async I/O Migration

**Task 11.1: Async ClickHouse Client**
```bash
File: ml_workspace/utils/async_clickhouse.py

from clickhouse_connect import get_async_client

async def load_training_data(
    start: datetime,
    end: datetime
) -> pd.DataFrame:
    async with get_async_client() as client:
        result = await client.query(...)
    return result

Acceptance:
- Data loading 2-3x faster
- Concurrent symbol processing
```

### Week 12: Documentation and Cleanup

**Task 12.1: API Documentation**
```bash
Rust:
- Add /// doc comments to all public APIs
- Generate rustdoc in CI
- Publish to GitHub Pages

Python:
- Google-style docstrings everywhere
- Generate Sphinx docs
- Add usage examples

Acceptance:
- 100% public API documented
- Examples compile/run
```

**Task 12.2: Technical Debt Tracking**
```bash
Action:
1. Convert 49 TODO comments → GitHub issues
2. Tag with priority/effort labels
3. Create debt reduction roadmap

Format:
// TODO(#123): Replace with async implementation
//   Priority: Medium, Effort: 8h, Target: Q1 2026

Acceptance:
- All TODOs tracked in issues
- 10% reduction target set
```

---

## Continuous Monitoring

### Metrics to Track

**Code Quality**:
- Unwrap count: Target 0 in hot paths
- Test coverage: 5.5% → 40% → 80%
- Type hint coverage: 79% → 100%
- Dependency age: <6 months

**Performance**:
- tick() latency p99: Target <100us (Phase 1)
- Aggregation latency: Target <5us
- End-to-end latency: Target <25us (final)

**Security**:
- cargo audit: Zero high/critical
- bandit: Zero high severity
- Dependency CVEs: Zero

---

## Success Criteria

### Sprint 1 Complete:
- ✅ Zero unwrap() in engine hot path
- ✅ Python 3.11+ in use
- ✅ Zero high/critical vulnerabilities
- ✅ Zero println! in production code
- ✅ Pre-commit hooks active

### Sprint 2 Complete:
- ✅ FxHashMap standardized
- ✅ 100% Python type hints
- ✅ All dependencies <6 months old
- ✅ pydantic-settings config

### Sprint 3 Complete:
- ✅ 40%+ test coverage
- ✅ Property-based tests active
- ✅ Async I/O in data loading
- ✅ 100% API documentation
- ✅ Technical debt tracked

---

## Risk Mitigation

**Risk 1: Breaking Changes in Dependencies**
- Mitigation: Test in staging first, have rollback plan
- Contingency: Pin to working versions, defer major updates

**Risk 2: Performance Regression**
- Mitigation: Benchmark before/after each change
- Contingency: Revert commits causing >5% regression

**Risk 3: Python 3.11 Compatibility**
- Mitigation: Test all scripts in isolated environment
- Contingency: Virtual environment per Python version

---

## Weekly Status Template

```markdown
## Week X Status

### Completed:
- [ ] Task X.Y: Description
- [ ] Task X.Z: Description

### In Progress:
- [ ] Task X.A: Description (60% complete)

### Blocked:
- [ ] Task X.B: Waiting for dependency update

### Metrics:
- Unwrap count: X → Y (Z% reduction)
- Test coverage: X% → Y%
- Latency p99: Xus → Yus

### Next Week:
- Focus: ...
- Risks: ...
```

---

## Resources Required

**Time**: 12 weeks (3 engineers × 40h/week = 480 hours total)

**Budget**:
- CI/CD compute: $100/month
- Dependency licenses: $0 (all open source)
- Training: 8 hours (Rust async patterns, Python 3.11)

**Tools**:
- cargo-audit, clippy, rustfmt
- mypy, ruff, black, bandit
- pre-commit framework
- GitHub Actions (free tier)

---

**Action Plan Owner**: Engineering Lead
**Review Cadence**: Weekly sprint reviews
**Escalation Path**: Architecture review for breaking changes
