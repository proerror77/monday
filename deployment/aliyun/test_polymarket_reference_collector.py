import importlib.util
import json
import sys
import tempfile
import unittest
from unittest import mock
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace


MODULE_PATH = Path(__file__).with_name("polymarket_reference_collector.py")
SPEC = importlib.util.spec_from_file_location("polymarket_reference_collector", MODULE_PATH)
COLLECTOR = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = COLLECTOR
SPEC.loader.exec_module(COLLECTOR)


def sample_market(**overrides):
    market = {
        "id": "2916403",
        "question": "Dogecoin Up or Down - July 14, 11:00PM-11:15PM ET",
        "conditionId": "0xcondition",
        "slug": "doge-updown-15m-1784084400",
        "startDate": "2026-07-15T03:00:00Z",
        "endDate": "2026-07-15T03:15:00Z",
        "closed": False,
        "clobTokenIds": '["up-token","down-token"]',
        "outcomes": '["Up","Down"]',
        "outcomePrices": '["0.445","0.555"]',
        "volume": 100,
        "orderPriceMinTickSize": 0.01,
        "orderMinSize": 5,
        "makerBaseFee": 1000,
        "takerBaseFee": 1000,
    }
    market.update(overrides)
    return market


class ReferenceCollectorUnitTests(unittest.TestCase):
    def test_v2_trade_identity_migration_reopens_completed_markets(self):
        with tempfile.TemporaryDirectory() as directory:
            spool = Path(directory)
            (spool / "collector-state.json").write_text(
                json.dumps(
                    {
                        "markets": {"market-1": {"trade_complete": True}},
                        "trade_seen": {"0xcondition": {"old-id": 1}},
                    }
                ),
                encoding="utf-8",
            )
            (spool / COLLECTOR.ACTIVE_TAPE).write_text("legacy\n", encoding="utf-8")
            collector = COLLECTOR.ReferenceCollector(
                SimpleNamespace(spool_dir=spool, symbols=["BTCUSDT"])
            )

            self.assertEqual(collector.state["trade_id_version"], "v2")
            self.assertEqual(collector.state["trade_seen"], {})
            self.assertEqual(len(list(spool.glob("superseded-v1-market-updates.ndjson.*"))), 1)
            self.assertFalse(collector.state["markets"]["market-1"]["trade_complete"])
            collector.writer.close()

    def test_target_market_keeps_all_seven_assets_and_supported_windows(self):
        symbols = {symbol for symbol, _aliases in COLLECTOR.SYMBOL_ALIASES}
        questions = {
            "BTCUSDT": "Bitcoin Up or Down - 5 minutes",
            "ETHUSDT": "Ethereum Up or Down - 5 minutes",
            "SOLUSDT": "Solana Up or Down - 5 minutes",
            "XRPUSDT": "XRP Up or Down - 5 minutes",
            "DOGEUSDT": "Dogecoin Up or Down - 5 minutes",
            "HYPEUSDT": "Hyperliquid Up or Down - 5 minutes",
            "BNBUSDT": "BNB Up or Down - 5 minutes",
        }
        for expected, question in questions.items():
            market = sample_market(
                question=question,
                slug=f"{expected.lower()}-updown-5m-1784084400",
                startDate="2026-07-15T03:00:00Z",
                endDate="2026-07-15T03:05:00Z",
            )
            self.assertEqual(COLLECTOR.is_target_market(market, symbols), (expected, 300))

    def test_settlement_requires_closed_terminal_prices(self):
        market = sample_market(closed=True, outcomePrices='["0.0005","0.9995"]')
        update = COLLECTOR.settlement_from_market(
            market, "DOGEUSDT", 900, "2026-07-15T03:16:00Z"
        )
        self.assertIsNotNone(update)
        self.assertEqual(update["winning_token_id"], "down-token")
        self.assertFalse(update["resolved_up_won"])
        self.assertEqual(update["market"]["makerBaseFee"], 1000)
        self.assertIsNone(
            COLLECTOR.settlement_from_market(
                sample_market(), "DOGEUSDT", 900, "2026-07-15T03:16:00Z"
            )
        )

    def test_trade_updates_are_stable_and_deduplicated_across_polls(self):
        collector = COLLECTOR.ReferenceCollector.__new__(COLLECTOR.ReferenceCollector)
        collector.state = {"trade_seen": {}}
        collector.args = SimpleNamespace(market_lookback_secs=7200)
        now = datetime(2026, 7, 15, 3, 10, tzinfo=timezone.utc)
        trade = {
            "proxyWallet": "0xwallet",
            "side": "BUY",
            "asset": "up-token",
            "conditionId": "0xcondition",
            "size": 10,
            "price": 0.78,
            "timestamp": int(now.timestamp()) - 5,
            "outcome": "Up",
            "outcomeIndex": 0,
            "transactionHash": "0xtx",
        }
        second_trade = dict(trade, proxyWallet="0xother", size=11, price=0.79)
        first, malformed = collector.trade_updates(
            "2916403", "0xcondition", "DOGEUSDT", 900, [trade, second_trade], now
        )
        second, second_malformed = collector.trade_updates(
            "2916403", "0xcondition", "DOGEUSDT", 900, [trade, second_trade], now
        )
        self.assertEqual(len(first), 2)
        self.assertEqual(second, [])
        self.assertEqual((malformed, second_malformed), ({}, {}))
        self.assertEqual(first[0]["market_id"], "2916403")
        self.assertEqual(first[0]["record_id_version"], "v2")
        self.assertEqual(first[0]["record_id"], COLLECTOR.stable_trade_id(trade))
        self.assertNotEqual(
            COLLECTOR.stable_trade_id(trade), COLLECTOR.stable_trade_id(second_trade)
        )

    def test_staged_trade_updates_do_not_advance_live_state(self):
        collector = COLLECTOR.ReferenceCollector.__new__(COLLECTOR.ReferenceCollector)
        collector.state = {"trade_seen": {}}
        collector.args = SimpleNamespace(market_lookback_secs=7200)
        staged = {"trade_seen": {}}
        now = datetime(2026, 7, 15, 3, 10, tzinfo=timezone.utc)
        trade = {
            "proxyWallet": "0xwallet",
            "side": "BUY",
            "asset": "up-token",
            "conditionId": "0xcondition",
            "size": 10,
            "price": 0.78,
            "timestamp": int(now.timestamp()) - 5,
            "outcome": "Up",
            "outcomeIndex": 0,
            "transactionHash": "0xtx",
        }

        collector.trade_updates(
            "2916403", "0xcondition", "DOGEUSDT", 900, [trade], now, staged
        )

        self.assertEqual(collector.state, {"trade_seen": {}})
        self.assertIn(COLLECTOR.stable_trade_id(trade), staged["trade_seen"]["0xcondition"])

    def test_v2_replay_preserves_all_prints_that_collided_under_v1(self):
        collector = COLLECTOR.ReferenceCollector.__new__(COLLECTOR.ReferenceCollector)
        collector.args = SimpleNamespace(market_lookback_secs=7200)
        now = datetime(2026, 7, 15, 3, 10, tzinfo=timezone.utc)
        first = {
            "transactionHash": "0xtx",
            "conditionId": "0xcondition",
            "asset": "up-token",
            "side": "BUY",
            "timestamp": int(now.timestamp()) - 5,
            "proxyWallet": "0xfirst",
            "size": 10,
            "price": 0.78,
            "outcome": "Up",
            "outcomeIndex": 0,
        }
        collision = dict(first, proxyWallet="0xsecond", size=11, price=0.79)
        collector.state = {"trade_seen": {}}

        updates, malformed = collector.trade_updates(
            "2916403", "0xcondition", "DOGEUSDT", 900, [first, collision], now
        )

        self.assertEqual(
            [item["record_id"] for item in updates],
            [COLLECTOR.stable_trade_id(first), COLLECTOR.stable_trade_id(collision)],
        )
        self.assertEqual(malformed, {})

    def test_malformed_trade_is_counted_without_blocking_valid_rows(self):
        collector = COLLECTOR.ReferenceCollector.__new__(COLLECTOR.ReferenceCollector)
        collector.args = SimpleNamespace(market_lookback_secs=7200)
        collector.state = {"trade_seen": {}}
        now = datetime(2026, 7, 15, 3, 10, tzinfo=timezone.utc)
        valid = {
            "transactionHash": "0xtx",
            "conditionId": "0xcondition",
            "asset": "up-token",
            "side": "BUY",
            "timestamp": int(now.timestamp()) - 5,
            "proxyWallet": "0xwallet",
            "size": 10,
            "price": 0.78,
            "outcome": "Up",
            "outcomeIndex": 0,
        }

        updates, malformed = collector.trade_updates(
            "2916403",
            "0xcondition",
            "DOGEUSDT",
            900,
            [
                dict(valid, timestamp=None),
                dict(valid, side="HOLD"),
                dict(valid, size="bad"),
                dict(valid, price=2),
                dict(valid, transactionHash=""),
                valid,
            ],
            now,
        )

        self.assertEqual(
            malformed,
            {
                "invalid_timestamp": 1,
                "invalid_side": 1,
                "invalid_size": 1,
                "invalid_price": 1,
                "missing_transaction_hash": 1,
            },
        )
        self.assertEqual(len(updates), 1)

    def test_trade_finalization_requires_lag_and_stable_polls_and_resets_on_late_trade(self):
        settled_at = datetime(2026, 7, 15, 3, 0, tzinfo=timezone.utc)
        tracked = {}
        self.assertFalse(
            COLLECTOR.advance_trade_finalization(
                tracked, settled_at, COLLECTOR.iso_z(settled_at), 0, False, False, 1800, 2
            )
        )
        after_lag = settled_at.replace(minute=30)
        self.assertFalse(
            COLLECTOR.advance_trade_finalization(
                tracked, after_lag, COLLECTOR.iso_z(after_lag), 0, False, True, 1800, 2
            )
        )
        late_trade_at = settled_at.replace(minute=31)
        self.assertFalse(
            COLLECTOR.advance_trade_finalization(
                tracked,
                late_trade_at,
                COLLECTOR.iso_z(late_trade_at),
                1,
                False,
                True,
                1800,
                2,
            )
        )
        final_poll_one = settled_at.replace(hour=4, minute=1)
        self.assertFalse(
            COLLECTOR.advance_trade_finalization(
                tracked,
                final_poll_one,
                COLLECTOR.iso_z(final_poll_one),
                0,
                False,
                True,
                1800,
                2,
            )
        )
        final_poll_two = settled_at.replace(hour=4, minute=2)
        self.assertTrue(
            COLLECTOR.advance_trade_finalization(
                tracked,
                final_poll_two,
                COLLECTOR.iso_z(final_poll_two),
                0,
                False,
                True,
                1800,
                2,
            )
        )


