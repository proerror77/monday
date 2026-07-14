import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "replay_dryrun_parity.py"


def evidence_payload(
    *,
    intent_id="intent-1",
    order_id="order-1",
    fill_id="fill-1",
    order_side="BUY",
    fill_side="BUY",
    limit_price="0.42",
    fill_price="0.41",
    deployment_id="pm5d.threelayer.test",
    created_at="2026-05-02T01:02:03Z",
    fill_timestamp="2026-05-02T01:02:04Z",
    event_side="BUY",
    purpose="ENTRY",
):
    event_pnl = f"-{DecimalString.mul('10', fill_price)}"
    return {
        "runtime_evidence": {
            "events": [
                {
                    "deployment_id": deployment_id,
                    "intent_id": intent_id,
                    "order_id": order_id,
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "decision_ts": created_at,
                    "quote": limit_price,
                    "signal_inputs": {
                        "purpose": "ENTRY",
                        "requested_qty": "10",
                        "limit_price": limit_price,
                    },
                    "side": event_side,
                    "entry_price": fill_price,
                    "fill_status": "FILLED",
                    "settlement": "open",
                    "pnl": event_pnl,
                }
            ],
            "orders": [
                {
                    "deployment_id": deployment_id,
                    "intent_id": intent_id,
                    "order_id": order_id,
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "order_side": order_side,
                    "purpose": purpose,
                    "quantity": "10",
                    "limit_price": limit_price,
                    "filled_quantity": "10.0000000",
                    "status": "FILLED",
                    "created_at": created_at,
                }
            ],
            "fills": [
                {
                    "deployment_id": deployment_id,
                    "intent_id": intent_id,
                    "order_id": order_id,
                    "fill_id": fill_id,
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "fill_side": fill_side,
                    "purpose": purpose,
                    "quantity": "10",
                    "price": fill_price,
                    "fee": "0",
                    "fill_timestamp": fill_timestamp,
                }
            ],
        }
    }


class DecimalString:
    @staticmethod
    def mul(left, right):
        from decimal import Decimal

        return format((Decimal(str(left)) * Decimal(str(right))).normalize(), "f")


