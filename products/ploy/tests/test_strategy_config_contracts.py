from pathlib import Path
from decimal import Decimal
import json
import subprocess
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
STRATEGY_DIR = ROOT / "config" / "strategies"
DEPLOYMENT_DIR = ROOT / "config" / "deployments"


class StrategyConfigContractTests(unittest.TestCase):
    def test_live_manifest_is_paused_and_unrendered(self) -> None:
        deployment = json.loads(
            (DEPLOYMENT_DIR / "pm5d.threelayer.live.json").read_text()
        )

        self.assertEqual(deployment["runtime_mode"], "live")
        self.assertEqual(deployment["desired_state"], "paused")
        self.assertEqual(deployment["account_id"], "live-wallet-must-be-rendered")
        self.assertEqual(Decimal(deployment["max_gross_exposure"]), Decimal("5.0"))

    def test_live_fixed_stake_does_not_exceed_cap(self) -> None:
        strategy = tomllib.loads(
            (STRATEGY_DIR / "02-pm5d-threelayer.live.toml").read_text()
        )["strategy"]
        deployment = json.loads(
            (DEPLOYMENT_DIR / "pm5d.threelayer.live.json").read_text()
        )

        stake = Decimal(str(strategy["stake_usd"]))
        cap = Decimal(deployment["max_gross_exposure"])
        self.assertEqual(stake, Decimal("5.0"))
        self.assertLessEqual(stake, cap)

    def test_live_profile_declares_exactly_pm5d_window(self) -> None:
        strategy = tomllib.loads(
            (STRATEGY_DIR / "02-pm5d-threelayer.live.toml").read_text()
        )["strategy"]

        self.assertEqual(strategy["allowed_window_secs"], [300])
        self.assertEqual(strategy["three_layer_strategy_profile"], "settlement_probability")
        self.assertEqual(strategy["max_positions"], 1)
        self.assertEqual(strategy["max_daily_trades"], 10)

    def test_live_gate_requires_and_normalizes_wallet(self) -> None:
        script = (ROOT / "scripts" / "drills" / "pm5d_threelayer_live_gate.sh").read_text()

        self.assertIn("PLOY_LIVE_ACCOUNT_ID", script)
        self.assertIn("NORMALIZED_LIVE_ACCOUNT_ID", script)
        self.assertIn("trading principal", script)
        self.assertIn("trading readiness", script)
        self.assertIn('require_env_key "POLYMARKET_PRIVATE_KEY"', script)
        self.assertIn("poly1271 is not supported", script)
        self.assertIn("proxy:PROXY", script)
        self.assertIn("gnosis_safe:SAFE", script)
        self.assertIn("does not match execution principal", script)
        self.assertIn("mktemp", script)
        self.assertIn('deployments apply "$RENDERED_MANIFEST"', script)
        self.assertIn("Only the protected live-approval workflow may resume", script)
        self.assertNotIn("--go-live", script)
        self.assertNotIn('deployments resume "$DEPLOYMENT_ID"', script)
        self.assertLess(
            script.index("trading readiness"),
            script.index('deployments apply "$RENDERED_MANIFEST"'),
        )

    def test_live_gate_manifest_config_parity_fixture_executes(self) -> None:
        script = (ROOT / "scripts" / "drills" / "pm5d_threelayer_live_gate.sh").read_text()
        start_marker = "python3 - \"$MANIFEST\" \"$DRYRUN_CONFIG\" \"$LIVE_CONFIG\" <<'PY'\n"
        start = script.index(start_marker) + len(start_marker)
        parity_program = script[start : script.index("\nPY\n", start)]

        completed = subprocess.run(
            [
                "python3",
                "-",
                str(DEPLOYMENT_DIR / "pm5d.threelayer.live.json"),
                str(
                    STRATEGY_DIR
                    / "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
                ),
                str(STRATEGY_DIR / "02-pm5d-threelayer.live.toml"),
            ],
            input=parity_program,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("manifest/config gate: paused apply only", completed.stdout)

    def test_paper_manifests_use_paper_namespace(self) -> None:
        manifests = [
            json.loads(path.read_text())
            for path in sorted(DEPLOYMENT_DIR.glob("*.json"))
        ]
        paper_manifests = [
            manifest for manifest in manifests if manifest["runtime_mode"] == "paper"
        ]

        self.assertTrue(paper_manifests)
        for manifest in paper_manifests:
            self.assertTrue(
                manifest["account_id"].startswith("paper:"),
                manifest["deployment_id"],
            )

    def test_pm5d_threelayer_recording_sources_are_explicit_and_bounded(self) -> None:
        dryrun_configs = sorted(STRATEGY_DIR.glob("02-pm5d-threelayer.*-dryrun.toml"))
        self.assertGreaterEqual(len(dryrun_configs), 4)

        recorders = []
        for path in dryrun_configs:
            config = tomllib.loads(path.read_text())
            runtime = config.get("runtime", {})
            record_path = runtime.get("record_market_updates_to")
            if record_path:
                recorders.append((path.name, runtime))

        self.assertEqual(
            [name for name, _ in recorders],
            [
                "02-pm5d-threelayer.obi-soft-dryrun.toml",
                "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
            ],
        )
        self.assertEqual(
            [runtime["record_market_updates_to"] for _, runtime in recorders],
            [
                "/opt/ploy/data/recordings/pm5d-threelayer-canonical.ndjson",
                "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
            ],
        )
        self.assertEqual(
            len({runtime["record_market_updates_to"] for _, runtime in recorders}),
            len(recorders),
        )
        for _, runtime in recorders:
            self.assertGreater(runtime["record_market_updates_max_records"], 0)
            self.assertGreater(runtime["record_market_updates_max_bytes"], 0)

    def test_settlement_probability_dryrun_deployment_exposure_covers_strategy_sizing(self) -> None:
        strategy = tomllib.loads(
            (
                STRATEGY_DIR
                / "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
            ).read_text()
        )
        deployment = json.loads(
            (
                DEPLOYMENT_DIR
                / "pm5d.threelayer.settlement-probability-btc-eth.dryrun.json"
            ).read_text()
        )

        strategy_config = strategy["strategy"]
        expected_exposure = Decimal(str(strategy_config["stake_usd"])) * Decimal(
            str(strategy_config["max_positions"])
        )
        deployment_limit = Decimal(deployment["max_gross_exposure"])

        self.assertEqual(
            deployment["bundle_id"],
            "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun",
        )
        self.assertGreaterEqual(deployment_limit, expected_exposure)

    def test_settlement_probability_dryrun_uses_reviewed_autofactor_candidate(self) -> None:
        strategy = tomllib.loads(
            (
                STRATEGY_DIR
                / "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
            ).read_text()
        )["strategy"]

        self.assertEqual(strategy["three_layer_strategy_profile"], "settlement_probability")
        self.assertEqual(
            strategy["three_layer_autofactor_runtime_score"],
            "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
        )
        self.assertEqual(Decimal(str(strategy["three_layer_min_entry_score"])), Decimal("0.10"))

    def test_settlement_probability_dryrun_disables_daily_trade_cap(self) -> None:
        strategy = tomllib.loads(
            (
                STRATEGY_DIR
                / "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
            ).read_text()
        )["strategy"]

        self.assertEqual(strategy["max_daily_trades"], 0)


if __name__ == "__main__":
    unittest.main()
