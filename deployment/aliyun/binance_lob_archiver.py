#!/opt/monday/venv/bin/python
import argparse
import asyncio
import hashlib
import json
import logging
import os
import shutil
import signal
import subprocess
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid
from collections import Counter
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from urllib.error import HTTPError, URLError

from websockets.asyncio.client import connect


LOG = logging.getLogger("binance-lob-archiver")
MARKET = os.getenv("MARKET", "spot").strip().lower()
DATASET = os.getenv("DATASET", f"{MARKET}_all").strip().lower()
SHARD_ID = os.getenv("SHARD_ID", "all").strip().lower()
SYMBOLS_SETTING = os.getenv(
    "SYMBOLS", "btcusdt,ethusdt,bnbusdt,solusdt,xrpusdt,dogeusdt"
).strip()
SYMBOLS = tuple(
    item.strip().lower()
    for item in SYMBOLS_SETTING.split(",")
    if item.strip()
)
SECURITY_TOKEN_SYMBOLS: tuple[str, ...] = ()
EXCLUDED_SYMBOLS: tuple[str, ...] = ()
MODE = os.getenv("DEPTH_MODE", "diff").strip().lower()
SEGMENT_SECONDS = max(60, int(os.getenv("SEGMENT_SECONDS", "3600")))
SPOOL_DIR = Path(os.getenv("SPOOL_DIR", "/data/monday/spool/binance-lob"))
BUCKET = os.getenv("OSS_BUCKET", "monday-lob-apne1-1045353359")
ENDPOINT = os.getenv("OSS_ENDPOINT", "oss-ap-northeast-1-internal.aliyuncs.com")
REGION = os.getenv("OSS_REGION", "ap-northeast-1")
PROFILE = os.getenv("ALIYUN_PROFILE", "ecs-role")
REST_BASE = os.getenv(
    "BINANCE_REST_BASE",
    "https://data-api.binance.vision"
    if MARKET == "spot"
    else "https://fapi.binance.com",
)
SNAPSHOT_LIMIT = int(os.getenv("SNAPSHOT_LIMIT", "100"))
SNAPSHOT_REQUESTS_PER_SECOND = float(
    os.getenv("SNAPSHOT_REQUESTS_PER_SECOND", "15")
)
WS_SHARD_SIZE = int(os.getenv("WS_SHARD_SIZE", "100"))
SYNC_TIMEOUT_SECONDS = int(os.getenv("SYNC_TIMEOUT_SECONDS", "20"))
STALL_TIMEOUT_SECONDS = int(os.getenv("STALL_TIMEOUT_SECONDS", "60"))
MAX_BUFFERED_DIFFS = int(os.getenv("MAX_BUFFERED_DIFFS", "250000"))
STARTUP_DELAY_SECONDS = int(os.getenv("STARTUP_DELAY_SECONDS", "0"))
SNAPSHOT_RETRY_ATTEMPTS = max(
    1, int(os.getenv("SNAPSHOT_RETRY_ATTEMPTS", "6"))
)
MAX_PENDING_DIFFS_TOTAL = int(os.getenv("MAX_PENDING_DIFFS_TOTAL", "250000"))
MIN_FREE_GB = int(os.getenv("MIN_FREE_GB", "20"))
ZSTD_TIMEOUT_SECONDS = int(os.getenv("ZSTD_TIMEOUT_SECONDS", "300"))
OSS_COPY_TIMEOUT_SECONDS = int(os.getenv("OSS_COPY_TIMEOUT_SECONDS", "300"))
UPLOAD_STATUS: dict[str, str | None] = {
    "last_success_at": None,
    "last_error_at": None,
    "last_error": None,
}


class SequenceGap(RuntimeError):
    def __init__(
        self, symbol: str, expected: int, first_update_id: int, final_update_id: int
    ) -> None:
        self.symbol = symbol
        self.expected = expected
        self.first_update_id = first_update_id
        self.final_update_id = final_update_id
        super().__init__(
            f"{symbol} sequence gap expected={expected} "
            f"received={first_update_id}-{final_update_id}"
        )


class SnapshotUnavailable(RuntimeError):
    def __init__(self, symbol: str, status: int) -> None:
        self.symbol = symbol.upper()
        self.status = status
        super().__init__(f"snapshot unavailable symbol={self.symbol} status={status}")


class PendingBudget:
    def __init__(self, limit: int) -> None:
        self.limit = limit
        self.count = 0

    def reserve(self) -> None:
        if self.count >= self.limit:
            raise RuntimeError(f"pending diff budget exceeded limit={self.limit}")
        self.count += 1

    def release(self, count: int) -> None:
        self.count = max(0, self.count - count)