class TapeWriterTests(unittest.TestCase):
    def test_write_all_retries_short_writes(self):
        class ShortWriter:
            def __init__(self):
                self.data = bytearray()

            def write(self, value):
                count = min(2, len(value))
                self.data.extend(value[:count])
                return count

        handle = ShortWriter()
        COLLECTOR.TapeWriter._write_all(handle, b"complete")
        self.assertEqual(bytes(handle.data), b"complete")

    def test_failed_batch_is_truncated_and_sequence_is_restored(self):
        with tempfile.TemporaryDirectory() as directory:
            spool = Path(directory)
            writer = COLLECTOR.TapeWriter(spool)
            with mock.patch.object(
                COLLECTOR.os, "fsync", side_effect=[OSError("disk full"), None]
            ):
                with self.assertRaisesRegex(OSError, "disk full"):
                    writer.write_updates(
                        [{"kind": "market_metadata"}, {"kind": "polymarket_trade"}],
                        datetime(2026, 7, 15, 3, 0, tzinfo=timezone.utc),
                    )
            self.assertEqual(writer.sequence, 0)
            self.assertEqual((spool / COLLECTOR.ACTIVE_TAPE).read_bytes(), b"")
            writer.close()

    def test_rotation_produces_closed_zero_based_sessions(self):
        with tempfile.TemporaryDirectory() as directory:
            spool = Path(directory)
            writer = COLLECTOR.TapeWriter(spool)
            writer.write_updates(
                [{"kind": "market_metadata"}],
                datetime(2026, 7, 15, 3, 59, tzinfo=timezone.utc),
            )
            writer.write_updates(
                [{"kind": "market_metadata"}],
                datetime(2026, 7, 15, 4, 0, tzinfo=timezone.utc),
            )
            writer.close()
            closed = list(spool.glob("market-updates.*.ndjson"))
            self.assertEqual(len(closed), 1)
            self.assertEqual(json.loads(closed[0].read_text())["sequence"], 0)
            self.assertEqual(json.loads((spool / COLLECTOR.ACTIVE_TAPE).read_text())["sequence"], 0)

    def test_startup_truncates_only_incomplete_tail_and_resumes_sequence(self):
        with tempfile.TemporaryDirectory() as directory:
            spool = Path(directory)
            active = spool / COLLECTOR.ACTIVE_TAPE
            active.write_bytes(
                b'{"sequence":0,"recorded_at":"2026-07-15T03:00:00Z","update":{}}\n'
                b'{"sequence":1'
            )
            writer = COLLECTOR.TapeWriter(spool)
            writer.write_updates(
                [{"kind": "market_metadata"}],
                datetime(2026, 7, 15, 3, 1, tzinfo=timezone.utc),
            )
            writer.close()
            rows = [json.loads(line) for line in active.read_text().splitlines()]
            self.assertEqual([row["sequence"] for row in rows], [0, 1])

    def test_collector_recovers_trade_dedupe_state_from_durable_tape(self):
        with tempfile.TemporaryDirectory() as directory:
            spool = Path(directory)
            writer = COLLECTOR.TapeWriter(spool)
            writer.write_updates(
                [
                    {
                        "kind": "polymarket_trade",
                        "market_id": "market-1",
                        "condition_id": "0xcondition",
                        "record_id": "trade-1",
                        "record_id_version": "v2",
                        "trade_ts_unix": 1784084400,
                    }
                ],
                datetime(2026, 7, 15, 3, 0, tzinfo=timezone.utc),
            )
            writer.close()
            collector = COLLECTOR.ReferenceCollector.__new__(COLLECTOR.ReferenceCollector)
            collector.state = {"markets": {}, "trade_seen": {}}
            collector.writer = COLLECTOR.TapeWriter(spool)

            collector.recover_state_from_active_tape()
            collector.writer.close()

            self.assertEqual(
                collector.state["trade_seen"]["0xcondition"]["trade-1"], 1784084400
            )


if __name__ == "__main__":
    unittest.main()
