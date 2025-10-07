#!/usr/bin/env python3
"""
流式训练器 - 解决大数据集内存问题的最佳方案
不再一次性加载所有数据，而是分页训练
"""

import torch
import numpy as np
import pandas as pd
import sys
import os
from typing import Iterator, Tuple, List
from datetime import datetime, timedelta

# 添加项目路径
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from utils.clickhouse_client import ClickHouseClient
from utils.ch_queries import build_feature_sql

def to_ms(dt):
    """将datetime转换为毫秒时间戳"""
    return int(dt.timestamp() * 1000)


class StreamingDataLoader:
    """流式数据加载器 - 按页加载数据"""

    def __init__(
        self,
        symbol: str,
        start_ms: int = 1756839483492,  # 直接使用毫秒时间戳
        end_ms: int = 1757079170578,    # 直接使用毫秒时间戳
        page_hours: int = 4,  # 每页4小时数据
        seq_len: int = 60,
        horizons: List[int] = [1, 5, 10, 30, 60]
    ):
        self.symbol = symbol
        self.start_ms = start_ms
        self.end_ms = end_ms
        self.page_hours = page_hours
        self.seq_len = seq_len
        self.horizons = horizons

        self.client = ClickHouseClient(
            host="https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443",
            username="default",
            password="sIiFK.4ygf.9R",
            database="hft"
        )

        total_hours = (self.end_ms - self.start_ms) / (60 * 60 * 1000)
        self.total_pages = int(total_hours / page_hours) + 1

        print(f"📊 流式数据加载器配置:")
        print(f"   总时长: {total_hours:.1f} 小时")
        print(f"   分页大小: {page_hours} 小时/页")
        print(f"   总页数: {self.total_pages}")

    def get_data_pages(self) -> Iterator[pd.DataFrame]:
        """生成数据页的迭代器"""
        page_duration_ms = self.page_hours * 60 * 60 * 1000
        current_ms = self.start_ms

        for page_idx in range(self.total_pages):
            page_end_ms = min(current_ms + page_duration_ms, self.end_ms)

            page_start_dt = datetime.fromtimestamp(current_ms / 1000)
            page_end_dt = datetime.fromtimestamp(page_end_ms / 1000)

            print(f"📄 加载页 {page_idx + 1}/{self.total_pages}: {page_start_dt.strftime('%m-%d %H:%M')} - {page_end_dt.strftime('%m-%d %H:%M')}")

            # 直接使用实际的表和字段结构
            sql = f"""
            SELECT
                exchange_ts,
                symbol,
                mid_price,
                spread_abs, spread_pct, best_bid_size, best_ask_size, size_imbalance,
                instant_trades, instant_volume, instant_buy_volume, instant_sell_volume, instant_volume_imbalance,
                trade_aggression, trade_price_deviation_bps, price_change, bid_change, ask_change,
                bid_size_change, ask_size_change, inferred_book_impact,
                cum_buy_volume_1m, cum_sell_volume_1m, cum_volume_imbalance_1m, cum_trades_1m,
                cum_ask_consumption_1m, cum_bid_consumption_1m, order_flow_imbalance_1m, cum_book_pressure_1m,
                cum_buy_volume_5m, cum_sell_volume_5m, cum_volume_imbalance_5m,
                aggressive_trades_1m, aggressive_trade_ratio_1m, trade_frequency,
                price_volatility_1m, spread_volatility_1m, combined_imbalance_signal,
                liquidity_stress_indicator, volatility_flow_interaction, depth_to_activity_ratio, spread_volatility_ratio
            FROM hft_features_eth_local_ob
            WHERE exchange_ts >= {current_ms} AND exchange_ts <= {page_end_ms}
            ORDER BY exchange_ts
            """

            try:
                import time
                start_time = time.time()

                page_df = self.client.query_to_dataframe(sql)
                elapsed = time.time() - start_time

                if page_df is not None and not page_df.empty:
                    print(f"   ✅ {len(page_df):,} 行 (耗时: {elapsed:.1f}秒)")
                    yield page_df
                else:
                    print(f"   ⚠️  无数据")

            except Exception as e:
                print(f"   ❌ 失败: {str(e)[:100]}...")
                continue

            current_ms = page_end_ms

    def compute_returns(self, prices: np.ndarray, horizons: List[int]) -> np.ndarray:
        """计算未来收益率"""
        n = len(prices)
        returns = np.zeros((n, len(horizons)))

        for i, h in enumerate(horizons):
            future_prices = np.roll(prices, -h)
            future_prices[-h:] = prices[-h:]
            returns[:, i] = (future_prices - prices) / (prices + 1e-10)
            returns[-h:, i] = 0

        return returns

    def prepare_sequences_from_page(self, page_df: pd.DataFrame) -> Tuple[torch.Tensor, torch.Tensor]:
        """从单页数据准备训练序列"""
        exclude_cols = ['exchange_ts', 'symbol']
        feature_cols = [c for c in page_df.columns if c not in exclude_cols]

        # 确保所有特征列都是数值类型
        for col in feature_cols:
            if col != 'mid_price':  # mid_price已经是float64
                page_df[col] = pd.to_numeric(page_df[col], errors='coerce')

        # 填充NaN值
        page_df[feature_cols] = page_df[feature_cols].fillna(0)

        features = page_df[feature_cols].values
        prices = page_df['mid_price'].values

        # 计算收益率
        future_returns = self.compute_returns(prices, self.horizons)

        # 构建序列
        sequences = []
        targets = []

        valid_sequences = len(features) - self.seq_len - max(self.horizons)

        if valid_sequences <= 0:
            return None, None

        for i in range(valid_sequences):
            # 输入序列
            seq = features[i:i + self.seq_len].copy()

            # 滚动标准化
            mean = seq.mean(axis=0, keepdims=True)
            std = seq.std(axis=0, keepdims=True) + 1e-8
            seq = (seq - mean) / std

            sequences.append(seq)

            # 目标收益率
            target_idx = i + self.seq_len
            targets.append(future_returns[target_idx])

        if not sequences:
            return None, None

        X = torch.tensor(np.array(sequences), dtype=torch.float32)
        y = torch.tensor(np.array(targets), dtype=torch.float32)

        return X, y