class OrderBookState:
    def __init__(
        self,
        symbol: str,
        market: str = "spot",
        pending_budget: PendingBudget | None = None,
    ) -> None:
        self.symbol = symbol.upper()
        self.market = market
        self.bids: dict[str, str] = {}
        self.asks: dict[str, str] = {}
        self.last_update_id: int | None = None
        self.synced = False
        self.bridged = False
        self.pending: list[dict] = []
        self.pending_budget = pending_budget or PendingBudget(MAX_PENDING_DIFFS_TOTAL)

    @staticmethod
    def _update_side(side: dict[str, str], levels: list[list[str]]) -> None:
        for price, quantity in levels:
            if Decimal(quantity) == 0:
                side.pop(price, None)
            else:
                side[price] = quantity

    def _apply_levels(self, event: dict) -> None:
        self._update_side(self.bids, event.get("b", []))
        self._update_side(self.asks, event.get("a", []))
        self.last_update_id = int(event["u"])

    def _apply_after_snapshot(self, event: dict) -> bool:
        assert self.last_update_id is not None
        first_update_id = int(event["U"])
        final_update_id = int(event["u"])
        if final_update_id <= self.last_update_id:
            return False

        if not self.bridged:
            if self.market == "usdm":
                previous_update_id = int(event.get("pu", -1))
                if previous_update_id == self.last_update_id:
                    self._apply_levels(event)
                    self.synced = True
                    self.bridged = True
                    return True
            expected = self.last_update_id + (1 if self.market == "spot" else 0)
            if first_update_id <= expected <= final_update_id:
                self._apply_levels(event)
                self.synced = True
                self.bridged = True
                return True
            self.synced = False
            raise SequenceGap(
                self.symbol, expected, first_update_id, final_update_id
            )

        if self.market == "usdm":
            previous_update_id = int(event.get("pu", -1))
            if previous_update_id == self.last_update_id:
                self._apply_levels(event)
                return True
            self.synced = False
            raise SequenceGap(
                self.symbol,
                self.last_update_id,
                previous_update_id,
                final_update_id,
            )

        expected = self.last_update_id + 1
        if first_update_id <= expected <= final_update_id:
            self._apply_levels(event)
            return True
        self.synced = False
        raise SequenceGap(
            self.symbol, expected, first_update_id, final_update_id
        )

    def apply_diff(self, event: dict) -> bool:
        if self.last_update_id is None:
            self.pending_budget.reserve()
            self.pending.append(event)
            return False
        return self._apply_after_snapshot(event)

    def install_snapshot(self, snapshot: dict) -> None:
        self.bids = {price: quantity for price, quantity in snapshot["bids"]}
        self.asks = {price: quantity for price, quantity in snapshot["asks"]}
        self.last_update_id = int(snapshot["lastUpdateId"])
        self.synced = True
        self.bridged = False
        pending, self.pending = self.pending, []
        self.pending_budget.release(len(pending))
        for event in pending:
            self._apply_after_snapshot(event)

    def invalidate_for_resync(self) -> None:
        self.pending_budget.release(len(self.pending))
        self.last_update_id = None
        self.synced = False
        self.bridged = False
        self.pending = []

    def checkpoint(self, session_id: str) -> dict:
        if self.last_update_id is None:
            raise RuntimeError(f"{self.symbol} has no snapshot")
        bids = sorted(
            self.bids.items(), key=lambda item: Decimal(item[0]), reverse=True
        )
        asks = sorted(self.asks.items(), key=lambda item: Decimal(item[0]))
        return {
            "session_id": session_id,
            "symbol": self.symbol,
            "last_update_id": self.last_update_id,
            "synced": self.synced,
            "bridged": self.bridged,
            "bids": [list(level) for level in bids],
            "asks": [list(level) for level in asks],
        }


def stream_suffix() -> str:
    if MODE != "diff":
        raise ValueError(f"snapshot reconciliation requires DEPTH_MODE=diff, got {MODE}")
    return "@depth@100ms"


