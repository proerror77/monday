#!/usr/bin/env python3
"""Run all collector release contracts, with two isolated slow fixtures at a time."""
import argparse
from collections import deque
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time


FAST = (
    "test-polymarket-market-tape-upload-contract.sh",
    "test-polymarket-reference-upload-contract.sh",
    "test-trading-ecs-host-contract.sh",
    "test-polymarket-raw-ops-stage.sh",
    "test-collector-health-unit-release.sh",
    "test-rust-lob-controller-release.sh",
    "test-rust-lob-restore.sh",
    "test-binance-fee-release-contract.sh",
    "test-binance-usdm-account-release-contract.sh",
    "test-binance-fee-cutover.sh",
    "test-bybit-options-release-contract.sh",
    "test-bybit-options-shadow-gate.sh",
)
# Longest first, using the successful 36833622104 log's marker intervals.
# Each suite has its own mktemp root, mocks and fixture-local locks. Retention
# remains inside recovery-queue, once; no production service or port is used.
SLOW = (
    "test-rust-lob-recovery-queue.sh",
    "test-rust-lob-control-plane.sh",
    "test-monday-collector-health.sh",
)


def run(root):
    active = {}
    failed = False
    began = time.monotonic()

    def interrupted(signum, _frame):
        raise InterruptedError(signum)

    def start(name, logs):
        log = (logs / name).open("w+")
        process = subprocess.Popen(
            ["bash", str(root / "deployment/aliyun" / name)],
            cwd=root / "rust_hft", stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        active[process] = (name, log, time.monotonic())
        print(f"contract_start script={name}", flush=True)

    def finish(process):
        nonlocal failed
        name, log, started = active.pop(process)
        status = process.wait()
        log.seek(0)
        print(f"::group::{name}", flush=True)
        for line in log:
            print(line, end="")
        log.close()
        print("::endgroup::", flush=True)
        print(f"contract_result script={name} exit={status} "
              f"elapsed_seconds={time.monotonic() - started:.3f}", flush=True)
        failed |= status != 0

    handlers = {sig: signal.signal(sig, interrupted)
                for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        with tempfile.TemporaryDirectory(prefix="collector-contract-logs-") as temp:
            logs = Path(temp)
            for name in FAST:
                start(name, logs)
                process = next(iter(active))
                process.wait()
                finish(process)
                if failed:
                    return 1  # Do not spend minutes after a fast contract failure.
            pending = deque(SLOW)
            while pending or active:
                while pending and len(active) < 2:
                    start(pending.popleft(), logs)
                for process in list(active):
                    if process.poll() is not None:
                        finish(process)
                if active:
                    time.sleep(0.05)
            return int(failed)
    except InterruptedError as error:
        return 128 + error.args[0]
    finally:
        for sig in handlers:
            signal.signal(sig, signal.SIG_IGN)
        # Kill only process groups created by this runner, including descendants
        # that retain a fixture lock. Allow EXIT cleanup, then bound termination.
        for process in active:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 2
        while active and time.monotonic() < deadline:
            if all(process.poll() is not None for process in active):
                break
            time.sleep(0.05)
        for process, (_, log, _) in active.items():
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            log.close()
        for sig, handler in handlers.items():
            signal.signal(sig, handler)
        print(f"contract_suite elapsed_seconds={time.monotonic() - began:.3f}",
              flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path,
                        default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    raise SystemExit(run(args.root.resolve()))
