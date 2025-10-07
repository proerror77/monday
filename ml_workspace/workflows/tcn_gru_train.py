#!/usr/bin/env python3
"""
Unsupervised TCN-GRU Training Workflow
Implements cost-aware training for HFT strategies
"""

import os
import json
import argparse
import torch
import torch.nn as nn
import numpy as np
import pandas as pd
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Dict, List, Tuple, Optional
from torch.utils.data import DataLoader, TensorDataset
from sklearn.preprocessing import StandardScaler
import joblib

import sys
import os
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from utils.clickhouse_client import ClickHouseClient
from utils.ch_queries import build_feature_sql
from utils.time import to_ms, iso_to_ms
from utils.cache import cache_enabled, make_features_cache_path, load_cached_df, save_df
from utils.reproducibility import set_global_seed
from utils.lob_features import enhance_features_with_lob_dynamics
from algorithms.tcn_gru import TCNGRU, CostAwareLoss, UnsupervisedTCNGRULoss, resolve_loss_config


class StreamingSequenceDataset(torch.utils.data.Dataset):
    """A memory-efficient dataset that generates sequences on the fly."""
    def __init__(self, df: pd.DataFrame, seq_len: int, horizons: List[int], feature_cols: List[str], mean: np.ndarray, std: np.ndarray):
        self.seq_len = seq_len
        self.horizons = horizons
        self.feature_cols = feature_cols
        self.mean = np.nan_to_num(mean.astype(np.float32), nan=0.0)
        self.std = np.nan_to_num(std.astype(np.float32), nan=1.0, posinf=1.0, neginf=1.0)

        # Extract numpy arrays for performance
        self.features = df[self.feature_cols].values.astype(np.float32)
        self.prices = df['mid_price'].values.astype(np.float32)
        self.timestamps_ms = self._extract_timestamps(df)
        self.horizon_ms = np.asarray(self.horizons, dtype=np.int64) * 1000

        # Pre-compute future returns
        self.future_returns = self._compute_returns()
        
        # Calculate total number of possible sequences
        self.num_sequences = len(self.features) - self.seq_len - max(self.horizons)

    @staticmethod
    def _extract_timestamps(df: pd.DataFrame) -> np.ndarray:
        if 'exchange_ts' not in df.columns:
            raise ValueError("exchange_ts column is required for time-aligned returns")

        ts = df['exchange_ts']

        if np.issubdtype(ts.dtype, np.datetime64):
            return ts.values.astype('datetime64[ns]').astype(np.int64) // 1_000_000

        values = ts.values
        if values.dtype == object:
            values = pd.to_datetime(ts, utc=True).values
            return values.astype('datetime64[ns]').astype(np.int64) // 1_000_000

        values = values.astype(np.int64)
        max_abs = np.abs(values).max()
        if max_abs > 1e14:
            return values // 1_000_000
        if max_abs > 1e11:
            return values // 1_000
        return values

    def _compute_returns(self) -> np.ndarray:
        """Compute future returns for multiple horizons."""
        n = len(self.prices)
        returns = np.zeros((n, len(self.horizons)), dtype=np.float32)

        for idx_h, horizon_ms in enumerate(self.horizon_ms):
            target_times = self.timestamps_ms + horizon_ms
            future_indices = np.searchsorted(self.timestamps_ms, target_times, side='left')

            valid_mask = future_indices < n
            if not np.any(valid_mask):
                continue

            base_idx = np.where(valid_mask)[0]
            future_idx = future_indices[valid_mask]

            base_prices = self.prices[base_idx]
            future_prices = self.prices[future_idx]

            returns[base_idx, idx_h] = (future_prices - base_prices) / (base_prices + 1e-10)

        return returns

    def __len__(self):
        return self.num_sequences

    def __getitem__(self, idx):
        if idx < 0 or idx >= self.num_sequences:
            raise IndexError("Index out of range")

        # Define sequence boundaries
        seq_start = idx
        seq_end = idx + self.seq_len
        target_idx = seq_end

        # Get sequences
        feature_seq = self.features[seq_start:seq_end]
        next_feature = self.features[target_idx]
        
        # On-the-fly scaling
        feature_seq_scaled = (feature_seq - self.mean) / self.std
        next_feature_scaled = (next_feature - self.mean) / self.std

        # Get target
        target = self.future_returns[target_idx]

        return (
            torch.from_numpy(feature_seq_scaled),
            torch.from_numpy(target),
            torch.from_numpy(next_feature_scaled)
        )



