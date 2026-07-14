# Live Dry-Run Drill Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a repeatable remote-host dry-run acceptance checklist and `ployctl`-driven drill script that validates live-host readiness without touching real funds.

**Architecture:** The change is documentation-first with one operator script. The script relies only on the existing `ployd` HTTP surface, `systemd`, `curl`, and `ployctl`; it creates a paper deployment for the drill, validates platform health, and cleans up on exit. Runbooks route operators through three distinct phases: deploy, dry-run acceptance, and manual live enablement.

**Tech Stack:** Bash, systemd, curl, ployctl, Markdown runbooks, JSON deployment manifest

---

### Task 1: Document the approved design and track the work

**Files:**
- Modify: `tasks/todo.md`
- Create: `docs/plans/2026-03-24-live-dry-run-drill-design.md`
- Create: `docs/plans/2026-03-25-live-dry-run-drill-implementation-plan.md`

**Step 1: Record the task in the tracker**

Add a dedicated section at the top of `tasks/todo.md` describing the goal, file ownership, tasks, and progress notes for the live dry-run drill.

**Step 2: Save the approved design**

Write the already-approved dry-run drill design to `docs/plans/2026-03-24-live-dry-run-drill-design.md`.

**Step 3: Save this implementation plan**

Write the implementation plan to `docs/plans/2026-03-25-live-dry-run-drill-implementation-plan.md`.

**Step 4: Verify the docs exist**

Run:

```bash
ls docs/plans | rg 'live-dry-run-drill'
```

Expected: both design and implementation plan files appear.

### Task 2: Add the sample dry-run deployment manifest

**Files:**
- Create: `config/deployments/example.live.dry-run.json`
- Modify: `config/deployments/README.md`

**Step 1: Add the drill manifest**

Create a JSON manifest with:

- a distinct `deployment_id`
- `runtime_mode: "paper"`
- a drill-specific `account_id`
- a conservative `max_gross_exposure`
- `desired_state: "running"`

**Step 2: Clarify the boundary**

Update `config/deployments/README.md` to distinguish:

- live deployment manifests
- paper drill manifests
- the fact that the drill manifest is for readiness checks only

**Step 3: Validate the manifest shape**

Run:

```bash
python3 -m json.tool config/deployments/example.live.dry-run.json >/dev/null
```

Expected: command exits 0.

### Task 3: Write the remote dry-run drill script

**Files:**
- Create: `scripts/drills/live_dry_run.sh`

**Step 1: Write the script skeleton**

The script should:

- use `set -euo pipefail`
- accept `--host-root`, `--addr`, `--manifest`, and `--deployment-id`
- default to `/opt/ploy`, `http://127.0.0.1:8081`, and the new sample manifest

**Step 2: Add baseline checks**

Implement:

- `systemctl is-active ployd`
- `curl -fsS $ADDR/health`
- `ployctl system status`
- `ployctl system metrics`
- `ployctl system alerts`
- `ployctl system audit`

**Step 3: Add config and path presence checks**

Require:

- `/opt/ploy/.env`
- key live/operator env variables present in `.env`
- runtime snapshot files under `/opt/ploy/run/platform/`

**Step 4: Add paper deployment drill**

Implement:

- `ployctl deployments apply <manifest>`
- `ployctl deployments inspect <id>`
- `ployctl deployments pause <id>`
- `ployctl deployments resume <id>`
- `ployctl deployments stop <id>`

Add cleanup with `trap` so the deployment is stopped even on failure.

**Step 5: Add final pass/fail output**

Emit a clear final summary with `PASS`, `WARN`, or `FAIL`.

**Step 6: Validate shell syntax**

Run:

```bash
bash -n scripts/drills/live_dry_run.sh
```

Expected: command exits 0.

### Task 4: Write operator runbooks for acceptance and drill execution

**Files:**
- Create: `docs/runbooks/live-deployment-checklist.md`
- Create: `docs/runbooks/live-dry-run-drill.md`

**Step 1: Write the checklist**

Document:

- host prerequisites
- required env vars
- auth expectations
- go/no-go checks before moving toward real live enablement

**Step 2: Write the drill walkthrough**

Document:

- what the script checks
- what it intentionally does not do
- how to run it on the remote host
- how to interpret `PASS`, `WARN`, and `FAIL`

**Step 3: Link the sample manifest**

Reference `config/deployments/example.live.dry-run.json` explicitly in the drill runbook.

### Task 5: Route README and existing runbooks to the new acceptance path

**Files:**
- Modify: `README.md`
- Modify: `docs/runbooks/platform-deploy.md`
- Modify: `docs/runbooks/platform-startup.md`

**Step 1: Update README routing**

Add links that direct operators to:

- the deploy runbook
- the live deployment checklist
- the dry-run drill runbook

**Step 2: Update deploy/startup runbooks**

Clarify that:

- deployment/install is separate from acceptance
- the dry-run drill is the default remote-host readiness path
- real live enablement happens only after the dry-run passes

**Step 3: Check for consistent terminology**

Run:

```bash
rg -n "dry-run drill|live deployment checklist|example.live.dry-run" README.md docs/runbooks config/deployments
```

Expected: the new routing terms appear in the intended docs.

### Task 6: Run focused verification and capture results

**Files:**
- Modify: `tasks/todo.md`

**Step 1: Run doc/script checks**

Run:

```bash
bash -n scripts/drills/live_dry_run.sh
python3 -m json.tool config/deployments/example.live.dry-run.json >/dev/null
```

Expected: both commands exit 0.

**Step 2: Run focused regression coverage**

Run:

```bash
rtk cargo test --test platform_smoke
```

Expected: PASS.

**Step 3: Update progress notes**

Record the commands run and the outcome in `tasks/todo.md`.

**Step 4: Commit**

```bash
git add tasks/todo.md docs/plans/2026-03-24-live-dry-run-drill-design.md docs/plans/2026-03-25-live-dry-run-drill-implementation-plan.md docs/runbooks/live-deployment-checklist.md docs/runbooks/live-dry-run-drill.md docs/runbooks/platform-deploy.md docs/runbooks/platform-startup.md README.md config/deployments/README.md config/deployments/example.live.dry-run.json scripts/drills/live_dry_run.sh
git commit -m "docs: add live dry-run deployment drill"
```
