# Ploy Platform Watchdog Implementation Plan

**Goal:** Add a minimal host-side watchdog that restarts `ploy-platform` when it has been cleanly left inactive, preventing multi-day market-data capture gaps.

**Architecture:** Add a small shell watchdog script plus a oneshot systemd service/timer. The watchdog only checks unit liveness, skips when a maintenance lock file exists or maintenance is running, and starts the configured platform unit if it is unexpectedly inactive. Wire the deployment files and install script so hosts can opt into the timer without touching Rust runtime code.

**Tech Stack:** bash, systemd service/timer units, existing deployment/install-service flow

---

### Task 1: Add a failing shell-level regression harness

**Files:**
- Create: `scripts/tests/test_ploy_platform_watchdog.sh`
- Create: `scripts/tests/lib/assert.sh`

**Step 1: Write the failing test**

Add shell tests for:
- inactive unit -> watchdog calls `systemctl start`
- inactive unit + lock file -> watchdog does not start
- inactive unit + active maintenance service -> watchdog does not start
- active unit -> watchdog does nothing

**Step 2: Run test to verify it fails**

Run: `bash scripts/tests/test_ploy_platform_watchdog.sh`
Expected: FAIL because watchdog script does not exist yet.

### Task 2: Implement the minimal watchdog script

**Files:**
- Create: `scripts/ploy_platform_watchdog.sh`

**Step 1: Write minimal implementation**

Implement a bash script that:
- accepts `PLOY_PLATFORM_WATCHDOG_UNIT`
- accepts `PLOY_PLATFORM_WATCHDOG_LOCK_FILE`
- skips when maintenance service is active
- starts the configured unit when it is inactive and no lock file exists

**Step 2: Run targeted test**

Run: `bash scripts/tests/test_ploy_platform_watchdog.sh`
Expected: PASS

### Task 3: Add deployment units and installation wiring

**Files:**
- Create: `deployment/ploy-platform-watchdog.service`
- Create: `deployment/ploy-platform-watchdog.timer`
- Historical modify target: `scripts/install-service.sh`

Note: the old `scripts/install-service.sh` compatibility path has since been
removed with the retired legacy root-runtime archive. The active
maintenance/watchdog install path is `scripts/install-platform-service.sh`
through `.github/workflows/release-platform.yml`.

**Step 1: Add systemd units**

Create a oneshot service that runs the watchdog script from `/opt/ploy/scripts`, and a timer that runs every 5 minutes with persistence enabled.

**Step 2: Wire install path**

Install the two new unit files when present and enable the timer during the
active platform installer.

**Step 3: Run targeted test / lint**

Run: `bash scripts/tests/test_ploy_platform_watchdog.sh`
Expected: PASS

### Task 4: Verify the deployment diff

**Files:**
- Modify: `tasks/todo.md`

**Step 1: Record the watchdog plan and validation**

Add a short progress note linking the host outage root cause to the new watchdog.

**Step 2: Run final checks**

Run:
- `bash scripts/tests/test_ploy_platform_watchdog.sh`
- `rtk git diff -- deployment/ploy-platform-watchdog.service deployment/ploy-platform-watchdog.timer scripts/ploy_platform_watchdog.sh scripts/install-platform-service.sh tasks/todo.md`

**Step 3: Commit**

```bash
git add docs/plans/2026-03-11-ploy-platform-watchdog-design.md \
  deployment/ploy-platform-watchdog.service \
  deployment/ploy-platform-watchdog.timer \
  scripts/ploy_platform_watchdog.sh \
  scripts/tests/test_ploy_platform_watchdog.sh \
  scripts/install-platform-service.sh \
  tasks/todo.md
git commit -m "ops: add ploy platform watchdog"
```
