#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
多品种队列动力学特征数据集构建（ClickHouse → 本地 Parquet）

- 时间窗：默认 2025-09-03 00:00:00 ~ 2025-09-05 23:59:59（UTC）
- 数据源：hft.bitget_futures_bbo / depth / trades
- 特征：按秒聚合（sec），使用 BBO 最新值 + Depth(Top5/Top10) + 成交1秒统计
- 输出：workspace/datasets/multisymbol_queue_features_<start>_<end>.parquet

注意：
- 仅做只读查询，不修改 ClickHouse 表。
- 需要 ClickHouse 账号可访问 hft 数据库。
"""

from __future__ import annotations

import os
import argparse
from datetime import datetime, timezone
from typing import List

import pandas as pd

from utils.clickhouse_client import ClickHouseClient
from utils.time import to_ms
from utils.ch_queries import build_feature_sql



def find_common_symbols(client: ClickHouseClient, start_ms: int, end_ms: int, top_n: int = 20) -> pd.DataFrame:
    qtpl = """
    SELECT symbol, count() as cnt, toInt64(min(exchange_ts)) as min_ts, toInt64(max(exchange_ts)) as max_ts
    FROM hft.{tbl}
    WHERE exchange_ts BETWEEN {start} AND {end}
    GROUP BY symbol
    ORDER BY cnt DESC
    LIMIT {top}
    """
    bbo = client.query_to_dataframe(qtpl.format(tbl='bitget_futures_bbo', start=start_ms, end=end_ms, top=top_n))
    depth = client.query_to_dataframe(qtpl.format(tbl='bitget_futures_depth', start=start_ms, end=end_ms, top=top_n))
    trades = client.query_to_dataframe(qtpl.format(tbl='bitget_futures_trades', start=start_ms, end=end_ms, top=top_n))

    if any(df is None or df.empty for df in (bbo, depth, trades)):
        return pd.DataFrame(columns=['symbol', 'bbo_cnt', 'depth_cnt', 'trades_cnt'])

    common = list(set(bbo['symbol']) & set(depth['symbol']) & set(trades['symbol']))
    rows = []
    for s in common:
        bb = bbo[bbo.symbol == s].iloc[0]
        dp = depth[depth.symbol == s].iloc[0]
        tr = trades[trades.symbol == s].iloc[0]
        rows.append({
            'symbol': s,
            'bbo_cnt': int(bb.cnt),
            'depth_cnt': int(dp.cnt),
            'trades_cnt': int(tr.cnt)
        })
    return pd.DataFrame(rows).sort_values(by=['bbo_cnt', 'depth_cnt', 'trades_cnt'], ascending=False)




def main():
    ap = argparse.ArgumentParser(description='构建多品种队列动力学特征数据集（按秒聚合）')
    ap.add_argument('--start', default='2025-09-03T00:00:00Z')
    ap.add_argument('--end', default='2025-09-05T23:59:59Z')
    ap.add_argument('--top', type=int, default=12, help='候选品种数量上限')
    ap.add_argument('--pick', type=int, default=8, help='最终选择的品种数（从候选中截取）')
    ap.add_argument('--out', default='./workspace/datasets', help='输出目录')
    ap.add_argument('--host', default=os.getenv('CLICKHOUSE_HOST'))
    ap.add_argument('--user', default=os.getenv('CLICKHOUSE_USERNAME', 'default'))
    ap.add_argument('--password', default=os.getenv('CLICKHOUSE_PASSWORD', ''))
    ap.add_argument('--database', default=os.getenv('CLICKHOUSE_DATABASE', 'hft'))
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    start_dt = datetime.fromisoformat(args.start.replace('Z', '+00:00'))
    end_dt = datetime.fromisoformat(args.end.replace('Z', '+00:00'))
    start_ms = to_ms(start_dt)
    end_ms = to_ms(end_dt)

    # 连接 ClickHouse
    client = ClickHouseClient(host=args.host, username=args.user, password=args.password, database=args.database)
    if not client.test_connection():
        raise RuntimeError('无法连接 ClickHouse，请检查 host/凭证')

    # 选品：三表同时存在的前 top 个
    cand_df = find_common_symbols(client, start_ms, end_ms, top_n=args.top)
    if cand_df.empty:
        raise RuntimeError('时间窗内未找到同时存在于 BBO/Depth/Trades 的品种')

    symbols = cand_df['symbol'].tolist()[: args.pick]
    print('选择品种:', symbols)

    # 拉取特征并拼接
    frames: List[pd.DataFrame] = []
    for sym in symbols:
        sql = build_feature_sql(sym, start_ms, end_ms)
        df = client.query_to_dataframe(sql)
        if df is None or df.empty:
            print(f'[警告] {sym} 无特征数据，跳过')
            continue
        # 衍生/标准化时间列
        if 'exchange_ts' not in df.columns and 'sec' in df.columns:
            df['exchange_ts'] = (df['sec'] * 1000).astype('int64')
        elif 'exchange_ts' in df.columns:
            df['exchange_ts'] = df['exchange_ts'].astype('int64')
        else:
            # 无法推断时间列，跳过该品种
            print(f'[警告] {sym} 缺少 exchange_ts/sec 列，跳过')
            continue
        df['datetime'] = pd.to_datetime(df['exchange_ts'], unit='ms', utc=True)
        frames.append(df)
        print(f'{sym} 特征行数: {len(df):,}')

    if not frames:
        raise RuntimeError('未生成任何品种的特征数据')

    full = pd.concat(frames, ignore_index=True)
    # 去除缺失/无效值
    num_cols = full.select_dtypes(include=['float64', 'float32', 'int64', 'int32']).columns
    full[num_cols] = full[num_cols].fillna(0.0)

    # 保存
    start_tag = start_dt.strftime('%Y%m%d')
    end_tag = end_dt.strftime('%Y%m%d')
    out_path = os.path.join(args.out, f'multisymbol_queue_features_{start_tag}_{end_tag}.parquet')
    full.to_parquet(out_path, index=False)
    print('已保存：', out_path, ' 行数=', len(full))


if __name__ == '__main__':
    main()
