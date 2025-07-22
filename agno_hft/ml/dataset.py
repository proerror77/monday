"""
數據集類
=======

提供各種 ML 任務的數據集實現：
- TLOB 分類數據集
- PPO 強化學習數據集
- 時間序列預測數據集
- 數據增強和轉換
"""

import logging
from typing import Dict, List, Tuple, Optional, Union, Callable
from pathlib import Path
import pickle

import torch
from torch.utils.data import Dataset, DataLoader, Sampler
import numpy as np
import pandas as pd
from sklearn.preprocessing import StandardScaler, MinMaxScaler

# 本地導入
from data.data_loader import DataLoader as HFTDataLoader
from data.labeling import LabelGenerator
from data.preprocessing import LOBPreprocessor

logger = logging.getLogger(__name__)


class TLOBDataset(Dataset):
    """
    TLOB 分類任務數據集
    
    支持特徵：
    - 序列化的 LOB 數據
    - 多種標籤生成策略
    - 數據增強
    - 記憶體映射（大數據集）
    """
    
    def __init__(
        self,
        sequences: np.ndarray,
        labels: np.ndarray,
        transforms: Optional[Callable] = None,
        return_indices: bool = False
    ):
        """
        Args:
            sequences: 序列數據 [n_samples, seq_len, features]
            labels: 標籤 [n_samples]
            transforms: 數據變換函數
            return_indices: 是否返回索引
        """
        self.sequences = torch.FloatTensor(sequences)
        self.labels = torch.LongTensor(labels)
        self.transforms = transforms
        self.return_indices = return_indices
        
        # 驗證數據
        assert len(self.sequences) == len(self.labels), "序列和標籤長度不匹配"
        
        logger.info(f"Dataset created with {len(self)} samples")
        logger.info(f"Sequence shape: {self.sequences.shape}")
        logger.info(f"Label distribution: {torch.bincount(self.labels)}")
        
    def __len__(self) -> int:
        return len(self.sequences)
        
    def __getitem__(self, idx: int) -> Union[Tuple[torch.Tensor, torch.Tensor], 
                                           Tuple[torch.Tensor, torch.Tensor, int]]:
        sequence = self.sequences[idx]
        label = self.labels[idx]
        
        # 應用變換
        if self.transforms:
            sequence = self.transforms(sequence)
            
        if self.return_indices:
            return sequence, label, idx
        else:
            return sequence, label
            
    def get_class_weights(self) -> torch.Tensor:
        """計算類別權重（用於不平衡數據）"""
        class_counts = torch.bincount(self.labels)
        total_samples = len(self.labels)
        class_weights = total_samples / (len(class_counts) * class_counts.float())
        return class_weights
        
    def get_sample_weights(self) -> torch.Tensor:
        """計算樣本權重"""
        class_weights = self.get_class_weights()
        sample_weights = class_weights[self.labels]
        return sample_weights
        
    @classmethod
    def from_raw_data(
        cls,
        data: pd.DataFrame,
        preprocessor: LOBPreprocessor,
        label_generator: LabelGenerator,
        **kwargs
    ) -> 'TLOBDataset':
        """從原始數據創建數據集"""
        # 生成標籤
        labeled_data = label_generator.generate_labels(data)
        
        # 預處理
        sequences = preprocessor.transform(labeled_data)
        
        # 提取標籤
        seq_len = preprocessor.config.sequence_length
        labels = labeled_data['label'].iloc[seq_len-1:].values
        
        return cls(sequences, labels, **kwargs)
        
    def save(self, filepath: str):
        """保存數據集"""
        data = {
            'sequences': self.sequences.numpy(),
            'labels': self.labels.numpy(),
        }
        
        Path(filepath).parent.mkdir(parents=True, exist_ok=True)
        
        if filepath.endswith('.npz'):
            np.savez_compressed(filepath, **data)
        else:
            with open(filepath, 'wb') as f:
                pickle.dump(data, f)
                
        logger.info(f"Dataset saved to {filepath}")
        
    @classmethod
    def load(cls, filepath: str, **kwargs) -> 'TLOBDataset':
        """加載數據集"""
        if filepath.endswith('.npz'):
            data = np.load(filepath)
            sequences = data['sequences']
            labels = data['labels']
        else:
            with open(filepath, 'rb') as f:
                data = pickle.load(f)
            sequences = data['sequences']
            labels = data['labels']
            
        return cls(sequences, labels, **kwargs)