def discover_symbols_sync() -> tuple[tuple[str, ...], tuple[str, ...]]:
    if MARKET == "spot":
        url = f"{REST_BASE}/api/v3/exchangeInfo"
    elif MARKET == "usdm":
        url = f"{REST_BASE}/fapi/v1/exchangeInfo"
    else:
        raise ValueError(f"unsupported MARKET={MARKET}")
    request = urllib.request.Request(
        url, headers={"User-Agent": "monday-lob-archiver/2"}
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        exchange_info = json.load(response)

    symbols = []
    security_tokens = []
    for item in exchange_info["symbols"]:
        if item.get("status") != "TRADING":
            continue
        if MARKET == "spot" and not item.get("isSpotTradingAllowed", True):
            continue
        if MARKET == "usdm" and item.get("contractType") != "PERPETUAL":
            continue
        symbol = item["symbol"].lower()
        symbols.append(symbol)
        permission_groups = {
            permission
            for group in item.get("permissionSets", [])
            for permission in group
        }
        if "TRD_GRP_261" in permission_groups:
            security_tokens.append(symbol)
    return tuple(sorted(symbols)), tuple(sorted(security_tokens))


def stream_urls() -> tuple[str, ...]:
    base = (
        "wss://data-stream.binance.vision/stream?streams="
        if MARKET == "spot"
        else "wss://fstream.binance.com/stream?streams="
    )
    urls = []
    for index in range(0, len(SYMBOLS), WS_SHARD_SIZE):
        streams = "/".join(
            symbol + stream_suffix()
            for symbol in SYMBOLS[index : index + WS_SHARD_SIZE]
        )
        urls.append(base + streams)
    return tuple(urls)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def segment_partition(timestamp_ns: int) -> tuple[str, str]:
    value = datetime.fromtimestamp(timestamp_ns / 1_000_000_000, timezone.utc)
    return value.strftime("%Y-%m-%d"), value.strftime("%H")


def finalize_segment(
    path: Path,
    counts: Counter,
    start_ns: int,
    end_ns: int,
    schema: str = "binance.lob_tape.v2",
    replay_safe: bool = False,
) -> Path | None:
    if not path.exists() or path.stat().st_size == 0:
        path.unlink(missing_ok=True)
        return None

    output = path.with_suffix("").with_suffix(".jsonl.zst")
    temporary_output = output.with_suffix(output.suffix + ".tmp")
    subprocess.run(
        [
            "zstd",
            "-q",
            "-f",
            "-T1",
            "-3",
            str(path),
            "-o",
            str(temporary_output),
        ],
        check=True,
        timeout=ZSTD_TIMEOUT_SECONDS,
    )
    with temporary_output.open("rb") as compressed:
        os.fsync(compressed.fileno())
    temporary_output.replace(output)
    date, hour = segment_partition(start_ns)
    metadata = {
        "schema": schema,
        "venue": "binance",
        "market": MARKET,
        "dataset": DATASET,
        "shard_id": SHARD_ID,
        "mode": MODE,
        "symbols": list(SYMBOLS),
        "security_token_symbols": list(SECURITY_TOKEN_SYMBOLS),
        "excluded_symbols": list(EXCLUDED_SYMBOLS),
        "snapshot_limit": SNAPSHOT_LIMIT,
        "replay_scope": "captured_snapshot_seed_plus_sequence_checked_diffs",
        "venue_depth_complete": False,
        "events": sum(counts.values()),
        "event_types": dict(sorted(counts.items())),
        "has_replay_safe_checkpoint": replay_safe and counts["checkpoint"] > 0,
        "start_received_at_ns": start_ns,
        "end_received_at_ns": end_ns,
        "date": date,
        "hour": hour,
        "file": output.name,
        "bytes": output.stat().st_size,
        "sha256": sha256(output),
    }
    manifest = output.with_name(output.name + ".manifest.json")
    temporary = manifest.with_suffix(manifest.suffix + ".tmp")
    temporary.write_text(json.dumps(metadata, sort_keys=True) + "\n")
    temporary.replace(manifest)
    path.unlink()
    return manifest


def upload(manifest: Path) -> None:
    metadata = json.loads(manifest.read_text())
    data = manifest.with_name(metadata["file"])
    prefix = (
        f"lake/raw/venue=binance/market={metadata['market']}"
        f"/dataset={metadata['dataset']}/shard={metadata['shard_id']}"
        f"/date={metadata['date']}/hour={metadata['hour']}"
    )
    common = [
        "--profile",
        PROFILE,
        "--endpoint",
        ENDPOINT,
        "--region",
        REGION,
        "--force",
    ]

    def copy(source: Path, name: str) -> None:
        subprocess.run(
            [
                "aliyun",
                "ossutil",
                "cp",
                str(source),
                f"oss://{BUCKET}/{prefix}/{name}",
                *common,
            ],
            check=True,
            timeout=OSS_COPY_TIMEOUT_SECONDS,
        )

    copy(data, data.name)
    copy(manifest, manifest.name)
    success = manifest.with_name(data.name + "._SUCCESS")
    success.write_text(metadata["sha256"] + "\n")
    copy(success, success.name)
    data.unlink()
    manifest.unlink()
    success.unlink()
    UPLOAD_STATUS["last_success_at"] = datetime.now(timezone.utc).isoformat()
    LOG.info(
        "uploaded segment events=%s types=%s key=%s/%s",
        metadata["events"],
        metadata["event_types"],
        prefix,
        data.name,
    )


def upload_pending() -> None:
    had_error = False
    for manifest in sorted(SPOOL_DIR.rglob("*.manifest.json")):
        try:
            upload(manifest)
        except Exception as error:
            had_error = True
            UPLOAD_STATUS["last_error_at"] = datetime.now(timezone.utc).isoformat()
            UPLOAD_STATUS["last_error"] = str(error)[:500]
            LOG.exception("pending upload failed: %s", manifest)
    if not had_error:
        UPLOAD_STATUS["last_error_at"] = None
        UPLOAD_STATUS["last_error"] = None


def pending_upload_count() -> int:
    return sum(1 for _ in SPOOL_DIR.rglob("*.manifest.json"))


def recover_parts() -> None:
    for path in sorted(SPOOL_DIR.rglob("*.jsonl.part")):
        counts = Counter()
        start_ns = end_ns = path.stat().st_mtime_ns
        schema = "binance.lob_tape.v2"
        dropped = 0
        valid_lines = 0
        recovering = path.with_suffix(path.suffix + ".recovering")
        with path.open("rb") as source, recovering.open("wb") as target:
            for line in source:
                try:
                    event = json.loads(line)
                    received = int(event["received_at_ns"])
                    event_type = event.get("type")
                    if event_type is None:
                        event_type = "diff"
                        schema = "binance.raw_diff_tape.v1"
                    counts[event_type] += 1
                    if valid_lines == 0:
                        start_ns = received
                    end_ns = received
                    target.write(line if line.endswith(b"\n") else line + b"\n")
                    valid_lines += 1
                except (ValueError, KeyError, json.JSONDecodeError):
                    dropped += 1
            target.flush()
            os.fsync(target.fileno())
        recovering.replace(path)
        if dropped:
            LOG.warning(
                "recovery dropped %s invalid JSONL line(s): %s", dropped, path
            )
        finalize_segment(
            path,
            counts,
            start_ns,
            end_ns,
            schema=schema,
            replay_safe=False,
        )


class Segment:
    def __init__(self) -> None:
        self.start_ns = time.time_ns()
        self.end_ns = self.start_ns
        date, hour = segment_partition(self.start_ns)
        directory = SPOOL_DIR / f"date={date}" / f"hour={hour}"
        directory.mkdir(parents=True, exist_ok=True)
        self.path = directory / f"part-{self.start_ns}.jsonl.part"
        self.file = self.path.open("ab", buffering=1024 * 1024)
        self.counts: Counter = Counter()
        self.replay_safe = True

    def write(self, event_type: str, payload: dict, received_at_ns: int | None = None) -> None:
        received_at_ns = received_at_ns or time.time_ns()
        self.end_ns = max(self.end_ns, received_at_ns)
        envelope = {
            "received_at_ns": received_at_ns,
            "type": event_type,
            **payload,
        }
        self.file.write(
            json.dumps(envelope, separators=(",", ":"), sort_keys=False).encode()
            + b"\n"
        )
        self.counts[event_type] += 1

    def due(self) -> bool:
        old_date, old_hour = segment_partition(self.start_ns)
        new_date, new_hour = segment_partition(time.time_ns())
        return (
            time.time_ns() - self.start_ns >= SEGMENT_SECONDS * 1_000_000_000
            or (old_date, old_hour) != (new_date, new_hour)
        )

    def mark_replay_unsafe(self) -> None:
        self.replay_safe = False

    def close(self) -> Path | None:
        self.file.flush()
        os.fsync(self.file.fileno())
        self.file.close()
        return finalize_segment(
            self.path,
            self.counts,
            self.start_ns,
            self.end_ns,
            replay_safe=self.replay_safe,
        )


def fetch_snapshot_sync(symbol: str) -> dict:
    query = urllib.parse.urlencode({"symbol": symbol.upper(), "limit": SNAPSHOT_LIMIT})
    path = "/api/v3/depth" if MARKET == "spot" else "/fapi/v1/depth"
    request = urllib.request.Request(
        f"{REST_BASE}{path}?{query}",
        headers={"User-Agent": "monday-lob-archiver/2"},
    )
    started_at_ns = time.time_ns()
    for attempt in range(SNAPSHOT_RETRY_ATTEMPTS):
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                snapshot = json.load(response)
            break
        except HTTPError as error:
            if error.code == 400:
                try:
                    payload = json.load(error)
                except (ValueError, json.JSONDecodeError):
                    payload = {}
                if payload.get("code") == -1121:
                    raise SnapshotUnavailable(symbol, error.code) from error
            retryable = error.code == 429 or 500 <= error.code < 600
            if not retryable or attempt + 1 == SNAPSHOT_RETRY_ATTEMPTS:
                raise
            retry_after = error.headers.get("Retry-After")
            delay = float(retry_after) if retry_after else min(60, 2**attempt)
            LOG.warning(
                "snapshot retry symbol=%s status=%s delay=%ss",
                symbol,
                error.code,
                delay,
            )
            time.sleep(delay)
        except URLError:
            if attempt + 1 == SNAPSHOT_RETRY_ATTEMPTS:
                raise
            time.sleep(min(30, 2**attempt))
    received_at_ns = time.time_ns()
    if "lastUpdateId" not in snapshot:
        raise RuntimeError(f"snapshot missing lastUpdateId for {symbol}")
    return {
        "symbol": symbol.upper(),
        "request_started_at_ns": started_at_ns,
        "received_at_ns": received_at_ns,
        "snapshot": snapshot,
    }


async def receive_url(url: str, queue: asyncio.Queue, stop: asyncio.Event) -> None:
    async with connect(
        url, open_timeout=20, ping_interval=20, max_size=8 * 1024 * 1024
    ) as websocket:
        while not stop.is_set():
            try:
                message = await asyncio.wait_for(
                    websocket.recv(), timeout=STALL_TIMEOUT_SECONDS
                )
            except asyncio.TimeoutError:
                raise RuntimeError(
                    f"no depth frames for {STALL_TIMEOUT_SECONDS}s on websocket shard"
                )
            if isinstance(message, str):
                await queue.put(("diff", time.time_ns(), json.loads(message)))


async def produce_snapshots(
    queue: asyncio.Queue,
    limiter: "SnapshotRateLimiter | None" = None,
) -> None:
    limiter = limiter or SnapshotRateLimiter(SNAPSHOT_REQUESTS_PER_SECOND)
    for symbol in SYMBOLS:
        snapshot = await limiter.fetch(symbol)
        await queue.put(("snapshot", snapshot["received_at_ns"], snapshot))


class SnapshotRateLimiter:
    def __init__(self, requests_per_second: float) -> None:
        self.interval = 1 / requests_per_second
        self.next_started_at = 0.0
        self.lock = asyncio.Lock()

    async def fetch(self, symbol: str) -> dict:
        async with self.lock:
            now = time.monotonic()
            delay = max(0.0, self.next_started_at - now)
            if delay:
                await asyncio.sleep(delay)
            self.next_started_at = time.monotonic() + self.interval
        return await asyncio.to_thread(fetch_snapshot_sync, symbol)


async def produce_snapshot(
    symbol: str,
    queue: asyncio.Queue,
    limiter: SnapshotRateLimiter | None = None,
) -> None:
    limiter = limiter or SnapshotRateLimiter(SNAPSHOT_REQUESTS_PER_SECOND)
    snapshot = await limiter.fetch(symbol)
    await queue.put(("snapshot", snapshot["received_at_ns"], snapshot))


def frame_data(frame: dict) -> tuple[str, dict]:
    data = frame.get("data", frame)
    symbol = str(data.get("s", "")).upper()
    if not symbol or "U" not in data or "u" not in data:
        raise ValueError("depth frame missing symbol or sequence fields")
    return symbol, data


def is_stalled(last_frame_at: float, now: float) -> bool:
    return now - last_frame_at > STALL_TIMEOUT_SECONDS


def bridge_timed_out(
    previously_synced: bool, sync_deadline: float | None, now: float
) -> bool:
    return (
        not previously_synced
        and sync_deadline is not None
        and now > sync_deadline
    )


def begin_resync(now: float) -> tuple[bool, float]:
    return False, now + SYNC_TIMEOUT_SECONDS


def exclude_unavailable_symbols(
    excluded_symbols: tuple[str, ...],
    symbols: tuple[str, ...],
    security_tokens: tuple[str, ...],
) -> tuple[tuple[str, ...], tuple[str, ...]]:
    excluded = {symbol.lower() for symbol in excluded_symbols}
    return (
        tuple(item for item in symbols if item not in excluded),
        tuple(item for item in security_tokens if item not in excluded),
    )


def disk_headroom() -> tuple[float, bool]:
    free_gb = shutil.disk_usage(SPOOL_DIR).free / 1024**3
    return round(free_gb, 1), free_gb < MIN_FREE_GB


def warn_if_disk_low() -> tuple[float, bool]:
    free_gb, warning = disk_headroom()
    if warning:
        LOG.warning(
            "spool free space below %sGiB: %.1fGiB; continuing collection; "
            "successfully uploaded segments are removed locally, but pending "
            "segments are retained to prevent data loss",
            MIN_FREE_GB,
            free_gb,
        )
    return free_gb, warning


def write_health(
    states: dict[str, OrderBookState], session_id: str, status: str, gaps: int
) -> None:
    disk_free_gb, disk_warning = disk_headroom()
    pending_segments = pending_upload_count()
    health = {
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "status": status,
        "market": MARKET,
        "dataset": DATASET,
        "symbol_count": len(states),
        "security_token_count": len(SECURITY_TOKEN_SYMBOLS),
        "excluded_symbols": list(EXCLUDED_SYMBOLS),
        "session_id": session_id,
        "sequence_gaps": gaps,
        "disk_free_gb": disk_free_gb,
        "disk_warning": disk_warning,
        "disk_warning_threshold_gb": MIN_FREE_GB,
        "pending_upload_segments": pending_segments,
        "upload_warning": UPLOAD_STATUS["last_error_at"] is not None,
        "last_upload_success_at": UPLOAD_STATUS["last_success_at"],
        "last_upload_error_at": UPLOAD_STATUS["last_error_at"],
        "last_upload_error": UPLOAD_STATUS["last_error"],
        "symbols": {
            symbol: {
                "synced": state.synced,
                "bridged": state.bridged,
                "last_update_id": state.last_update_id,
                "bid_levels": len(state.bids),
                "ask_levels": len(state.asks),
            }
            for symbol, state in sorted(states.items())
        },
    }
    path = SPOOL_DIR / "health.json"
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(health, sort_keys=True) + "\n")
    temporary.replace(path)


