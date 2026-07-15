# DevOps Quick Reference Card

**One-page guide to Ploy DevOps setup**

---

## Current Status

| Component | Status | Priority |
|-----------|--------|----------|
| Build | ✓ Working | - |
| Testing | ✓ Good | - |
| CI/CD | ⚠ Basic | Fix now |
| Deployment | ✗ Manual | Fix now |
| Rollback | ✗ Manual | Fix now |
| Monitoring | ✗ None | Fix soon |
| Security | ⚠ Minimal | Fix soon |
| HA/Failover | ✗ None | Fix later |

---

## Critical Commands

### Build & Test
```bash
cargo build --release              # Build for deployment
cargo test --features rl           # Run all tests
cargo fmt --all -- --check         # Check formatting
cargo clippy --all-targets -- -D warnings  # Check linting
cargo audit                        # Check security
```

### Deploy
```bash
./scripts/deploy.sh <host>         # Deploy to EC2
./scripts/health-check.sh          # Verify health
./scripts/rollback.sh <host>       # Rollback (manual)
./scripts/validate-config.sh       # Check configuration
```

### Systemd
```bash
sudo systemctl start ploy          # Start service
sudo systemctl stop ploy           # Stop service
sudo systemctl restart ploy        # Restart
sudo systemctl status ploy         # Check status
sudo journalctl -u ploy -f         # View logs (live)
```

### Docker
```bash
docker build -t ploy:latest .      # Build image
docker run -it ploy:latest shell   # Run container
docker push <registry>/ploy:latest # Push to registry
```

---

## GitHub Actions Workflows

| Workflow | Trigger | Purpose |
|----------|---------|---------|
| **test.yml** | PR, push | Build & test code |
| **deploy.yml** | Push to main | Deploy to EC2 |
| **deploy-aws-jp.yml** | Manual (dispatch) | Deploy to Japan region |

### Disable/Enable Workflows
```bash
# Disable workflow (via GitHub UI)
Settings → Actions → Disable/Enable workflow

# Check workflow status
gh workflow list
gh run list
```

---

## Configuration Files

| File | Purpose | Status |
|------|---------|--------|
| `Cargo.toml` | Dependencies, build settings | ✓ Good |
| `.rustfmt.toml` | Formatting rules | ✗ Missing |
| `clippy.toml` | Linting rules | ✗ Missing |
| `.pre-commit-config.yaml` | Local git hooks | ✗ Missing |
| `Dockerfile` | Container image | ✓ Good |
| `.github/workflows/*` | CI/CD pipelines | ⚠ Needs improvements |
| `config/default.toml` | Bot configuration | ✓ Good |
| `config/production.example.toml` | Production template | ✓ Good |
| `deployment/ploy.service` | Systemd service | ✓ Good (gitignored) |

---

## Environment Variables

**Required for trading**:
```bash
POLYMARKET_PRIVATE_KEY    # Wallet private key (0x...)
POLYMARKET_API_KEY        # API credentials
POLYMARKET_SECRET         # API secret
DATABASE_URL              # PostgreSQL connection
```

**Optional**:
```bash
GROK_API_KEY              # Grok AI integration
THE_ODDS_API_KEY          # Sports odds data
RUST_LOG                  # Logging level (default: info)
PLOY_JSON_LOGS            # Enable JSON logging (true/false)
```

**GitHub Actions secrets**:
```
EC2_HOST                  # Production server IP/hostname
EC2_SSH_KEY               # SSH private key for EC2
AWS_ACCESS_KEY_ID         # AWS credentials
AWS_SECRET_ACCESS_KEY     # AWS credentials
AWS_EC2_PRIVATE_KEY       # EC2 SSH key (duplicate)
AWS_EC2_HOST              # EC2 hostname (duplicate)
POLYMARKET_PRIVATE_KEY    # Wallet key (duplicate)
```

---

## Deployment Checklist

### Pre-Deployment
- [ ] All tests passing: `cargo test --features rl`
- [ ] No clippy warnings: `cargo clippy -- -D warnings`
- [ ] Code formatted: `cargo fmt --all -- --check`
- [ ] No vulnerabilities: `cargo audit`
- [ ] Code reviewed and approved
- [ ] Staging tested (if available)

### During Deployment
- [ ] Monitor GitHub Actions workflow
- [ ] Watch build logs for errors
- [ ] Check artifact creation
- [ ] Monitor SSH deployment steps
- [ ] Watch service startup

### Post-Deployment
- [ ] Health check passes: `curl http://localhost:8080/health`
- [ ] Logs show normal startup
- [ ] Service is running: `systemctl status ploy`
- [ ] No recent errors: `journalctl -u ploy -n 50`
- [ ] Database connection works
- [ ] API connection works
- [ ] Monitor for 15 minutes

---

## Troubleshooting

### Service won't start
```bash
sudo journalctl -u ploy -n 50 --no-pager    # View errors
echo $POLYMARKET_PRIVATE_KEY                # Check secrets set
psql $DATABASE_URL -c "SELECT 1;"           # Check DB
sudo systemctl restart ploy                 # Try restart
```

