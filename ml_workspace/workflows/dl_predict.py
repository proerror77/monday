#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
监督学习模型推理模块
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
from typing import Dict, List, Optional, Tuple, Any

import numpy as np
import pandas as pd
import torch
from torch import nn

from utils.clickhouse_client import ClickHouseClient
from utils.ch_queries import build_feature_sql
from utils.cache import cache_enabled, make_features_cache_path, load_cached_df, save_df
from components.feature_engineering import order_feature_columns
from algorithms.dl.sequence import build_sequences
from algorithms.dl.trainer import SequenceRegressor, DLTrainConfig


def load_dl_model(model_path: str, device: torch.device) -> Tuple[nn.Module, Dict[str, Any]]:
    """Load trained DL model and config"""
    checkpoint = torch.load(model_path, map_location=device)
    
    config_dict = checkpoint['config']
    config = DLTrainConfig(**config_dict)
    
    feature_dim = checkpoint['feature_dim']
    
    model = SequenceRegressor(
        feature_dim=feature_dim,
        d_model=config.d_model,
        nhead=config.nhead,
        num_layers=config.num_layers,
        dropout=config.dropout
    )
    model.load_state_dict(checkpoint['state_dict'])
    model.to(device)
    model.eval()
    
    metadata = {
        'feature_dim': feature_dim,
        'sequence_length': checkpoint['sequence_length'],
        'config': config
    }
    
    return model, metadata


def predict_single_horizon(
    client: ClickHouseClient,
    model: nn.Module,
    symbol: str,
    start_ms: int,
    end_ms: int,
    seq_len: int,
    horizon: int,
    device: torch.device,
    refresh: bool = False
) -> pd.DataFrame:
    """对单一品种进行推理预测"""
    
    # 获取特征数据（带本地快取）
    cache_path = make_features_cache_path(symbol, start_ms, end_ms, suffix=f"h{horizon}")
    df = load_cached_df(cache_path, refresh=refresh) if cache_enabled() else None
    if df is None or df.empty:
        sql = build_feature_sql(symbol, start_ms, end_ms)
        df = client.query_to_dataframe(sql)
        if df is not None and not df.empty and cache_enabled():
            save_df(df, cache_path)
    
    if df is None or df.empty:
        return pd.DataFrame()
    
    # 确保有mid_price列
    if 'mid_price' not in df.columns:
        raise ValueError("Feature data must include mid_price column")
    
    # 获取特征列（與訓練一致：按契約排序並排除 metadata 與 mid_price）
    feature_cols = order_feature_columns(list(df.columns))
    
    if len(feature_cols) == 0:
        raise ValueError("No feature columns found")
    
    # 构造序列，但不使用标签（只需要特征）
    horizons = [horizon]  # 单一horizon用于兼容build_sequences
    X, _ = build_sequences(
        df=df,
        feature_cols=feature_cols,
        horizons=horizons,
        seq_len=seq_len,
        use_rolling_norm=True  # 使用与训练一致的标准化
    )
    
    if X.numel() == 0:
        return pd.DataFrame()
    
    # 推理预测
    predictions = []
    timestamps = []
    mid_prices = []
    
    # 获取对应的时间戳
    valid_indices = range(seq_len, len(df) - max(horizons))
    
    with torch.no_grad():
        X = X.to(device)
        pred = model(X).cpu().numpy()  # [N] - scalar predictions
        
        for i, p in enumerate(pred):
            if i < len(valid_indices):
                idx = valid_indices[i]
                predictions.append(float(p))  # p is already a scalar
                timestamps.append(int(df.iloc[idx]['sec']))
                mid_prices.append(float(df.iloc[idx]['mid_price']))
    
    return pd.DataFrame({
        'sec': timestamps,
        'mid_price': mid_prices,
        'prediction': predictions,
        'symbol': symbol,
        'horizon': horizon
    })


def main():
    parser = argparse.ArgumentParser(description='监督学习模型推理')
    parser.add_argument('--model', required=True, help='模型文件路径(.pt)')
    parser.add_argument('--symbol', required=True, help='交易品种')
    parser.add_argument('--start', required=True, help='开始时间 (ISO format)')
    parser.add_argument('--end', required=True, help='结束时间 (ISO format)')
    parser.add_argument('--horizon', type=int, default=5, help='预测时间地平线(秒)')
    parser.add_argument('--output', help='输出文件路径')
    parser.add_argument('--host', default=os.getenv('CLICKHOUSE_HOST'))
    parser.add_argument('--user', default=os.getenv('CLICKHOUSE_USERNAME', 'default'))
    parser.add_argument('--password', default=os.getenv('CLICKHOUSE_PASSWORD', ''))
    parser.add_argument('--database', default=os.getenv('CLICKHOUSE_DATABASE', 'hft'))
    
    args = parser.parse_args()
    
    # 验证模型文件
    model_path = Path(args.model)
    if not model_path.exists():
        raise FileNotFoundError(f"Model file not found: {model_path}")
    
    # 解析时间
    from datetime import datetime
    from utils.time import to_ms
    
    start_dt = datetime.fromisoformat(args.start.replace('Z', '+00:00'))
    end_dt = datetime.fromisoformat(args.end.replace('Z', '+00:00'))
    start_ms = to_ms(start_dt)
    end_ms = to_ms(end_dt)
    
    # 设备设置
    device = torch.device(
        'mps' if torch.backends.mps.is_available() else 
        ('cuda' if torch.cuda.is_available() else 'cpu')
    )
    
    # 加载模型
    model, metadata = load_dl_model(str(model_path), device)
    seq_len = metadata['sequence_length']
    
    # 连接ClickHouse
    client = ClickHouseClient(
        host=args.host,
        username=args.user,
        password=args.password,
        database=args.database
    )
    
    if not client.test_connection():
        raise RuntimeError('Cannot connect to ClickHouse')
    
    # 执行推理
    results = predict_single_horizon(
        client=client,
        model=model,
        symbol=args.symbol,
        start_ms=start_ms,
        end_ms=end_ms,
        seq_len=seq_len,
        horizon=args.horizon,
        device=device
    )
    
    if results.empty:
        print("No predictions generated - insufficient data or empty result")
        return
    
    print(f"Generated {len(results)} predictions for {args.symbol}")
    print(f"Prediction range: [{results['prediction'].min():.6f}, {results['prediction'].max():.6f}]")
    print(f"Time range: {results['sec'].min()} - {results['sec'].max()}")
    
    # 输出结果
    if args.output:
        output_path = Path(args.output)
        if output_path.suffix == '.parquet':
            results.to_parquet(output_path, index=False)
        else:
            results.to_csv(output_path, index=False)
        print(f"Results saved to: {output_path}")
    else:
        print("\nFirst 10 predictions:")
        print(results.head(10).to_string(index=False))


if __name__ == '__main__':
    main()