class ArchiveRuntime:
    def __init__(self) -> None:
        self.segment = Segment()
        self.total_gaps = 0
        self.last_upload_retry = 0.0
        self.upload_thread: threading.Thread | None = None

    def write_checkpoints(
        self, states: dict[str, OrderBookState], session_id: str, reason: str
    ) -> None:
        if not self.segment.replay_safe:
            return
        if not states or any(not state.synced for state in states.values()):
            self.segment.mark_replay_unsafe()
            return
        for state in states.values():
            self.segment.write(
                "checkpoint",
                {
                    "reason": reason,
                    "replay_safe": True,
                    **state.checkpoint(session_id),
                },
            )

    async def retry_uploads_if_due(self, force: bool = False) -> None:
        if self.upload_thread is not None and not self.upload_thread.is_alive():
            self.upload_thread = None
        now = time.monotonic()
        if not force and now - self.last_upload_retry < 300:
            return
        if self.upload_thread is not None:
            return
        self.last_upload_retry = now
        self.upload_thread = threading.Thread(
            target=upload_pending,
            name=f"binance-oss-upload-{MARKET}",
            daemon=True,
        )
        self.upload_thread.start()

    async def finish_uploads(self) -> None:
        if self.upload_thread is not None and self.upload_thread.is_alive():
            LOG.warning(
                "shutdown leaving OSS upload in progress; local segment remains "
                "retryable if the process is terminated"
            )

    async def rotate(
        self,
        states: dict[str, OrderBookState],
        session_id: str,
        reason: str,
        create_next: bool = True,
    ) -> None:
        self.write_checkpoints(states, session_id, reason)
        closing_segment = self.segment
        if create_next:
            self.segment = Segment()
        manifest = await asyncio.to_thread(closing_segment.close)
        if manifest:
            await self.retry_uploads_if_due(force=True)


