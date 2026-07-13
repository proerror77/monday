import argparse
import unittest

from scripts import reset_strategy_runtime_evidence as reset


class ResetStrategyRuntimeEvidenceTest(unittest.TestCase):
    def test_order_predicate_is_deployment_scoped_and_quotes_literals(self):
        args = argparse.Namespace(
            deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
            strategy_id="three_layer",
            runtime_modes=["dry_run"],
            after_ts="2026-05-12T11:00:00+08:00",
            before_ts="2026-05-13T00:00:00+08:00",
        )

        predicate = reset.order_predicate(args)

        self.assertIn("deployment_id = 'pm5d.threelayer.settlement-probability-btc-eth.dryrun'", predicate)
        self.assertIn("runtime_mode IN ('dry_run')", predicate)
        self.assertIn("strategy_id = 'three_layer'", predicate)
        self.assertIn("recorded_at >= '2026-05-12T11:00:00+08:00'::timestamptz", predicate)
        self.assertIn("recorded_at < '2026-05-13T00:00:00+08:00'::timestamptz", predicate)

    def test_fill_predicate_deletes_only_fills_attached_to_matching_orders(self):
        args = argparse.Namespace(
            deployment_id="deploy-a",
            strategy_id="",
            runtime_modes=["dry_run", "paper"],
            after_ts=None,
            before_ts=None,
        )

        predicate = reset.fill_predicate(args)

        self.assertIn("EXISTS (SELECT 1 FROM strategy_runtime_orders o", predicate)
        self.assertIn("o.order_id = strategy_runtime_fills.order_id", predicate)
        self.assertIn("deployment_id = 'deploy-a'", predicate)
        self.assertIn("runtime_mode IN ('dry_run', 'paper')", predicate)

    def test_execute_requires_confirmation(self):
        with self.assertRaises(SystemExit):
            reset.parse_args(
                [
                    "--deployment-id",
                    "deploy-a",
                    "--backup-dir",
                    "/tmp/backup",
                    "--execute",
                ]
            )

    def test_timestamp_window_must_be_ordered(self):
        with self.assertRaises(SystemExit):
            reset.parse_args(
                [
                    "--deployment-id",
                    "deploy-a",
                    "--backup-dir",
                    "/tmp/backup",
                    "--after-ts",
                    "2026-05-13T00:00:00+08:00",
                    "--before-ts",
                    "2026-05-12T00:00:00+08:00",
                ]
            )


if __name__ == "__main__":
    unittest.main()
