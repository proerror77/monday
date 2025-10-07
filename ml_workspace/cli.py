#!/usr/bin/env python3
"""
Unified CLI for ML Workspace

Subcommands:
- qa run: data quality checks (coverage/schema/gaps)
- dataset build: build multi-symbol feature dataset to Parquet
- train fit: supervised DL training using FeatureEngineer + DLTrainer
- features verify: validate data/tables compatibility and coverage

Usage examples:
- python cli.py qa run --config configs/qa.yaml
- python cli.py dataset build --config configs/features_build.yaml
- python cli.py train fit --config configs/train_fit.yaml
- python cli.py features verify --config configs/qa.yaml
"""

from __future__ import annotations

import argparse
import asyncio
import json
from pathlib import Path
from typing import Any, Dict, List

import yaml
from utils.logging import configure_logging


def _load_yaml(path: str | Path) -> Dict[str, Any]:
    with open(path, "r", encoding="utf-8") as f:
        return yaml.safe_load(f) or {}


def cmd_qa_run(args: argparse.Namespace) -> None:
    from workflows.qa.quality_checks import run_checks

    cfg = _load_yaml(args.config)

    def _get(d: Dict[str, Any], key: str, default=None):
        return d.get(key, default)

    result = run_checks(
        host=_get(cfg, "host"),
        user=_get(cfg, "user", "default"),
        password=_get(cfg, "password", ""),
        database=_get(cfg, "database", "hft"),
        symbol=_get(cfg, "symbol", "WLFIUSDT"),
        start_iso=_get(cfg, "start"),
        end_iso=_get(cfg, "end"),
        tables=_get(cfg, "tables"),
        top_gaps=int(_get(cfg, "top_gaps", 0) or 0),
        show_tables=bool(_get(cfg, "list_tables", False)),
        calc_overlap=bool(_get(cfg, "overlap", False)),
        compat_profile=_get(cfg, "compat_profile"),
        compat_profile_json=_get(cfg, "compat_profile_json"),
        schema_dump_tables=_get(cfg, "schema_dump_tables"),
        search_symbol=_get(cfg, "search_symbol"),
        search_tables=_get(cfg, "search_tables"),
        load_tests_hours=_get(cfg, "load_tests_hours"),
        range_compare=(
            (_get(cfg, "range_compare", {}) or {}).get("raw"),
            (_get(cfg, "range_compare", {}) or {}).get("feature"),
            (_get(cfg, "range_compare", {}) or {}).get("symbol"),
        ) if _get(cfg, "range_compare") else None,
    )

    print(json.dumps(result, indent=2, ensure_ascii=False))


def cmd_dataset_build(args: argparse.Namespace) -> None:
    from utils.clickhouse_client import ClickHouseClient
    from utils.time import to_ms
    from utils.ch_queries import build_feature_sql
    from workflows.build_multisymbol_dataset import find_common_symbols
    import pandas as pd
    from datetime import datetime, timezone

    cfg = _load_yaml(args.config)

    host = cfg.get("host")
    user = cfg.get("user", "default")
    password = cfg.get("password", "")
    database = cfg.get("database", "hft")
    start_iso = cfg.get("start")
    end_iso = cfg.get("end")
    top = int(cfg.get("top", 12))
    pick = int(cfg.get("pick", 8))
    out_dir = Path(cfg.get("out", "./workspace/datasets"))
    out_dir.mkdir(parents=True, exist_ok=True)

    def _iso_to_ms(s: str) -> int:
        dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
        return to_ms(dt)

    start_ms = _iso_to_ms(start_iso)
    end_ms = _iso_to_ms(end_iso)

    client = ClickHouseClient(host=host, username=user, password=password, database=database)
    if not client.test_connection():
        raise RuntimeError("无法连接 ClickHouse，请检查 host/凭证")

    cand_df = find_common_symbols(client, start_ms, end_ms, top_n=top)
    if cand_df.empty:
        raise RuntimeError("时间窗内未找到同时存在于 BBO/Depth/Trades 的品种")

    symbols = cand_df["symbol"].tolist()[:pick]
    frames: List[pd.DataFrame] = []
    for sym in symbols:
        sql = build_feature_sql(sym, start_ms, end_ms)
        df = client.query_to_dataframe(sql)
        if df is None or df.empty:
            print(f"[警告] {sym} 无特征数据，跳过")
            continue
        # 标准化时间列
        if "exchange_ts" in df.columns:
            df["datetime"] = pd.to_datetime(df["exchange_ts"], unit="ms", utc=True)
        frames.append(df)
        print(f"{sym} 特征行数: {len(df):,}")

    if not frames:
        raise RuntimeError("未生成任何品种的特征数据")

    full = pd.concat(frames, ignore_index=True)
    # 清理缺失
    num_cols = full.select_dtypes(include=["float64", "float32", "int64", "int32"]).columns
    full[num_cols] = full[num_cols].fillna(0.0)

    start_tag = start_iso[:10].replace("-", "")
    end_tag = end_iso[:10].replace("-", "")
    out_path = out_dir / f"multisymbol_queue_features_{start_tag}_{end_tag}.parquet"
    full.to_parquet(out_path, index=False)
    print(f"✅ 数据集已保存: {out_path}")