class TCNGRUDataset:
    """Dataset builder for unsupervised TCN-GRU training"""
    
    def __init__(
        self,
        symbol: str,
        start_date: str,
        end_date: str,
        seq_len: int = 60,
        horizons: List[int] = [1, 5, 10, 30, 60],
        use_rolling_norm: bool = True,
        refresh: bool = False
    ):
        self.symbol = symbol
        self.start_date = start_date
        self.end_date = end_date

        # 使用配置時間窗口（從參數動態計算，取代硬編碼）
        try:
            self.start_ms = iso_to_ms(self.start_date)
            self.end_ms = iso_to_ms(self.end_date)
        except Exception:
            # 後備：若解析失敗，嘗試 to_ms + fromisoformat
            from datetime import datetime
            self.start_ms = to_ms(datetime.fromisoformat(self.start_date.replace('Z', '+00:00')))
            self.end_ms = to_ms(datetime.fromisoformat(self.end_date.replace('Z', '+00:00')))

        self.seq_len = seq_len
        self.horizons = horizons
        self.use_rolling_norm = use_rolling_norm
        self.refresh = refresh
        # ClickHouse 連線：使用環境或配置的預設（不在代碼中硬編碼憑證）
        self.client = ClickHouseClient()
        
    def load_data(self) -> pd.DataFrame:
        """Load data from ClickHouse using smart batching with local caching."""
        
        # Define cache path (shared cache util)
        cache_file = make_features_cache_path(self.symbol, self.start_ms, self.end_ms, suffix="unsup")

        # 1. Check if cache exists
        if cache_enabled() and (not self.refresh):
            print(f"✅ 發現本地快取，從 {cache_file} 載入數據...")
            start_time = time.time()
            df = load_cached_df(cache_file, refresh=False)
            elapsed = time.time() - start_time
            if df is not None and not df.empty:
                print(f"   📈 成功載入 {len(df):,} 行數據 (耗時: {elapsed:.1f}秒)")
                return df

        # 2. If cache doesn't exist, query from ClickHouse
        print(f"⚠️ 本地快取未找到，開始從 ClickHouse 查詢...")
        
        print("🔗 测试ClickHouse连接...")
        if not self.client.test_connection():
            raise RuntimeError("❌ Cannot connect to ClickHouse")
        print("✅ ClickHouse连接成功")

        start_ms = self.start_ms
        end_ms = self.end_ms
        total_hours = (end_ms - start_ms) / (60 * 60 * 1000)

        print(f"📊 查询数据范围:")
        print(f"   交易对: {self.symbol}")
        print(f"   开始时间: {self.start_date} (毫秒: {start_ms})")
        print(f"   结束时间: {self.end_date} (毫秒: {end_ms})")
        print(f"   时长: {total_hours:.1f} 小时")

        if total_hours > 24:
            batch_hours = 8
            batch_duration_ms = batch_hours * 60 * 60 * 1000
            num_batches = int((end_ms - start_ms) / batch_duration_ms) + 1
            print(f"📦 大数据集检测，启用分批处理: {num_batches} 批，每批 {batch_hours} 小时")

            all_dfs = []
            current_ms = start_ms
            total_start_time = time.time()

            for batch_idx in range(num_batches):
                batch_end_ms = min(current_ms + batch_duration_ms, end_ms)
                batch_start_dt = datetime.fromtimestamp(current_ms / 1000)
                batch_end_dt = datetime.fromtimestamp(batch_end_ms / 1000)

                print(f"\n🔄 处理批次 {batch_idx + 1}/{num_batches}: {batch_start_dt.strftime('%m-%d %H:%M')} - {batch_end_dt.strftime('%m-%d %H:%M')}")
                
                sql = f"""
                SELECT exchange_ts, symbol, mid_price, spread_abs, spread_pct, best_bid_size, best_ask_size, size_imbalance,
                       instant_trades, instant_volume, instant_buy_volume, instant_sell_volume, instant_volume_imbalance,
                       trade_aggression, trade_price_deviation_bps, price_change, bid_change, ask_change,
                       bid_size_change, ask_size_change, inferred_book_impact, cum_buy_volume_1m, cum_sell_volume_1m,
                       cum_volume_imbalance_1m, cum_trades_1m, cum_ask_consumption_1m, cum_bid_consumption_1m,
                       order_flow_imbalance_1m, cum_book_pressure_1m, cum_buy_volume_5m, cum_sell_volume_5m,
                       cum_volume_imbalance_5m, aggressive_trades_1m, aggressive_trade_ratio_1m, trade_frequency,
                       price_volatility_1m, spread_volatility_1m, combined_imbalance_signal, liquidity_stress_indicator,
                       volatility_flow_interaction, depth_to_activity_ratio, spread_volatility_ratio
                FROM hft_features_eth_local_ob WHERE exchange_ts >= {current_ms} AND exchange_ts <= {batch_end_ms} ORDER BY exchange_ts
                """
                batch_start_time = time.time()
                try:
                    batch_df = self.client.query_to_dataframe(sql)
                    elapsed = time.time() - batch_start_time
                    if batch_df is None or batch_df.empty:
                        print(f"   ⚠️  批次 {batch_idx + 1} 无数据，跳过")
                        continue
                    print(f"   ✅ 成功: {len(batch_df):,} 行 (耗時: {elapsed:.1f}秒)")
                    all_dfs.append(batch_df)
                except Exception as e:
                    elapsed = time.time() - batch_start_time
                    print(f"   ❌ 批次 {batch_idx + 1} 失败 (耗時: {elapsed:.1f}秒): {str(e)[:100]}...")
                current_ms = batch_end_ms
            
            if not all_dfs: raise RuntimeError("❌ 所有批次都失败，未获取到数据")
            
            print("\n🔗 合并 {len(all_dfs)} 个批次...")
            df = pd.concat(all_dfs, ignore_index=True).sort_values('exchange_ts').reset_index(drop=True)
            total_elapsed = time.time() - total_start_time
            print(f"✅ 分批数据加载完成! 📈 总行数: {len(df):,} | ⏱️  总耗时: {total_elapsed:.1f}秒")
        else:
            df = self._execute_single_query(start_ms, end_ms)

        # 3. Clean and process the dataframe
        print("🔧 数据类型转换和清理...")
        for col in df.columns:
            if col not in ['symbol']:
                df[col] = pd.to_numeric(df[col], errors='coerce')
        
        initial_rows = len(df)
        df.dropna(inplace=True)
        final_rows = len(df)
        if initial_rows != final_rows:
            print(f"   ⚠️  删除了 {initial_rows - final_rows:,} 行包含NaN的数据")
        print(f"   ✅ 数据清理完成，最终: {final_rows:,} 行")

        # 4. Save the newly queried data to cache
        print(f"\n💾 正在將數據快取至 {cache_file}...")
        try:
            if cache_enabled():
                save_df(df, cache_file)
                print("   ✅ 數據快取成功！")
        except Exception as e:
            print(f"   ❌ 快取數據失敗: {e}")

        return df

    def _execute_single_query(self, start_ms: int, end_ms: int) -> pd.DataFrame:
        """执行单次查询（与成功脚本完全相同的SQL）"""

        # 使用与成功脚本完全相同的SQL查询方式
        sql = f"""
        SELECT
            exchange_ts,
            symbol,

            -- 基础BBO特征
            mid_price,
            spread_abs,
            spread_pct,
            best_bid_size,
            best_ask_size,
            size_imbalance,

            -- 交易特征
            instant_trades,
            instant_volume,
            instant_buy_volume,
            instant_sell_volume,
            instant_volume_imbalance,

            -- 本地订单簿重构特征
            trade_aggression,
            trade_price_deviation_bps,
            price_change,
            bid_change,
            ask_change,
            bid_size_change,
            ask_size_change,
            inferred_book_impact,

            -- 累积特征 1分钟
            cum_buy_volume_1m,
            cum_sell_volume_1m,
            cum_volume_imbalance_1m,
            cum_trades_1m,
            cum_ask_consumption_1m,
            cum_bid_consumption_1m,
            order_flow_imbalance_1m,
            cum_book_pressure_1m,

            -- 累积特征 5分钟
            cum_buy_volume_5m,
            cum_sell_volume_5m,
            cum_volume_imbalance_5m,

            -- 交易侵略性特征
            aggressive_trades_1m,
            aggressive_trade_ratio_1m,
            trade_frequency,

            -- 波动性特征
            price_volatility_1m,
            spread_volatility_1m,

            -- 组合信号特征
            combined_imbalance_signal,
            liquidity_stress_indicator,
            volatility_flow_interaction,
            depth_to_activity_ratio,
            spread_volatility_ratio

        FROM hft_features_eth_local_ob
        WHERE exchange_ts >= {start_ms}
          AND exchange_ts <= {end_ms}
        ORDER BY exchange_ts
        """

        print(f"🔄 从ClickHouse获取本地订单簿特征数据...")
        print(f"   交易对: {self.symbol}")
        print(f"   时间范围: {start_ms} - {end_ms}")
        print(f"   查询无LIMIT限制（获取全部数据）")

        import time
        start_time = time.time()
        df = self.client.query_to_dataframe(sql)
        elapsed = time.time() - start_time

        if df.empty:
            raise ValueError("❌ 未获取到数据")

        print(f"✅ 数据获取完成! 耗时: {elapsed:.1f}秒")
        print(f"   📈 总行数: {len(df):,}")
        print(f"   📊 列数: {df.shape[1]}")
        print(f"   🎯 特征维度: {df.shape[1] - 2}")  # 减去exchange_ts和symbol

        # 数据类型转换和清理
        print("🔧 数据类型转换和清理...")

        # 确保所有数值列都是正确的类型
        for col in df.columns:
            if col not in ['symbol']:  # 保留symbol为字符串
                try:
                    df[col] = pd.to_numeric(df[col], errors='coerce')
                except:
                    pass

        # 删除包含NaN的行
        initial_rows = len(df)
        df = df.dropna()
        final_rows = len(df)

        if initial_rows != final_rows:
            print(f"   ⚠️  删除了 {initial_rows - final_rows:,} 行包含NaN的数据")

        print(f"   ✅ 数据清理完成，最终: {final_rows:,} 行")

        return df
        
    


