#!/usr/bin/env python3
"""
Train a Temporal Convolutional Network (TCN) to predict y_up
(Polymarket official settlement) from Binance LOB-derived features,
and export the model to ONNX.

This script reads directly from Postgres (no dataset file required).
Training source is fixed to:
  `sync_records + pm_market_metadata + pm_token_settlements`
with required `--horizon 5m|15m` to train per-window models.

Install deps on the training machine:
  python3 -m pip install torch psycopg2-binary

Optional (save a Parquet snapshot for reproducibility):
  python3 -m pip install pandas pyarrow
"""

from __future__ import annotations

import argparse
import json
import math
import os
import random
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import List, Optional, Tuple


FEATURE_ORDER = [
    "obi5",
    "obi10",
    "spread_bps",
    "bid_volume_5",
    "ask_volume_5",
    "momentum_1s",
    "momentum_5s",
    "spot_price",
    "remaining_secs",
    "price_to_beat",
    "distance_to_beat",
]

SEQ_LEN_BY_HORIZON = {"5m": 60, "15m": 180}


@dataclass
class Dataset:
    x: List[List[float]]
    y: List[int]
    ts: List[str]  # RFC3339


@dataclass
class SequenceRow:
    market_slug: str
    horizon: str
    ts: datetime
    y_up: int
    features: List[float]


def mean_std(x: List[List[float]]) -> Tuple[List[float], List[float]]:
    n = len(x)
    d = len(x[0])
    mean = [0.0] * d
    var = [0.0] * d

    for row in x:
        for j, v in enumerate(row):
            mean[j] += v
    mean = [m / n for m in mean]

    for row in x:
        for j, v in enumerate(row):
            dv = v - mean[j]
            var[j] += dv * dv
    var = [v / max(1, n - 1) for v in var]
    std = [math.sqrt(v) for v in var]
    std = [s if s > 1e-12 else 1.0 for s in std]
    return mean, std


def zscore(x: List[List[float]], mean: List[float], std: List[float]) -> List[List[float]]:
    out: List[List[float]] = []
    for row in x:
        out.append([(row[j] - mean[j]) / std[j] for j in range(len(row))])
    return out


def acc_at_05(y_true: List[int], p: List[float]) -> float:
    c = 0
    for yt, pi in zip(y_true, p):
        pred = 1 if pi >= 0.5 else 0
        if pred == yt:
            c += 1
    return c / max(1, len(y_true))


def brier(y_true: List[int], p: List[float]) -> float:
    s = 0.0
    for yt, pi in zip(y_true, p):
        s += (pi - float(yt)) ** 2
    return s / max(1, len(y_true))


def log_loss(y_true: List[int], p: List[float]) -> float:
    eps = 1e-12
    s = 0.0
    for yt, pi in zip(y_true, p):
        pi = min(1.0 - eps, max(eps, pi))
        if yt == 1:
            s += -math.log(pi)
        else:
            s += -math.log(1.0 - pi)
    return s / max(1, len(y_true))


def chronological_split(ds: Dataset, test_ratio: float) -> Tuple[Dataset, Dataset]:
    n = len(ds.y)
    if n < 100:
        raise SystemExit(f"dataset too small: n={n} (need >=100)")
    if not (0.05 <= test_ratio <= 0.5):
        raise SystemExit("--test-ratio must be in [0.05, 0.5]")

    idx = list(range(n))
    idx.sort(key=lambda i: ds.ts[i])

    cut = int((1.0 - test_ratio) * n)
    cut = max(1, min(cut, n - 1))

    def take(idxs: List[int]) -> Dataset:
        return Dataset(
            x=[ds.x[i] for i in idxs],
            y=[ds.y[i] for i in idxs],
            ts=[ds.ts[i] for i in idxs],
        )

    return take(idx[:cut]), take(idx[cut:])