class TimeSeriesDataset(Dataset):
    """時間序列預測數據集"""
    
    def __init__(
        self,
        data: np.ndarray,
        sequence_length: int,
        prediction_horizon: int = 1,
        stride: int = 1,
        target_columns: Optional[List[int]] = None
    ):
        """
        Args:
            data: 時間序列數據 [timesteps, features]
            sequence_length: 輸入序列長度
            prediction_horizon: 預測時間範圍
            stride: 滑動步長
            target_columns: 目標列索引
        """
        self.data = torch.FloatTensor(data)
        self.sequence_length = sequence_length
        self.prediction_horizon = prediction_horizon
        self.stride = stride
        self.target_columns = target_columns or list(range(data.shape[1]))
        
        # 計算有效樣本數
        self.valid_length = (len(data) - sequence_length - prediction_horizon + 1)
        self.indices = list(range(0, self.valid_length, stride))
        
    def __len__(self) -> int:
        return len(self.indices)
        
    def __getitem__(self, idx: int) -> Tuple[torch.Tensor, torch.Tensor]:
        start_idx = self.indices[idx]
        end_idx = start_idx + self.sequence_length
        target_idx = end_idx + self.prediction_horizon - 1
        
        # 輸入序列
        sequence = self.data[start_idx:end_idx]
        
        # 目標值
        target = self.data[target_idx, self.target_columns]
        
        return sequence, target


class RLTrajectoryDataset(Dataset):
    """強化學習軌跡數據集"""
    
    def __init__(
        self,
        observations: np.ndarray,
        actions: np.ndarray,
        rewards: np.ndarray,
        dones: np.ndarray,
        values: Optional[np.ndarray] = None,
        advantages: Optional[np.ndarray] = None
    ):
        """
        Args:
            observations: 觀察值
            actions: 動作
            rewards: 獎勵
            dones: 結束標誌
            values: 價值估計
            advantages: 優勢值
        """
        self.observations = torch.FloatTensor(observations)
        self.actions = torch.LongTensor(actions)
        self.rewards = torch.FloatTensor(rewards)
        self.dones = torch.BoolTensor(dones)
        
        if values is not None:
            self.values = torch.FloatTensor(values)
        else:
            self.values = None
            
        if advantages is not None:
            self.advantages = torch.FloatTensor(advantages)
        else:
            self.advantages = None
            
    def __len__(self) -> int:
        return len(self.observations)
        
    def __getitem__(self, idx: int) -> Dict[str, torch.Tensor]:
        item = {
            'observations': self.observations[idx],
            'actions': self.actions[idx],
            'rewards': self.rewards[idx],
            'dones': self.dones[idx]
        }
        
        if self.values is not None:
            item['values'] = self.values[idx]
            
        if self.advantages is not None:
            item['advantages'] = self.advantages[idx]
            
        return item


class BalancedBatchSampler(Sampler):
    """平衡批次採樣器"""
    
    def __init__(
        self,
        labels: torch.Tensor,
        batch_size: int,
        samples_per_class: Optional[int] = None
    ):
        """
        Args:
            labels: 樣本標籤
            batch_size: 批次大小
            samples_per_class: 每類樣本數（None 表示自動計算）
        """
        self.labels = labels
        self.batch_size = batch_size
        
        # 獲取每個類別的索引
        self.class_indices = {}
        unique_labels = torch.unique(labels)
        
        for label in unique_labels:
            self.class_indices[label.item()] = torch.where(labels == label)[0]
            
        self.num_classes = len(unique_labels)
        
        # 計算每類樣本數
        if samples_per_class is None:
            self.samples_per_class = batch_size // self.num_classes
        else:
            self.samples_per_class = samples_per_class
            
        # 計算 epoch 中的批次數
        min_class_size = min(len(indices) for indices in self.class_indices.values())
        self.num_batches = min_class_size // self.samples_per_class
        
    def __iter__(self):
        for _ in range(self.num_batches):
            batch_indices = []
            
            for class_label, indices in self.class_indices.items():
                # 隨機採樣
                sampled_indices = torch.randperm(len(indices))[:self.samples_per_class]
                batch_indices.extend(indices[sampled_indices].tolist())
                
            # 打亂批次內順序
            batch_indices = torch.tensor(batch_indices)[torch.randperm(len(batch_indices))]
            yield batch_indices.tolist()
            
    def __len__(self) -> int:
        return self.num_batches


class DataAugmentation:
    """數據增強類"""
    
    @staticmethod
    def add_noise(sequence: torch.Tensor, noise_std: float = 0.01) -> torch.Tensor:
        """添加高斯噪聲"""
        noise = torch.randn_like(sequence) * noise_std
        return sequence + noise
        
    @staticmethod
    def time_shift(sequence: torch.Tensor, max_shift: int = 5) -> torch.Tensor:
        """時間偏移"""
        shift = torch.randint(-max_shift, max_shift + 1, (1,)).item()
        if shift == 0:
            return sequence
            
        if shift > 0:
            # 向右偏移
            shifted = torch.cat([sequence[shift:], sequence[-shift:]], dim=0)
        else:
            # 向左偏移
            shifted = torch.cat([sequence[:-shift], sequence[:-shift]], dim=0)
            
        return shifted
        
    @staticmethod
    def feature_dropout(sequence: torch.Tensor, dropout_prob: float = 0.1) -> torch.Tensor:
        """特徵dropout"""
        mask = torch.rand(sequence.shape[-1]) > dropout_prob
        return sequence * mask
        
    @staticmethod
    def magnitude_scaling(sequence: torch.Tensor, scale_range: Tuple[float, float] = (0.8, 1.2)) -> torch.Tensor:
        """幅度縮放"""
        scale = torch.uniform(*scale_range, (1,)).item()
        return sequence * scale


