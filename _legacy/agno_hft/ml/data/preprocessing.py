"""
TLOB 數據預處理器 (任務 1.3)
========================

實現 TLOB 計劃中的數據轉換流程：
1. 扁平化：15檔 LOB 快照 → 60維向量
2. 拼接：添加 Ticker 數據  
3. 標準化：計算並保存 mean/std (關鍵：與 Rust 一致性)
4. 序列化：創建滾動窗口

這個模塊的實現必須與 Rust 端完全一致！
"""

import pandas as pd
import numpy as np
from typing import Dict, List, Tuple, Optional, Union
from dataclasses import dataclass
import json
import logging
from pathlib import Path

logger = logging.getLogger(__name__)


@dataclass
class PreprocessingConfig:
    """預處理配置"""
    # LOB 配置
    lob_levels: int = 15  # 使用的檔位數量
    
    # 序列配置
    sequence_length: int = 100  # 序列長度 N
    
    # 特徵配置
    include_ticker: bool = True  # 是否包含 Ticker 數據
    ticker_features: List[str] = None  # Ticker 特徵列表
    
    # 標準化配置
    normalization_method: str = 'zscore'  # 'zscore' 或 'minmax'
    save_normalization_stats: bool = True  # 是否保存標準化統計信息
    
    # 輸出配置
    output_format: str = 'numpy'  # 'numpy' 或 'torch'
    
    def __post_init__(self):
        if self.ticker_features is None:
            self.ticker_features = ['last_price', 'volume']


