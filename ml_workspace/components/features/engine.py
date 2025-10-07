#!/usr/bin/env python3
"""
ClickHouse特征计算引擎（迁移到 components.features.engine）
"""

# 原实现迁移自根目录 clickhouse_feature_engine.py
# 对于 SQL/DDL 文件的路径解析增加了更深层级的候选路径，确保模块移动后仍能找到根目录文件。

import logging
import numpy as np
import pandas as pd
from datetime import datetime, timedelta
from typing import Dict, List, Optional, Tuple, Any
import clickhouse_connect
from clickhouse_connect.driver import Client
import asyncio
import json
from pathlib import Path

logger = logging.getLogger(__name__)


def _find_repo_file(filename: str) -> Optional[Path]:
    here = Path(__file__).resolve()
    # candidate paths: cwd, module dir, parents, and _cleanup/temp under those roots
    bases = [
        Path.cwd(),
        here.parent,
        here.parent.parent,
        here.parent.parent.parent,
        here.parent.parent.parent.parent,
    ]
    candidates = []
    for b in bases:
        candidates.append(b / filename)
        candidates.append(b / "_cleanup" / "temp" / filename)
    for p in candidates:
        if p.exists():
            return p
    return None


class ClickHouseFeatureEngine:
    """ClickHouse特征计算引擎"""

    def __init__(
        self,
        host: str = 'localhost',
        port: int = 8123,
        username: str = 'default',
        password: str = '',
        database: str = 'wlfi_trading',
    ):

        self.host = host
        self.port = port
        self.username = username
        self.password = password
        self.database = database
        self.client: Optional[Client] = None

        logger.info(f"🏗️ 初始化ClickHouse特征引擎: {host}:{port}/{database}")

    def connect(self) -> bool:
        """连接到ClickHouse"""
        try:
            self.client = clickhouse_connect.get_client(
                host=self.host,
                port=self.port,
                username=self.username,
                password=self.password,
                database=self.database,
            )

            # 测试连接
            _ = self.client.query('SELECT 1')
            logger.info("✅ ClickHouse连接成功")
            return True

        except Exception as e:
            logger.error(f"❌ ClickHouse连接失败: {e}")
            return False

    def initialize_tables(self) -> bool:
        """初始化所有特征表"""
        if not self.client:
            logger.error("❌ ClickHouse客户端未初始化")
            return False

        try:
            sql_path = _find_repo_file('clickhouse_feature_tables.sql')
            if not sql_path:
                logger.error('❌ 未找到 clickhouse_feature_tables.sql，請確認文件位置')
                return False

            sql_commands = sql_path.read_text(encoding='utf-8')
            statements = [stmt.strip() for stmt in sql_commands.split(';') if stmt.strip()]

            for i, statement in enumerate(statements):
                if statement and not statement.startswith('--'):
                    try:
                        self.client.command(statement)
                        logger.info(f"✅ 执行SQL语句 {i+1}/{len(statements)}")
                    except Exception as e:
                        logger.warning(f"⚠️ SQL语句执行失败 {i+1}: {e}")

            logger.info("🎯 ClickHouse表初始化完成")
            return True

        except Exception as e:
            logger.error(f"❌ 表初始化失败: {e}")
            return False

    # 其余方法原样保留（为节省篇幅，这里省略注释，仅复制实现）
    def calculate_l2_features(self, depth_data: pd.DataFrame, trades_data: pd.DataFrame, bbo_data: pd.DataFrame) -> pd.DataFrame:
        features_list = []
        for i in range(len(bbo_data)):
            ts = bbo_data.iloc[i]['ts']
            window_start = ts - pd.Timedelta(seconds=60)
            depth_window = depth_data[(depth_data['ts'] >= window_start) & (depth_data['ts'] <= ts)]
            trades_window = trades_data[(trades_data['ts'] >= window_start) & (trades_data['ts'] <= ts)]
            if len(depth_window) == 0:
                continue
            features = self._compute_queue_dynamics(depth_window, trades_window, bbo_data.iloc[i])
            features['ts'] = ts
            features['symbol'] = bbo_data.iloc[i]['symbol']
            features_list.append(features)
        return pd.DataFrame(features_list)

    def _compute_queue_dynamics(self, depth_df: pd.DataFrame, trades_df: pd.DataFrame, current_bbo: pd.Series) -> Dict[str, float]:
        features = {}
        if len(depth_df) == 0:
            return {f'feature_{i}': 0.0 for i in range(19)}
        try:
            mid_prices = (depth_df['bid_price'] + depth_df['ask_price']) / 2
            spreads = depth_df['ask_price'] - depth_df['bid_price']
            bid_sizes = depth_df['bid_size']
            ask_sizes = depth_df['ask_size']
        except KeyError as e:
            logger.error(f"❌ 字段名错误: {e}")
            logger.info(f"📋 可用字段: {list(depth_df.columns)}")
            return self._get_default_features()
        try:
            total_bid = bid_sizes.sum(); total_ask = ask_sizes.sum(); total_size = total_bid + total_ask
            features['queue_imbalance'] = (total_bid - total_ask) / total_size if total_size > 0 else 0.0
            if len(trades_df) > 0:
                buy_trades = trades_df[trades_df['side'] == 'buy']; sell_trades = trades_df[trades_df['side'] == 'sell']
                if len(buy_trades) > 0 and len(sell_trades) > 0:
                    buy_vwap = (buy_trades['price'] * buy_trades['size']).sum() / buy_trades['size'].sum()
                    sell_vwap = (sell_trades['price'] * sell_trades['size']).sum() / sell_trades['size'].sum()
                    mid_vwap = mid_prices.mean()
                    features['order_flow_toxicity'] = (abs(buy_vwap - mid_vwap) + abs(sell_vwap - mid_vwap)) / (mid_vwap + 1e-10)
                else:
                    features['order_flow_toxicity'] = 0.0
            else:
                features['order_flow_toxicity'] = 0.0
            features['price_impact'] = mid_prices.diff().abs().mean() if len(mid_prices) > 1 else 0.0
            if len(trades_df) > 0:
                total_trade_volume = trades_df['size'].sum(); avg_depth = (bid_sizes.mean() + ask_sizes.mean()) / 2
                features['liquidity_consumption'] = total_trade_volume / avg_depth if avg_depth > 0 else 0.0
            else:
                features['liquidity_consumption'] = 0.0
            time_span_minutes = len(depth_df) * 0.1 / 60.0
            features['queue_duration'] = min(time_span_minutes, 10.0)
            features['cancel_intensity'] = (spreads.std() / spreads.mean()) if (spreads.std() > 0 and spreads.mean() > 0) else 0.0
            if len(depth_df) > 1:
                bid_changes = (depth_df['bid_price'].diff().abs() > 1e-8).sum()
                ask_changes = (depth_df['ask_price'].diff().abs() > 1e-8).sum()
                features['renewal_rate'] = (bid_changes + ask_changes) / len(depth_df)
            else:
                features['renewal_rate'] = 0.0
            features['depth_persistence'] = 1.0 / (spreads.std() + 1e-8) if spreads.std() > 0 else 1.0
            if len(trades_df) > 1:
                volume_changes = trades_df['size'].diff().abs();
                features['volume_clustering'] = (volume_changes.std() / volume_changes.mean()) if (volume_changes.std() > 0 and volume_changes.mean() > 0) else 0.0
            else:
                features['volume_clustering'] = 0.0
            features['time_priority_value'] = (bid_sizes.std() / bid_sizes.mean()) if (bid_sizes.std() > 0 and bid_sizes.mean() > 0) else 0.0
            if len(mid_prices) > 1 and len(trades_df) > 0:
                price_volatility = mid_prices.std(); trade_frequency = len(trades_df) / (time_span_minutes * 60 + 1e-8)
                features['adverse_selection'] = price_volatility * trade_frequency
            else:
                features['adverse_selection'] = 0.0
            if len(trades_df) > 0:
                large_threshold = trades_df['size'].quantile(0.8)
                if large_threshold > 0:
                    large_trades = trades_df[trades_df['size'] > large_threshold]
                    features['informed_trading'] = len(large_trades) / len(trades_df)
                else:
                    features['informed_trading'] = 0.0
            else:
                features['informed_trading'] = 0.0
            if len(mid_prices) > 5:
                try:
                    autocorr = np.corrcoef(mid_prices[:-1], mid_prices[1:])[0, 1]
                    features['market_impact_decay'] = 1.0 - abs(autocorr) if not np.isnan(autocorr) else 0.5
                except:
                    features['market_impact_decay'] = 0.5
            else:
                features['market_impact_decay'] = 0.5
            features['volatility_signature'] = (mid_prices.std() / mid_prices.mean()) if (mid_prices.std() > 0 and mid_prices.mean() > 0) else 0.0
            if len(mid_prices) > 2:
                second_diff = mid_prices.diff().diff(); features['microstructure_noise'] = second_diff.std()
            else:
                features['microstructure_noise'] = 0.0
            features['inventory_risk'] = abs(features['queue_imbalance']) * features['volatility_signature']
            features['execution_cost'] = (spreads.mean() / mid_prices.mean()) if mid_prices.mean() > 0 else 0.0
            if len(mid_prices) > 1:
                if mid_prices.iloc[0] > 0:
                    momentum = (mid_prices.iloc[-1] - mid_prices.iloc[0]) / mid_prices.iloc[0]
                    features['price_momentum'] = momentum
                else:
                    features['price_momentum'] = 0.0
            else:
                features['price_momentum'] = 0.0
            if len(mid_prices) > 2:
                diffs = mid_prices.diff().fillna(0)
                trend = diffs.rolling(window=min(len(diffs), 5)).mean().iloc[-1]
                features['trend_strength'] = float(trend)
            else:
                features['trend_strength'] = 0.0
            return features
        except Exception:
            return self._get_default_features()

    def _get_default_features(self) -> Dict[str, float]:
        defaults = {
            'queue_imbalance': 0.0,
            'order_flow_toxicity': 0.0,
            'price_impact': 0.0,
            'liquidity_consumption': 0.0,
            'queue_duration': 0.0,
            'cancel_intensity': 0.0,
            'renewal_rate': 0.0,
            'depth_persistence': 0.0,
            'volume_clustering': 0.0,
            'time_priority_value': 0.0,
            'adverse_selection': 0.0,
            'informed_trading': 0.0,
            'market_impact_decay': 0.5,
            'volatility_signature': 0.0,
            'microstructure_noise': 0.0,
            'inventory_risk': 0.0,
            'execution_cost': 0.0,
            'price_momentum': 0.0,
            'trend_strength': 0.0,
        }
        return defaults