class ReplayDryrunParityTests(unittest.TestCase):
    def run_script(self, replay, dryrun, extra_args=None):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            replay_path = tmp_path / "replay.json"
            dryrun_path = tmp_path / "dryrun.json"
            output_path = tmp_path / "out" / "parity.json"
            replay_path.write_text(json.dumps(replay), encoding="utf-8")
            dryrun_path.write_text(json.dumps(dryrun), encoding="utf-8")

            args = [
                sys.executable,
                str(SCRIPT),
                "--replay-json",
                str(replay_path),
                "--dryrun-json",
                str(dryrun_path),
                "--output-json",
                str(output_path),
            ]
            if extra_args:
                args.extend(extra_args)
            subprocess.run(
                args,
                check=True,
                cwd=ROOT,
                capture_output=True,
                text=True,
            )
            return json.loads(output_path.read_text(encoding="utf-8"))

    def test_runtime_evidence_parity_ready_for_matching_events_orders_and_fills(self):
        result = self.run_script(evidence_payload(), evidence_payload())

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "continue")
        self.assertEqual(runtime["events"]["shared_count"], 1)
        self.assertEqual(runtime["orders"]["shared_count"], 1)
        self.assertEqual(runtime["fills"]["shared_count"], 1)
        self.assertEqual(result["blocking_risk_flags"], [])
        self.assertEqual(result["advisory_flags"], [])

    def test_runtime_evidence_normalizes_settlement_decimal_scale(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        replay["runtime_evidence"]["events"][0]["settlement"] = "1.000000"
        dryrun["runtime_evidence"]["events"][0]["settlement"] = "1.00000000000000000000"

        result = self.run_script(replay, dryrun)

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(runtime["events"]["mismatches"], [])
        self.assertEqual(result["decision"], "continue")

    def test_runtime_evidence_allows_partial_fill_then_residual_cancel_status_drift(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        replay["runtime_evidence"]["events"][0]["fill_status"] = "CANCELED"
        replay["runtime_evidence"]["orders"][0]["status"] = "CANCELED"
        replay["runtime_evidence"]["orders"][0]["filled_quantity"] = "4"
        replay["runtime_evidence"]["fills"][0]["quantity"] = "4"
        dryrun["runtime_evidence"]["events"][0]["fill_status"] = "PARTIALLY_FILLED"
        dryrun["runtime_evidence"]["orders"][0]["status"] = "PARTIALLY_FILLED"
        dryrun["runtime_evidence"]["orders"][0]["filled_quantity"] = "4.0000000"
        dryrun["runtime_evidence"]["fills"][0]["quantity"] = "4.0000000"

        result = self.run_script(replay, dryrun)

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "continue")
        self.assertEqual(runtime["mismatches"], [])
        self.assertEqual(result["blocking_risk_flags"], [])
        ignored = runtime["ignored_partial_fill_cancel_status_mismatches"]
        self.assertEqual(len(ignored), 2)
        self.assertEqual({mismatch["field"] for mismatch in ignored}, {"fill_status", "status"})

    def test_runtime_evidence_blocks_partial_fill_cancel_status_drift_without_fill(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        replay["runtime_evidence"]["events"][0]["fill_status"] = "CANCELED"
        replay["runtime_evidence"]["orders"][0]["status"] = "CANCELED"
        replay["runtime_evidence"]["orders"][0]["filled_quantity"] = "4"
        replay["runtime_evidence"]["fills"] = []
        dryrun["runtime_evidence"]["events"][0]["fill_status"] = "PARTIALLY_FILLED"
        dryrun["runtime_evidence"]["orders"][0]["status"] = "PARTIALLY_FILLED"
        dryrun["runtime_evidence"]["orders"][0]["filled_quantity"] = "4"
        dryrun["runtime_evidence"]["fills"] = []

        result = self.run_script(replay, dryrun)

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("runtime_evidence_field_mismatches", result["blocking_risk_flags"])
        self.assertEqual(runtime["ignored_partial_fill_cancel_status_mismatches"], [])

    def test_runtime_evidence_matches_semantic_rows_with_different_generated_ids(self):
        result = self.run_script(
            evidence_payload(order_id="replay-order", fill_id="replay-fill", order_side="UNKNOWN"),
            evidence_payload(order_id="dryrun-order", fill_id="dryrun-fill"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "continue")
        self.assertEqual(runtime["events"]["shared_count"], 1)
        self.assertEqual(runtime["orders"]["shared_count"], 1)
        self.assertEqual(runtime["fills"]["shared_count"], 1)

    def test_runtime_evidence_blocks_price_mismatch(self):
        result = self.run_script(
            evidence_payload(fill_price="0.41"),
            evidence_payload(fill_price="0.43"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("runtime_evidence_field_mismatches", result["risk_flags"])
        self.assertIn("runtime_evidence_field_mismatches", result["blocking_risk_flags"])
        self.assertIn(runtime["mismatches"][0]["field"], {"entry_price", "pnl", "price"})

    def test_runtime_evidence_classifies_settlement_exit_price_mismatch(self):
        result = self.run_script(
            evidence_payload(
                intent_id="tl_settle_event-1_up",
                order_side="SELL",
                fill_side="SELL",
                limit_price="0",
                fill_price="0",
                purpose="SETTLEMENT_EXIT",
            ),
            evidence_payload(
                intent_id="tl_settle_event-1_up",
                order_side="SELL",
                fill_side="SELL",
                limit_price="1",
                fill_price="1",
                purpose="SETTLEMENT_EXIT",
            ),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertIn("runtime_evidence_field_mismatches", result["risk_flags"])
        self.assertIn("settlement_exit_price_mismatches", result["risk_flags"])
        self.assertIn("settlement_exit_price_mismatches", result["blocking_risk_flags"])
        self.assertEqual(
            runtime["settlement_exit_mismatches"][0]["dryrun_row"]["intent_id"],
            "tl_settle_event-1_up",
        )

    def test_runtime_evidence_blocks_fill_side_mismatch(self):
        result = self.run_script(
            evidence_payload(fill_side="BUY"),
            evidence_payload(fill_side="SELL"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("runtime_evidence_field_mismatches", result["risk_flags"])
        self.assertEqual(runtime["mismatches"][0]["field"], "fill_side")

    def test_runtime_evidence_blocks_different_semantic_identity(self):
        result = self.run_script(
            evidence_payload(intent_id="replay-intent"),
            evidence_payload(intent_id="dryrun-intent"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("orders_present_in_replay_missing_from_dryrun", result["risk_flags"])
        self.assertIn("fills_present_in_replay_missing_from_dryrun", result["risk_flags"])
        self.assertEqual(runtime["orders"]["shared_count"], 0)
        self.assertEqual(runtime["fills"]["shared_count"], 0)

    def test_runtime_evidence_ignores_zero_fill_take_profit_rejections(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        rejected_exit = evidence_payload(
            intent_id="tl_tp_token-up_1778614178726",
            order_id="rejected-exit-order",
            fill_id="unused-fill",
            order_side="SELL",
            fill_side="SELL",
            limit_price="0.99",
            fill_price="0",
            event_side="SELL",
            created_at="2026-05-02T01:03:00Z",
        )
        rejected_exit["runtime_evidence"]["events"][0]["fill_status"] = "REJECTED"
        rejected_exit["runtime_evidence"]["events"][0]["entry_price"] = "0.99"
        rejected_exit["runtime_evidence"]["events"][0]["pnl"] = "0"
        rejected_exit["runtime_evidence"]["orders"][0]["status"] = "REJECTED"
        rejected_exit["runtime_evidence"]["orders"][0]["filled_quantity"] = "0"
        rejected_exit["runtime_evidence"]["orders"][0]["rejection_reason"] = "No full-depth liquidity"
        rejected_exit["runtime_evidence"]["fills"] = []
        dryrun["runtime_evidence"]["events"].extend(rejected_exit["runtime_evidence"]["events"])
        dryrun["runtime_evidence"]["orders"].extend(rejected_exit["runtime_evidence"]["orders"])

        result = self.run_script(replay, dryrun)

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(result["blocking_risk_flags"], [])
        self.assertEqual(runtime["events"]["dryrun_count"], 1)
        self.assertEqual(runtime["orders"]["dryrun_count"], 1)
        ignored = runtime["ignored_non_executed_exit_attempts"]
        self.assertEqual(len(ignored["dryrun_events"]), 1)
        self.assertEqual(len(ignored["dryrun_orders"]), 1)
        self.assertEqual(
            ignored["dryrun_orders"][0]["intent_id"],
            "tl_tp_token-up_1778614178726",
        )

    def test_empty_runtime_window_collects_more_instead_of_fixing_runtime(self):
        result = self.run_script({"runtime_evidence": {}}, {"runtime_evidence": {}})

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "collect-more")
        self.assertIn("no_comparable_runtime_sample", result["advisory_flags"])
        self.assertEqual(result["blocking_risk_flags"], [])
        self.assertEqual(runtime["events"]["shared_count"], 0)
        self.assertEqual(runtime["orders"]["shared_count"], 0)
        self.assertEqual(runtime["fills"]["shared_count"], 0)

    def test_filters_limit_comparison_to_matching_deployment_window(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        extra = evidence_payload(
            intent_id="intent-outside-window",
            order_id="order-outside-window",
            fill_id="fill-outside-window",
            created_at="2026-05-02T01:30:03Z",
            fill_timestamp="2026-05-02T01:30:04Z",
        )
        dryrun["runtime_evidence"]["orders"].extend(extra["runtime_evidence"]["orders"])
        dryrun["runtime_evidence"]["fills"].extend(extra["runtime_evidence"]["fills"])
        dryrun["runtime_evidence"]["events"].extend(extra["runtime_evidence"]["events"])

        result = self.run_script(
            replay,
            dryrun,
            extra_args=[
                "--deployment-id",
                "pm5d.threelayer.test",
                "--since",
                "2026-05-02T01:00:00Z",
                "--until",
                "2026-05-02T01:05:00Z",
            ],
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(runtime["events"]["dryrun_count"], 1)
        self.assertEqual(runtime["orders"]["dryrun_count"], 1)
        self.assertEqual(runtime["fills"]["dryrun_count"], 1)
        self.assertEqual(result["filters"]["deployment_id"], "pm5d.threelayer.test")

    def test_runtime_evidence_blocks_missing_event_rows(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        replay["runtime_evidence"]["events"] = []

        result = self.run_script(replay, dryrun)

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("replay_has_no_event_level_rows", result["blocking_risk_flags"])
        self.assertEqual(runtime["events"]["replay_count"], 0)

    def test_legacy_event_comparison_normalizes_timestamp_and_numeric_shapes(self):
        replay = evidence_payload(
            created_at="2026-05-11T17:40:00Z",
            limit_price="1",
            fill_price="1",
        )
        dryrun = evidence_payload(
            created_at="2026-05-12T01:40:00+08:00",
            limit_price=1.0,
            fill_price=1.0,
        )
        dryrun["runtime_evidence"]["events"][0]["signal_inputs"] = {
            "purpose": "ENTRY",
            "requested_qty": 10.0,
            "limit_price": 1.0,
        }

        result = self.run_script(replay, dryrun)

        self.assertTrue(result["runtime_evidence_comparison"]["strict_parity_ready"])
        self.assertTrue(result["event_comparison"]["strict_parity_ready"])
        self.assertEqual(result["event_comparison"]["mismatches"], [])
        self.assertNotIn("legacy_event_strict_field_mismatches", result["legacy_event_flags"])

    def test_legacy_event_drift_is_diagnostic_when_runtime_evidence_is_strict_ready(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        dryrun["events"] = [
            {
                "event_id": "legacy-only",
                "decision_ts": "2026-05-02T01:02:03Z",
                "quote": "0.42",
                "signal_inputs": {},
                "side": "BUY",
                "entry_price": "0.41",
                "fill_status": "FILLED",
                "settlement": "open",
                "pnl": "0",
            }
        ]

        result = self.run_script(replay, dryrun)

        self.assertTrue(result["runtime_evidence_comparison"]["strict_parity_ready"])
        self.assertEqual(result["blocking_risk_flags"], [])
        self.assertEqual(result["advisory_flags"], [])
        self.assertEqual(result["risk_flags"], [])
        self.assertIn(
            "legacy_events_present_in_dryrun_missing_from_replay",
            result["legacy_event_flags"],
        )


if __name__ == "__main__":
    unittest.main()