class LOBPreprocessor:
    """
    TLOB LOB 數據預處理器
    
    按照 TLOB 計劃實現精確的數據轉換邏輯。
    ⚠️ 重要：此實現必須與 Rust 端的 Preprocessor 完全一致！
    """
    
    def __init__(self, config: Optional[PreprocessingConfig] = None):
        self.config = config or PreprocessingConfig()
        self.normalization_stats = None
        self.feature_names = None
        self.is_fitted = False
        
    def fit(
        self, 
        df: pd.DataFrame,
        save_stats_path: Optional[str] = None
    ) -> 'LOBPreprocessor':
        """
        在訓練集上擬合預處理器
        
        Args:
            df: 訓練數據
            save_stats_path: 保存標準化統計信息的路徑
            
        Returns:
            self (支持鏈式調用)
        """
        # 驗證輸入
        self._validate_input(df)
        
        # 提取並扁平化特徵
        features = self._extract_features(df)
        
        # 計算標準化統計信息
        self.normalization_stats = self._compute_normalization_stats(features)
        self.feature_names = self._get_feature_names()
        self.is_fitted = True
        
        # 保存標準化統計信息
        if save_stats_path and self.config.save_normalization_stats:
            self.save_normalization_stats(save_stats_path)
            
        logger.info(f"Fitted preprocessor on {len(features)} samples")
        logger.info(f"Feature dimensions: {features.shape[1]}")
        
        return self
        
    def transform(self, df: pd.DataFrame) -> np.ndarray:
        """
        轉換數據
        
        Args:
            df: 要轉換的數據
            
        Returns:
            預處理後的特徵數組
        """
        if not self.is_fitted:
            raise ValueError("Preprocessor must be fitted before transform")
            
        # 驗證輸入
        self._validate_input(df)
        
        # 提取特徵
        features = self._extract_features(df)
        
        # 標準化
        normalized_features = self._normalize_features(features)
        
        # 創建序列
        sequences = self._create_sequences(normalized_features)
        
        logger.info(f"Transformed {len(df)} samples to {len(sequences)} sequences")
        
        return sequences
        
    def fit_transform(
        self, 
        df: pd.DataFrame,
        save_stats_path: Optional[str] = None
    ) -> np.ndarray:
        """擬合並轉換（便捷方法）"""
        return self.fit(df, save_stats_path).transform(df)
        
    def _validate_input(self, df: pd.DataFrame):
        """驗證輸入數據格式"""
        # 檢查必需的 LOB 列
        required_lob_columns = []
        for level in range(1, self.config.lob_levels + 1):
            required_lob_columns.extend([
                f'bid_price_{level}',
                f'bid_size_{level}',
                f'ask_price_{level}',
                f'ask_size_{level}'
            ])
            
        missing_lob_columns = [col for col in required_lob_columns if col not in df.columns]
        if missing_lob_columns:
            raise ValueError(f"Missing LOB columns: {missing_lob_columns}")
            
        # 檢查 Ticker 列（如果啟用）
        if self.config.include_ticker:
            missing_ticker_columns = [
                col for col in self.config.ticker_features 
                if col not in df.columns
            ]
            if missing_ticker_columns:
                logger.warning(f"Missing ticker columns: {missing_ticker_columns}")
                
    def _extract_features(self, df: pd.DataFrame) -> np.ndarray:
        """
        提取並扁平化特徵
        
        按照 TLOB 計劃：
        1. 扁平化 15檔 LOB → 60維向量
        2. 拼接 Ticker 數據
        """
        feature_list = []
        
        # 1. 扁平化 LOB 數據 (15檔 × 4特徵 = 60維)
        lob_features = self._flatten_lob(df)
        feature_list.append(lob_features)
        
        # 2. 拼接 Ticker 數據
        if self.config.include_ticker:
            ticker_features = self._extract_ticker_features(df)
            if ticker_features.size > 0:
                feature_list.append(ticker_features)
                
        # 合併所有特徵
        features = np.concatenate(feature_list, axis=1)
        
        return features
        
    def _flatten_lob(self, df: pd.DataFrame) -> np.ndarray:
        """
        扁平化 LOB 數據
        
        將每檔的 [bid_price, bid_size, ask_price, ask_size] 按順序排列
        結果維度：n_samples × (15檔 × 4特徵) = n_samples × 60
        """
        lob_data = []
        
        for level in range(1, self.config.lob_levels + 1):
            # 按固定順序添加特徵：買價、買量、賣價、賣量
            lob_data.append(df[f'bid_price_{level}'].values)
            lob_data.append(df[f'bid_size_{level}'].values)
            lob_data.append(df[f'ask_price_{level}'].values)
            lob_data.append(df[f'ask_size_{level}'].values)
            
        # 轉置以獲得正確的形狀 (n_samples, n_features)
        flattened_lob = np.array(lob_data).T
        
        return flattened_lob
        
    def _extract_ticker_features(self, df: pd.DataFrame) -> np.ndarray:
        """提取 Ticker 特徵"""
        ticker_data = []
        
        for feature in self.config.ticker_features:
            if feature in df.columns:
                ticker_data.append(df[feature].values)
            else:
                # 如果特徵不存在，用零填充
                logger.warning(f"Ticker feature {feature} not found, using zeros")
                ticker_data.append(np.zeros(len(df)))
                
        if ticker_data:
            return np.array(ticker_data).T
        else:
            return np.empty((len(df), 0))
            
    def _compute_normalization_stats(self, features: np.ndarray) -> Dict:
        """
        計算標準化統計信息
        
        ⚠️ 關鍵：這些統計信息必須保存並在 Rust 端使用！
        """
        if self.config.normalization_method == 'zscore':
            stats = {
                'method': 'zscore',
                'mean': features.mean(axis=0).tolist(),
                'std': features.std(axis=0, ddof=0).tolist(),  # 使用 ddof=0 確保一致性
                'feature_dim': features.shape[1]
            }
        elif self.config.normalization_method == 'minmax':
            stats = {
                'method': 'minmax',
                'min': features.min(axis=0).tolist(),
                'max': features.max(axis=0).tolist(),
                'feature_dim': features.shape[1]
            }
        else:
            raise ValueError(f"Unknown normalization method: {self.config.normalization_method}")
            
        return stats
        
    def _normalize_features(self, features: np.ndarray) -> np.ndarray:
        """應用標準化"""
        if self.normalization_stats['method'] == 'zscore':
            mean = np.array(self.normalization_stats['mean'])
            std = np.array(self.normalization_stats['std'])
            
            # 避免除零
            std = np.where(std == 0, 1.0, std)
            
            normalized = (features - mean) / std
            
        elif self.normalization_stats['method'] == 'minmax':
            min_vals = np.array(self.normalization_stats['min'])
            max_vals = np.array(self.normalization_stats['max'])
            
            # 避免除零
            range_vals = max_vals - min_vals
            range_vals = np.where(range_vals == 0, 1.0, range_vals)
            
            normalized = (features - min_vals) / range_vals
            
        return normalized
        
    def _create_sequences(self, features: np.ndarray) -> np.ndarray:
        """
        創建序列數據
        
        將連續 N 個特徵向量堆疊成序列
        輸出形狀：(n_sequences, sequence_length, feature_dim)
        """
        n_samples, feature_dim = features.shape
        seq_len = self.config.sequence_length
        
        if n_samples < seq_len:
            raise ValueError(
                f"Not enough samples ({n_samples}) to create sequences of length {seq_len}"
            )
            
        # 計算可以創建的序列數量
        n_sequences = n_samples - seq_len + 1
        
        # 創建序列
        sequences = np.zeros((n_sequences, seq_len, feature_dim))
        
        for i in range(n_sequences):
            sequences[i] = features[i:i + seq_len]
            
        return sequences
        
    def _get_feature_names(self) -> List[str]:
        """獲取特徵名稱（用於調試和可視化）"""
        names = []
        
        # LOB 特徵名稱
        for level in range(1, self.config.lob_levels + 1):
            names.extend([
                f'bid_price_{level}',
                f'bid_size_{level}',
                f'ask_price_{level}',
                f'ask_size_{level}'
            ])
            
        # Ticker 特徵名稱
        if self.config.include_ticker:
            names.extend(self.config.ticker_features)
            
        return names
        
    def save_normalization_stats(self, filepath: str):
        """
        保存標準化統計信息
        
        ⚠️ 關鍵：Rust 端必須加載這個文件！
        """
        if not self.is_fitted:
            raise ValueError("Preprocessor must be fitted before saving stats")
            
        stats_with_config = {
            'normalization_stats': self.normalization_stats,
            'config': {
                'lob_levels': self.config.lob_levels,
                'sequence_length': self.config.sequence_length,
                'include_ticker': self.config.include_ticker,
                'ticker_features': self.config.ticker_features,
                'normalization_method': self.config.normalization_method
            },
            'feature_names': self.feature_names,
            'version': '1.0'  # 版本號，用於兼容性檢查
        }
        
        # 確保目錄存在
        Path(filepath).parent.mkdir(parents=True, exist_ok=True)
        
        with open(filepath, 'w') as f:
            json.dump(stats_with_config, f, indent=2)
            
        logger.info(f"Saved normalization stats to {filepath}")
        
    @classmethod
    def load_normalization_stats(cls, filepath: str) -> 'LOBPreprocessor':
        """從文件加載標準化統計信息"""
        with open(filepath, 'r') as f:
            stats_data = json.load(f)
            
        # 重建配置
        config_dict = stats_data['config']
        config = PreprocessingConfig(**config_dict)
        
        # 創建預處理器
        preprocessor = cls(config)
        preprocessor.normalization_stats = stats_data['normalization_stats']
        preprocessor.feature_names = stats_data['feature_names']
        preprocessor.is_fitted = True
        
        logger.info(f"Loaded preprocessor from {filepath}")
        
        return preprocessor
        
    def validate_consistency(self, other_stats_path: str) -> bool:
        """
        驗證與另一個統計文件的一致性
        
        用於確保 Python 和 Rust 使用相同的預處理參數
        """
        if not self.is_fitted:
            raise ValueError("Preprocessor must be fitted")
            
        with open(other_stats_path, 'r') as f:
            other_stats = json.load(f)
            
        # 比較關鍵參數
        our_stats = self.normalization_stats
        other_norm_stats = other_stats['normalization_stats']
        
        tolerance = 1e-10
        
        if our_stats['method'] != other_norm_stats['method']:
            return False
            
        if our_stats['method'] == 'zscore':
            mean_diff = np.abs(np.array(our_stats['mean']) - np.array(other_norm_stats['mean']))
            std_diff = np.abs(np.array(our_stats['std']) - np.array(other_norm_stats['std']))
            
            return np.all(mean_diff < tolerance) and np.all(std_diff < tolerance)
            
        elif our_stats['method'] == 'minmax':
            min_diff = np.abs(np.array(our_stats['min']) - np.array(other_norm_stats['min']))
            max_diff = np.abs(np.array(our_stats['max']) - np.array(other_norm_stats['max']))
            
            return np.all(min_diff < tolerance) and np.all(max_diff < tolerance)
            
        return False
        
    def export_test_data(
        self, 
        df: pd.DataFrame, 
        output_path: str,
        n_samples: int = 100
    ):
        """
        導出測試數據用於 Python-Rust 一致性驗證
        
        這是 TLOB 計劃任務 4.2 的關鍵部分
        """
        if not self.is_fitted:
            raise ValueError("Preprocessor must be fitted")
            
        # 取樣測試數據
        test_df = df.head(n_samples + self.config.sequence_length)
        
        # 提取原始特徵
        raw_features = self._extract_features(test_df)
        
        # 標準化特徵
        normalized_features = self._normalize_features(raw_features)
        
        # 創建序列
        sequences = self._create_sequences(normalized_features)
        
        # 保存為二進制格式（便於精確比較）
        test_data = {
            'raw_features': raw_features.astype(np.float64),
            'normalized_features': normalized_features.astype(np.float64),
            'sequences': sequences.astype(np.float64),
            'config': {
                'lob_levels': self.config.lob_levels,
                'sequence_length': self.config.sequence_length,
                'feature_dim': raw_features.shape[1]
            }
        }
        
        np.savez_compressed(output_path, **test_data)
        logger.info(f"Exported test data to {output_path}")
        
        return output_path