def archive_and_apply_diff(
    runtime: ArchiveRuntime,
    states: dict[str, OrderBookState],
    session_id: str,
    received_at_ns: int,
    frame: dict,
) -> None:
    symbol, data = frame_data(frame)
    runtime.segment.write(
        "diff", {"session_id": session_id, "frame": frame}, received_at_ns
    )
    try:
        states[symbol].apply_diff(data)
    except SequenceGap as gap:
        runtime.segment.mark_replay_unsafe()
        runtime.segment.write(
            "sequence_gap",
            {
                "session_id": session_id,
                "symbol": gap.symbol,
                "expected": gap.expected,
                "first_update_id": gap.first_update_id,
                "final_update_id": gap.final_update_id,
            },
        )
        raise


async def run_session(
    stop: asyncio.Event,
    runtime: ArchiveRuntime,
    states: dict[str, OrderBookState],
    session_id: str,
) -> None:
    queue: asyncio.Queue = asyncio.Queue(maxsize=MAX_BUFFERED_DIFFS)
    failure: BaseException | None = None
    receivers = [
        asyncio.create_task(receive_url(url, queue, stop)) for url in stream_urls()
    ]
    snapshot_limiter = SnapshotRateLimiter(SNAPSHOT_REQUESTS_PER_SECOND)
    snapshotter = asyncio.create_task(produce_snapshots(queue, snapshot_limiter))
    tasks = [*receivers, snapshotter]
    resync_tasks: dict[str, asyncio.Task] = {}
    LOG.info(
        "connected market=%s symbols=%s websocket_shards=%s session=%s",
        MARKET,
        len(SYMBOLS),
        len(receivers),
        session_id,
    )
    runtime.segment.write(
        "session_start",
        {
            "session_id": session_id,
            "market": MARKET,
            "symbols": len(SYMBOLS),
            "websocket_shards": len(receivers),
        },
    )
    sync_deadline: float | None = None
    last_frame_at = time.monotonic()
    last_health_write = 0.0
    last_task_check = 0.0
    last_disk_check = 0.0
    previously_synced = False
    try:
        while not stop.is_set():
            try:
                event_type, received_at_ns, payload = await asyncio.wait_for(
                    queue.get(), timeout=1
                )
            except asyncio.TimeoutError:
                for receiver in receivers:
                    if receiver.done():
                        await receiver
                if snapshotter.done():
                    await snapshotter
                    sync_deadline = sync_deadline or (
                        time.monotonic() + SYNC_TIMEOUT_SECONDS
                    )
                if is_stalled(last_frame_at, time.monotonic()):
                    raise RuntimeError(
                        f"no depth frames for {STALL_TIMEOUT_SECONDS}s"
                    )
                if runtime.segment.due():
                    await runtime.rotate(states, session_id, "scheduled")
                now = time.monotonic()
                if now - last_disk_check >= 60:
                    warn_if_disk_low()
                    last_disk_check = now
                await runtime.retry_uploads_if_due()
                continue

            try:
                if event_type == "diff":
                    archive_and_apply_diff(
                        runtime, states, session_id, received_at_ns, payload
                    )
                    last_frame_at = time.monotonic()
                else:
                    symbol = payload["symbol"]
                    runtime.segment.write(
                        "snapshot",
                        {
                            "session_id": session_id,
                            "symbol": symbol,
                            "request_started_at_ns": payload["request_started_at_ns"],
                            "snapshot": payload["snapshot"],
                        },
                        received_at_ns,
                    )
                    states[symbol].install_snapshot(payload["snapshot"])
            except SequenceGap as gap:
                if event_type == "snapshot":
                    runtime.segment.mark_replay_unsafe()
                    runtime.segment.write(
                        "sequence_gap",
                        {
                            "session_id": session_id,
                            "symbol": gap.symbol,
                            "expected": gap.expected,
                            "first_update_id": gap.first_update_id,
                            "final_update_id": gap.final_update_id,
                        },
                    )
                runtime.total_gaps += 1
                states[gap.symbol].invalidate_for_resync()
                previously_synced, sync_deadline = begin_resync(time.monotonic())
                write_health(states, session_id, "resyncing", runtime.total_gaps)
                await runtime.rotate(states, session_id, "sequence_gap")
                if gap.symbol not in resync_tasks or resync_tasks[gap.symbol].done():
                    resync_tasks[gap.symbol] = asyncio.create_task(
                        produce_snapshot(
                            gap.symbol.lower(), queue, snapshot_limiter
                        )
                    )
                continue

            if snapshotter.done():
                await snapshotter
                sync_deadline = sync_deadline or (
                    time.monotonic() + SYNC_TIMEOUT_SECONDS
                )
            all_synced = all(state.synced for state in states.values())
            now = time.monotonic()
            if all_synced and snapshotter.done():
                if not previously_synced or now - last_health_write >= 30:
                    write_health(states, session_id, "synced", runtime.total_gaps)
                    last_health_write = now
                    previously_synced = True
            elif bridge_timed_out(previously_synced, sync_deadline, now):
                missing = [symbol for symbol, state in states.items() if not state.synced]
                raise RuntimeError(f"snapshot bridge timed out: {missing}")
            if runtime.segment.due():
                await runtime.rotate(states, session_id, "scheduled")
            if now - last_task_check >= 1:
                for receiver in receivers:
                    if receiver.done():
                        await receiver
                for symbol, task in tuple(resync_tasks.items()):
                    if task.done():
                        await task
                        del resync_tasks[symbol]
                last_task_check = now
            if now - last_disk_check >= 60:
                warn_if_disk_low()
                last_disk_check = now
            await runtime.retry_uploads_if_due()
    except BaseException as error:
        failure = error
        raise
    finally:
        for task in [*tasks, *resync_tasks.values()]:
            task.cancel()
        await asyncio.gather(
            *tasks, *resync_tasks.values(), return_exceptions=True
        )
        archive_only = isinstance(failure, SequenceGap)
        drain_gap: SequenceGap | None = None
        while not queue.empty():
            event_type, received_at_ns, payload = queue.get_nowait()
            if event_type == "snapshot":
                symbol = payload["symbol"]
                runtime.segment.write(
                    "snapshot",
                    {
                        "session_id": session_id,
                        "symbol": symbol,
                        "request_started_at_ns": payload["request_started_at_ns"],
                        "snapshot": payload["snapshot"],
                    },
                    received_at_ns,
                )
                if not archive_only:
                    states[symbol].install_snapshot(payload["snapshot"])
                continue
            if archive_only:
                runtime.segment.mark_replay_unsafe()
                runtime.segment.write(
                    "diff",
                    {
                        "session_id": session_id,
                        "archived_only": True,
                        "frame": payload,
                    },
                    received_at_ns,
                )
                continue
            try:
                archive_and_apply_diff(
                    runtime, states, session_id, received_at_ns, payload
                )
            except SequenceGap as gap:
                drain_gap = gap
                archive_only = True
        if drain_gap is not None:
            raise drain_gap


