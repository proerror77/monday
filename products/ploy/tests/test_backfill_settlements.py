import unittest
from datetime import datetime, timezone

from scripts.backfill_settlements import (
    coverage_blockers,
    parse_utc_ts,
    report_sha256,
    settlement_rows_from_gamma,
    settlement_rows_with_reason_from_gamma,
)


class BackfillSettlementsTest(unittest.TestCase):
    def test_parse_utc_ts_returns_timezone_aware_datetime(self) -> None:
        parsed = parse_utc_ts("2026-05-16T00:00:00Z")

        self.assertEqual(datetime(2026, 5, 16, tzinfo=timezone.utc), parsed)
        self.assertIs(timezone.utc, parsed.tzinfo)

    def test_parse_utc_ts_accepts_empty_optional_end(self) -> None:
        self.assertIsNone(parse_utc_ts(""))

    def test_settlement_rows_from_gamma_requires_closed_binary_payouts(self) -> None:
        rows = settlement_rows_from_gamma(
            {
                "closed": True,
                "clobTokenIds": '["up-token", "down-token"]',
                "outcomePrices": '["1", "0"]',
            },
            up_token="up-token",
            down_token="down-token",
        )

        self.assertEqual(
            [("up-token", "winner", 1.0), ("down-token", "loser", 0.0)],
            rows,
        )

    def test_settlement_rows_reports_skip_reasons(self) -> None:
        cases = [
            ({"closed": False}, "not_closed"),
            ({"closed": True, "clobTokenIds": "not json", "outcomePrices": "[]"}, "malformed_gamma_payload"),
            (
                {
                    "closed": True,
                    "clobTokenIds": '["up-token", "down-token"]',
                    "outcomePrices": '["0.50", "0.50"]',
                },
                "unresolved_prices",
            ),
            (
                {
                    "closed": True,
                    "clobTokenIds": '["other-up", "other-down"]',
                    "outcomePrices": '["1", "0"]',
                },
                "token_mismatch",
            ),
            (
                {
                    "closed": True,
                    "clobTokenIds": '["up-token", "up-token"]',
                    "outcomePrices": '["1", "0"]',
                },
                "token_mismatch",
            ),
        ]

        for payload, reason in cases:
            with self.subTest(reason=reason):
                rows, actual_reason = settlement_rows_with_reason_from_gamma(
                    payload,
                    up_token="up-token",
                    down_token="down-token",
                )
                self.assertEqual([], rows)
                self.assertEqual(reason, actual_reason)

    def test_coverage_blockers_require_execute_complete_binary_settlement(self) -> None:
        valid_report = {
            "dry_run": False,
            "candidate_market_count": 2,
            "settled_count": 1,
            "unchanged_count": 3,
            "active_reset_count": 0,
            "open_market_count": 0,
            "malformed_payload_count": 0,
            "unresolved_price_count": 0,
            "token_mismatch_count": 0,
            "skipped_count": 0,
            "error_count": 0,
        }
        self.assertEqual([], coverage_blockers(valid_report))

        blocked_report = {
            **valid_report,
            "dry_run": True,
            "unchanged_count": 2,
            "token_mismatch_count": 1,
        }
        self.assertEqual(
            [
                "dry_run_not_durable_coverage",
                "settlement_token_count:3!=4",
                "token_mismatch_count:1",
            ],
            coverage_blockers(blocked_report),
        )
        duplicate_count_report = {
            **valid_report,
            "unchanged_count": 4,
        }
        self.assertEqual(
            ["settlement_token_count:5!=4"],
            coverage_blockers(duplicate_count_report),
        )

    def test_report_sha256_is_canonical(self) -> None:
        self.assertEqual(
            report_sha256({"b": 2, "a": 1}),
            report_sha256({"a": 1, "b": 2}),
        )


if __name__ == "__main__":
    unittest.main()
