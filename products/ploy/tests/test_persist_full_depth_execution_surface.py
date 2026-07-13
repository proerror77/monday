import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts.persist_full_depth_execution_surface import build_row


def write_surface(root: Path, payload: dict) -> Path:
    path = root / "full-depth-execution-surface.json"
    path.write_text(json.dumps(payload, sort_keys=True), encoding="utf-8")
    return path


def valid_surface(**overrides) -> dict:
    payload = {
        "schema_version": "full_depth_execution_surface.v1",
        "surface": "clob_orderbook_snapshots",
        "source": "orderbook_snapshot_archive",
        "start_ts": "2026-05-17T00:00:00Z",
        "end_ts": "2026-05-18T00:00:00Z",
        "checked_hours": 24,
        "existing_hours": 20,
        "exported_hours": 4,
        "row_count": 2_214_371,
        "full_fidelity": True,
        "incomplete": False,
    }
    payload.update(overrides)
    return payload


class PersistFullDepthExecutionSurfaceTest(unittest.TestCase):
    def test_exported_hours_count_as_covered_hours(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = write_surface(Path(tmp), valid_surface(existing_hours=0, exported_hours=24))

            row = build_row(
                surface_json=path,
                run_id="collect-full-depth-execution-surface:123",
                source_workflow="collect-full-depth-execution-surface.yml",
                workflow_run_id="123",
                workflow_run_url="https://github.com/proerror77/ploy/actions/runs/123",
                artifact_name="full-depth-execution-surface-123",
            )

        self.assertTrue(row.valid)
        self.assertEqual([], row.blockers)

    def test_missing_hours_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = write_surface(Path(tmp), valid_surface(existing_hours=10, exported_hours=2))

            row = build_row(
                surface_json=path,
                run_id="collect-full-depth-execution-surface:123",
                source_workflow="collect-full-depth-execution-surface.yml",
                workflow_run_id="123",
                workflow_run_url="https://github.com/proerror77/ploy/actions/runs/123",
                artifact_name="full-depth-execution-surface-123",
            )

        self.assertFalse(row.valid)
        self.assertIn("missing_hours:12<24", row.blockers)

    def test_require_valid_rejects_invalid_surface_before_persist(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            path = write_surface(root, valid_surface(row_count=0))
            report = root / "persist.json"

            proc = subprocess.run(
                [
                    sys.executable,
                    "scripts/persist_full_depth_execution_surface.py",
                    "--surface-json",
                    str(path),
                    "--dry-run",
                    "--require-valid",
                    "--report-json",
                    str(report),
                ],
                cwd=Path(__file__).resolve().parents[1],
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(0, proc.returncode)
            self.assertIn("refusing to persist", proc.stderr)
            payload = json.loads(report.read_text(encoding="utf-8"))
        self.assertFalse(payload["valid"])
        self.assertIn("row_count_empty", payload["blockers"])


if __name__ == "__main__":
    unittest.main()
