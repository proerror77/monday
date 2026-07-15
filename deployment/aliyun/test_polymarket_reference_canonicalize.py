import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


BASE = Path(__file__).parent
sys.path.insert(0, str(BASE))
SPEC = importlib.util.spec_from_file_location(
    "polymarket_reference_canonicalize", BASE / "polymarket_reference_canonicalize.py"
)
CANONICALIZE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(CANONICALIZE)


class CanonicalizeTests(unittest.TestCase):
    def test_union_rekeys_v1_trades_and_removes_only_v2_duplicates(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first.ndjson"
            second = root / "second.ndjson"
            output = root / "market-updates.repair.ndjson"
            trade = {
                "transactionHash": "0xtx",
                "conditionId": "0xcondition",
                "asset": "token",
                "side": "BUY",
                "timestamp": 1784084400,
                "proxyWallet": "0xwallet",
                "size": 10,
                "price": 0.5,
                "outcome": "Up",
                "outcomeIndex": 0,
            }
            collision = dict(trade, proxyWallet="0xother", size=11)
            rows = [
                {"sequence": 0, "recorded_at": "2026-07-15T03:00:00Z", "update": {"kind": "polymarket_trade", "record_id": "v1", "trade": trade}},
                {"sequence": 1, "recorded_at": "2026-07-15T03:00:01Z", "update": {"kind": "polymarket_trade", "record_id": "v1", "trade": collision}},
            ]
            first.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
            second.write_text(json.dumps(rows[0]) + "\n", encoding="utf-8")

            summary = CANONICALIZE.canonicalize([first, second], output)
            written = [json.loads(line) for line in output.read_text().splitlines()]

            self.assertEqual(summary["canonical_v2_trades"], 2)
            self.assertEqual(summary["duplicate_trades_removed"], 1)
            self.assertEqual([row["sequence"] for row in written], [0, 1])
            self.assertEqual(
                {row["update"]["record_id_version"] for row in written}, {"v2"}
            )
            self.assertEqual(len({row["update"]["record_id"] for row in written}), 2)


if __name__ == "__main__":
    unittest.main()