async def collect(stop: asyncio.Event) -> None:
    global SYMBOLS, SECURITY_TOKEN_SYMBOLS, EXCLUDED_SYMBOLS
    SPOOL_DIR.mkdir(parents=True, exist_ok=True)
    recover_parts()
    runtime = ArchiveRuntime()
    backoff = 1
    initial_budget = PendingBudget(MAX_PENDING_DIFFS_TOTAL)
    last_states = {
        symbol.upper(): OrderBookState(symbol, MARKET, initial_budget)
        for symbol in SYMBOLS
    }
    last_session_id = "startup"
    try:
        while not stop.is_set():
            pending_budget = PendingBudget(MAX_PENDING_DIFFS_TOTAL)
            states = {
                symbol.upper(): OrderBookState(symbol, MARKET, pending_budget)
                for symbol in SYMBOLS
            }
            session_id = uuid.uuid4().hex
            last_states = states
            last_session_id = session_id
            try:
                await run_session(stop, runtime, states, session_id)
                backoff = 1
            except SequenceGap as gap:
                runtime.total_gaps += 1
                write_health(states, session_id, "sequence_gap", runtime.total_gaps)
                LOG.exception("sequence gap; reconnecting in %ss", backoff)
                await runtime.rotate(states, session_id, "sequence_gap")
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, 30)
            except SnapshotUnavailable as error:
                write_health(
                    states, session_id, "catalog_refresh", runtime.total_gaps
                )
                LOG.exception(
                    "snapshot rejected; refreshing catalog and excluding %s",
                    error.symbol,
                )
                await runtime.rotate(states, session_id, "snapshot_unavailable")
                if SYMBOLS_SETTING.upper() == "ALL":
                    discovered, security_tokens = await asyncio.to_thread(
                        discover_symbols_sync
                    )
                    EXCLUDED_SYMBOLS = tuple(
                        sorted({*EXCLUDED_SYMBOLS, error.symbol.lower()})
                    )
                    SYMBOLS, SECURITY_TOKEN_SYMBOLS = exclude_unavailable_symbols(
                        EXCLUDED_SYMBOLS, discovered, security_tokens
                    )
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, 30)
            except Exception:
                write_health(states, session_id, "reconnecting", runtime.total_gaps)
                LOG.exception("websocket session failed; reconnecting in %ss", backoff)
                await runtime.rotate(states, session_id, "reconnect")
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, 30)
    finally:
        await runtime.rotate(
            last_states, last_session_id, "shutdown", create_next=False
        )
        await runtime.finish_uploads()