def build_sequence_dataset(rows: List[SequenceRow], sequence_len: int, stride: int = 1) -> Dataset:
    grouped: dict[str, List[SequenceRow]] = {}
    for row in rows:
        grouped.setdefault(row.market_slug, []).append(row)

    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []

    for market_slug, bucket in grouped.items():
        bucket.sort(key=lambda r: r.ts)

        # Keep one snapshot per second to stabilize sequence spacing.
        one_sec: List[SequenceRow] = []
        for row in bucket:
            row_ts = row.ts
            if row_ts.tzinfo is None:
                row_ts = row_ts.replace(tzinfo=timezone.utc)
            sec = int(row_ts.timestamp())
            if one_sec:
                last_ts = one_sec[-1].ts
                if last_ts.tzinfo is None:
                    last_ts = last_ts.replace(tzinfo=timezone.utc)
                if int(last_ts.timestamp()) == sec:
                    one_sec[-1] = row
                    continue
            one_sec.append(row)

        if len(one_sec) < sequence_len:
            continue

        for i in range(sequence_len - 1, len(one_sec), stride):
            window = one_sec[i - sequence_len + 1 : i + 1]
            flat: List[float] = []
            for snap in window:
                flat.extend(snap.features)
            x.append(flat)
            y.append(int(window[-1].y_up))
            out_ts = window[-1].ts
            if out_ts.tzinfo is None:
                out_ts = out_ts.replace(tzinfo=timezone.utc)
            ts.append(out_ts.isoformat())

    if not x:
        raise SystemExit(
            f"no usable sequence windows (seq_len={sequence_len}); check horizon/lookback/data density"
        )

    return Dataset(x=x, y=y, ts=ts)


def fetch_from_order_executions(
    db_url: str,
    lookback_hours: int,
    account_id: Optional[str],
    agent_id: Optional[str],
    live_only: bool,
    limit: int,
) -> Dataset:
    try:
        import psycopg2  # type: ignore
    except Exception as e:
        raise SystemExit(
            "psycopg2 is required.\n"
            "Install: python3 -m pip install psycopg2-binary\n"
            f"Import error: {e}"
        )

    if limit <= 0:
        raise SystemExit("--limit must be > 0")

    where = [
        "e.executed_at >= NOW() - (%s::bigint * INTERVAL '1 hour')",
        "e.filled_shares > 0",
        "LOWER(e.domain) = 'crypto'",
        "s.resolved = TRUE",
        "s.settled_price IS NOT NULL",
        # Prefer entry intents
        "((e.metadata ? 'signal_type' AND RIGHT(e.metadata->>'signal_type', 6) = '_entry') OR (NOT (e.metadata ? 'signal_type') AND e.is_buy = TRUE))",
        # Require feature keys so casts do not throw
        "e.metadata ? 'lob_obi_5'",
        "e.metadata ? 'lob_obi_10'",
        "e.metadata ? 'lob_spread_bps'",
        "e.metadata ? 'lob_bid_volume_5'",
        "e.metadata ? 'lob_ask_volume_5'",
        "e.metadata ? 'signal_momentum_1s'",
        "e.metadata ? 'signal_momentum_5s'",
    ]
    params: List[object] = [lookback_hours]

    if account_id:
        where.append("e.account_id = %s")
        params.append(account_id)
    if agent_id:
        where.append("e.agent_id = %s")
        params.append(agent_id)
    if live_only:
        where.append("e.dry_run = FALSE")

    sql = f"""
    SELECT
      e.executed_at,
      (e.metadata->>'lob_obi_5')::double precision as obi5,
      (e.metadata->>'lob_obi_10')::double precision as obi10,
      (e.metadata->>'lob_spread_bps')::double precision as spread_bps,
      (e.metadata->>'lob_bid_volume_5')::double precision as bid_volume_5,
      (e.metadata->>'lob_ask_volume_5')::double precision as ask_volume_5,
      (e.metadata->>'signal_momentum_1s')::double precision as momentum_1s,
      (e.metadata->>'signal_momentum_5s')::double precision as momentum_5s,
      CASE
        WHEN e.market_side = 'UP' THEN CASE WHEN s.settled_price > 0.5 THEN 1 ELSE 0 END
        WHEN e.market_side = 'DOWN' THEN CASE WHEN s.settled_price > 0.5 THEN 0 ELSE 1 END
        ELSE NULL
      END as y_up
    FROM agent_order_executions e
    JOIN pm_token_settlements s
      ON s.token_id = e.token_id
    WHERE {" AND ".join(where)}
    ORDER BY e.executed_at ASC
    LIMIT {int(limit)}
    """

    x: List[List[float]] = []
    y: List[int] = []
    ts: List[str] = []

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            cur.execute(sql, params)
            for row in cur.fetchall():
                executed_at = row[0]
                feats = list(row[1:8]) + [0.0, 0.0, 0.0, 0.0]
                yi = row[8]
                if yi not in (0, 1):
                    continue
                if any(v is None or (isinstance(v, float) and not math.isfinite(v)) for v in feats):
                    continue
                x.append([float(v) for v in feats])
                y.append(int(yi))
                if hasattr(executed_at, "isoformat"):
                    ts.append(executed_at.replace(tzinfo=timezone.utc).isoformat())
                else:
                    ts.append(str(executed_at))
    finally:
        conn.close()

    if not x:
        raise SystemExit("no usable rows fetched")
    return Dataset(x=x, y=y, ts=ts)


