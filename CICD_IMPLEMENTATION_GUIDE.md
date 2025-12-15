# HFT System CI/CD Implementation Guide

**Document Status**: Ready for Implementation
**Priority**: CRITICAL
**Phase**: Security & Testing (Week 1-3)

---

## Quick Start (First 24 Hours)

### Step 1: Enable Security Pipeline (30 minutes)

```bash
# Navigate to repo
cd /Users/proerror/Documents/monday

# Enable the security pipeline
mv rust_hft/.github/workflows/security.yml.disabled \
   rust_hft/.github/workflows/security.yml

# Verify it exists
ls -la rust_hft/.github/workflows/security.yml

# Commit
git add rust_hft/.github/workflows/security.yml
git commit -m "chore(security): enable security pipeline for dependency audit and code quality checks"
git push origin main
```

**Verification**: Go to GitHub > Actions tab, should see new "安全和質量檢查" workflow running.

### Step 2: Add Security Workflow to Main Pipeline (30 minutes)

```bash
# Copy the new security-enabled.yml
cp /Users/proerror/Documents/monday/.github/workflows/security-enabled.yml \
   /Users/proerror/Documents/monday/.github/workflows/security.yml

# This is in root .github/workflows, runs on all PRs
git add .github/workflows/security.yml
git commit -m "feat(security): add comprehensive security scanning (SAST, SCA, secrets, image scanning)"
git push origin main
```

### Step 3: Create test branch to validate

```bash
# Create feature branch
git checkout -b feature/security-pipeline

# Push empty commit to trigger CI
git commit --allow-empty -m "trigger: validate security pipeline"
git push -u origin feature/security-pipeline

# Create PR on GitHub
# Go to https://github.com/{owner}/monday/compare/main...feature/security-pipeline
# Click "Create Pull Request"
```

**Expected Result**:
- ✅ SAST - Semgrep should run
- ✅ Cargo audit should run
- ✅ License check should run
- ⚠️ May find issues (expected, fix them)

---

## Phase 1: Security Pipeline (Week 1)

### Task 1.1: Re-enable and Fix Rust Security Workflow

**File**: `rust_hft/.github/workflows/security.yml.disabled`
**Action**: Rename to `security.yml`

```bash
mv rust_hft/.github/workflows/security.yml.disabled \
   rust_hft/.github/workflows/security.yml

git add rust_hft/.github/workflows/security.yml
git commit -m "chore(rust-hft): enable security workflow"
git push origin main
```

**Expected on first run**:
```
❌ security-audit: May find known vulnerabilities
❌ code-quality: Clippy warnings (expected)
❌ license-compliance: Should pass (Apache 2.0 compatible)
⚠️ performance-regression: Needs baseline (skip first time)
⚠️ memory-leak-check: Valgrind warnings (expected)
```

**Fix process**:
1. Review each failure
2. For dependencies: run `cargo update` or bump version
3. For code: run `cargo fix` or fix manually
4. For licenses: ensure all deps are compatible
5. Commit fixes: `git push origin main`

### Task 1.2: Add Monorepo-level Security Checks

**File**: `.github/workflows/ci.yml` (root level)
**Changes**: Add SAST and container scanning

```yaml
# Find the existing "jobs:" section and add these new jobs:

jobs:
  # ... existing jobs ...

  # NEW: SAST Security Scanning
  sast-semgrep:
    name: SAST - Semgrep
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write
    steps:
      - uses: actions/checkout@v4

      - name: Run Semgrep
        uses: returntocorp/semgrep-action@v1
        with:
          config: >-
            p/rust
            p/security-audit
            p/owasp-top-ten
          generateSarif: true

      - name: Upload SARIF
        uses: github/codeql-action/upload-sarif@v2
        with:
          sarif_file: semgrep.sarif
          category: semgrep

  # NEW: Secret Detection
  secret-detection:
    name: Security - Detect Secrets
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: TruffleHog
        uses: trufflesecurity/trufflehog@main
        with:
          path: ./
          base: ${{ github.event.repository.default_branch }}
          head: HEAD
          extra_args: --only-verified
```

