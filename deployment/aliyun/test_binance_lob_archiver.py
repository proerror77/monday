import importlib.util
import io
import tempfile
import unittest
from email.message import Message
from pathlib import Path
from types import SimpleNamespace
from urllib.error import HTTPError
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("binance_lob_archiver.py")
SPEC = importlib.util.spec_from_file_location("binance_lob_archiver", MODULE_PATH)
ARCHIVER = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(ARCHIVER)


def diff(first_update_id, final_update_id, bids=None, asks=None):
    return {
        "U": first_update_id,
        "u": final_update_id,
        "b": bids or [],
        "a": asks or [],
    }


class OrderBookStateTests(unittest.TestCase):
    def test_snapshot_bridges_buffered_diff_and_deletes_zero_quantity(self):
        state = ARCHIVER.OrderBookState("BTCUSDT")
        state.apply_diff(diff(98, 100, bids=[["99", "3"]]))
        state.apply_diff(
            diff(101, 102, bids=[["100", "0"], ["101", "4"]])
        )

        state.install_snapshot(
            {
                "lastUpdateId": 100,
                "bids": [["100", "2"]],
                "asks": [["102", "5"]],
            }
        )

        self.assertTrue(state.synced)
        self.assertEqual(state.last_update_id, 102)
        self.assertNotIn("100", state.bids)
        self.assertEqual(state.bids["101"], "4")

    def test_sequence_gap_is_rejected_after_sync(self):
        state = ARCHIVER.OrderBookState("ETHUSDT")
        state.install_snapshot(
            {"lastUpdateId": 10, "bids": [["9", "1"]], "asks": [["11", "1"]]}
        )
        state.apply_diff(diff(11, 11))

        with self.assertRaises(ARCHIVER.SequenceGap) as caught:
            state.apply_diff(diff(13, 13))

        self.assertEqual(caught.exception.expected, 12)
        self.assertEqual(caught.exception.first_update_id, 13)
        self.assertFalse(state.synced)

    def test_checkpoint_round_trips_full_book(self):
        state = ARCHIVER.OrderBookState("SOLUSDT")
        state.install_snapshot(
            {
                "lastUpdateId": 20,
                "bids": [["20", "2"], ["19", "1"]],
                "asks": [["21", "3"], ["22", "4"]],
            }
        )
        state.apply_diff(diff(21, 21, asks=[["21", "0"], ["23", "5"]]))

        checkpoint = state.checkpoint("session-1")

        self.assertEqual(checkpoint["last_update_id"], 21)
        self.assertEqual(checkpoint["bids"], [["20", "2"], ["19", "1"]])
        self.assertEqual(checkpoint["asks"], [["22", "4"], ["23", "5"]])

    def test_usdm_uses_snapshot_bridge_then_previous_update_id(self):
        state = ARCHIVER.OrderBookState("BTCUSDT", "usdm")
        state.install_snapshot(
            {"lastUpdateId": 100, "bids": [["99", "1"]], "asks": [["101", "1"]]}
        )
        state.apply_diff({**diff(99, 101), "pu": 98})
        self.assertTrue(state.bridged)

        state.apply_diff({**diff(102, 103), "pu": 101})
        self.assertEqual(state.last_update_id, 103)

        with self.assertRaises(ARCHIVER.SequenceGap):
            state.apply_diff({**diff(104, 104), "pu": 102})

    def test_usdm_can_bridge_when_global_update_range_moved_past_snapshot(self):
        state = ARCHIVER.OrderBookState("1000SHIBUSDT", "usdm")
        state.install_snapshot(
            {"lastUpdateId": 100, "bids": [["9", "1"]], "asks": [["11", "1"]]}
        )

        state.apply_diff({**diff(150, 175), "pu": 100})

        self.assertTrue(state.bridged)
        self.assertEqual(state.last_update_id, 175)

    def test_snapshot_fetch_retries_rate_limit_without_restarting_session(self):
        rate_limit = HTTPError(
            "https://example.test/depth",
            429,
            "Too Many Requests",
            Message(),
            None,
        )
        response = io.BytesIO(
            b'{"lastUpdateId":1,"bids":[],"asks":[]}'
        )
        with (
            patch.object(
                ARCHIVER.urllib.request,
                "urlopen",
                side_effect=[rate_limit, response],
            ),
            patch.object(ARCHIVER.time, "sleep") as sleep,
        ):
            snapshot = ARCHIVER.fetch_snapshot_sync("btcusdt")

        self.assertEqual(snapshot["snapshot"]["lastUpdateId"], 1)
        sleep.assert_called_once_with(1)

    def test_pending_budget_is_shared_across_symbols(self):
        budget = ARCHIVER.PendingBudget(1)
        btc = ARCHIVER.OrderBookState("BTCUSDT", pending_budget=budget)
        eth = ARCHIVER.OrderBookState("ETHUSDT", pending_budget=budget)
        btc.apply_diff(diff(1, 1))

        with self.assertRaisesRegex(RuntimeError, "pending diff budget"):
            eth.apply_diff(diff(1, 1))

        btc.install_snapshot(
            {"lastUpdateId": 1, "bids": [], "asks": []}
        )
        eth.apply_diff(diff(1, 1))


