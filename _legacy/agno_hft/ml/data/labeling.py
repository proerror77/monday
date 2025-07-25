"""
TLOB 標籤生成器 (任務 1.2)
=======================

根據 TLOB 計劃實現標籤生成邏輯：
1. 定義預測窗口 k 和波動閾值 α
2. 計算未來時間窗口內的價格變動
3. 生成三分類標籤：上漲(2)、下跌(0)、不變(1)
"""

import pandas as pd
import numpy as np
from typing import Tuple, Optional
from dataclasses import dataclass
import logging

logger = logging.getLogger(__name__)


@dataclass
class LabelConfig:
    """標籤生成配置"""
    prediction_window: int = 10  # 預測窗口 k (ticks)
    volatility_threshold: float = 0.0001  # 波動閾值 α (0.01%)
    price_column: str = 'mid_price'  # 用於計算的價格列
    timestamp_column: str = 'timestamp'  # 時間戳列


class LabelGenerator:
    """
    TLOB 標籤生成器
    
    按照計劃規格實現：
    - 對於 t 時刻的 LOB，計算 t 到 t+k 時間窗口內的未來中間價
    - 與 t 時刻的中間價比較生成標籤
    - 標籤映射：0=下跌, 1=不變, 2=上漲
    """
    
    def __init__(self, config: Optional[LabelConfig] = None):
        self.config = config or LabelConfig()
        
    def generate_labels(self, df: pd.DataFrame) -> pd.DataFrame:
        """
        為 LOB 數據生成標籤
        
        Args:
            df: 包含 LOB 數據的 DataFrame，必須包含時間戳和價格列
            
        Returns:
            添加了 'label' 列的 DataFrame
        """
        # 驗證輸入數據
        self._validate_input(df)
        
        # 複製數據避免修改原始數據
        labeled_df = df.copy()
        
        # 計算中間價（如果不存在）
        if self.config.price_column not in labeled_df.columns:
            labeled_df = self._calculate_mid_price(labeled_df)
            
        # 生成標籤
        labels = self._compute_labels(labeled_df)
        labeled_df['label'] = labels
        
        # 移除無法標記的樣本（數據末尾）
        labeled_df = labeled_df.dropna(subset=['label'])
        
        logger.info(f"Generated labels for {len(labeled_df)} samples")
        logger.info(f"Label distribution: {labeled_df['label'].value_counts().to_dict()}")
        
        return labeled_df
        
    def _validate_input(self, df: pd.DataFrame):
        """驗證輸入數據格式"""
        required_columns = [self.config.timestamp_column]
        
        # 檢查是否存在中間價或可以計算中間價的列
        if self.config.price_column not in df.columns:
            if not all(col in df.columns for col in ['bid_price_1', 'ask_price_1']):
                raise ValueError(
                    f"Either {self.config.price_column} or both bid_price_1/ask_price_1 must be present"
                )
                
        missing_columns = [col for col in required_columns if col not in df.columns]
        if missing_columns:
            raise ValueError(f"Missing required columns: {missing_columns}")
            
        if len(df) < self.config.prediction_window + 1:
            raise ValueError(
                f"DataFrame too short ({len(df)} rows). "
                f"Need at least {self.config.prediction_window + 1} rows."
            )
            
    def _calculate_mid_price(self, df: pd.DataFrame) -> pd.DataFrame:
        """計算中間價"""
        if 'bid_price_1' in df.columns and 'ask_price_1' in df.columns:
            df[self.config.price_column] = (df['bid_price_1'] + df['ask_price_1']) / 2
        else:
            raise ValueError("Cannot calculate mid_price: missing bid_price_1 or ask_price_1")
        return df
        
    def _compute_labels(self, df: pd.DataFrame) -> np.ndarray:
        """
        計算標籤
        
        實現 TLOB 計劃中的標籤生成邏輯：
        - 若 (m_future - m_t) / m_t > α，標籤為 2 (上漲)
        - 若 (m_t - m_future) / m_t > α，標籤為 0 (下跌)  
        - 否則，標籤為 1 (不變)
        """
        prices = df[self.config.price_column].values
        n_samples = len(prices)
        labels = np.full(n_samples, np.nan)
        
        # 計算每個時間點的標籤
        for i in range(n_samples - self.config.prediction_window):
            current_price = prices[i]  # m_t
            
            # 計算預測窗口內的未來價格
            future_window = prices[i + 1:i + 1 + self.config.prediction_window]
            
            if len(future_window) == 0:
                continue
                
            # 使用預測窗口內的最後一個價格作為未來價格
            future_price = future_window[-1]  # m_future
            
            # 計算相對變化
            if current_price > 0:  # 避免除零
                relative_change = (future_price - current_price) / current_price
                
                if relative_change > self.config.volatility_threshold:
                    labels[i] = 2  # 上漲
                elif relative_change < -self.config.volatility_threshold:
                    labels[i] = 0  # 下跌
                else:
                    labels[i] = 1  # 不變
                    
        return labels
        
    def analyze_label_distribution(self, df: pd.DataFrame) -> dict:
        """分析標籤分布"""
        if 'label' not in df.columns:
            raise ValueError("DataFrame must contain 'label' column")
            
        label_counts = df['label'].value_counts().sort_index()
        total_samples = len(df)
        
        analysis = {
            'total_samples': total_samples,
            'label_counts': label_counts.to_dict(),
            'label_percentages': (label_counts / total_samples * 100).round(2).to_dict(),
            'class_names': {0: 'down', 1: 'stable', 2: 'up'},
            'is_balanced': self._check_balance(label_counts),
            'config': {
                'prediction_window': self.config.prediction_window,
                'volatility_threshold': self.config.volatility_threshold
            }
        }
        
        return analysis
        
    def _check_balance(self, label_counts: pd.Series) -> bool:
        """檢查類別是否平衡"""
        if len(label_counts) < 2:
            return False
            
        max_count = label_counts.max()
        min_count = label_counts.min()
        
        # 如果最大類別的樣本數不超過最小類別的3倍，認為是相對平衡的
        return max_count / min_count <= 3.0
        
    def create_balanced_dataset(
        self, 
        df: pd.DataFrame, 
        method: str = 'undersample'
    ) -> pd.DataFrame:
        """
        創建平衡的數據集
        
        Args:
            df: 包含標籤的數據
            method: 平衡方法 ('undersample', 'oversample')
            
        Returns:
            平衡後的數據集
        """
        if 'label' not in df.columns:
            raise ValueError("DataFrame must contain 'label' column")
            
        # 統計各類別樣本數
        label_counts = df['label'].value_counts()
        
        if method == 'undersample':
            # 下採樣到最小類別的樣本數
            min_samples = label_counts.min()
            balanced_dfs = []
            
            for label_value in label_counts.index:
                label_data = df[df['label'] == label_value]
                sampled_data = label_data.sample(n=min_samples, random_state=42)
                balanced_dfs.append(sampled_data)
                
            balanced_df = pd.concat(balanced_dfs, ignore_index=True)
            
        elif method == 'oversample':
            # 上採樣到最大類別的樣本數
            max_samples = label_counts.max()
            balanced_dfs = []
            
            for label_value in label_counts.index:
                label_data = df[df['label'] == label_value]
                current_samples = len(label_data)
                
                if current_samples < max_samples:
                    # 重複採樣
                    oversample_ratio = max_samples // current_samples
                    remainder = max_samples % current_samples
                    
                    oversampled_data = pd.concat([label_data] * oversample_ratio, ignore_index=True)
                    if remainder > 0:
                        extra_samples = label_data.sample(n=remainder, random_state=42)
                        oversampled_data = pd.concat([oversampled_data, extra_samples], ignore_index=True)
                    
                    balanced_dfs.append(oversampled_data)
                else:
                    balanced_dfs.append(label_data)
                    
            balanced_df = pd.concat(balanced_dfs, ignore_index=True)
            
        else:
            raise ValueError(f"Unknown balancing method: {method}")
            
        # 打亂順序
        balanced_df = balanced_df.sample(frac=1, random_state=42).reset_index(drop=True)
        
        logger.info(f"Created balanced dataset using {method}")
        logger.info(f"New label distribution: {balanced_df['label'].value_counts().to_dict()}")
        
        return balanced_df
        
    def save_labeling_config(self, filepath: str):
        """保存標籤生成配置"""
        import json
        
        config_dict = {
            'prediction_window': self.config.prediction_window,
            'volatility_threshold': self.config.volatility_threshold,
            'price_column': self.config.price_column,
            'timestamp_column': self.config.timestamp_column
        }
        
        with open(filepath, 'w') as f:
            json.dump(config_dict, f, indent=2)
            
        logger.info(f"Saved labeling config to {filepath}")
        
    @classmethod
    def load_labeling_config(cls, filepath: str) -> 'LabelGenerator':
        """從文件加載標籤生成配置"""
        import json
        
        with open(filepath, 'r') as f:
            config_dict = json.load(f)
            
        config = LabelConfig(**config_dict)
        return cls(config)