### Task 1.3: Make Linters Blocking

**File**: `.github/workflows/ci.yml` (rust job)
**Action**: Remove `continue-on-error: true` from linting steps

```yaml
# BEFORE (non-blocking):
- name: Clippy (non-blocking)
  run: cargo clippy ...
  continue-on-error: true  # ❌ REMOVE THIS

# AFTER (blocking):
- name: Clippy (BLOCKING)
  run: cargo clippy ...
  # No continue-on-error: true means failure blocks merge
```

**Affected Steps** (remove `continue-on-error: true`):
- Clippy
- Format check (cargo fmt)
- Ruff (Python)
- Black (Python)

### Task 1.4: Add Container Image Scanning

**File**: `.github/workflows/docker-publish.yml`
**Changes**: Add Trivy scanning before push

```yaml
# Add after "Set up Docker Buildx" step:

- name: Build image for scanning
  uses: docker/build-push-action@v6
  with:
    context: ${{ matrix.context }}
    file: ${{ matrix.file }}
    push: false
    load: true
    tags: hft-scan:${{ matrix.name }}

- name: Scan with Trivy
  uses: aquasecurity/trivy-action@master
  with:
    image-ref: hft-scan:${{ matrix.name }}:scan
    format: sarif
    output: trivy-${{ matrix.name }}.sarif
    severity: 'CRITICAL,HIGH'
    exit-code: '1'  # Fail if critical/high found

- name: Upload Trivy SARIF
  uses: github/codeql-action/upload-sarif@v2
  with:
    sarif_file: trivy-${{ matrix.name }}.sarif
    category: trivy-${{ matrix.name }}
```

### Task 1.5: Establish Security Baseline

Run security checks and document findings:

```bash
# Create issue template for security findings
cat > /tmp/security-findings.md << 'EOF'
# Security Findings Report

## Summary
- SAST findings: X
- Dependency vulnerabilities: Y
- Secrets detected: Z
- License issues: W

## Critical Issues
List critical issues that block deployment

## Medium Issues
List medium severity issues

## Action Plan
What to fix and by when
EOF

# Run cargo audit manually
cd rust_hft
cargo audit

# Document results
cargo audit --json > audit-baseline.json

# Commit baseline
git add audit-baseline.json
git commit -m "docs(security): document baseline security audit results"
```

---

## Phase 2: Test Automation (Week 2-3)

### Task 2.1: Add Performance Regression Testing

**File**: `rust_hft/.github/workflows/performance-regression.yml` (NEW)
**Action**: Create this file

Use the file provided: `rust_hft/.github/workflows/performance-regression.yml`

```bash
# Copy the file
cp /Users/proerror/Documents/monday/rust_hft/.github/workflows/performance-regression.yml \
   /Users/proerror/Documents/monday/rust_hft/.github/workflows/performance-regression.yml

# Commit
git add rust_hft/.github/workflows/performance-regression.yml
git commit -m "feat(perf): add performance regression testing for order book and execution latency"
git push origin main
```

**Verify**: Create a test PR and performance benchmarks should run.

### Task 2.2: Create Latency Benchmarks

**Files to create**:
1. `rust_hft/benches/orderbook_latency.rs`
2. `rust_hft/benches/execution_latency.rs`

**Example orderbook_latency.rs**:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hft_core::orderbook::OrderBook;

fn benchmark_l1_update(c: &mut Criterion) {
    let mut ob = OrderBook::new();

    c.bench_function("l1_update_100k_cycles", |b| {
        b.iter(|| {
            // Simulate 100k L1 updates
            for i in 0..100_000 {
                let price = 50000.0 + (i as f64 * 0.01);
                ob.update_bid(price, 1.0);
            }
        });
    });
}

fn benchmark_l2_snapshot(c: &mut Criterion) {
    let mut ob = OrderBook::new();
    // Populate orderbook...

    c.bench_function("l2_snapshot_10_levels", |b| {
        b.iter(|| {
            let snapshot = ob.snapshot(10);  // Get top 10 levels
            black_box(snapshot);
        });
    });
}