class TCNGRUTrainer:
    """Trainer for unsupervised TCN-GRU model"""
    
    def __init__(
        self,
        model: TCNGRU,
        horizons: List[int],
        taker_fee: float = 0.0006,
        maker_fee: float = 0.0002,
        use_maker: bool = False,
        device: str = None,
        loss_config: Optional[Dict[str, Any]] = None
    ):
        # 自动检测最佳设备
        if device is None:
            if torch.backends.mps.is_available():
                device = torch.device("mps")
                print("🚀 使用 MPS (Apple Silicon GPU) 加速")
            elif torch.cuda.is_available():
                device = torch.device("cuda")
                print("🚀 使用 CUDA 加速")
            else:
                device = torch.device("cpu")
                print("⚠️  使用 CPU 训练（较慢）")
        
        self.model = model.to(device)
        self.horizons = horizons
        self.device = device
        self.loss_config = resolve_loss_config(loss_config)
        
        # Loss functions
        cost_weights = self.loss_config['cost_weights']
        cost_aware = CostAwareLoss(
            taker_fee=taker_fee,
            maker_fee=maker_fee,
            slippage=self.loss_config['slippage'],
            use_maker=use_maker,
            mse_weight=cost_weights['mse_weight'],
            direction_weight=cost_weights['direction_weight'],
            profit_weight=cost_weights['profit_weight'],
            confidence_weight=cost_weights['confidence_weight'],
            margin=self.loss_config['margin'],
            direction_temperature=self.loss_config['direction_temperature'],
            adaptive_horizon_scaling=self.loss_config['adaptive_horizon_scaling']
        )
        self.criterion = UnsupervisedTCNGRULoss(
            reconstruction_weight=self.loss_config['reconstruction_weight'],
            next_step_weight=self.loss_config['next_step_weight'],
            prediction_weight=self.loss_config['prediction_weight'],
            cost_aware_loss=cost_aware,
            adaptive_balance=self.loss_config['adaptive_balance']
        )

        # Initialize GradScaler for mixed precision
        # The scaler is only enabled for CUDA devices, as it's not typically required for MPS/CPU
        self.scaler = torch.cuda.amp.GradScaler(enabled=(self.device.type == 'cuda'))
        print(f"   -> Mixed Precision enabled via autocast. GradScaler enabled: {self.scaler.is_enabled()}")
        print(
            "   🔧 Loss weights -> recon: {:.2f}, next: {:.2f}, pred: {:.2f} | cost (mse: {:.2f}, dir: {:.2f}, profit: {:.2f}, conf: {:.2f})".format(
                self.loss_config['reconstruction_weight'],
                self.loss_config['next_step_weight'],
                self.loss_config['prediction_weight'],
                cost_weights['mse_weight'],
                cost_weights['direction_weight'],
                cost_weights['profit_weight'],
                cost_weights['confidence_weight']
            )
        )
        
    def train_epoch(
        self,
        dataloader: DataLoader,
        optimizer: torch.optim.Optimizer,
        epoch: int
    ) -> Dict[str, float]:
        """Train one epoch"""
        self.model.train()
        losses = {'reconstruction': 0, 'next_step': 0, 'prediction': 0, 'total': 0}
        n_batches = 0
        
        for batch_idx, (sequences, returns, next_features) in enumerate(dataloader):
            sequences = sequences.to(self.device)
            returns = returns.to(self.device)
            next_features = next_features.to(self.device)
            
            optimizer.zero_grad()

            # Use autocast for mixed precision
            with torch.autocast(device_type=self.device.type, dtype=torch.bfloat16 if self.device.type == 'cpu' else torch.float16):
                # Forward pass
                predictions, aux_outputs = self.model(sequences)
                
                # Calculate losses
                batch_losses = self.criterion(
                    predictions=predictions,
                    aux_outputs=aux_outputs,
                    input_sequence=sequences,
                    future_returns=returns,
                    next_features=next_features,
                    horizons=self.horizons
                )

            # Backward pass with scaler
            self.scaler.scale(batch_losses['total']).backward()
            self.scaler.unscale_(optimizer) # Unscale gradients before clipping
            torch.nn.utils.clip_grad_norm_(self.model.parameters(), max_norm=1.0)
            self.scaler.step(optimizer)
            self.scaler.update()
            
            # Accumulate losses
            for key in losses:
                losses[key] += batch_losses[key].item()
            n_batches += 1
            
            if batch_idx % 10 == 0:
                print(f"  Batch {batch_idx}/{len(dataloader)}: "
                      f"Total={batch_losses['total'].item():.4f} | "
                      f"Recon={batch_losses['reconstruction'].item():.4f}, "
                      f"NextStep={batch_losses['next_step'].item():.4f}, "
                      f"Pred={batch_losses['prediction'].item():.4f}")
                
        # Average losses
        for key in losses:
            losses[key] /= n_batches
            
        return losses
        
    def validate(self, dataloader: DataLoader) -> Dict[str, float]:
        """Validate model"""
        self.model.eval()
        losses = {'reconstruction': 0, 'next_step': 0, 'prediction': 0, 'total': 0}
        predictions_all = []
        targets_all = []
        n_batches = 0
        
        with torch.no_grad():
            for sequences, returns, next_features in dataloader:
                sequences = sequences.to(self.device)
                returns = returns.to(self.device)
                next_features = next_features.to(self.device)
                
                with torch.autocast(device_type=self.device.type, dtype=torch.bfloat16 if self.device.type == 'cpu' else torch.float16):
                    # Forward pass
                    predictions, aux_outputs = self.model(sequences)
                    
                    # Calculate losses
                    batch_losses = self.criterion(
                        predictions=predictions,
                        aux_outputs=aux_outputs,
                        input_sequence=sequences,
                        future_returns=returns,
                        next_features=next_features,
                        horizons=self.horizons
                    )
                
                # Accumulate
                for key in losses:
                    losses[key] += batch_losses[key].item()
                predictions_all.append(predictions.cpu())
                targets_all.append(returns.cpu())
                n_batches += 1
                
        # Average losses
        for key in losses:
            losses[key] /= n_batches
            
        # Calculate metrics
        predictions_all = torch.cat(predictions_all, dim=0)
        targets_all = torch.cat(targets_all, dim=0)
        
        # Information coefficient per horizon
        ic_scores = []
        for i in range(len(self.horizons)):
            pred_i = predictions_all[:, i].numpy()
            target_i = targets_all[:, i].numpy()
            # Remove zeros (padding)
            mask = target_i != 0
            if mask.sum() > 0:
                ic = np.corrcoef(pred_i[mask], target_i[mask])[0, 1]
                ic_scores.append(ic if not np.isnan(ic) else 0)
            else:
                ic_scores.append(0)
                
        losses['ic_mean'] = np.mean(ic_scores)
        losses['ic_per_horizon'] = ic_scores
        
        return losses


