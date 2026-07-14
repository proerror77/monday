#!/usr/bin/env python3
"""Orchestrate the settlement-probability PRD evidence gate.

The default gate is intentionally strict:

1. Reuse a retained complete sampled research snapshot artifact supplied with
   --snapshot-run-id.
2. Feed that snapshot artifact into the GitHub-hosted Factor Walk-Forward V2
   artifact workflow.
3. Optionally attach a replay/dry-run parity artifact to the promotion gate.

For PM 5m/15m event research, use --data-quality-mode event-complete when a
global lookback contains known collector outages but enough per-event samples
are complete. In that mode the snapshot still records the scoped audit status,
but promotion data quality is judged by complete executable event rows rather
than continuous wall-clock coverage.

This script is a control-plane helper around existing GitHub Actions workflows;
it does not run database research locally.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

try:
    from validate_autofactor_handoff_replay_gate import validate_handoff_payload
except ModuleNotFoundError:  # unittest imports this file as scripts.<module>.
    from scripts.validate_autofactor_handoff_replay_gate import validate_handoff_payload


HOSTED_WALK_FORWARD_WORKFLOW = "factor-walk-forward-v2-hosted-artifact.yml"


@dataclass(frozen=True)
class WorkflowRun:
    database_id: int
    status: str
    conclusion: str | None
    url: str
    created_at: datetime


@dataclass(frozen=True)
class PromotionGateEvaluation:
    ready: bool
    evidence: str
    blocked_gates: tuple[str, ...]


def run_command(args: list[str], *, dry_run: bool = False) -> str:
    print("+ " + " ".join(args), flush=True)
    if dry_run:
        return ""
    completed = subprocess.run(
        args,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.stderr.strip():
        print(completed.stderr.strip(), file=sys.stderr)
    return completed.stdout


def run_json(args: list[str]) -> Any:
    output = run_command(args)
    return json.loads(output) if output.strip() else None


def git_branch() -> str:
    try:
        value = run_command(["git", "branch", "--show-current"]).strip()
    except subprocess.CalledProcessError:
        value = ""
    return value or "main"


def parse_created_at(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def find_latest_run(workflow: str, created_after: datetime) -> WorkflowRun:
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        rows = run_json(
            [
                "gh",
                "run",
                "list",
                "--workflow",
                workflow,
                "--event",
                "workflow_dispatch",
                "--limit",
                "50",
                "--json",
                "databaseId,status,conclusion,url,createdAt",
            ]
        )
        candidates = []
        for row in rows or []:
            created_at = parse_created_at(row["createdAt"])
            if created_at >= created_after - timedelta(seconds=5):
                candidates.append(
                    WorkflowRun(
                        database_id=int(row["databaseId"]),
                        status=str(row["status"]),
                        conclusion=row.get("conclusion"),
                        url=str(row["url"]),
                        created_at=created_at,
                    )
                )
        if candidates:
            return sorted(candidates, key=lambda run: run.created_at)[-1]
        time.sleep(5)
    raise RuntimeError(f"could not find dispatched run for {workflow}")


def refresh_run(run_id: int) -> WorkflowRun:
    row = run_json(
        [
            "gh",
            "run",
            "view",
            str(run_id),
            "--json",
            "databaseId,status,conclusion,url,createdAt",
        ]
    )
    return WorkflowRun(
        database_id=int(row["databaseId"]),
        status=str(row["status"]),
        conclusion=row.get("conclusion"),
        url=str(row["url"]),
        created_at=parse_created_at(row["createdAt"]),
    )


def wait_for_run(run: WorkflowRun, *, timeout_minutes: int, poll_seconds: int) -> WorkflowRun:
    deadline = time.monotonic() + timeout_minutes * 60
    current = run
    while current.status != "completed":
        if time.monotonic() >= deadline:
            raise TimeoutError(f"timed out waiting for run {run.database_id}: {run.url}")
        print(
            f"waiting run={current.database_id} status={current.status} url={current.url}",
            flush=True,
        )
        time.sleep(poll_seconds)
        current = refresh_run(run.database_id)
    print(
        f"completed run={current.database_id} conclusion={current.conclusion} url={current.url}",
        flush=True,
    )
    return current


def dispatch_workflow(
    workflow: str,
    fields: dict[str, str],
    *,
    workflow_ref: str,
    dry_run: bool,
) -> WorkflowRun | None:
    marker = datetime.now(timezone.utc)
    args = ["gh", "workflow", "run", workflow, "--ref", workflow_ref]
    for key, value in fields.items():
        args.extend(["-f", f"{key}={value}"])
    run_command(args, dry_run=dry_run)
    if dry_run:
        return None
    return find_latest_run(workflow, marker)


def issue_comment(issue_number: str, body: str, *, dry_run: bool) -> None:
    if not issue_number:
        return
    run_command(["gh", "issue", "comment", issue_number, "--body", body], dry_run=dry_run)


def compact_json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, separators=(",", ":"), sort_keys=True)


def split_replay_parity_input(value: str, artifact_name: str) -> tuple[str, str]:
    """Parse workflow-friendly replay parity input.

    GitHub workflow_dispatch is limited to 10 inputs, so the workflow accepts a
    compact replay input of either "<run_id>" or "<run_id>:<artifact_name>".
    Direct CLI callers can still use --replay-parity-artifact-name.
    """

    stripped = value.strip()
    if not stripped or artifact_name:
        return stripped, artifact_name
    if ":" not in stripped:
        return stripped, artifact_name
    run_id, artifact = stripped.split(":", 1)
    return run_id.strip(), artifact.strip()


def parse_promotion_gate(report_text: str) -> PromotionGateEvaluation:
    ready: bool | None = None
    blocked_gates: list[str] = []
    evidence_lines: list[str] = []
    in_gate_table = False

    for line in report_text.splitlines():
        if line.startswith("ready_for_dry_run_handoff="):
            ready = line.split("=", 1)[1].split(maxsplit=1)[0].lower() == "true"
            evidence_lines.append(line)
            continue
        if line == "gate,passed,evidence":
            in_gate_table = True
            continue
        if in_gate_table:
            if not line.strip():
                break
            parts = line.split(",", 2)
            if len(parts) != 3:
                continue
            gate, passed, evidence = parts
            if gate == "recorded_replay_parity":
                continue
            if passed.lower() != "true":
                blocked_gates.append(f"{gate}: {evidence}")

    if ready is None:
        return PromotionGateEvaluation(
            ready=False,
            evidence="missing Settlement Probability PRD Promotion Gate ready line",
            blocked_gates=("missing_promotion_gate",),
        )

    return PromotionGateEvaluation(
        ready=ready,
        evidence=evidence_lines[0],
        blocked_gates=tuple(blocked_gates),
    )


def download_and_evaluate_promotion_gate(walk_run_id: int) -> PromotionGateEvaluation:
    with tempfile.TemporaryDirectory(prefix=f"settlement-prd-gate-{walk_run_id}-") as tmp:
        output_dir = Path(tmp)
        run_command(
            [
                "gh",
                "run",
                "download",
                str(walk_run_id),
                "--name",
                f"factor-walk-forward-v2-{walk_run_id}",
                "--dir",
                str(output_dir),
            ]
        )
        report_path = output_dir / "factor-walk-forward-v2" / "report.txt"
        if not report_path.is_file():
            return PromotionGateEvaluation(
                ready=False,
                evidence=f"missing report artifact file: {report_path}",
                blocked_gates=("missing_report_artifact",),
            )
        gate = parse_promotion_gate(report_path.read_text(encoding="utf-8"))
        if not gate.ready:
            return gate
        handoff_path = output_dir / "factor-walk-forward-v2" / "autofactor-strategy-handoff.json"
        if not handoff_path.is_file():
            return PromotionGateEvaluation(
                ready=False,
                evidence=gate.evidence,
                blocked_gates=("missing_autofactor_strategy_handoff",),
            )
        handoff = json.loads(handoff_path.read_text(encoding="utf-8"))
        replay_blockers = validate_handoff_payload(handoff)
        if replay_blockers:
            return PromotionGateEvaluation(
                ready=False,
                evidence=gate.evidence,
                blocked_gates=tuple(f"handoff_replay_gate:{item}" for item in replay_blockers),
            )
        return gate


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--git-ref", default="", help="Branch/SHA to dispatch; defaults to current branch")
    parser.add_argument("--start-date", required=True, help="Research window start date, YYYY-MM-DD")
    parser.add_argument("--end-date", required=True, help="Research window end date, YYYY-MM-DD")
    parser.add_argument("--symbols", default="BTCUSDT,ETHUSDT,SOLUSDT")
    parser.add_argument("--stake-usd", default="15")
    parser.add_argument(
        "--snapshot-run-id",
        default="",
        help=(
            "Existing workflow run id that owns a complete sampled research-snapshot artifact. "
            "Required for the default gate so walk-forward consumes a retained "
            "snapshot contract."
        ),
    )
    parser.add_argument(
        "--snapshot-artifact-name",
        default="",
        help="Optional snapshot artifact name; defaults to research-snapshot-<snapshot-run-id>",
    )
    parser.add_argument("--issue-number", default="321")
    parser.add_argument("--audit-lookback-hours", default="168")
    parser.add_argument(
        "--data-quality-mode",
        choices=("strict-continuous", "event-complete"),
        default="strict-continuous",
        help="Data gate semantics: strict continuous source audit or event-complete executable samples",
    )
    parser.add_argument("--min-event-complete-events", default="20")
    parser.add_argument("--min-event-complete-rows", default="40")
    parser.add_argument(
        "--lob-sample-secs",
        default="",
        help="Override LOB sampling seconds; empty inherits the existing snapshot manifest",
    )
    parser.add_argument(
        "--pm-book-sample-secs",
        default="",
        help="Override PM full-book sampling seconds; empty inherits the existing snapshot manifest",
    )
    parser.add_argument(
        "--observation-sample-secs",
        default="",
        help="Override observation sampling seconds; empty inherits the existing snapshot manifest",
    )
    parser.add_argument(
        "--max-quote-age-secs",
        default="",
        help="Override quote-age seconds; empty inherits the existing snapshot manifest",
    )
    parser.add_argument("--train-window-days", default="2")
    parser.add_argument("--test-window-days", default="1")
    parser.add_argument("--step-days", default="1")
    parser.add_argument("--min-observations", default="20")
    parser.add_argument("--top-quantile", default="0.2")
    parser.add_argument("--top-n", default="20")
    parser.add_argument("--replay-parity-run-id", default="")
    parser.add_argument("--replay-parity-artifact-name", default="")
    parser.add_argument("--walk-timeout-minutes", type=int, default=120)
    parser.add_argument("--poll-seconds", type=int, default=30)
    parser.add_argument("--no-wait", action="store_true", help="Dispatch walk-forward and return")
    parser.add_argument("--dry-run", action="store_true", help="Print workflow dispatches without running them")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    git_ref = args.git_ref or git_branch()
    rust_data_quality_mode = args.data_quality_mode.replace("-", "_")
    replay_parity_run_id, replay_parity_artifact_name = split_replay_parity_input(
        args.replay_parity_run_id,
        args.replay_parity_artifact_name,
    )

    if args.snapshot_run_id:
        snapshot_result = refresh_run(int(args.snapshot_run_id))
        print(
            "Using existing complete sampled research snapshot artifact "
            f"snapshot_run_id={snapshot_result.database_id} url={snapshot_result.url}.",
            flush=True,
        )
    else:
        message = "\n".join(
            [
                "Settlement probability PRD gate blocked before dispatch:",
                "",
                "- Missing required `--snapshot-run-id` for a retained complete sampled research snapshot.",
                "- Legacy pm5d-vol snapshot build fallback has been removed from this gate.",
                "- Decision: dispatch or select a research-snapshot artifact first, then rerun the PRD gate.",
            ]
        )
        print(message, flush=True)
        issue_comment(args.issue_number, message, dry_run=args.dry_run)
        return 2

    walk_options = {
        "train_window_days": int(args.train_window_days),
        "test_window_days": int(args.test_window_days),
        "step_days": int(args.step_days),
        "lob_sample_secs": int(args.lob_sample_secs) if args.lob_sample_secs else "",
        "pm_book_sample_secs": int(args.pm_book_sample_secs) if args.pm_book_sample_secs else "",
        "observation_sample_secs": (
            int(args.observation_sample_secs) if args.observation_sample_secs else ""
        ),
        "max_quote_age_secs": int(args.max_quote_age_secs) if args.max_quote_age_secs else "",
        "top_n": int(args.top_n),
        "min_observations": int(args.min_observations),
        "top_quantile": float(args.top_quantile),
        "factor_name_filter": "",
        "data_quality_mode": rust_data_quality_mode,
        "min_event_complete_events": int(args.min_event_complete_events),
        "min_event_complete_rows": int(args.min_event_complete_rows),
        "replay_parity_json": "",
        "replay_parity_run_id": replay_parity_run_id,
        "replay_parity_artifact_name": replay_parity_artifact_name,
        "required_strategy_profile": "settlement_probability",
        "allowed_target": "full_depth_settlement_executable_pnl",
        "issue_number": args.issue_number,
        "create_handoff_issue": False,
        "fail_if_blocked": False,
    }
    walk_fields = {
        "git_ref": git_ref,
        "snapshot_run_id": str(snapshot_result.database_id),
        "snapshot_artifact_name": args.snapshot_artifact_name,
        "start_date": args.start_date,
        "end_date": args.end_date,
        "symbols": args.symbols,
        "stake_usd": args.stake_usd,
        "options_json": compact_json(walk_options),
    }

    print("Dispatching hosted settlement probability promotion gate.", flush=True)
    walk_run = dispatch_workflow(
        HOSTED_WALK_FORWARD_WORKFLOW,
        walk_fields,
        workflow_ref=git_ref,
        dry_run=args.dry_run,
    )
    if args.dry_run:
        return 0
    if walk_run is None:
        raise RuntimeError("walk-forward dispatch did not return a run")
    print(f"walk_forward_run_id={walk_run.database_id} url={walk_run.url}", flush=True)
    if args.no_wait:
        return 0
    walk_result = wait_for_run(
        walk_run,
        timeout_minutes=args.walk_timeout_minutes,
        poll_seconds=args.poll_seconds,
    )
    if walk_result.conclusion != "success":
        issue_comment(
            args.issue_number,
            "\n".join(
                [
                    "Settlement probability PRD gate orchestration failed at walk-forward:",
                    "",
                    f"- Snapshot run: {snapshot_result.url}",
                    f"- Walk-forward run: {walk_result.url}",
                    f"- Walk-forward conclusion: `{walk_result.conclusion}`",
                    "- Decision: inspect the workflow failure before any dry-run handoff.",
                ]
            ),
            dry_run=False,
        )
        return 1

    gate = download_and_evaluate_promotion_gate(walk_result.database_id)
    blocker_text = "\n".join(f"  - {item}" for item in gate.blocked_gates) or "  - none"
    decision = (
        "ready for dry-run handoff review"
        if gate.ready
        else "blocked: do not create dry-run handoff"
    )

    body = "\n".join(
        [
            "Settlement probability PRD gate orchestration completed:",
            "",
            f"- Snapshot run: {snapshot_result.url}",
            f"- Walk-forward run: {walk_result.url}",
            f"- Walk-forward conclusion: `{walk_result.conclusion}`",
            f"- Replay parity run supplied: `{replay_parity_run_id or 'none'}`",
            f"- Replay parity artifact supplied: `{replay_parity_artifact_name or 'default'}`",
            f"- Data quality mode: `{args.data_quality_mode}`",
            f"- Gate: `{gate.evidence}`",
            f"- Decision: {decision}",
            "",
            "Blocked gates:",
            blocker_text,
        ]
    )
    issue_comment(args.issue_number, body, dry_run=False)
    return 0 if gate.ready else 3


if __name__ == "__main__":
    raise SystemExit(main())