def fetch_from_sync_records(
    db_url: str,
    lookback_hours: int,
    symbol: Optional[str],
    horizon: Optional[str],
    limit: int,
    stride: int = 1,
) -> Dataset:
    try:
        import psycopg2  # type: ignore
    except Exception as e:
        raise SystemExit(
            "psycopg2 is required.\n"
            "Install: python3 -m pip install psycopg2-binary\n"
            f"Import error: {e}"
        )

    if limit <= 0:
        raise SystemExit("--limit must be > 0")

    if horizon is None:
        raise SystemExit("--horizon is required for sequence training (5m or 15m)")
    horizon_norm = horizon.strip().lower()
    if horizon_norm not in ("5m", "15m"):
        raise SystemExit("--horizon must be one of: 5m, 15m")
    sequence_len = SEQ_LEN_BY_HORIZON[horizon_norm]

    where = [
        "sr.timestamp >= NOW() - (%s::bigint * INTERVAL '1 hour')",
        "sr.pm_market_slug IS NOT NULL",
        "ml.y_up IS NOT NULL",
        "md.price_to_beat IS NOT NULL",
        "md.horizon = %s",
        "md.end_time IS NOT NULL",
        "sr.timestamp <= md.end_time",
        "sr.bn_mid_price IS NOT NULL",
        "sr.bn_obi_5 IS NOT NULL",
        "sr.bn_obi_10 IS NOT NULL",
        "sr.bn_spread_bps IS NOT NULL",
        "sr.bn_bid_volume IS NOT NULL",
        "sr.bn_ask_volume IS NOT NULL",
        "sr.bn_price_change_1s IS NOT NULL",
        "sr.bn_price_change_5s IS NOT NULL",
    ]
    params: List[object] = [lookback_hours, horizon_norm]

    if symbol:
        where.append("sr.symbol = %s")
        params.append(symbol.strip().upper())

    sql = f"""
    WITH market_labels AS (
      SELECT
        market_slug,
        CASE
          WHEN SUM(CASE WHEN LOWER(outcome) LIKE '%up%' THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) LIKE '%up%' THEN CASE WHEN settled_price > 0.5 THEN 1 ELSE 0 END END)
          WHEN SUM(CASE WHEN LOWER(outcome) IN ('yes', 'true') THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) IN ('yes', 'true') THEN CASE WHEN settled_price > 0.5 THEN 1 ELSE 0 END END)
          WHEN SUM(CASE WHEN LOWER(outcome) LIKE '%down%' THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) LIKE '%down%' THEN CASE WHEN settled_price > 0.5 THEN 0 ELSE 1 END END)
          WHEN SUM(CASE WHEN LOWER(outcome) IN ('no', 'false') THEN 1 ELSE 0 END) > 0 THEN
            MAX(CASE WHEN LOWER(outcome) IN ('no', 'false') THEN CASE WHEN settled_price > 0.5 THEN 0 ELSE 1 END END)
          ELSE NULL
        END AS y_up
      FROM pm_token_settlements
      WHERE resolved = TRUE
        AND settled_price IS NOT NULL
        AND market_slug IS NOT NULL
      GROUP BY market_slug
    )
    SELECT
      sr.timestamp,
      sr.pm_market_slug,
      md.horizon,
      sr.symbol,
      sr.bn_mid_price::double precision AS spot_price,
      sr.bn_obi_5::double precision AS obi5,
      sr.bn_obi_10::double precision AS obi10,
      sr.bn_spread_bps::double precision AS spread_bps,
      sr.bn_bid_volume::double precision AS bid_volume_5,
      sr.bn_ask_volume::double precision AS ask_volume_5,
      sr.bn_price_change_1s::double precision AS momentum_1s,
      sr.bn_price_change_5s::double precision AS momentum_5s,
      md.price_to_beat::double precision AS price_to_beat,
      EXTRACT(EPOCH FROM (md.end_time - sr.timestamp))::double precision AS remaining_secs,
      ml.y_up
    FROM sync_records sr
    JOIN market_labels ml
      ON ml.market_slug = sr.pm_market_slug
    JOIN pm_market_metadata md
      ON md.market_slug = sr.pm_market_slug
    WHERE {" AND ".join(where)}
    ORDER BY sr.pm_market_slug ASC, sr.timestamp ASC
    LIMIT {int(limit)}
    """

    rows: List[SequenceRow] = []

    conn = psycopg2.connect(db_url)
    try:
        with conn.cursor() as cur:
            cur.execute(sql, params)
            for row in cur.fetchall():
                timestamp = row[0]
                market_slug = row[1]
                horizon_value = row[2]
                _symbol = row[3]
                spot_price = row[4]
                base_feats = list(row[5:12])
                price_to_beat = row[12]
                remaining_secs = row[13]
                yi = row[14]

                if yi not in (0, 1):
                    continue
                if (
                    market_slug is None
                    or horizon_value not in ("5m", "15m")
                    or spot_price is None
                    or price_to_beat is None
                    or remaining_secs is None
                    or remaining_secs < 0
                ):
                    continue
                if any(v is None or (isinstance(v, float) and not math.isfinite(v)) for v in base_feats):
                    continue
                if not isinstance(spot_price, (int, float)) or not math.isfinite(float(spot_price)):
                    continue
                if not isinstance(price_to_beat, (int, float)) or not math.isfinite(float(price_to_beat)):
                    continue
                if not isinstance(remaining_secs, (int, float)) or not math.isfinite(float(remaining_secs)):
                    continue

                spot = float(spot_price)
                threshold = float(price_to_beat)
                remaining = float(remaining_secs)
                distance = 0.0 if abs(spot) < 1e-12 else (threshold - spot) / spot
                feats = [float(v) for v in base_feats] + [spot, remaining, threshold, distance]

                if hasattr(timestamp, "replace"):
                    ts_dt = timestamp
                    if ts_dt.tzinfo is None:
                        ts_dt = ts_dt.replace(tzinfo=timezone.utc)
                else:
                    continue

                rows.append(
                    SequenceRow(
                        market_slug=str(market_slug),
                        horizon=str(horizon_value),
                        ts=ts_dt,
                        y_up=int(yi),
                        features=feats,
                    )
                )
    finally:
        conn.close()

    if not rows:
        raise SystemExit("no usable rows fetched from sync_records + pm_market_metadata")

    return build_sequence_dataset(rows, sequence_len, stride=stride)