def cmd_train_fit(args: argparse.Namespace) -> None:
    import torch
    from components.feature_engineering import FeatureEngineer, FEConfig
    from algorithms.dl.trainer import DLTrainer, DLTrainConfig
    from utils.clickhouse_client import ClickHouseClient

    cfg = _load_yaml(args.config)
    ch_cfg = cfg.get("clickhouse", {})
    symbol = cfg.get("symbol", "WLFIUSDT")
    hours = int(cfg.get("hours", 24))

    # Feature extraction
    ch_client = ClickHouseClient(
        host=ch_cfg.get("host"),
        username=ch_cfg.get("user", "default"),
        password=ch_cfg.get("password", ""),
        database=ch_cfg.get("database", "hft"),
    )
    fe = FeatureEngineer(FEConfig(seq_len=int(cfg.get("seq_len", 60))), client=ch_client)

    async def _run_fe() -> Dict[str, Any]:
        return await fe.extract_features_for_training(symbol=symbol, hours=hours, refresh=bool(args.refresh))

    features = asyncio.run(_run_fe())

    # Trainer config
    trainer_cfg = DLTrainConfig(
        d_model=int(cfg.get("d_model", 64)),
        nhead=int(cfg.get("nhead", 4)),
        num_layers=int(cfg.get("num_layers", 2)),
        dropout=float(cfg.get("dropout", 0.1)),
        epochs=int(cfg.get("epochs", 5)),
        batch_size=int(cfg.get("batch_size", 256)),
        lr=float(cfg.get("lr", 1e-3)),
        weight_decay=float(cfg.get("weight_decay", 1e-4)),
        export_torchscript=True,
        export_onnx=False,
    )

    trainer = DLTrainer(model_dir=cfg.get("model_dir", "./models"), config=trainer_cfg.__dict__)
    result = trainer.train(features)

    print(json.dumps(result, indent=2))


def cmd_features_verify(args: argparse.Namespace) -> None:
    from workflows.qa.quality_checks import run_checks

    cfg = _load_yaml(args.config)
    symbol = cfg.get("symbol", "WLFIUSDT")
    start = cfg.get("start")
    end = cfg.get("end")
    tables = cfg.get("tables", ["bitget_futures_bbo", "bitget_futures_depth", "bitget_futures_trades"])

    result = run_checks(
        host=cfg.get("host"),
        user=cfg.get("user", "default"),
        password=cfg.get("password", ""),
        database=cfg.get("database", "hft"),
        symbol=symbol,
        start_iso=start,
        end_iso=end,
        tables=tables,
        top_gaps=int(cfg.get("top_gaps", 0) or 0),
    )
    print(json.dumps(result, indent=2, ensure_ascii=False))


def cmd_unsup_train(args: argparse.Namespace) -> None:
    from workflows.tcn_gru_train import train_tcn_gru

    horizons = [int(x) for x in str(args.horizons).split(',') if x]
    result = train_tcn_gru(
        symbol=args.symbol,
        start_date=args.start,
        end_date=args.end,
        horizons=horizons,
        seq_len=int(args.seq_len),
        taker_fee=float(args.taker_fee),
        maker_fee=float(args.maker_fee),
        use_maker=bool(args.use_maker),
        epochs=int(args.epochs),
        batch_size=int(args.batch_size),
        learning_rate=float(args.lr),
        model_dir=str(args.model_dir),
        random_seed=int(args.seed),
        refresh=bool(args.refresh),
    )
    print(json.dumps(result, indent=2, ensure_ascii=False))


