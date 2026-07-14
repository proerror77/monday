import unittest

from scripts.validate_live_promotion_evidence import validate


SHA = "a" * 40
DEPLOYMENT_ID = "pm5d.threelayer.settlement-probability-btc-eth.dryrun"
CONFIG_SHA = "b" * 64
RECORDING_SHA = "c" * 64
RUNTIME_SCORE = "autofactor_formula:test"


def ready_evidence():
    replay = {
        "evidence_stage": "executable_replay",
        "basis": "runtime_market_update_replay",
        "promotion_ready": True,
        "blocking_risk_flags": [],
        "runner_git_sha": SHA,
        "deployment_id": DEPLOYMENT_ID,
        "strategy_profile": "settlement_probability",
        "runtime_score": RUNTIME_SCORE,
        "config_sha256": CONFIG_SHA,
        "recording_sha256": RECORDING_SHA,
        "decision_contract": {"official_settlement": True, "full_depth_entry": True},
        "metrics": {"trade_count": 20, "unique_event_count": 20, "roi": 0.1},
    }
    parity = {
        "blocking_risk_flags": [],
        "filters": {"deployment_id": DEPLOYMENT_ID},
        "runtime_evidence_comparison": {
            "strict_parity_ready": True,
            "orders": {"shared_count": 20},
            "fills": {"shared_count": 20},
        },
    }
    parity_provenance = {
        "deployment_id": DEPLOYMENT_ID,
        "config_sha256": CONFIG_SHA,
        "recording_sha256": RECORDING_SHA,
        "runner_source": "workflow_ref",
        "runner_git_sha": SHA,
        "skip_settlement_exits": False,
    }
    dryrun = {
        "strategies": [
            {
                "deployment_id": DEPLOYMENT_ID,
                "summary": {
                    "closed_trades": 30,
                    "realized_pnl": 12.0,
                    "open_positions": 0,
                },
                "metrics": {"max_drawdown": -10.0},
            }
        ]
    }
    return replay, parity, parity_provenance, dryrun


class LivePromotionEvidenceTests(unittest.TestCase):
    def test_ready_evidence_passes(self):
        replay, parity, parity_provenance, dryrun = ready_evidence()
        result = validate(
            replay,
            parity,
            parity_provenance,
            dryrun,
            git_sha=SHA,
            min_replay_trades=20,
            min_dryrun_closed_trades=30,
            max_drawdown_usd=25,
            expected_deployment_id=DEPLOYMENT_ID,
            expected_strategy_profile="settlement_probability",
            expected_runtime_score=RUNTIME_SCORE,
            expected_config_sha256=CONFIG_SHA,
        )
        self.assertTrue(result["ready_for_human_live_approval"])
        self.assertEqual(result["failures"], [])

    def test_each_safety_surface_fails_closed(self):
        mutations = {
            "sha": lambda r, p, d: r.update(runner_git_sha="b" * 40),
            "replay": lambda r, p, d: r.update(promotion_ready=False),
            "parity": lambda r, p, d: p["runtime_evidence_comparison"].update(strict_parity_ready=False),
            "drawdown": lambda r, p, d: d["strategies"][0]["metrics"].update(max_drawdown=-26),
            "open_positions": lambda r, p, d: d["strategies"][0]["summary"].update(open_positions=1),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                replay, parity, parity_provenance, dryrun = ready_evidence()
                mutate(replay, parity, dryrun)
                result = validate(
                    replay,
                    parity,
                    parity_provenance,
                    dryrun,
                    git_sha=SHA,
                    min_replay_trades=20,
                    min_dryrun_closed_trades=30,
                    max_drawdown_usd=25,
                    expected_deployment_id=DEPLOYMENT_ID,
                    expected_strategy_profile="settlement_probability",
                    expected_runtime_score=RUNTIME_SCORE,
                    expected_config_sha256=CONFIG_SHA,
                )
                self.assertFalse(result["ready_for_human_live_approval"])

    def test_inputs_cannot_weaken_policy_floors(self):
        replay, parity, parity_provenance, dryrun = ready_evidence()
        result = validate(
            replay,
            parity,
            parity_provenance,
            dryrun,
            git_sha=SHA,
            min_replay_trades=1,
            min_dryrun_closed_trades=1,
            max_drawdown_usd=1000,
            expected_deployment_id=DEPLOYMENT_ID,
            expected_strategy_profile="settlement_probability",
            expected_runtime_score=RUNTIME_SCORE,
            expected_config_sha256=CONFIG_SHA,
        )
        self.assertFalse(result["ready_for_human_live_approval"])
        self.assertIn("min_replay_trades_below_policy_floor", result["failures"])
        self.assertIn("max_drawdown_policy_limit_invalid", result["failures"])

    def test_parity_identity_must_match_replay_and_live_source(self):
        replay, parity, parity_provenance, dryrun = ready_evidence()
        parity_provenance["recording_sha256"] = "d" * 64
        parity_provenance["config_sha256"] = "e" * 64
        result = validate(
            replay,
            parity,
            parity_provenance,
            dryrun,
            git_sha=SHA,
            min_replay_trades=20,
            min_dryrun_closed_trades=30,
            max_drawdown_usd=25,
            expected_deployment_id=DEPLOYMENT_ID,
            expected_strategy_profile="settlement_probability",
            expected_runtime_score=RUNTIME_SCORE,
            expected_config_sha256=CONFIG_SHA,
        )
        self.assertFalse(result["ready_for_human_live_approval"])
        self.assertIn("parity_config_sha_mismatch", result["failures"])
        self.assertIn("parity_recording_sha_mismatch", result["failures"])


if __name__ == "__main__":
    unittest.main()