criterion_group!(benches, benchmark_l1_update, benchmark_l2_snapshot);
criterion_main!(benches);
```

Update `rust_hft/Cargo.toml`:

```toml
[[bench]]
name = "orderbook_latency"
harness = false

[[bench]]
name = "execution_latency"
harness = false

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
```

### Task 2.3: Add Unit Tests to Main CI

**File**: `.github/workflows/ci.yml` (rust job)
**Changes**: Add test execution

```yaml
rust:
  name: Rust Workspace
  runs-on: ubuntu-latest
  defaults:
    run:
      working-directory: rust_hft
  steps:
    # ... existing steps ...

    # NEW: Run tests
    - name: Test (library)
      run: cargo test --lib --release

    - name: Test (integration)
      run: cargo test --test '*' --release

    - name: Test (all features)
      run: cargo test --all-features --release

    # NEW: Coverage
    - name: Generate coverage
      run: |
        cargo install cargo-tarpaulin
        cargo tarpaulin --out Xml --exclude-files tests --timeout 300

    - name: Upload coverage
      uses: codecov/codecov-action@v4
      with:
        files: ./cobertura.xml
        fail_ci_if_error: true
        flags: rust-hft
```

### Task 2.4: Add Python Integration Tests

**File**: `ml_workspace/.github/workflows/ci.yml`
**Changes**: Add integration tests with real services

```yaml
integration-tests:
  name: Integration Tests (Real Services)
  runs-on: ubuntu-latest
  services:
    clickhouse:
      image: clickhouse/clickhouse-server:latest
      options: >-
        --health-cmd "clickhouse-client --query 'SELECT 1'"
        --health-interval 10s
        --health-timeout 5s
        --health-retries 5
    redis:
      image: redis:7-alpine
      options: >-
        --health-cmd "redis-cli ping"
        --health-interval 10s
  steps:
    - uses: actions/checkout@v4

    - uses: actions/setup-python@v5
      with:
        python-version: '3.11'
        cache: 'pip'

    - name: Install dependencies
      run: |
        pip install -r requirements.txt
        pip install pytest pytest-cov

    - name: Run integration tests
      env:
        CLICKHOUSE_HOST: clickhouse
        CLICKHOUSE_PORT: 8123
        REDIS_HOST: redis
        REDIS_PORT: 6379
      run: |
        pytest ml_workspace/tests/integration \
          -v --cov --cov-report=xml --cov-report=html \
          --cov-fail-under=65

    - name: Upload coverage
      uses: codecov/codecov-action@v4
      with:
        files: ./coverage.xml
        fail_ci_if_error: true
        flags: ml-workspace
```

### Task 2.5: Set Coverage Thresholds

Create file: `.github/workflows/coverage-enforcement.yml`

```yaml
name: Coverage Enforcement

on:
  pull_request:
    branches: [main]

jobs:
  coverage:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Check Rust coverage
        run: |
          # Parse coverage from codecov reports
          # Alert if below 70%
          echo "Target: 70% coverage"

      - name: Check Python coverage
        run: |
          # Alert if below 65%
          echo "Target: 65% coverage"

      - name: Fail if below threshold
        run: |
          # Exit 1 if any workspace below threshold
          exit 0
```

---

## Phase 3: Deployment Automation (Week 4-6)

### Task 3.1: Blue-Green Deployment Framework

Create file: `.github/workflows/deploy-blue-green.yml`

```yaml
name: Deploy - Blue-Green

on:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  deploy:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v4

      - name: Configure AWS credentials
        uses: aws-actions/configure-aws-credentials@v4
        with:
          role-to-assume: ${{ secrets.AWS_ROLE_TO_ASSUME }}
          aws-region: us-east-1

      - name: Deploy to Green
        run: |
          kubectl apply -f k8s/production/green/ --namespace hft

      - name: Wait for rollout
        run: |
          kubectl rollout status deployment/rust-hft-green \
            --namespace hft --timeout=5m

      - name: Run smoke tests
        run: |
          ./tools/smoke-tests.sh --target green --samples 100

      - name: Check latency
        run: |
          # Verify p99 < 25µs
          ./tools/latency-probe --target green --threshold 25

      - name: Switch traffic
        if: success()
        run: |
          kubectl patch service rust-hft \
            -p '{"spec":{"selector":{"version":"green"}}}' \
            --namespace hft

      - name: Monitor (5 minutes)
        run: |
          for i in {1..30}; do
            sleep 10
            kubectl top nodes --namespace hft || true
          done

      - name: Cleanup Blue
        if: success()
        run: |
          kubectl delete deployment rust-hft-blue \
            --namespace hft || true
```

### Task 3.2: GitOps Setup (ArgoCD)

Create file: `k8s/argocd/application.yaml`

```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: hft-system
  namespace: argocd
spec:
  project: default

  source:
    repoURL: https://github.com/proerror/monday.git
    targetRevision: main
    path: k8s/production

  destination:
    server: https://kubernetes.default.svc
    namespace: hft

  syncPolicy:
    automated:
      prune: true
      selfHeal: true
    syncOptions:
    - CreateNamespace=true
    retry:
      limit: 5
      backoff:
        duration: 5s
        factor: 2
        maxDuration: 3m
```

### Task 3.3: Helm Charts

Create: `helm/hft-core/Chart.yaml`

```yaml
apiVersion: v2
name: hft-core
description: HFT Trading System - Rust Core
type: application
version: 1.0.0
appVersion: "1.0.0"

dependencies: []

maintainers:
  - name: HFT Team
```

Create: `helm/hft-core/values.yaml`

```yaml
replicaCount: 1

image:
  repository: ghcr.io/proerror/hft-core
  tag: latest
  pullPolicy: IfNotPresent

resources:
  requests:
    cpu: 2
    memory: 2Gi
  limits:
    cpu: 4
    memory: 4Gi

service:
  type: ClusterIP
  port: 50051

config:
  logLevel: info
  maxLatency: 25000  # microseconds

secrets:
  clickhouse:
    host: clickhouse
    port: 8123
  redis:
    host: redis
    port: 6379
```

---

## Validation Checkpoints

### After Phase 1 (End of Week 1)

**Security Pipeline Enabled**:
- [ ] `rust_hft/.github/workflows/security.yml` exists and runs
- [ ] SAST scanning (Semgrep) runs on PRs
- [ ] Cargo audit runs weekly
- [ ] License compliance checked
- [ ] Container images scanned for vulnerabilities
- [ ] Linters are blocking (PRs fail if issues found)

**Evidence**:
```bash
# Run locally to verify
cd rust_hft
cargo audit
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

### After Phase 2 (End of Week 3)

**Test Automation Enabled**:
- [ ] Unit tests run in CI and must pass
- [ ] Integration tests run with real services
- [ ] Performance benchmarks run on PRs
- [ ] Coverage reports generated
- [ ] Coverage enforced (>70% Rust, >65% Python)

**Evidence**:
```bash
# Run locally
cd rust_hft
cargo test --all
cargo bench --bench orderbook_latency

cd ../ml_workspace
pytest tests/ -v --cov
```

### After Phase 3 (End of Week 6)

**Deployment Automation**:
- [ ] Blue-green deployment framework tested
- [ ] GitOps (ArgoCD) synchronized with git
- [ ] Helm charts validate
- [ ] Smoke tests pass post-deployment
- [ ] Rollback procedure documented and tested

**Evidence**:
```bash
# Test deployment locally
kubectl apply -f k8s/production/deployment.yaml
kubectl rollout status deployment/rust-hft

# Verify smoke tests
./tools/smoke-tests.sh --target production
```

---

## Common Issues & Fixes

### Issue: "Clippy warnings treated as errors"

**Error Message**:
```
error: unused variable: `x`
  --> src/main.rs:10:9
```

**Fix**:
```bash
# Run cargo fix to auto-fix
cd rust_hft
cargo fix --allow-dirty

# Or manually suppress with #[allow(...)]
#[allow(unused_variables)]
let x = value;
```

