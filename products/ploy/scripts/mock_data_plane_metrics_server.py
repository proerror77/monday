#!/usr/bin/env python3
"""
Deterministic Prometheus text endpoint for Phase 0 baseline tooling.

Use with `scripts/collect_data_plane_baseline.py` to generate a reproducible
seed baseline without requiring live exchange connectivity.
"""

from __future__ import annotations

import argparse
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Dict, Tuple


SymbolKey = Tuple[str, str]


class MetricsModel:
    def __init__(self, time_scale: float) -> None:
        self.start = time.time()
        self.time_scale = time_scale

        self.symbol_rates: Dict[SymbolKey, float] = {
            ("binance_spot", "BTCUSDT"): 24.0,
            ("binance_spot", "ETHUSDT"): 18.0,
            ("binance_kline", "BTCUSDT"): 1.2,
            ("binance_kline", "ETHUSDT"): 1.1,
            ("polymarket_ws", "0xpm-btc"): 7.5,
            ("polymarket_ws", "0xpm-eth"): 6.8,
            ("chainlink_rtds", "BTCUSDT"): 2.4,
            ("chainlink_rtds", "ETHUSDT"): 2.2,
        }
        self.symbol_bases: Dict[SymbolKey, float] = {
            key: 10_000.0 + idx * 500.0 for idx, key in enumerate(self.symbol_rates)
        }
        self.source_bases: Dict[str, float] = {}
        self.source_rates: Dict[str, float] = {}
        for (source_name, _symbol), rate in self.symbol_rates.items():
            self.source_rates[source_name] = self.source_rates.get(source_name, 0.0) + rate
        for idx, source in enumerate(self.source_rates):
            self.source_bases[source] = 25_000.0 + idx * 1_000.0

        self.feed_health: Dict[str, float] = {source: 1.0 for source in self.source_rates}
        self.subscriptions: Dict[str, float] = {
            "binance_spot": 8.0,
            "binance_kline": 12.0,
            "polymarket_ws": 16.0,
            "chainlink_rtds": 6.0,
        }

        self.broadcast_lag_base = 2000.0
        self.broadcast_drop_base = 250.0
        self.broadcast_lag_rate = 0.4
        self.broadcast_drop_rate = 0.03

    def elapsed_virtual(self) -> float:
        return max(0.0, (time.time() - self.start) * self.time_scale)

    def symbol_counter(self, key: SymbolKey) -> float:
        source, symbol = key
        base = self.symbol_bases[(source, symbol)]
        rate = self.symbol_rates[(source, symbol)]
        t = self.elapsed_virtual()
        return base + rate * t

    def source_counter(self, source: str) -> float:
        base = self.source_bases[source]
        rate = self.source_rates[source]
        t = self.elapsed_virtual()
        return base + rate * t

    def broadcast_lag(self) -> float:
        t = self.elapsed_virtual()
        return self.broadcast_lag_base + self.broadcast_lag_rate * t

    def broadcast_drop(self) -> float:
        t = self.elapsed_virtual()
        return self.broadcast_drop_base + self.broadcast_drop_rate * t

    def render_prometheus(self) -> str:
        lines = []
        for (source, symbol) in sorted(self.symbol_rates):
            value = self.symbol_counter((source, symbol))
            lines.append(
                f'ploy_symbol_updates_total{{source="{source}",symbol="{symbol}"}} {value:.6f}'
            )
        for source in sorted(self.source_rates):
            value = self.source_counter(source)
            lines.append(f'ploy_source_messages_total{{source="{source}"}} {value:.6f}')
            lines.append(f'ploy_source_feed_health{{source="{source}"}} {self.feed_health[source]:.0f}')
            subs = self.subscriptions.get(source, 0.0)
            lines.append(f'ploy_source_subscriptions_total{{source="{source}"}} {subs:.0f}')
        lines.append(f"ploy_broadcast_lag_total {self.broadcast_lag():.6f}")
        lines.append(f"ploy_broadcast_drop_total {self.broadcast_drop():.6f}")
        return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Serve deterministic data-plane metrics")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=19090)
    parser.add_argument("--time-scale", type=float, default=1.0)
    args = parser.parse_args()

    if args.time_scale <= 0:
        raise SystemExit("--time-scale must be > 0")

    model = MetricsModel(time_scale=args.time_scale)

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802
            if self.path not in ("/metrics", "/metrics/"):
                self.send_response(404)
                self.end_headers()
                return
            body = model.render_prometheus().encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; version=0.0.4")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, fmt: str, *args_: object) -> None:
            return

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(
        f"mock data-plane metrics server listening on http://{args.host}:{args.port}/metrics "
        f"(time_scale={args.time_scale})"
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
