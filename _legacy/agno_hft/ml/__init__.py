"""
Python ML 訓練層
===============

完整的深度學習和強化學習訓練基礎設施，符合 TLOB 計劃和三層架構設計。

核心功能：
- 數據處理：ClickHouse 加載、標籤生成、預處理
- 模型架構：TLOB Transformer、PPO Agent、通用 Transformer
- 訓練組件：統一訓練器、數據集、評估器
- 模型導出：TorchScript 導出，支持 Rust 集成

模塊結構：
- data/: 數據處理管道（加載、標籤、預處理）
- models/: 深度學習模型（TLOB、Transformer、PPO）
- utils/: 工具函數（特徵工程等）
- train.py: 主訓練腳本
- evaluate.py: 模型評估腳本
- trainer.py: 可重用訓練組件
- dataset.py: 數據集實現
- export.py: 模型導出工具
"""

# 數據處理
from .data import DataLoader, LabelGenerator, LOBPreprocessor

# 模型
from .models import TLOB, TLOBConfig, LOBTransformer, PPOAgent

# 訓練和評估
from .trainer import BaseTrainer, TLOBTrainerImplementation, create_optimizer, create_scheduler
from .dataset import TLOBDataset, TLOBDataModule, create_tlob_datasets
from .export import ModelExporter

# 工具
from .utils.features import FeatureExtractor

__version__ = "1.0.0"

__all__ = [
    # 數據處理
    'DataLoader',
    'LabelGenerator', 
    'LOBPreprocessor',
    
    # 模型
    'TLOB',
    'TLOBConfig',
    'LOBTransformer',
    'PPOAgent',
    
    # 訓練
    'BaseTrainer',
    'TLOBTrainerImplementation',
    'create_optimizer',
    'create_scheduler',
    
    # 數據集
    'TLOBDataset',
    'TLOBDataModule',
    'create_tlob_datasets',
    
    # 導出
    'ModelExporter',
    
    # 工具
    'FeatureExtractor'
]