def cmd_backtest_dl(args: argparse.Namespace) -> None:
    from pathlib import Path
    from datetime import datetime
    import torch
    from utils.time import to_ms
    from utils.clickhouse_client import ClickHouseClient
    from workflows.dl_predict import load_dl_model, predict_single_horizon
    from workflows.dl_backtest import BacktestConfig, run_simple_backtest, plot_equity_curve

    model_path = Path(args.model)
    if not model_path.exists():
        raise FileNotFoundError(f"Model file not found: {model_path}")

    # time window
    start_ms = to_ms(datetime.fromisoformat(args.start.replace('Z', '+00:00')))
    end_ms = to_ms(datetime.fromisoformat(args.end.replace('Z', '+00:00')))

    # device
    device = torch.device('mps' if torch.backends.mps.is_available() else ('cuda' if torch.cuda.is_available() else 'cpu'))

    # load model
    model, metadata = load_dl_model(str(model_path), device)
    seq_len = int(metadata.get('sequence_length', 60))

    # client
    client = ClickHouseClient(host=args.host, username=args.user, password=args.password, database=args.database)
    if not client.test_connection():
        raise RuntimeError('Cannot connect to ClickHouse')

    # predict
    preds = predict_single_horizon(
        client=client,
        model=model,
        symbol=args.symbol,
        start_ms=start_ms,
        end_ms=end_ms,
        seq_len=seq_len,
        horizon=int(args.horizon),
        device=device,
        refresh=bool(args.refresh),
    )
    if preds.empty:
        print(json.dumps({"ok": False, "error": "no predictions generated"}, ensure_ascii=False))
        return

    # backtest
    cfg = BacktestConfig(
        taker_fee=float(args.taker_fee),
        slippage=float(args.slippage),
        min_edge_bps=float(args.min_edge_bps),
        max_position_pct=float(args.max_position_pct),
    )
    result = run_simple_backtest(preds, cfg, float(args.initial_equity))

    # optional: save trades
    if args.trades_output and result.get('trades'):
        import json as _json
        outp = Path(args.trades_output)
        outp.parent.mkdir(parents=True, exist_ok=True)
        with open(outp, 'w', encoding='utf-8') as f:
            _json.dump(result['trades'], f, ensure_ascii=False, indent=2)
    # optional: save equity curve plot
    if args.plot_output and result.get('trades'):
        outp = Path(args.plot_output)
        outp.parent.mkdir(parents=True, exist_ok=True)
        try:
            plot_equity_curve(result['trades'], float(args.initial_equity), str(outp))
        except Exception as e:
            print(json.dumps({"ok": False, "error": f"plot failed: {e}"}, ensure_ascii=False))
    # optional: save summary
    if args.summary_output:
        summ = {k: v for k, v in result.items() if k != 'trades'}
        outp = Path(args.summary_output)
        outp.parent.mkdir(parents=True, exist_ok=True)
        with open(outp, 'w', encoding='utf-8') as f:
            json.dump(summ, f, ensure_ascii=False, indent=2)

    print(json.dumps({"ok": True, "summary": {k: v for k, v in result.items() if k != 'trades'}, "num_trades": result.get('num_trades', 0)}, ensure_ascii=False, indent=2))


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="Unified CLI for ML Workspace")
    sub = p.add_subparsers(dest="command", required=True)

    # qa run
    qa = sub.add_parser("qa", help="Data quality checks")
    qa_sub = qa.add_subparsers(dest="subcommand", required=True)
    qa_run = qa_sub.add_parser("run", help="Run quality checks from config")
    qa_run.add_argument("--config", required=True, help="Path to qa.yaml")
    qa_run.set_defaults(func=cmd_qa_run)

    # dataset build
    ds = sub.add_parser("dataset", help="Dataset operations")
    ds_sub = ds.add_subparsers(dest="subcommand", required=True)
    ds_build = ds_sub.add_parser("build", help="Build multi-symbol feature dataset to Parquet")
    ds_build.add_argument("--config", required=True, help="Path to features_build.yaml")
    ds_build.set_defaults(func=cmd_dataset_build)

    # train fit
    tr = sub.add_parser("train", help="Training operations")
    tr_sub = tr.add_subparsers(dest="subcommand", required=True)
    tr_fit = tr_sub.add_parser("fit", help="Supervised training with DLTrainer")
    tr_fit.add_argument("--config", required=True, help="Path to train_fit.yaml")
    tr_fit.add_argument("--refresh", action="store_true", help="Bypass local cache and requery features")
    tr_fit.set_defaults(func=cmd_train_fit)

    # features verify
    fv = sub.add_parser("features", help="Feature operations")
    fv_sub = fv.add_subparsers(dest="subcommand", required=True)
    fv_verify = fv_sub.add_parser("verify", help="Verify features/tables coverage and schema")
    fv_verify.add_argument("--config", required=True, help="Path to qa.yaml")
    fv_verify.set_defaults(func=cmd_features_verify)

    # unsupervised TCN-GRU training
    us = sub.add_parser("unsup", help="Unsupervised training operations")
    us_sub = us.add_subparsers(dest="subcommand", required=True)
    us_train = us_sub.add_parser("train", help="Train TCN-GRU unsupervised model")
    us_train.add_argument("--symbol", default="WLFIUSDT")
    us_train.add_argument("--start", required=True, help="ISO start, e.g. 2025-09-01T00:00:00Z")
    us_train.add_argument("--end", required=True, help="ISO end, e.g. 2025-09-08T00:00:00Z")
    us_train.add_argument("--horizons", default="1,5,10,30,60")
    us_train.add_argument("--seq_len", type=int, default=60)
    us_train.add_argument("--epochs", type=int, default=20)
    us_train.add_argument("--batch_size", type=int, default=256)
    us_train.add_argument("--lr", type=float, default=1e-3)
    us_train.add_argument("--taker_fee", type=float, default=0.0006)
    us_train.add_argument("--maker_fee", type=float, default=0.0002)
    us_train.add_argument("--use_maker", action="store_true")
    us_train.add_argument("--model_dir", default="./models/tcn_gru")
    us_train.add_argument("--seed", type=int, default=42)
    us_train.add_argument("--refresh", action="store_true", help="Bypass local cache and requery features")
    us_train.set_defaults(func=cmd_unsup_train)

    # backtest dl (load -> predict -> backtest)
    bt = sub.add_parser("backtest", help="Backtest operations")
    bt_sub = bt.add_subparsers(dest="subcommand", required=True)
    bt_dl = bt_sub.add_parser("dl", help="Backtest a supervised DL model end-to-end")
    bt_dl.add_argument("--model", required=True, help="Path to .pt checkpoint")
    bt_dl.add_argument("--symbol", required=True)
    bt_dl.add_argument("--start", required=True)
    bt_dl.add_argument("--end", required=True)
    bt_dl.add_argument("--horizon", type=int, default=5)
    bt_dl.add_argument("--min_edge_bps", type=float, default=5.0)
    bt_dl.add_argument("--taker_fee", type=float, default=0.0006)
    bt_dl.add_argument("--slippage", type=float, default=0.0001)
    bt_dl.add_argument("--initial_equity", type=float, default=10000.0)
    bt_dl.add_argument("--max_position_pct", type=float, default=0.1)
    bt_dl.add_argument("--host", default=None)
    bt_dl.add_argument("--user", default=None)
    bt_dl.add_argument("--password", default=None)
    bt_dl.add_argument("--database", default=None)
    bt_dl.add_argument("--trades_output", default=None, help="Optional path to save trades JSON")
    bt_dl.add_argument("--plot_output", default=None, help="Optional path to save equity curve image (e.g., equity.png)")
    bt_dl.add_argument("--summary_output", default=None, help="Optional path to save backtest summary JSON")
    bt_dl.add_argument("--refresh", action="store_true", help="Bypass local cache and requery features")
    bt_dl.set_defaults(func=cmd_backtest_dl)

    return p


def main() -> None:
    configure_logging()  # honor LOG_LEVEL/LOG_JSON
    parser = build_parser()
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
