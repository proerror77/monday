#!/usr/bin/env python3
"""Exercise coverage, fail-fast ordering, concurrency and descendant cancellation."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

RUNNER = Path(__file__).with_name("run-collector-control-contracts.py")
# Coverage carried forward from the pre-scheduling workflow, independently of
# the runner's fast/slow partition. Nested retention stays in recovery-queue.
CONTRACTS = (
    "polymarket-market-tape-upload-contract", "polymarket-reference-upload-contract",
    "monday-collector-health", "collector-health-unit-release", "rust-lob-control-plane",
    "rust-lob-controller-release", "rust-lob-recovery-queue", "rust-lob-restore",
    "polymarket-raw-ops-stage", "trading-ecs-host-contract", "binance-fee-release-contract",
    "binance-usdm-account-release-contract", "binance-fee-cutover",
    "bybit-options-release-contract", "bybit-options-shadow-gate",
)
STUB = '''import fcntl, json, os, pathlib, signal, subprocess, sys, time
root = pathlib.Path(__file__).parent
name = pathlib.Path(sys.argv[1]).name
slow = name in ("test-monday-collector-health.sh", "test-rust-lob-control-plane.sh",
               "test-rust-lob-recovery-queue.sh")
def event(action):
    with (root / "events").open("a") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        f.write(json.dumps([action, name, slow]) + "\\n")
event("start")
if slow and os.environ.get("HOLD"):
    child = subprocess.Popen([sys.executable, "-c",
        "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)"])
    (root / (name + ".pid")).write_text(str(child.pid))
    time.sleep(60)
time.sleep(0.2 if slow else 0)
event("end")
sys.exit(37 if name == os.environ.get("FAIL") else 0)
'''


class Scheduling(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="control-scheduling-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "rust_hft").mkdir()
        directory = self.root / "deployment/aliyun"
        directory.mkdir(parents=True)
        (self.root / "stub.py").write_text(STUB)
        for name in CONTRACTS:
            (directory / f"test-{name}.sh").write_text(
                'exec python3 "../stub.py" "$0"\n')

    def launch(self, **env):
        process = subprocess.Popen(
            ["python3", str(RUNNER), "--root", str(self.root)],
            env={**os.environ, **env}, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True,
        )
        def cleanup():
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=8)
        self.addCleanup(cleanup)
        return process

    def events(self):
        return [json.loads(line) for line in (self.root / "events").read_text().splitlines()]

    def test_coverage_order_and_two_workers(self):
        process = self.launch()
        output, _ = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 0, output)
        events = self.events()
        starts = [name for event, name, _ in events if event == "start"]
        self.assertCountEqual(starts, [f"test-{name}.sh" for name in CONTRACTS])
        first_slow = next(i for i, (_, _, slow) in enumerate(events) if slow)
        self.assertEqual(sum(event == "end" for event, _, _ in events[:first_slow]), 12)
        active = peak = 0
        for event, _, slow in events:
            if slow:
                active += 1 if event == "start" else -1
                peak = max(peak, active)
        self.assertEqual(peak, 2)
        self.assertEqual(active, 0)
        self.assertEqual(output.count("contract_result script="), 15)
        self.assertIn("elapsed_seconds=", output)

    def test_fast_failure_does_not_launch_slow_work(self):
        process = self.launch(FAIL="test-trading-ecs-host-contract.sh")
        output, _ = process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0, output)
        self.assertFalse(any(slow for _, _, slow in self.events()))
        self.assertIn("exit=37", output)

    def test_slow_failure_preserves_other_slow_coverage(self):
        process = self.launch(FAIL="test-rust-lob-recovery-queue.sh")
        output, _ = process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0, output)
        self.assertEqual(sum(event == "end" for event, _, _ in self.events()), 15)
        self.assertIn("exit=37", output)

    def test_cancellation_terminates_owned_descendants(self):
        process = self.launch(HOLD="1")
        deadline = time.monotonic() + 10
        while len(list(self.root.glob("*.pid"))) < 2 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(len(list(self.root.glob("*.pid"))), 2)
        children = [int(path.read_text()) for path in self.root.glob("*.pid")]
        time.sleep(0.1)  # Let the descendant install its TERM handler.
        process.send_signal(signal.SIGTERM)
        output, _ = process.communicate(timeout=8)
        self.assertEqual(process.returncode, 143, output)
        for pid in children:
            stat = Path(f"/proc/{pid}/stat")
            self.assertTrue(not stat.exists() or stat.read_text().split(") ", 1)[1][0] == "Z",
                            f"descendant {pid} survived cancellation")


if __name__ == "__main__":
    unittest.main()
