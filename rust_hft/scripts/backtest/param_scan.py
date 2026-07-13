#!/usr/bin/env python3
"""
簡易參數掃描腳本

需求：
    pip install pyyaml

用法示例：
    python scripts/backtest/param_scan.py \
        --config config/backtest/default.yaml \
        --grid config/backtest/param_grid.yaml \
        --output runs/backtest-sweep
"""

import argparse
import copy
import itertools
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import yaml


def parse_args():
    parser = argparse.ArgumentParser(description="L3 策略參數掃描")
    parser.add_argument("--config", required=True, help="基礎回測設定檔")
    parser.add_argument("--grid", required=True, help="參數格點 (YAML)")
    parser.add_argument("--output", default="runs/backtest-sweep", help="輸出根目錄")
    parser.add_argument(
        "--binary",
        default=os.getenv("HFT_BACKTEST_BIN", "target/release/hft-backtest"),
        help="預編譯 hft-backtest 路徑；不會在掃描期間呼叫 cargo build/run",
    )
    parser.add_argument(
        "--shard-index",
        type=int,
        default=int(os.getenv("JOB_COMPLETION_INDEX", "0")),
        help="目前分片索引，適用於 Kubernetes Indexed Job",
    )
    parser.add_argument(
        "--shard-count",
        type=int,
        default=int(os.getenv("BACKTEST_SHARD_COUNT", "1")),
        help="總分片數；每個分片執行自己的參數子集",
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="僅列出組合，不實際執行"
    )
    return parser.parse_args()


def load_yaml(path):
    with open(path, "r", encoding="utf-8") as f:
        return yaml.safe_load(f)


def ensure_dir(path: Path):
    path.mkdir(parents=True, exist_ok=True)


def override_strategy(config: dict, overrides: dict):
    cfg = copy.deepcopy(config)
    strategy = cfg.setdefault("strategy", {})
    strategy.update(overrides)
    return cfg


def slugify(combo):
    parts = []
    for key, value in combo.items():
        if isinstance(value, float):
            parts.append(f"{key}-{value:.3f}".replace(".", "p"))
        else:
            parts.append(f"{key}-{value}")
    return "_".join(parts)


def resolve_binary(binary: str) -> str:
    candidate = Path(binary).expanduser()
    if candidate.is_file() and os.access(candidate, os.X_OK):
        return str(candidate.resolve())

    resolved = shutil.which(binary)
    if resolved:
        return resolved

    raise FileNotFoundError(
        f"找不到可執行的 hft-backtest: {binary}; "
        "請先執行 cargo build -p hft-backtest --release，"
        "或用 --binary 指向已發布映像內的二進位"
    )


def run_backtest(binary: str, config_obj: dict, output_dir: Path):
    ensure_dir(output_dir)
    with tempfile.NamedTemporaryFile("w+", suffix=".yaml", delete=False) as tmp:
        yaml.safe_dump(config_obj, tmp)
        tmp_path = Path(tmp.name)

    cmd = [
        binary,
        "--config",
        str(tmp_path),
        "--output-dir",
        str(output_dir),
    ]
    print(f"[RUN] {' '.join(cmd)}")
    try:
        subprocess.run(cmd, check=True)
    finally:
        tmp_path.unlink(missing_ok=True)


def read_summary(summary_path: Path):
    metrics_json = summary_path.parent / "backtest_metrics.json"
    if metrics_json.exists():
        with open(metrics_json, "r", encoding="utf-8") as f:
            return json.load(f)
    if summary_path.exists():
        with open(summary_path, "r", encoding="utf-8") as f:
            rows = f.read().strip().splitlines()
            if len(rows) >= 2:
                header = rows[0].split(",")
                values = rows[1].split(",")
                return dict(zip(header, values))
    return None


def main():
    args = parse_args()
    if args.shard_count < 1:
        raise ValueError("--shard-count 必須大於 0")
    if args.shard_index < 0 or args.shard_index >= args.shard_count:
        raise ValueError("--shard-index 必須介於 0 與 shard-count - 1")

    base_cfg = load_yaml(args.config)
    grid_cfg = load_yaml(args.grid)

    strategy_grid = grid_cfg.get("strategy", {})
    if not strategy_grid:
        raise ValueError("格點檔案需至少包含 strategy 欄位")

    keys = list(strategy_grid.keys())
    values = [strategy_grid[k] for k in keys]
    combinations = []

    for combo in itertools.product(*values):
        param_map = dict(zip(keys, combo))
        combinations.append(param_map)

    selected = [
        (index, combo)
        for index, combo in enumerate(combinations)
        if index % args.shard_count == args.shard_index
    ]

    print(
        f"參數總數={len(combinations)} 分片={args.shard_index}/{args.shard_count} "
        f"本分片待回測={len(selected)}"
    )
    ensure_dir(Path(args.output))
    binary = None if args.dry_run else resolve_binary(args.binary)

    for combination_index, param_map in selected:
        slug = slugify(param_map)
        combo_output = Path(args.output) / f"{combination_index:06d}_{slug}"
        combo_cfg = override_strategy(base_cfg, param_map)
        print(f"\n==> 組合 index={combination_index} slug={slug}")

        if args.dry_run:
            print(json.dumps(combo_cfg["strategy"], indent=2))
            continue

        run_backtest(binary, combo_cfg, combo_output)
        summary = read_summary(combo_output / "backtest_summary.csv")
        if summary:
            print(f"結果摘要: {summary}")


if __name__ == "__main__":
    main()