### Health check failing
```bash
curl -v http://localhost:8080/health        # Get full response
sudo systemctl restart ploy                 # Restart
sleep 10
curl http://localhost:8080/health           # Retry
```

### Deployment failed
```bash
./scripts/rollback.sh <ec2-host>            # Rollback
git log --oneline -5                        # Check recent commits
./scripts/health-check.sh                   # Verify old version
```

### Database issues
```bash
psql $DATABASE_URL -c "SELECT NOW();"       # Test connection
psql $DATABASE_URL -c "\\dt"                # List tables
sudo systemctl restart postgresql           # Restart DB
```

### High CPU/Memory
```bash
top                                         # View processes
ps aux | grep ploy                          # Check ploy process
docker stats                                # If using Docker
```

---

## Performance Benchmarks

| Operation | Target | Current |
|-----------|--------|---------|
| Test suite | < 3 min | ~ 5 min |
| Build release | < 5 min | ~ 5 min |
| Docker build | < 10 min | ~ 8 min |
| Deployment | < 10 min | ~ 5 min |
| Health check | < 30 sec | ~ 15 sec |
| Order placement | < 1 sec | Unknown |
| Order fill time | < 5 sec | Unknown |

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                                                     │
│  GitHub Actions (CI/CD)                            │
│  ├─ test.yml: Run tests, build release             │
│  ├─ deploy.yml: Deploy to production               │
│  └─ deploy-aws-jp.yml: Deploy to Japan region      │
│                                                     │
└────────────────┬────────────────────────────────────┘
                 │
                 ▼
        ┌────────────────┐
        │  AWS ECR       │
        │  (Container    │
        │  Registry)     │
        └────────┬───────┘
                 │
                 ▼
    ┌────────────────────────────┐
    │  AWS EC2 Instance(s)       │
    │  ├─ Systemd service        │
    │  ├─ PostgreSQL             │
    │  ├─ Health endpoint        │
    │  └─ Logging                │
    └────────────────────────────┘
                 │
                 ▼
    ┌────────────────────────────┐
    │  Polymarket CLOB           │
    │  ├─ REST API               │
    │  └─ WebSocket              │
    └────────────────────────────┘
```

---

## Security Checklist

- [ ] Private keys not in source code
- [ ] Secrets stored in GitHub Actions only
- [ ] SSH keys use passphrases
- [ ] Database uses strong passwords
- [ ] TLS/HTTPS for all external calls
- [ ] No hardcoded credentials in logs
- [ ] No debug mode in production
- [ ] All dependencies audited
- [ ] Container runs as non-root user

---

## Monitoring Dashboard

If CloudWatch configured:
- Order placement success rate
- Order fill latency (p50, p95, p99)
- API error rate
- Service uptime
- CPU/Memory usage
- Database query latency
- Disk usage
- Network I/O

If Prometheus configured:
```
curl http://localhost:9090/metrics
```

---

## Maintenance Windows

**Schedule**: Weekends 2-4 AM UTC (low trading volume)

**Procedure**:
1. Announce in team chat
2. Enable dry-run mode or stop trading
3. Stop ploy service
4. Apply updates (patches, dependencies)
5. Test locally
6. Restart service
7. Verify health
8. Resume trading

**Emergency**: Can stop service anytime without notice

---

## Team Responsibilities

| Role | Tasks |
|------|-------|
| **DevOps Lead** | Maintain CI/CD, monitor deployments, oncall |
| **Backend Dev** | Code changes, testing, PR reviews |
| **DBA** | Database maintenance, backups, migrations |
| **Platform Eng** | Infrastructure, monitoring, HA setup |

---

## Escalation Contacts

```
Service Down          → On-call DevOps engineer
Trading Issues        → Trading bot developer
Database Issues       → DBA or database engineer
Security Issue        → Security team
Infrastructure        → Platform engineering
```

---

## Documentation Links

- **Full Assessment**: `DEVOPS_ASSESSMENT.md`
- **Implementation Guide**: `DEVOPS_IMPLEMENTATION_GUIDE.md`
- **Deployment Guide**: `DEPLOYMENT.md` (to be created)
- **Operational Runbooks**: `RUNBOOKS.md` (to be created)
- **Source Code**: `/Users/proerror/Documents/ploy/`

---

## Key Metrics to Track

```bash
# Deployment success rate
# (Successful deployments / Total deployments) × 100

# Mean time to recovery (MTTR)
# Average time from issue detection to resolution

# Change failure rate
# (Failed deployments / Total deployments) × 100

# Lead time for changes
# Time from code commit to production

# Deployment frequency
# Number of successful deployments per week
```

---

## Resources

- **GitHub Actions Docs**: https://docs.github.com/en/actions
- **Rust Book**: https://doc.rust-lang.org/book/
- **Cargo Guide**: https://doc.rust-lang.org/cargo/
- **Docker Docs**: https://docs.docker.com/
- **Systemd Manual**: https://www.freedesktop.org/software/systemd/man/
- **PostgreSQL Docs**: https://www.postgresql.org/docs/

---

## Last Updated

**Date**: 2026-01-05
**By**: Claude Opus 4.5
**Version**: 1.0

---

**Print this page and keep near your desk for quick reference during deployments!**