def train_tcn_gru(
    symbol: str,
    start_date: str,
    end_date: str,
    horizons: List[int] = [1, 5, 10, 30, 60],
    seq_len: int = 60,
    feature_table: str = 'hft_features_eth_local_ob',
    taker_fee: float = 0.0006,
    maker_fee: float = 0.0002,
    use_maker: bool = False,
    epochs: int = 20,
    batch_size: int = 256,
    learning_rate: float = 0.001,
    patience: int = 10,
    model_dir: str = "./models/tcn_gru",
    random_seed: int = 42,
    resume: bool = True,
    loss_config: Optional[Dict[str, Any]] = None,
    refresh: bool = False,
) -> Dict:
    """Main training function with streaming data pipeline."""
    
    set_global_seed(random_seed)
    
    print("="*80)
    print("🚀 TCN-GRU Unsupervised Training (Fully Streaming)")
    print("="*80)
    print(f"📊 交易对: {symbol}")
    print(f"📅 时间范围: {start_date} 至 {end_date}")
    print(f"🎯 预测视界: {horizons} 秒")
    print(f"💰 策略: {'Maker' if use_maker else 'Taker'}")
    print(f"💸 费率: {(maker_fee if use_maker else taker_fee)*100:.3f}%")
    print(f"🧠 模型: TCN-GRU with 39-dim local orderbook features")
    print(f"⚙️  参数: epochs={epochs}, batch_size={batch_size}, lr={learning_rate}, seq_len={seq_len}")
    print("="*80)
    
    # Load raw data
    dataset_loader = TCNGRUDataset(symbol, start_date, end_date, seq_len, horizons, refresh=refresh)
    df = dataset_loader.load_data()

    # Define feature columns
    exclude_cols = ['exchange_ts', 'sec', 'symbol']
    feature_cols = [c for c in df.columns if c not in exclude_cols]
    feature_dim = len(feature_cols)
    
    # Train/validation split on the DataFrame
    n = len(df)
    n_train = int(0.8 * n)
    train_df = df.iloc[:n_train]
    val_df = df.iloc[n_train:].reset_index(drop=True)

    # --- START: Feature Scaling ---
    print("\n🧬 Calculating scaling parameters from training data...")
    train_data = train_df[feature_cols].values.astype(np.float32)
    train_data = np.nan_to_num(train_data, nan=0.0, posinf=0.0, neginf=0.0)
    mean = train_data.mean(axis=0)
    std = train_data.std(axis=0)
    std[std < 1e-8] = 1.0  # Avoid division by zero for stable scaling
    mean = np.nan_to_num(mean, nan=0.0)
    std = np.nan_to_num(std, nan=1.0, posinf=1.0, neginf=1.0)
    print("✅ Scaling parameters calculated.")
    # --- END: Feature Scaling ---

    # Create streaming datasets
    print("🚀 Initializing streaming datasets...")
    train_dataset = StreamingSequenceDataset(train_df, seq_len, horizons, feature_cols, mean, std)
    val_dataset = StreamingSequenceDataset(val_df, seq_len, horizons, feature_cols, mean, std)
    print(f"   Train sequences: {len(train_dataset):,}")
    print(f"   Validation sequences: {len(val_dataset):,}")

    use_mps = torch.backends.mps.is_available()
    worker_count = 0 if use_mps else 4
    pin_memory = torch.cuda.is_available()

    # Create DataLoaders with multiple workers for performance
    train_loader = DataLoader(
        train_dataset,
        batch_size=batch_size,
        shuffle=True,
        num_workers=worker_count,
        pin_memory=pin_memory
    )
    val_loader = DataLoader(
        val_dataset,
        batch_size=batch_size,
        shuffle=False,
        num_workers=worker_count,
        pin_memory=pin_memory
    )
    
    # Initialize model
    model = TCNGRU(
        input_dim=feature_dim,
        tcn_channels=[64, 128, 256],
        gru_hidden=128,
        gru_layers=2,
        horizons=horizons,
        kernel_size=3,
        dropout=0.2
    )
    
    # Training setup
    trainer = TCNGRUTrainer(
        model,
        horizons,
        taker_fee,
        maker_fee,
        use_maker,
        loss_config=loss_config
    )
    optimizer = torch.optim.AdamW(model.parameters(), lr=learning_rate, weight_decay=0.0001)
    scheduler = torch.optim.lr_scheduler.ReduceLROnPlateau(optimizer, mode='min', factor=0.5, patience=3)
    
    # Training loop setup
    checkpoint_path = os.path.join(model_dir, "latest_checkpoint.pt")
    start_epoch = 0
    best_val_loss = float('inf')
    best_model_state = None
    training_history = []
    patience_counter = 0

    if resume and os.path.exists(checkpoint_path):
        print(f"🔄 發現檢查點，從 {checkpoint_path} 恢復訓練...")
        try:
            checkpoint = torch.load(checkpoint_path, map_location=trainer.device)
            model.load_state_dict(checkpoint['model_state_dict'])
            optimizer.load_state_dict(checkpoint['optimizer_state_dict'])
            scheduler.load_state_dict(checkpoint['scheduler_state_dict'])
            start_epoch = checkpoint['epoch'] + 1
            best_val_loss = checkpoint['best_val_loss']
            training_history = checkpoint['training_history']
            patience_counter = checkpoint['patience_counter']
            if 'best_model_state' in checkpoint and checkpoint['best_model_state'] is not None:
                best_model_state = checkpoint['best_model_state']
            else:
                best_model_state = model.state_dict().copy()

            print(f"✅ 成功恢復，將從 epoch {start_epoch + 1} 繼續。")
        except Exception as e:
            print(f"⚠️ 無法載入檢查點: {e}。將從頭開始訓練。")
            start_epoch = 0
            best_val_loss = float('inf')
            training_history = []
            patience_counter = 0
    else:
        print("🏁 從頭開始新的訓練。")

    # Training loop
    for epoch in range(start_epoch, epochs):

        print(f"\nEpoch {epoch+1}/{epochs}")
        
        train_losses = trainer.train_epoch(train_loader, optimizer, epoch)
        print(f"Train - Total: {train_losses['total']:.4f}, Recon: {train_losses['reconstruction']:.4f}, Pred: {train_losses['prediction']:.4f}")
        
        val_losses = trainer.validate(val_loader)
        print(f"Val - Total: {val_losses['total']:.4f}, IC: {val_losses['ic_mean']:.4f}")
        
        print("IC per horizon:", end=" ")
        for h, ic in zip(horizons, val_losses['ic_per_horizon']):
            print(f"{h}s:{ic:.3f}", end=" ")
        print()
        
        scheduler.step(val_losses['total'])
        
        if val_losses['total'] < best_val_loss:
            best_val_loss = val_losses['total']
            best_model_state = model.state_dict().copy()
            patience_counter = 0
            print("  -> New best model!")
        else:
            patience_counter += 1
            print(f"  -> No improvement. Patience: {patience_counter}/{patience}")
            
        training_history.append({
            'epoch': epoch + 1,
            'train_loss': train_losses['total'],
            'val_loss': val_losses['total'],
            'val_ic': val_losses['ic_mean'],
            'ic_per_horizon': val_losses['ic_per_horizon']
        })

        # Save checkpoint at the end of every epoch
        print("  -> 正在保存檢查點...")
        torch.save({
            'epoch': epoch,
            'model_state_dict': model.state_dict(),
            'optimizer_state_dict': optimizer.state_dict(),
            'scheduler_state_dict': scheduler.state_dict(),
            'best_val_loss': best_val_loss,
            'training_history': training_history,
            'patience_counter': patience_counter,
            'best_model_state': best_model_state,
        }, checkpoint_path)
        print(f"     ✅ 檢查點已保存至 {checkpoint_path}")

        if patience_counter >= patience:
            print(f"⏹️ Early stopping triggered after {patience} epochs of no improvement.")
            break
        
    # Load the best model state before saving the final model
    if best_model_state:
        print("\n🔄 載入最佳模型狀態...")
        model.load_state_dict(best_model_state)
    else:
        print("\n⚠️ 未找到最佳模型狀態，將使用最終模型。")
    
    os.makedirs(model_dir, exist_ok=True)
    timestamp = datetime.now().strftime('%Y%m%d_%H%M%S')
    
    model_path = os.path.join(model_dir, f"tcn_gru_{symbol}_{timestamp}.pt")
    torch.save({
        'model_state_dict': best_model_state,
        'scaling_params': {'mean': mean, 'std': std},
        'feature_cols': feature_cols,
        'model_config': {
            'input_dim': feature_dim,
            'tcn_channels': [64, 128, 256],
            'gru_hidden': 128,
            'gru_layers': 2,
            'horizons': horizons,
            'kernel_size': 3,
            'dropout': 0.2
        },
        'training_config': {
            'symbol': symbol, 'start_date': start_date, 'end_date': end_date,
            'seq_len': seq_len, 'horizons': horizons, 'taker_fee': taker_fee,
            'maker_fee': maker_fee, 'use_maker': use_maker,
            'best_val_loss': best_val_loss, 'best_val_ic': val_losses['ic_mean'],
            'loss_config': trainer.loss_config
        }
    }, model_path)
    
    print(f"\nModel saved to: {model_path}")
    
    history_path = os.path.join(model_dir, f"history_{symbol}_{timestamp}.json")
    with open(history_path, 'w') as f:
        json.dump(training_history, f, indent=2)
    
    model.eval()
    example_input = torch.randn(1, seq_len, feature_dim)
    traced_model = torch.jit.trace(model, (example_input,))
    torchscript_path = os.path.join(model_dir, f"tcn_gru_{symbol}_{timestamp}.torchscript.pt")
    traced_model.save(torchscript_path)
    
    # 顯式釋放 DataLoader worker，避免 MPS 環境阻塞退出
    del train_loader
    del val_loader
    if trainer.device.type == 'mps' and hasattr(torch, 'mps') and hasattr(torch.mps, 'empty_cache'):
        torch.mps.empty_cache()
    print(f"TorchScript model saved to: {torchscript_path}")

    return {
        'model_path': model_path,
        'torchscript_path': torchscript_path,
        'best_val_loss': best_val_loss,
        'best_val_ic': val_losses['ic_mean'],
        'ic_per_horizon': dict(zip(horizons, val_losses['ic_per_horizon'])),
        'history': training_history,
        'loss_config': trainer.loss_config
    }