class RuntimeContractTests(unittest.IsolatedAsyncioTestCase):
    async def test_websocket_close_timeout_fits_cancellation_budget(self):
        options = {}

        class Connection:
            async def __aenter__(self):
                return SimpleNamespace()

            async def __aexit__(self, *args):
                return False

        def connect(url, **kwargs):
            options.update(kwargs)
            return Connection()

        stop = ARCHIVER.asyncio.Event()
        stop.set()
        with patch.object(ARCHIVER, "connect", new=connect):
            await ARCHIVER.receive_url("wss://example.test", ARCHIVER.asyncio.Queue(), stop)

        self.assertEqual(options["close_timeout"], 1)
        self.assertLess(options["close_timeout"], ARCHIVER.TASK_CANCEL_TIMEOUT_SECONDS)

    async def asyncSetUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.original_spool = ARCHIVER.SPOOL_DIR
        ARCHIVER.SPOOL_DIR = Path(self.temporary.name)

    async def asyncTearDown(self):
        ARCHIVER.SPOOL_DIR = self.original_spool
        self.temporary.cleanup()

    async def test_unsafe_segment_does_not_emit_checkpoint(self):
        runtime = ARCHIVER.ArchiveRuntime()
        state = ARCHIVER.OrderBookState("BTCUSDT")
        state.install_snapshot(
            {"lastUpdateId": 10, "bids": [["9", "1"]], "asks": [["11", "1"]]}
        )
        state.apply_diff(diff(11, 11))
        runtime.segment.mark_replay_unsafe()

        runtime.write_checkpoints({"BTCUSDT": state}, "session-1", "gap")

        self.assertEqual(runtime.segment.counts["checkpoint"], 0)
        runtime.segment.close()

    async def test_finalize_keeps_source_until_manifest_is_committed(self):
        path = ARCHIVER.SPOOL_DIR / "part-1.jsonl.part"
        path.write_text('{"received_at_ns":1,"type":"diff"}\n')
        output = path.with_suffix("").with_suffix(".jsonl.zst")
        temporary_output = output.with_suffix(output.suffix + ".tmp")

        def compress(*args, **kwargs):
            self.assertTrue(path.exists())
            temporary_output.write_bytes(b"compressed")

        with patch.object(ARCHIVER.subprocess, "run", side_effect=compress):
            manifest = ARCHIVER.finalize_segment(
                path,
                ARCHIVER.Counter({"diff": 1}),
                1,
                1,
            )

        self.assertIsNotNone(manifest)
        self.assertFalse(path.exists())
        self.assertTrue(manifest.exists())

    async def test_recovery_drops_truncated_json_lines_before_compression(self):
        path = ARCHIVER.SPOOL_DIR / "part-1.jsonl.part"
        valid = b'{"received_at_ns":1,"type":"diff"}\n'
        path.write_bytes(valid + b'{"received_at_ns":')

        with (
            patch.object(ARCHIVER, "finalize_segment") as finalize,
            self.assertLogs("binance-lob-archiver", level="WARNING") as logs,
        ):
            ARCHIVER.recover_parts()

        self.assertEqual(path.read_bytes(), valid)
        counts = finalize.call_args.args[1]
        self.assertEqual(counts, ARCHIVER.Counter({"diff": 1}))
        self.assertIn("invalid trailing", logs.output[0])

    async def test_recovery_quarantines_invalid_middle_json(self):
        path = ARCHIVER.SPOOL_DIR / "part-1.jsonl.part"
        valid = b'{"received_at_ns":1,"type":"diff"}\n'
        path.write_bytes(valid + b"not-json\n" + valid)

        with (
            patch.object(ARCHIVER, "finalize_segment") as finalize,
            self.assertLogs("binance-lob-archiver", level="ERROR") as logs,
        ):
            ARCHIVER.recover_parts()

        self.assertFalse(path.exists())
        self.assertTrue(path.with_suffix(path.suffix + ".corrupt").exists())
        finalize.assert_not_called()
        self.assertIn("invalid middle", logs.output[0])

    async def test_recovery_quarantines_schema_invalid_middle_json(self):
        path = ARCHIVER.SPOOL_DIR / "part-1.jsonl.part"
        valid = b'{"received_at_ns":1,"type":"diff"}\n'
        path.write_bytes(valid + b"null\n" + valid)

        with (
            patch.object(ARCHIVER, "finalize_segment") as finalize,
            self.assertLogs("binance-lob-archiver", level="ERROR"),
        ):
            ARCHIVER.recover_parts()

        self.assertFalse(path.exists())
        self.assertTrue(path.with_suffix(path.suffix + ".corrupt").exists())
        finalize.assert_not_called()

    async def test_partial_market_state_cannot_emit_replay_safe_checkpoint(self):
        runtime = ARCHIVER.ArchiveRuntime()
        synced = ARCHIVER.OrderBookState("BTCUSDT")
        synced.install_snapshot(
            {"lastUpdateId": 10, "bids": [["9", "1"]], "asks": [["11", "1"]]}
        )
        unsynced = ARCHIVER.OrderBookState("ETHUSDT")

        runtime.write_checkpoints(
            {"BTCUSDT": synced, "ETHUSDT": unsynced},
            "session-1",
            "scheduled",
        )

        self.assertFalse(runtime.segment.replay_safe)
        self.assertEqual(runtime.segment.counts["checkpoint"], 0)
        runtime.segment.close()

    async def test_gap_is_recorded_before_trailing_archive_only_diffs(self):
        runtime = ARCHIVER.ArchiveRuntime()
        state = ARCHIVER.OrderBookState("ETHUSDT")
        state.install_snapshot(
            {"lastUpdateId": 10, "bids": [["9", "1"]], "asks": [["11", "1"]]}
        )
        state.apply_diff(diff(11, 11))

        with self.assertRaises(ARCHIVER.SequenceGap):
            ARCHIVER.archive_and_apply_diff(
                runtime,
                {"ETHUSDT": state},
                "session-1",
                123,
                {"data": {"s": "ETHUSDT", **diff(13, 13)}},
            )

        self.assertFalse(runtime.segment.replay_safe)
        self.assertEqual(runtime.segment.counts["diff"], 1)
        self.assertEqual(runtime.segment.counts["sequence_gap"], 1)
        runtime.segment.close()

    async def test_pending_uploads_are_retried_without_restart(self):
        runtime = ARCHIVER.ArchiveRuntime()

        with patch.object(ARCHIVER, "upload_pending") as pending:
            await runtime.retry_uploads_if_due(force=True)
            assert runtime.upload_thread is not None
            runtime.upload_thread.join(timeout=1)

        pending.assert_called_once_with()
        runtime.segment.close()

    async def test_upload_failure_is_exposed_to_health_checks(self):
        manifest = ARCHIVER.SPOOL_DIR / "failed.manifest.json"
        manifest.write_text("{}")
        with (
            patch.object(ARCHIVER, "upload", side_effect=RuntimeError("oss down")),
            self.assertLogs("binance-lob-archiver", level="ERROR"),
        ):
            ARCHIVER.upload_pending()

        self.assertIsNotNone(ARCHIVER.UPLOAD_STATUS["last_error_at"])
        self.assertEqual(ARCHIVER.UPLOAD_STATUS["last_error"], "oss down")

    async def test_collection_does_not_block_startup_on_pending_uploads(self):
        stop = ARCHIVER.asyncio.Event()

        class Runtime:
            total_gaps = 0

            async def rotate(self, *args, **kwargs):
                return None

            async def finish_uploads(self):
                return None

        async def run_session(*args):
            stop.set()

        with (
            patch.object(ARCHIVER, "recover_parts"),
            patch.object(ARCHIVER, "upload_pending") as upload_pending,
            patch.object(ARCHIVER, "ArchiveRuntime", return_value=Runtime()),
            patch.object(ARCHIVER, "run_session", side_effect=run_session),
            patch.object(ARCHIVER, "SYMBOLS", ("btcusdt",)),
        ):
            await ARCHIVER.collect(stop)

        upload_pending.assert_not_called()

    async def test_stall_watchdog_trips_only_after_timeout(self):
        with patch.object(ARCHIVER, "STALL_TIMEOUT_SECONDS", 60):
            self.assertFalse(ARCHIVER.is_stalled(100, 160))
            self.assertTrue(ARCHIVER.is_stalled(100, 160.1))

    async def test_process_watchdog_has_independent_deadline(self):
        with patch.object(ARCHIVER, "PROCESS_WATCHDOG_SECONDS", 180):
            self.assertFalse(ARCHIVER.process_watchdog_expired(100, 280))
            self.assertTrue(ARCHIVER.process_watchdog_expired(100, 280.1))

    async def test_task_cancellation_is_bounded(self):
        release = ARCHIVER.asyncio.Event()

        async def stubborn():
            try:
                await release.wait()
            except ARCHIVER.asyncio.CancelledError:
                await release.wait()

        task = ARCHIVER.asyncio.create_task(stubborn())
        await ARCHIVER.asyncio.sleep(0)
        with (
            patch.object(ARCHIVER, "TASK_CANCEL_TIMEOUT_SECONDS", 0.01),
            self.assertLogs("binance-lob-archiver", level="ERROR"),
        ):
            with self.assertRaises(ARCHIVER.TaskCancellationStuck):
                await ARCHIVER.cancel_tasks_bounded((task,))

        release.set()
        await task

    async def test_resync_does_not_reuse_expired_initial_deadline(self):
        self.assertTrue(ARCHIVER.bridge_timed_out(False, 100, 101))
        self.assertFalse(ARCHIVER.bridge_timed_out(True, 100, 101))
        synced, deadline = ARCHIVER.begin_resync(200)
        self.assertFalse(synced)
        self.assertEqual(deadline, 200 + ARCHIVER.SYNC_TIMEOUT_SECONDS)

    async def test_disk_watermark_warns_without_stopping_collection(self):
        with (
            patch.object(ARCHIVER, "MIN_FREE_GB", 20),
            patch.object(
                ARCHIVER.shutil,
                "disk_usage",
                return_value=SimpleNamespace(free=19 * 1024**3),
            ),
            self.assertLogs("binance-lob-archiver", level="WARNING") as logs,
        ):
            free_gb, warning = ARCHIVER.warn_if_disk_low()

        self.assertEqual(free_gb, 19.0)
        self.assertTrue(warning)
        self.assertIn("continuing collection", logs.output[0])

    async def test_health_reports_disk_warning(self):
        state = ARCHIVER.OrderBookState("BTCUSDT")
        with (
            patch.object(
                ARCHIVER,
                "disk_headroom",
                return_value=(19.0, True),
            ),
            patch.object(ARCHIVER, "pending_upload_count", return_value=3),
            patch.dict(
                ARCHIVER.UPLOAD_STATUS,
                {
                    "last_success_at": "2026-07-13T12:00:00+00:00",
                    "last_error_at": "2026-07-13T12:01:00+00:00",
                    "last_error": "oss down",
                },
                clear=True,
            ),
        ):
            ARCHIVER.write_health(
                {"BTCUSDT": state}, "session-1", "synced", 0
            )

        health = ARCHIVER.json.loads(
            (ARCHIVER.SPOOL_DIR / "health.json").read_text()
        )
        self.assertEqual(health["disk_free_gb"], 19.0)
        self.assertTrue(health["disk_warning"])
        self.assertEqual(health["disk_warning_threshold_gb"], 20)
        self.assertEqual(health["pending_upload_segments"], 3)
        self.assertTrue(health["upload_warning"])
        self.assertEqual(health["last_upload_error"], "oss down")

    async def test_streams_are_split_into_bounded_websocket_shards(self):
        with (
            patch.object(ARCHIVER, "MARKET", "spot"),
            patch.object(ARCHIVER, "SYMBOLS", ("a", "b", "c")),
            patch.object(ARCHIVER, "WS_SHARD_SIZE", 2),
        ):
            urls = ARCHIVER.stream_urls()

        self.assertEqual(len(urls), 2)
        self.assertIn("a@depth@100ms/b@depth@100ms", urls[0])
        self.assertTrue(urls[1].endswith("c@depth@100ms"))

    async def test_resync_snapshot_waits_without_blocking_queue_consumer(self):
        queue = ARCHIVER.asyncio.Queue(maxsize=1)
        await queue.put(("diff", 1, {}))
        replacement = {
            "symbol": "BTCUSDT",
            "received_at_ns": 2,
            "snapshot": {"lastUpdateId": 1, "bids": [], "asks": []},
        }
        with patch.object(ARCHIVER, "fetch_snapshot_sync", return_value=replacement):
            producer = ARCHIVER.asyncio.create_task(
                ARCHIVER.produce_snapshot("btcusdt", queue)
            )
            await ARCHIVER.asyncio.sleep(0)
            self.assertFalse(producer.done())
            await queue.get()
            await producer

        self.assertEqual((await queue.get())[0], "snapshot")

    async def test_initial_and_resync_snapshots_can_share_one_limiter(self):
        calls = []

        class Limiter:
            async def fetch(self, symbol):
                calls.append(symbol)
                return {
                    "symbol": symbol.upper(),
                    "received_at_ns": len(calls),
                    "request_started_at_ns": len(calls),
                    "snapshot": {"lastUpdateId": len(calls), "bids": [], "asks": []},
                }

        queue = ARCHIVER.asyncio.Queue()
        limiter = Limiter()
        with patch.object(ARCHIVER, "SYMBOLS", ("btcusdt", "ethusdt")):
            await ARCHIVER.produce_snapshots(queue, limiter)
        await ARCHIVER.produce_snapshot("solusdt", queue, limiter)

        self.assertEqual(calls, ["btcusdt", "ethusdt", "solusdt"])

    async def test_all_unavailable_symbols_stay_out_of_refreshed_catalog(self):
        symbols, security_tokens = ARCHIVER.exclude_unavailable_symbols(
            ("badusdt", "oldusdt"),
            ("badusdt", "btcusdt", "oldusdt"),
            ("badusdt", "oldusdt"),
        )

        self.assertEqual(symbols, ("btcusdt",))
        self.assertEqual(security_tokens, ())

    async def test_only_binance_invalid_symbol_error_refreshes_catalog(self):
        invalid_symbol = HTTPError(
            "https://example.test/depth",
            400,
            "Bad Request",
            Message(),
            io.BytesIO(b'{"code":-1121,"msg":"Invalid symbol."}'),
        )
        with patch.object(
            ARCHIVER.urllib.request,
            "urlopen",
            side_effect=invalid_symbol,
        ):
            with self.assertRaises(ARCHIVER.SnapshotUnavailable):
                ARCHIVER.fetch_snapshot_sync("badusdt")

    async def test_host_wide_403_does_not_exclude_symbol(self):
        forbidden = HTTPError(
            "https://example.test/depth",
            403,
            "Forbidden",
            Message(),
            io.BytesIO(b"forbidden"),
        )
        with patch.object(
            ARCHIVER.urllib.request,
            "urlopen",
            side_effect=forbidden,
        ):
            with self.assertRaises(HTTPError):
                ARCHIVER.fetch_snapshot_sync("btcusdt")


if __name__ == "__main__":
    unittest.main()
