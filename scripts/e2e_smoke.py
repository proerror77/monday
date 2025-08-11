#!/usr/bin/env python3
"""
Minimal E2E smoke test for control_ws orchestration.

Steps:
1) Publish ml.deploy with acceptable metrics (canary)
2) Publish system.metrics within thresholds to trigger early promote
3) Subscribe to model.status and print events

Env:
  REDIS_URL (default: redis://localhost:6379/0)
"""

import json
import os
import time
import uuid

import redis


def main():
    redis_url = os.getenv("REDIS_URL", "redis://localhost:6379/0")
    r = redis.Redis.from_url(redis_url, decode_responses=True)

    symbol = os.getenv("SMOKE_SYMBOL", "BTCUSDT")
    model_path = f"models/{symbol.lower()}_{uuid.uuid4().hex[:8]}.pt"

    # Subscribe to model.status
    ps = r.pubsub(ignore_subscribe_messages=True)
    ps.subscribe("model.status")

    # 1) Publish ml.deploy (canary)
    deploy = {
        "event": "model_ready",
        "symbol": symbol,
        "model_path": model_path,
        "metrics": {"ic": 0.035, "ir": 1.3, "max_drawdown": 0.04},
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "version": 1,
    }
    r.publish("ml.deploy", json.dumps(deploy))
    print("Published ml.deploy", deploy)

    # 2) Publish healthy system.metrics to promote
    time.sleep(1)
    metrics = {
        "event": "metrics",
        "symbol": symbol,
        "values": {"latency_p99_us": 8.0, "error_rate": 0.0, "pnl_drawdown": 0.0},
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "version": 1,
    }
    r.publish("system.metrics", json.dumps(metrics))
    print("Published system.metrics", metrics)

    # 3) Read model.status events
    deadline = time.time() + 10
    while time.time() < deadline:
        msg = ps.get_message(timeout=1.0)
        if msg and msg.get("type") == "message":
            data = json.loads(msg["data"]) if isinstance(msg.get("data"), str) else msg.get("data")
            print("model.status:", data)
            if data.get("symbol") == symbol and data.get("status") in ("canary", "promoted", "rolled_back", "failed"):
                if data.get("status") == "promoted":
                    print("Smoke test: PROMOTED ✅")
                    return 0
        time.sleep(0.2)

    print("Smoke test: TIMEOUT ⚠️")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