class TLOBDataModule:
    """TLOB 數據模塊（類似 PyTorch Lightning DataModule）"""
    
    def __init__(
        self,
        data_config: Dict,
        batch_size: int = 64,
        num_workers: int = 4,
        train_transforms: Optional[Callable] = None,
        val_transforms: Optional[Callable] = None
    ):
        self.data_config = data_config
        self.batch_size = batch_size
        self.num_workers = num_workers
        self.train_transforms = train_transforms
        self.val_transforms = val_transforms
        
        # 數據集
        self.train_dataset = None
        self.val_dataset = None
        self.test_dataset = None
        self.preprocessor = None
        
    def prepare_data(self):
        """準備數據（下載、預處理等）"""
        logger.info("Preparing data...")
        
        # 加載原始數據
        data_loader = HFTDataLoader()
        raw_data = data_loader.load_orderbook_data(
            symbol=self.data_config['symbol'],
            start_time=self.data_config['start_time'],
            end_time=self.data_config['end_time']
        )
        
        # 生成標籤
        label_generator = LabelGenerator()
        self.labeled_data = label_generator.generate_labels(raw_data)
        
        logger.info(f"Prepared {len(self.labeled_data)} samples")
        
    def setup(self, stage: Optional[str] = None):
        """設置數據集"""
        if self.labeled_data is None:
            self.prepare_data()
            
        # 分割數據
        train_size = int(len(self.labeled_data) * self.data_config.get('train_ratio', 0.7))
        val_size = int(len(self.labeled_data) * self.data_config.get('val_ratio', 0.15))
        
        train_data = self.labeled_data[:train_size]
        val_data = self.labeled_data[train_size:train_size + val_size]
        test_data = self.labeled_data[train_size + val_size:]
        
        # 創建預處理器
        self.preprocessor = LOBPreprocessor()
        self.preprocessor.fit(train_data)
        
        # 創建數據集
        if stage == 'fit' or stage is None:
            self.train_dataset = TLOBDataset.from_raw_data(
                train_data, self.preprocessor, LabelGenerator(),
                transforms=self.train_transforms
            )
            
            self.val_dataset = TLOBDataset.from_raw_data(
                val_data, self.preprocessor, LabelGenerator(),
                transforms=self.val_transforms
            )
            
        if stage == 'test' or stage is None:
            self.test_dataset = TLOBDataset.from_raw_data(
                test_data, self.preprocessor, LabelGenerator(),
                transforms=self.val_transforms
            )
            
    def train_dataloader(self) -> DataLoader:
        """訓練數據加載器"""
        return DataLoader(
            self.train_dataset,
            batch_size=self.batch_size,
            shuffle=True,
            num_workers=self.num_workers,
            pin_memory=True
        )
        
    def val_dataloader(self) -> DataLoader:
        """驗證數據加載器"""
        return DataLoader(
            self.val_dataset,
            batch_size=self.batch_size,
            shuffle=False,
            num_workers=self.num_workers,
            pin_memory=True
        )
        
    def test_dataloader(self) -> DataLoader:
        """測試數據加載器"""
        return DataLoader(
            self.test_dataset,
            batch_size=self.batch_size,
            shuffle=False,
            num_workers=self.num_workers,
            pin_memory=True
        )
        
    def predict_dataloader(self) -> DataLoader:
        """預測數據加載器"""
        return self.test_dataloader()


# 便捷函數
def create_tlob_datasets(
    data_config: Dict,
    train_ratio: float = 0.7,
    val_ratio: float = 0.15,
    augmentation: bool = False
) -> Tuple[TLOBDataset, TLOBDataset, TLOBDataset]:
    """創建 TLOB 訓練、驗證、測試數據集"""
    
    # 準備數據變換
    train_transforms = None
    if augmentation:
        def train_transform(x):
            # 隨機應用增強
            if torch.rand(1) < 0.3:
                x = DataAugmentation.add_noise(x, 0.01)
            if torch.rand(1) < 0.2:
                x = DataAugmentation.feature_dropout(x, 0.1)
            return x
        train_transforms = train_transform
        
    # 創建數據模塊
    data_module = TLOBDataModule(
        data_config,
        train_transforms=train_transforms
    )
    
    data_module.setup()
    
    return data_module.train_dataset, data_module.val_dataset, data_module.test_dataset


def create_balanced_dataloader(
    dataset: TLOBDataset,
    batch_size: int,
    **kwargs
) -> DataLoader:
    """創建平衡數據加載器"""
    sampler = BalancedBatchSampler(
        dataset.labels,
        batch_size
    )
    
    return DataLoader(
        dataset,
        batch_sampler=sampler,
        **kwargs
    )