class StreamingTCNGRUTrainer:
    """流式TCN-GRU训练器"""

    def __init__(self, model, device, optimizer, criterion):
        self.model = model.to(device)
        self.device = device
        self.optimizer = optimizer
        self.criterion = criterion
        self.horizons = [1, 5, 10, 30, 60]

    def train_on_streaming_data(self, data_loader: StreamingDataLoader, epochs: int = 1):
        """在流式数据上训练"""
        print(f"🚀 开始流式训练 (epochs={epochs})...")

        total_batches = 0
        total_loss = 0

        for epoch in range(epochs):
            print(f"\n📈 Epoch {epoch + 1}/{epochs}")

            epoch_batches = 0
            epoch_loss = 0

            # 遍历所有数据页
            for page_df in data_loader.get_data_pages():
                X_page, y_page = data_loader.prepare_sequences_from_page(page_df)

                if X_page is None:
                    print(f"   ⚠️  页面数据不足，跳过")
                    continue

                # 移动到设备
                X_page = X_page.to(self.device)
                y_page = y_page.to(self.device)

                # 分批训练
                batch_size = 64
                num_samples = len(X_page)
                num_batches = (num_samples + batch_size - 1) // batch_size

                page_loss = 0

                for batch_idx in range(num_batches):
                    start_idx = batch_idx * batch_size
                    end_idx = min(start_idx + batch_size, num_samples)

                    batch_X = X_page[start_idx:end_idx]
                    batch_y = y_page[start_idx:end_idx]

                    # 前向传播
                    self.optimizer.zero_grad()
                    predictions, aux_outputs = self.model(batch_X)

                    # 计算损失
                    batch_losses = self.criterion(
                        predictions=predictions,
                        aux_outputs=aux_outputs,
                        input_sequence=batch_X,
                        future_returns=batch_y,
                        next_features=batch_X[:, -1, :],  # 简化版
                        horizons=self.horizons
                    )

                    # 反向传播
                    batch_losses['total'].backward()
                    torch.nn.utils.clip_grad_norm_(self.model.parameters(), max_norm=1.0)
                    self.optimizer.step()

                    batch_loss = batch_losses['total'].item()
                    page_loss += batch_loss
                    total_loss += batch_loss
                    total_batches += 1
                    epoch_batches += 1

                avg_page_loss = page_loss / num_batches
                print(f"   📄 页面完成，平均损失: {avg_page_loss:.4f}")

                # 释放内存
                del X_page, y_page
                torch.cuda.empty_cache() if torch.cuda.is_available() else None

            avg_epoch_loss = epoch_loss / max(epoch_batches, 1)
            print(f"   📊 Epoch {epoch + 1} 完成，平均损失: {avg_epoch_loss:.4f}")

        avg_total_loss = total_loss / max(total_batches, 1)
        print(f"\n✅ 流式训练完成!")
        print(f"   总批次: {total_batches}")
        print(f"   平均损失: {avg_total_loss:.4f}")

        return avg_total_loss