def main():
    parser = argparse.ArgumentParser(description='TCN-GRU Unsupervised Training')
    parser.add_argument('--symbol', type=str, default='WLFIUSDT', help='Trading symbol')
    parser.add_argument('--start_date', type=str, default='2025-09-01T00:00:00Z', help='Start date')
    parser.add_argument('--end_date', type=str, default='2025-09-08T00:00:00Z', help='End date')
    parser.add_argument('--horizons', type=str, default='1,5,10,30,60', help='Prediction horizons (seconds)')
    parser.add_argument('--seq_len', type=int, default=60, help='Sequence length')
    parser.add_argument('--epochs', type=int, default=20, help='Number of epochs')
    parser.add_argument('--batch_size', type=int, default=256, help='Batch size')
    parser.add_argument('--learning_rate', type=float, default=0.001, help='Learning rate')
    parser.add_argument('--taker_fee', type=float, default=0.0006, help='Taker fee rate')
    parser.add_argument('--maker_fee', type=float, default=0.0002, help='Maker fee rate')
    parser.add_argument('--use_maker', action='store_true', help='Use maker strategy')
    parser.add_argument('--model_dir', type=str, default='./models/tcn_gru', help='Model directory')
    parser.add_argument('--random_seed', type=int, default=42, help='Random seed')
    
    args = parser.parse_args()
    
    # Parse horizons
    horizons = [int(h) for h in args.horizons.split(',')]
    
    # Train model
    result = train_tcn_gru(
        symbol=args.symbol,
        start_date=args.start_date,
        end_date=args.end_date,
        horizons=horizons,
        seq_len=args.seq_len,
        taker_fee=args.taker_fee,
        maker_fee=args.maker_fee,
        use_maker=args.use_maker,
        epochs=args.epochs,
        batch_size=args.batch_size,
        learning_rate=args.learning_rate,
        model_dir=args.model_dir,
        random_seed=args.random_seed
    )
    
    print("\n" + "="*60)
    print("Training Summary:")
    print(f"Best Validation Loss: {result['best_val_loss']:.4f}")
    print(f"Best IC: {result['best_val_ic']:.4f}")
    print("IC per Horizon:")
    for h, ic in result['ic_per_horizon'].items():
        print(f"  {h}s: {ic:.4f}")
    print("="*60)


if __name__ == '__main__':
    main()