def self_test() -> None:
    global SPOOL_DIR
    state = OrderBookState("BTCUSDT")
    state.apply_diff({"U": 101, "u": 102, "b": [["100", "0"]], "a": []})
    state.install_snapshot(
        {"lastUpdateId": 100, "bids": [["100", "2"]], "asks": [["101", "3"]]}
    )
    assert state.synced
    assert state.last_update_id == 102
    assert "100" not in state.bids

    with tempfile.TemporaryDirectory() as temporary:
        SPOOL_DIR = Path(temporary)
        segment = Segment()
        segment.write("snapshot", {"symbol": "BTCUSDT", "snapshot": {"lastUpdateId": 1}})
        segment.write("checkpoint", state.checkpoint("self-test"))
        manifest = segment.close()
        assert manifest is not None
        metadata = json.loads(manifest.read_text())
        assert metadata["schema"] == "binance.lob_tape.v2"
        assert metadata["replay_scope"] == (
            "captured_snapshot_seed_plus_sequence_checked_diffs"
        )
        assert metadata["venue_depth_complete"] is False
        assert metadata["event_types"] == {"checkpoint": 1, "snapshot": 1}
        assert metadata["sha256"] == sha256(manifest.with_name(metadata["file"]))
    print("self-test: ok")


def main() -> None:
    global SYMBOLS, SECURITY_TOKEN_SYMBOLS
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return

    if SYMBOLS_SETTING.upper() == "ALL":
        SYMBOLS, SECURITY_TOKEN_SYMBOLS = discover_symbols_sync()
    if not SYMBOLS:
        raise SystemExit("SYMBOLS must not be empty")
    if MODE != "diff":
        raise SystemExit("snapshot reconciliation requires DEPTH_MODE=diff")
    if MARKET not in {"spot", "usdm"}:
        raise SystemExit(f"unsupported MARKET={MARKET}")
    logging.basicConfig(
        level=os.getenv("LOG_LEVEL", "INFO"),
        format="%(asctime)s %(levelname)s %(message)s",
    )
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    stop = asyncio.Event()
    for name in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(name, stop.set)
    if STARTUP_DELAY_SECONDS:
        LOG.info("startup delay=%ss", STARTUP_DELAY_SECONDS)
        time.sleep(STARTUP_DELAY_SECONDS)
    loop.run_until_complete(collect(stop))


if __name__ == "__main__":
    main()