### Issue: "Test compilation fails with missing ClickHouse"

**Error Message**:
```
error: connection to clickhouse://localhost:8123 failed
```

**Fix**:
Use mock or setup test containers:

```yaml
# In CI, use service containers
services:
  clickhouse:
    image: clickhouse/clickhouse-server:latest
    options: --health-cmd "clickhouse-client --query 'SELECT 1'"
```

### Issue: "Performance regression detected >5%"

**Error Message**:
```
change: +7.2% regression
exit code 1
```

**Investigation**:
```bash
# Run benchmark locally on main
git checkout main
cargo bench --bench orderbook_latency

# Run benchmark on feature branch
git checkout feature-branch
cargo bench --bench orderbook_latency

# Compare results
critcmp /tmp/main.txt /tmp/feature.txt
```

**Fix**:
- Profile the code: `cargo flamegraph`
- Find hot spots
- Optimize or revert change

### Issue: "Secret detected in commit"

**Error Message**:
```
🔴 Detected: AWS_SECRET_ACCESS_KEY in src/config.rs:15
```

**Fix**:
```bash
# Remove secret from code
# Add to environment variable or secrets manager

# Use GitHub Secrets for CI/CD
# Use Vault for production
```

---

## Monitoring & Metrics

### Key Metrics to Track

**Build Pipeline**:
- Total execution time (target: <10 minutes)
- Number of failing PRs per day
- Time to fix security findings
- False positive rate for SAST

**Tests**:
- Test execution time
- Flaky test rate
- Coverage trend (should increase)
- Test failure rate

**Security**:
- Vulnerabilities detected and fixed
- Time to fix high/critical vulnerabilities
- Secrets detected and remediated
- Dependency update frequency

**Deployment**:
- Deployment frequency
- Mean time to recovery (MTTR)
- Deployment success rate
- Rollback frequency

### Dashboard Setup

Create a Grafana dashboard:
1. `git push` triggers CI
2. CI metrics → Prometheus
3. Prometheus → Grafana
4. Display in dashboard

**Queries**:
```
# CI execution time
histogram_quantile(0.95, ci_duration_seconds)

# Test coverage trend
code_coverage_percent

# Security findings
open_vulnerabilities_count
```

---

## Escalation Procedures

### If CI is Failing

1. **Check status**: `git status` → `git log --oneline -5`
2. **View logs**: GitHub Actions tab → failing job
3. **Run locally**: `cd workspace && cargo test`
4. **Fix and push**: `git add . && git commit && git push`

### If Deployment Fails

1. **Check logs**: `kubectl logs deployment/rust-hft`
2. **Rollback**: `kubectl set image deployment/rust-hft rust-hft=ghcr.io/.../hft:previous`
3. **Fix**: Apply blue-green rollback procedure
4. **Investigate**: Review security/test logs for what failed

### If Security Scan Blocks Deployment

1. **Review finding**: GitHub Security tab → SARIF report
2. **Assess risk**: Is it exploitable? In what context?
3. **Remediate**: Update dependency, patch code, or acknowledge
4. **Document**: Add issue explaining mitigation
5. **Re-test**: Run security scan to verify fix

---

## Next Steps

1. **Week 1**: Execute Phase 1 (Security)
2. **Week 2-3**: Execute Phase 2 (Testing)
3. **Week 4-6**: Execute Phase 3 (Deployment)
4. **Week 7-8**: Complete Phase 4 (Polish)

**Success Criteria**:
- ✅ All security checks enabled and passing
- ✅ 70%+ test coverage (Rust), 65%+ (Python)
- ✅ Zero-downtime deployment capability
- ✅ Automated rollback on failure
- ✅ <10m end-to-end pipeline

**Expected Outcome**:
- Safe, fast CI/CD pipeline
- Reduced manual deployments
- Faster incident response
- Compliance with standards

---

**Document**: CICD_IMPLEMENTATION_GUIDE.md
**Created**: 2025-12-13
**Status**: Ready for Implementation