def maybe_save_parquet(ds: Dataset, path: str) -> None:
    try:
        import pandas as pd  # type: ignore
    except Exception:
        print("[warn] pandas not installed; skipping parquet snapshot")
        return

    try:
        import pyarrow  # noqa: F401
        import pyarrow.parquet  # noqa: F401
    except Exception:
        print("[warn] pyarrow not installed; skipping parquet snapshot")
        return

    rows = []
    for feats, yi, ts in zip(ds.x, ds.y, ds.ts):
        r = {"executed_at": ts, "y_up": yi}
        for k, v in zip(FEATURE_ORDER, feats):
            r[k] = v
        rows.append(r)

    df = pd.DataFrame(rows)
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    df.to_parquet(path, index=False)
    print(f"Saved parquet snapshot: {path} (rows={len(df)})")


def train_and_export_onnx(
    train_ds: Dataset,
    test_ds: Dataset,
    sequence_len: int,
    channels: int,
    kernel_size: int,
    dropout: float,
    epochs: int,
    batch_size: int,
    lr: float,
    seed: int,
    onnx_path: str,
    opset: int,
) -> dict:
    try:
        import torch
        import torch.nn as nn
    except Exception as e:
        raise SystemExit(
            "PyTorch is required for training.\n"
            "Install: python3 -m pip install torch\n"
            f"Import error: {e}"
        )

    torch.manual_seed(seed)
    random.seed(seed)

    in_dim = len(train_ds.x[0])
    feature_dim = len(FEATURE_ORDER)
    expected_dim = sequence_len * feature_dim
    if in_dim != expected_dim:
        raise SystemExit(
            f"flattened input_dim mismatch: got {in_dim}, expected {expected_dim} "
            f"(sequence_len={sequence_len}, feature_dim={feature_dim})"
        )
    mean, std = mean_std(train_ds.x)
    x_train = zscore(train_ds.x, mean, std)
    x_test = zscore(test_ds.x, mean, std)

    if channels < 4:
        raise SystemExit("--channels must be >= 4")
    if kernel_size % 2 == 0 or kernel_size < 3:
        raise SystemExit("--kernel-size must be odd and >= 3")
    if not (0.0 <= dropout < 1.0):
        raise SystemExit("--dropout must be in [0, 1)")

    class LobTCN(nn.Module):
        def __init__(
            self,
            mean_vec,
            std_vec,
            ch: int,
            k: int,
            p_drop: float,
            seq_len: int,
            feat_dim: int,
        ):
            super().__init__()
            self.register_buffer("mean", torch.tensor(mean_vec, dtype=torch.float32))
            self.register_buffer("std", torch.tensor(std_vec, dtype=torch.float32))
            self.seq_len = seq_len
            self.feat_dim = feat_dim
            pad = k // 2
            self.net = nn.Sequential(
                nn.Conv1d(feat_dim, ch, kernel_size=k, padding=pad),
                nn.GELU(),
                nn.Dropout(p_drop),
                nn.Conv1d(ch, ch, kernel_size=k, padding=pad),
                nn.GELU(),
                nn.Dropout(p_drop),
                nn.Conv1d(ch, ch * 2, kernel_size=k, padding=pad),
                nn.GELU(),
            )
            self.pool = nn.AdaptiveAvgPool1d(1)
            self.head = nn.Linear(ch * 2, 1)

        def forward(self, x):
            x = (x - self.mean) / self.std
            x = x.view(-1, self.seq_len, self.feat_dim).transpose(1, 2).contiguous()
            h = self.net(x)
            h = self.pool(h).squeeze(-1)
            logits = self.head(h)
            return torch.sigmoid(logits)

    model = LobTCN(mean, std, channels, kernel_size, dropout, sequence_len, feature_dim)
    opt = torch.optim.Adam(model.parameters(), lr=lr)
    loss_fn = nn.BCELoss()

    xtr = torch.tensor(x_train, dtype=torch.float32)
    ytr = torch.tensor(train_ds.y, dtype=torch.float32).view(-1, 1)
    xte = torch.tensor(x_test, dtype=torch.float32)

    n = xtr.shape[0]
    idxs = list(range(n))

    for epoch in range(1, epochs + 1):
        model.train()
        random.shuffle(idxs)

        total_loss = 0.0
        for start in range(0, n, batch_size):
            bidx = idxs[start : start + batch_size]
            xb = xtr[bidx]
            yb = ytr[bidx]

            opt.zero_grad()
            p = model(xb)
            loss = loss_fn(p, yb)
            loss.backward()
            torch.nn.utils.clip_grad_norm_(model.parameters(), max_norm=1.0)
            opt.step()

            total_loss += float(loss.detach().cpu().item()) * len(bidx)

        if epoch == 1 or epoch == epochs or epoch % max(1, epochs // 5) == 0:
            model.eval()
            with torch.no_grad():
                p_test = model(xte).cpu().numpy().reshape(-1).tolist()
            print(
                f"epoch {epoch:>3}/{epochs}  "
                f"train_loss={total_loss/max(1,n):.6f}  "
                f"test_acc@0.5={acc_at_05(test_ds.y, p_test)*100:.2f}%"
            )

    model.eval()
    with torch.no_grad():
        p_test = model(xte).cpu().numpy().reshape(-1).tolist()

    metrics = {
        "n_train": len(train_ds.y),
        "n_test": len(test_ds.y),
        "acc_at_0.5": acc_at_05(test_ds.y, p_test),
        "brier": brier(test_ds.y, p_test),
        "log_loss": log_loss(test_ds.y, p_test),
    }
    try:
        from sklearn.metrics import roc_auc_score
        metrics["auc"] = roc_auc_score(test_ds.y, p_test)
    except Exception:
        metrics["auc"] = float("nan")

    os.makedirs(os.path.dirname(onnx_path) or ".", exist_ok=True)
    dummy = torch.zeros((1, in_dim), dtype=torch.float32)
    torch.onnx.export(
        model,
        dummy,
        onnx_path,
        input_names=["x"],
        output_names=["p_up"],
        opset_version=opset,
    )

    return {
        "type": "tcn_binary_classifier",
        "feature_order": FEATURE_ORDER,
        "sequence_len": sequence_len,
        "feature_dim": feature_dim,
        "input_dim": in_dim,
        "channels": channels,
        "kernel_size": kernel_size,
        "dropout": dropout,
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "metrics": metrics,
        "note": "Model includes z-score normalization inside ONNX graph.",
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--db-url",
        default=os.environ.get("DATABASE_URL", "postgres://localhost/ploy"),
        help="Postgres URL (or env DATABASE_URL)",
    )
    ap.add_argument("--lookback-hours", type=int, default=168)
    ap.add_argument(
        "--source",
        choices=["sync_records"],
        default="sync_records",
        help="training data source (db-only)",
    )
    ap.add_argument(
        "--symbol",
        default=None,
        help="optional symbol filter for sync_records source (e.g., BTCUSDT)",
    )
    ap.add_argument(
        "--horizon",
        choices=["5m", "15m"],
        default=None,
        help="horizon for sync_records sequence training (required for sync_records)",
    )
    ap.add_argument("--limit", type=int, default=50000)
    ap.add_argument("--test-ratio", type=float, default=0.2)
    ap.add_argument("--channels", type=int, default=32)
    ap.add_argument("--kernel-size", type=int, default=3)
    ap.add_argument("--dropout", type=float, default=0.10)
    ap.add_argument("--epochs", type=int, default=25)
    ap.add_argument("--batch-size", type=int, default=1024)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--output", default="./models/crypto/lob_tcn_v1.onnx")
    ap.add_argument("--meta", default="./models/crypto/lob_tcn_v1.meta.json")
    ap.add_argument("--save-parquet", default=None)
    ap.add_argument("--stride", type=int, default=1, help="step between sequence windows per market to reduce overlap (default: 1)")
    ap.add_argument("--export-scaler", default=None, help="export feature scaler (offsets/scales) as JSON for config-based normalization")

    args = ap.parse_args()

    print(f"Fetching from DB (source={args.source})...")
    if args.horizon is None:
        raise SystemExit("--horizon is required when --source=sync_records")
    sequence_len = SEQ_LEN_BY_HORIZON[args.horizon]
    ds = fetch_from_sync_records(
        db_url=args.db_url,
        lookback_hours=args.lookback_hours,
        symbol=args.symbol,
        horizon=args.horizon,
        limit=args.limit,
        stride=args.stride,
    )
    print(f"Rows: {len(ds.y)}")

    if args.save_parquet:
        maybe_save_parquet(ds, args.save_parquet)

    train_ds, test_ds = chronological_split(ds, args.test_ratio)
    print(f"Split: train={len(train_ds.y)} test={len(test_ds.y)}")

    meta = train_and_export_onnx(
        train_ds=train_ds,
        test_ds=test_ds,
        sequence_len=sequence_len,
        channels=args.channels,
        kernel_size=args.kernel_size,
        dropout=args.dropout,
        epochs=args.epochs,
        batch_size=max(1, args.batch_size),
        lr=args.lr,
        seed=args.seed,
        onnx_path=args.output,
        opset=args.opset,
    )

    os.makedirs(os.path.dirname(args.meta) or ".", exist_ok=True)
    with open(args.meta, "w") as f:
        json.dump(meta, f, indent=2)

    # Export scaler for config-based normalization (offset=mean, scale=1/std)
    if args.export_scaler:
        mean, std = mean_std(train_ds.x)
        scaler = {
            "feature_names": FEATURE_ORDER,
            "feature_offsets": mean,
            "feature_scales": [1.0 / s if s > 0 else 1.0 for s in std],
        }
        os.makedirs(os.path.dirname(args.export_scaler) or ".", exist_ok=True)
        with open(args.export_scaler, "w") as f:
            json.dump(scaler, f, indent=2)
        print(f"  scaler: {args.export_scaler}")

    m = meta["metrics"]
    print("\nExported:")
    print(f"  onnx: {args.output}")
    print(f"  meta: {args.meta}")
    print(
        f"  metrics: acc@0.5={m['acc_at_0.5']*100:.2f}%  brier={m['brier']:.6f}  ll={m['log_loss']:.6f}  auc={m.get('auc', float('nan')):.4f}"
    )

    print("\nEnable on EC2 (example):")
    print("  PLOY_CRYPTO_LOB_ML__ENABLED=true")
    print("  PLOY_CRYPTO_LOB_ML__MODEL_TYPE=onnx")
    print(f"  PLOY_CRYPTO_LOB_ML__MODEL_PATH={args.output}")
    print("  PLOY_CRYPTO_LOB_ML__MODEL_VERSION=lob_tcn_v1")
    print(f"  PLOY_CRYPTO_LOB_ML__MODEL_INPUT_DIM={meta['input_dim']}")
    print("  PLOY_CRYPTO_LOB_ML__WINDOW_FALLBACK_WEIGHT=0.00")


if __name__ == "__main__":
    main()