def train_with_streaming(
    symbol: str = 'ETHUSDT',
    start_ms: int = 1756839483492,  # 实际数据开始时间戳
    end_ms: int = 1757079170578,    # 实际数据结束时间戳
    page_hours: int = 4,
    epochs: int = 3,
    model_type: str = 'simple'
):
    """使用流式训练的主函数"""

    print("🌊 流式TCN-GRU训练")
    print("=" * 60)

    # 创建流式数据加载器
    data_loader = StreamingDataLoader(
        symbol=symbol,
        start_ms=start_ms,
        end_ms=end_ms,
        page_hours=page_hours
    )

    # 简化模型用于演示
    if model_type == 'simple':
        class SimpleTCNGRU(torch.nn.Module):
            def __init__(self, input_dim, hidden_dim=64):
                super().__init__()
                self.lstm = torch.nn.LSTM(input_dim, hidden_dim, batch_first=True)
                self.fc = torch.nn.Linear(hidden_dim, 5)  # 5个预测视界

            def forward(self, x):
                lstm_out, _ = self.lstm(x)
                predictions = self.fc(lstm_out[:, -1, :])  # 使用最后一个时间步

                # 返回格式兼容原有loss函数
                pred_dict = {
                    f'price_{h}s': predictions[:, i:i+1]
                    for i, h in enumerate([1, 5, 10, 30, 60])
                }
                aux_outputs = {'reconstruction': x}  # 简化版

                return pred_dict, aux_outputs

        # 获取特征维度
        first_page = next(data_loader.get_data_pages())
        feature_cols = [c for c in first_page.columns if c not in ['exchange_ts', 'symbol']]
        input_dim = len(feature_cols)

        print(f"🧠 创建简化模型，输入维度: {input_dim}")

        model = SimpleTCNGRU(input_dim)

        # 简化的损失函数
        class SimpleLoss:
            def __call__(self, predictions, aux_outputs, input_sequence, future_returns, next_features, horizons):
                total_loss = 0
                for i, h in enumerate(horizons):
                    pred_key = f'price_{h}s'
                    if pred_key in predictions:
                        pred = predictions[pred_key]
                        target = future_returns[:, i:i+1]
                        loss = torch.nn.functional.mse_loss(pred, target)
                        total_loss += loss

                return {'total': total_loss / len(horizons)}

        device = torch.device('mps' if torch.backends.mps.is_available() else 'cpu')
        optimizer = torch.optim.Adam(model.parameters(), lr=0.001)
        criterion = SimpleLoss()

        # 创建训练器
        trainer = StreamingTCNGRUTrainer(model, device, optimizer, criterion)

        # 开始流式训练
        final_loss = trainer.train_on_streaming_data(data_loader, epochs)

        print(f"\n🎉 训练完成! 最终损失: {final_loss:.4f}")

        return model, final_loss


if __name__ == '__main__':
    # 测试流式训练
    model, loss = train_with_streaming(
        page_hours=4,  # 每页4小时，内存友好
        epochs=2       # 少量epoch用于测试
    )