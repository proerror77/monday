#!/usr/bin/env python3
"""
TLOB特徵工程組件 - ML Workspace核心組件
=====================================

基於PRD第3.1節的特徵工程需求：
1. 從Rust HFT引擎獲取LOB數據
2. 提取微觀結構特徵
3. 構建TLOB Transformer輸入
4. 優化特徵以提升預測性能

重要：這裡整合原本的rust_hft_tools.py邏輯
"""

import numpy as np
import pandas as pd
import torch
from typing import Dict, List, Any, Optional, Tuple
import asyncio
import logging
from datetime import datetime, timedelta
import redis
import json

# 導入Rust HFT綁定
try:
    import rust_hft_py
    RUST_HFT_AVAILABLE = True
except ImportError:
    RUST_HFT_AVAILABLE = False
    logging.warning("Rust HFT Python綁定不可用，將使用模擬數據")

logger = logging.getLogger(__name__)

class FeatureEngineer:
    """
    TLOB特徵工程師
    
    負責將原始LOB數據轉換為TLOB Transformer模型的輸入特徵
    整合Rust HFT引擎的高性能數據處理能力
    """
    
    def __init__(self, rust_hft_path: Optional[str] = None):
        self.rust_hft_path = rust_hft_path
        self.rust_processor = None
        self.redis_client = None
        
        # 初始化Rust處理器
        self._initialize_rust_processor()
        
        # 初始化Redis連接
        self._initialize_redis()
        
        # 特徵配置
        self.feature_config = {
            "sequence_length": 128,      # TLOB序列長度
            "price_levels": 10,          # 價格檔位數
            "technical_indicators": 15,   # 技術指標數量
            "microstructure_features": 20 # 微觀結構特徵數
        }
        
        logger.info("✅ TLOB特徵工程器初始化完成")
    
    def _initialize_rust_processor(self):
        """初始化Rust數據處理器"""
        if RUST_HFT_AVAILABLE:
            try:
                self.rust_processor = rust_hft_py.DataProcessor()
                logger.info("✅ Rust數據處理器初始化成功")
            except Exception as e:
                logger.warning(f"⚠️ Rust處理器初始化失敗: {e}")
                self.rust_processor = None
        else:
            logger.info("ℹ️ 使用純Python特徵工程實現")
    
    def _initialize_redis(self):
        """初始化Redis連接"""
        try:
            self.redis_client = redis.Redis(host='localhost', port=6379, db=0)
            self.redis_client.ping()
            logger.info("✅ Redis連接建立成功")
        except Exception as e:
            logger.warning(f"⚠️ Redis連接失敗: {e}")
            self.redis_client = None
    
    async def extract_features_for_training(
        self, 
        symbol: str, 
        hours: int = 24,
        validation_split: float = 0.2
    ) -> Dict[str, torch.Tensor]:
        """
        為模型訓練提取特徵
        
        Args:
            symbol: 交易對符號
            hours: 數據時長（小時）
            validation_split: 驗證集比例
            
        Returns:
            包含訓練和驗證數據的特徵字典
        """
        logger.info(f"🔧 開始為{symbol}提取訓練特徵，時長: {hours}小時")
        
        try:
            # 1. 獲取原始LOB數據
            raw_data = await self._get_lob_data(symbol, hours)
            
            if raw_data is None:
                raise ValueError(f"無法獲取{symbol}的LOB數據")
            
            # 2. 數據清理和預處理
            cleaned_data = self._clean_and_preprocess(raw_data)
            
            # 3. 提取各類特徵
            price_features = self._extract_price_features(cleaned_data)
            volume_features = self._extract_volume_features(cleaned_data)
            imbalance_features = self._extract_imbalance_features(cleaned_data)
            technical_features = self._extract_technical_indicators(cleaned_data)
            microstructure_features = self._extract_microstructure_features(cleaned_data)
            
            # 4. 組合所有特徵
            combined_features = np.concatenate([
                price_features,
                volume_features, 
                imbalance_features,
                technical_features,
                microstructure_features
            ], axis=-1)
            
            # 5. 創建序列和標籤
            sequences, labels = self._create_sequences_and_labels(combined_features)
            
            # 6. 分割訓練和驗證集
            split_idx = int(len(sequences) * (1 - validation_split))
            
            train_features = torch.FloatTensor(sequences[:split_idx])
            train_labels = torch.FloatTensor(labels[:split_idx])
            val_features = torch.FloatTensor(sequences[split_idx:])
            val_labels = torch.FloatTensor(labels[split_idx:])
            
            logger.info(f"✅ 特徵提取完成: 訓練集{len(train_features)}, 驗證集{len(val_features)}")
            
            return {
                "train_features": train_features,
                "train_labels": train_labels,
                "val_features": val_features,
                "val_labels": val_labels,
                "feature_dim": combined_features.shape[-1],
                "sequence_length": self.feature_config["sequence_length"],
                "symbol": symbol
            }
            
        except Exception as e:
            logger.error(f"❌ 特徵提取失敗: {e}")
            raise
    
    async def extract_real_time_features(self, symbol: str) -> Optional[torch.Tensor]:
        """
        提取實時特徵用於推理
        
        Args:
            symbol: 交易對符號
            
        Returns:
            實時特徵張量
        """
        try:
            if self.rust_processor:
                # 使用Rust處理器獲取實時特徵
                features = self.rust_processor.extract_real_features(symbol)
                return self._convert_rust_features_to_tensor(features)
            else:
                # 備用：從Redis獲取實時數據
                return await self._get_real_time_features_from_redis(symbol)
                
        except Exception as e:
            logger.error(f"❌ 實時特徵提取失敗: {e}")
            return None
    
    async def _get_lob_data(self, symbol: str, hours: int) -> Optional[pd.DataFrame]:
        """從數據源獲取LOB數據"""
        if self.rust_processor:
            try:
                # 使用Rust處理器獲取歷史數據
                data = self.rust_processor.get_historical_lob_data(symbol, hours)
                return pd.DataFrame(data)
            except Exception as e:
                logger.warning(f"⚠️ Rust數據獲取失敗: {e}")
        
        # 備用：模擬數據生成
        logger.info("🔄 使用模擬LOB數據")
        return self._generate_mock_lob_data(symbol, hours)
    
    def _generate_mock_lob_data(self, symbol: str, hours: int) -> pd.DataFrame:
        """生成模擬LOB數據"""
        # 生成時間序列
        end_time = datetime.now()
        start_time = end_time - timedelta(hours=hours)
        timestamps = pd.date_range(start_time, end_time, freq='1S')
        
        np.random.seed(42)  # 確保可重現性
        
        # 基礎價格（根據symbol調整）
        base_prices = {
            "BTCUSDT": 45000,
            "ETHUSDT": 2500, 
            "SOLUSDT": 100,
            "ADAUSDT": 0.5,
            "DOTUSDT": 8
        }
        
        base_price = base_prices.get(symbol, 1000)
        
        # 生成價格路徑
        returns = np.random.normal(0, 0.001, len(timestamps))
        prices = base_price * np.exp(np.cumsum(returns))
        
        # 生成LOB數據
        data = []
        for i, (ts, price) in enumerate(zip(timestamps, prices)):
            # 買賣價差
            spread = price * 0.0001  # 0.01% spread
            bid_price = price - spread/2
            ask_price = price + spread/2
            
            # 深度數據
            levels = []
            for level in range(10):  # 10檔
                bid_size = np.random.exponential(100)
                ask_size = np.random.exponential(100)
                
                levels.extend([
                    bid_price - level * spread * 0.1,  # bid_price
                    bid_size,                          # bid_size
                    ask_price + level * spread * 0.1,  # ask_price
                    ask_size                           # ask_size
                ])
            
            data.append({
                "timestamp": ts,
                "symbol": symbol,
                "mid_price": price,
                "spread": spread,
                "levels": levels
            })
        
        return pd.DataFrame(data)
    
    def _clean_and_preprocess(self, data: pd.DataFrame) -> pd.DataFrame:
        """數據清理和預處理"""
        # 去除缺失值
        data = data.dropna()
        
        # 去除異常值 (3-sigma原則)
        for col in ['mid_price', 'spread']:
            if col in data.columns:
                mean = data[col].mean()
                std = data[col].std()
                data = data[abs(data[col] - mean) < 3 * std]
        
        # 時間排序
        data = data.sort_values('timestamp')
        
        return data
    
    def _extract_price_features(self, data: pd.DataFrame) -> np.ndarray:
        """提取價格相關特徵"""
        prices = data['mid_price'].values
        
        features = []
        
        # 價格本身（歸一化）
        features.append(self._normalize_prices(prices))
        
        # 價格變化率
        returns = np.diff(prices, prepend=prices[0]) / prices
        features.append(returns)
        
        # 價格動量（多時間窗口）
        for window in [5, 10, 20]:
            momentum = pd.Series(prices).rolling(window).mean().fillna(method='bfill').values
            features.append(momentum)
        
        # 價格波動率
        volatility = pd.Series(returns).rolling(20).std().fillna(method='bfill').values
        features.append(volatility)
        
        return np.column_stack(features)
    
    def _extract_volume_features(self, data: pd.DataFrame) -> np.ndarray:
        """提取成交量相關特徵"""
        # 模擬成交量數據
        volumes = np.random.exponential(1000, len(data))
        
        features = []
        
        # 成交量（歸一化）
        normalized_volume = (volumes - volumes.mean()) / volumes.std()
        features.append(normalized_volume)
        
        # 成交量移動平均
        for window in [5, 10, 20]:
            vol_ma = pd.Series(volumes).rolling(window).mean().fillna(method='bfill').values
            features.append(vol_ma)
        
        # 成交量加權平均價格 (VWAP)
        prices = data['mid_price'].values
        vwap = np.cumsum(prices * volumes) / np.cumsum(volumes)
        features.append(vwap)
        
        return np.column_stack(features)
    
    def _extract_imbalance_features(self, data: pd.DataFrame) -> np.ndarray:
        """提取訂單簿不平衡特徵"""
        # 模擬買賣盤數據
        bid_volumes = np.random.exponential(500, len(data))
        ask_volumes = np.random.exponential(500, len(data))
        
        features = []
        
        # 訂單簿不平衡
        imbalance = (bid_volumes - ask_volumes) / (bid_volumes + ask_volumes)
        features.append(imbalance)
        
        # 深度不平衡 (多檔位)
        for level in range(1, 6):  # 前5檔
            level_imbalance = np.random.normal(0, 0.1, len(data))
            features.append(level_imbalance)
        
        # 流動性指標
        liquidity = bid_volumes + ask_volumes
        normalized_liquidity = (liquidity - liquidity.mean()) / liquidity.std()
        features.append(normalized_liquidity)
        
        return np.column_stack(features)
    
    def _extract_technical_indicators(self, data: pd.DataFrame) -> np.ndarray:
        """提取技術指標特徵"""
        prices = data['mid_price'].values
        
        features = []
        
        # RSI
        rsi = self._calculate_rsi(prices)
        features.append(rsi)
        
        # MACD
        macd, signal = self._calculate_macd(prices)
        features.extend([macd, signal])
        
        # 布林帶
        upper_band, lower_band = self._calculate_bollinger_bands(prices)
        bb_ratio = (prices - lower_band) / (upper_band - lower_band)
        features.append(bb_ratio)
        
        # ATR (平均真實範圍)
        atr = self._calculate_atr(data)
        features.append(atr)
        
        return np.column_stack(features)
    
    def _extract_microstructure_features(self, data: pd.DataFrame) -> np.ndarray:
        """提取微觀結構特徵"""
        prices = data['mid_price'].values
        spreads = data['spread'].values
        
        features = []
        
        # 相對價差
        relative_spread = spreads / prices
        features.append(relative_spread)
        
        # 價格影響
        price_impact = np.abs(np.diff(prices, prepend=prices[0]))
        features.append(price_impact)
        
        # 跳躍檢測
        jump_indicator = self._detect_price_jumps(prices)
        features.append(jump_indicator)
        
        # 微觀結構噪音
        microstructure_noise = self._calculate_microstructure_noise(prices)
        features.append(microstructure_noise)
        
        return np.column_stack(features)
    
    def _create_sequences_and_labels(self, features: np.ndarray) -> Tuple[np.ndarray, np.ndarray]:
        """創建時間序列和標籤"""
        seq_len = self.feature_config["sequence_length"]
        
        sequences = []
        labels = []
        
        for i in range(seq_len, len(features)):
            # 特徵序列
            seq = features[i-seq_len:i]
            sequences.append(seq)
            
            # 標籤（下一個時間點的價格變化）
            if i < len(features) - 1:
                # 使用價格變化作為標籤
                price_change = features[i+1, 0] - features[i, 0]  # 第一個特徵是歸一化價格
                labels.append(price_change)
            else:
                labels.append(0.0)
        
        return np.array(sequences), np.array(labels)
    
    def _normalize_prices(self, prices: np.ndarray) -> np.ndarray:
        """價格歸一化"""
        return (prices - prices.mean()) / prices.std()
    
    def _calculate_rsi(self, prices: np.ndarray, period: int = 14) -> np.ndarray:
        """計算RSI指標"""
        deltas = np.diff(prices)
        gains = np.where(deltas > 0, deltas, 0)
        losses = np.where(deltas < 0, -deltas, 0)
        
        avg_gains = pd.Series(gains).rolling(period).mean().fillna(50).values
        avg_losses = pd.Series(losses).rolling(period).mean().fillna(50).values
        
        rs = avg_gains / (avg_losses + 1e-8)
        rsi = 100 - (100 / (1 + rs))
        
        return np.concatenate([[50], rsi])  # 填充第一個值
    
    def _calculate_macd(self, prices: np.ndarray) -> Tuple[np.ndarray, np.ndarray]:
        """計算MACD指標"""
        ema12 = pd.Series(prices).ewm(span=12).mean()
        ema26 = pd.Series(prices).ewm(span=26).mean()
        
        macd = ema12 - ema26
        signal = macd.ewm(span=9).mean()
        
        return macd.fillna(0).values, signal.fillna(0).values
    
    def _calculate_bollinger_bands(self, prices: np.ndarray, period: int = 20) -> Tuple[np.ndarray, np.ndarray]:
        """計算布林帶"""
        sma = pd.Series(prices).rolling(period).mean()
        std = pd.Series(prices).rolling(period).std()
        
        upper_band = sma + (std * 2)
        lower_band = sma - (std * 2)
        
        return upper_band.fillna(method='bfill').values, lower_band.fillna(method='bfill').values
    
    def _calculate_atr(self, data: pd.DataFrame, period: int = 14) -> np.ndarray:
        """計算平均真實範圍"""
        high = data['mid_price'] + data['spread'] / 2
        low = data['mid_price'] - data['spread'] / 2
        close = data['mid_price']
        
        tr1 = high - low
        tr2 = np.abs(high - close.shift(1))
        tr3 = np.abs(low - close.shift(1))
        
        true_range = np.maximum(tr1, np.maximum(tr2, tr3))
        atr = true_range.rolling(period).mean().fillna(method='bfill')
        
        return atr.values
    
    def _detect_price_jumps(self, prices: np.ndarray, threshold: float = 3.0) -> np.ndarray:
        """檢測價格跳躍"""
        returns = np.diff(prices, prepend=prices[0]) / prices
        std_returns = np.std(returns)
        
        jumps = np.abs(returns) > threshold * std_returns
        return jumps.astype(float)
    
    def _calculate_microstructure_noise(self, prices: np.ndarray) -> np.ndarray:
        """計算微觀結構噪音"""
        # 使用Hasbrouck的方法估計微觀結構噪音
        returns = np.diff(prices, prepend=prices[0])
        
        # 一階自相關作為噪音指標
        noise = pd.Series(returns).rolling(20).apply(
            lambda x: np.corrcoef(x[:-1], x[1:])[0, 1] if len(x) > 1 else 0
        ).fillna(0)
        
        return noise.values
    
    def _convert_rust_features_to_tensor(self, rust_features: Dict[str, Any]) -> torch.Tensor:
        """將Rust特徵轉換為PyTorch張量"""
        # 提取數值特徵
        feature_values = []
        
        for key in ['mid_price', 'spread', 'best_bid', 'best_ask', 'spread_bps']:
            if key in rust_features:
                feature_values.append(float(rust_features[key]))
        
        # 歸一化和填充到目標維度
        tensor = torch.FloatTensor(feature_values)
        
        # 確保特徵維度正確
        target_dim = sum([
            5,   # 價格特徵
            5,   # 成交量特徵 
            7,   # 不平衡特徵
            5,   # 技術指標
            4    # 微觀結構特徵
        ])
        
        if len(tensor) < target_dim:
            # 填充零值
            padding = torch.zeros(target_dim - len(tensor))
            tensor = torch.cat([tensor, padding])
        elif len(tensor) > target_dim:
            # 截斷
            tensor = tensor[:target_dim]
        
        return tensor.unsqueeze(0)  # 添加batch維度
    
    async def _get_real_time_features_from_redis(self, symbol: str) -> Optional[torch.Tensor]:
        """從Redis獲取實時特徵"""
        if not self.redis_client:
            return None
        
        try:
            # 從Redis獲取實時數據
            redis_key = f'hft:orderbook:{symbol}'
            redis_data = self.redis_client.get(redis_key)
            
            if redis_data:
                data = json.loads(redis_data)
                return self._convert_redis_data_to_tensor(data)
        
        except Exception as e:
            logger.error(f"❌ Redis實時特徵獲取失敗: {e}")
        
        return None
    
    def _convert_redis_data_to_tensor(self, redis_data: Dict[str, Any]) -> torch.Tensor:
        """將Redis數據轉換為特徵張量"""
        feature_values = []
        
        # 提取基本特徵
        for key in ['mid_price', 'best_bid', 'best_ask', 'spread']:
            feature_values.append(float(redis_data.get(key, 0)))
        
        # 計算派生特徵
        mid_price = float(redis_data.get('mid_price', 0))
        spread = float(redis_data.get('spread', 0))
        
        if mid_price > 0:
            relative_spread = spread / mid_price
            feature_values.append(relative_spread)
        else:
            feature_values.append(0.0)
        
        # 填充到目標維度（簡化版）
        while len(feature_values) < 20:  # 最小特徵數
            feature_values.append(0.0)
        
        return torch.FloatTensor(feature_values).unsqueeze(0)

# ==========================================
# 測試和驗證函數
# ==========================================

async def test_feature_engineer():
    """測試特徵工程器"""
    engineer = FeatureEngineer()
    
    # 測試訓練特徵提取
    features = await engineer.extract_features_for_training("BTCUSDT", hours=1)
    
    print(f"特徵提取結果:")
    print(f"  訓練集大小: {features['train_features'].shape}")
    print(f"  驗證集大小: {features['val_features'].shape}")
    print(f"  特徵維度: {features['feature_dim']}")
    print(f"  序列長度: {features['sequence_length']}")
    
    # 測試實時特徵提取
    real_time_features = await engineer.extract_real_time_features("BTCUSDT")
    if real_time_features is not None:
        print(f"  實時特徵: {real_time_features.shape}")
    
    return features

if __name__ == "__main__":
    asyncio.run(test_feature_engineer())