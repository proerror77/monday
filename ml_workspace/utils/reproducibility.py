#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
可重现性工具 - 设置随机种子以确保实验结果的可重现性
"""

import os
import random
from typing import Optional, Dict, Any

import numpy as np
import torch


def set_global_seed(seed: Optional[int] = None) -> int:
    """设置全局随机种子以确保可重现性
    
    Args:
        seed: 随机种子值，如果为None则使用默认值42
    
    Returns:
        实际使用的种子值
    """
    if seed is None:
        seed = 42
    
    # Python标准库
    random.seed(seed)
    os.environ['PYTHONHASHSEED'] = str(seed)
    
    # NumPy
    np.random.seed(seed)
    
    # PyTorch
    torch.manual_seed(seed)
    torch.cuda.manual_seed(seed)
    torch.cuda.manual_seed_all(seed)
    
    # 确保 CUDA 操作的确定性
    if torch.cuda.is_available():
        torch.backends.cudnn.deterministic = True
        torch.backends.cudnn.benchmark = False
    
    # MPS (Apple Silicon) - 检查API是否存在
    if (hasattr(torch.backends, 'mps') and torch.backends.mps.is_available() and 
        hasattr(torch, 'mps') and hasattr(torch.mps, 'manual_seed')):
        torch.mps.manual_seed(seed)
    
    return seed


def get_reproducibility_config(seed: Optional[int] = None) -> Dict[str, Any]:
    """获取可重现性配置信息
    
    Args:
        seed: 随机种子值
    
    Returns:
        包含种子和环境信息的配置字典
    """
    actual_seed = set_global_seed(seed)
    
    config = {
        'seed': actual_seed,
        'torch_version': torch.__version__,
        'numpy_version': np.__version__,
        'cuda_available': torch.cuda.is_available(),
        'cuda_deterministic': torch.backends.cudnn.deterministic if torch.cuda.is_available() else None,
        'cuda_benchmark': torch.backends.cudnn.benchmark if torch.cuda.is_available() else None,
    }
    
    if torch.cuda.is_available():
        config['cuda_version'] = torch.version.cuda
        config['cudnn_version'] = torch.backends.cudnn.version()
    
    if (hasattr(torch.backends, 'mps') and torch.backends.mps.is_available() and 
        hasattr(torch, 'mps') and hasattr(torch.mps, 'manual_seed')):
        config['mps_available'] = True
    
    return config


def log_reproducibility_info(seed: Optional[int] = None) -> None:
    """记录可重现性信息到日志"""
    import logging
    
    logger = logging.getLogger(__name__)
    config = get_reproducibility_config(seed)
    
    logger.info("=== Reproducibility Configuration ===")
    logger.info(f"Global seed: {config['seed']}")
    logger.info(f"PyTorch version: {config['torch_version']}")
    logger.info(f"NumPy version: {config['numpy_version']}")
    logger.info(f"CUDA available: {config['cuda_available']}")
    
    if config['cuda_available']:
        logger.info(f"CUDA version: {config.get('cuda_version', 'unknown')}")
        logger.info(f"cuDNN version: {config.get('cudnn_version', 'unknown')}")
        logger.info(f"CUDA deterministic: {config['cuda_deterministic']}")
        logger.info(f"CUDA benchmark: {config['cuda_benchmark']}")
    
    if config.get('mps_available'):
        logger.info("MPS (Apple Silicon) available and seeded")
    
    logger.info("=====================================")


class ReproducibleRandom:
    """可重现的随机数生成器上下文管理器"""
    
    def __init__(self, seed: int):
        self.seed = seed
        self.original_states = {}
    
    def __enter__(self):
        # 保存当前状态
        self.original_states = {
            'random': random.getstate(),
            'numpy': np.random.get_state(),
            'torch': torch.get_rng_state(),
        }
        
        if torch.cuda.is_available():
            self.original_states['torch_cuda'] = torch.cuda.get_rng_state()
        
        # 设置新的种子
        set_global_seed(self.seed)
        return self
    
    def __exit__(self, exc_type, exc_val, exc_tb):
        # 恢复原始状态
        random.setstate(self.original_states['random'])
        np.random.set_state(self.original_states['numpy'])
        torch.set_rng_state(self.original_states['torch'])
        
        if 'torch_cuda' in self.original_states:
            torch.cuda.set_rng_state(self.original_states['torch_cuda'])


def ensure_reproducible_dataloader(dataloader, worker_init_fn=None):
    """确保DataLoader的可重现性"""
    def seed_worker(worker_id):
        worker_seed = torch.initial_seed() % 2**32
        np.random.seed(worker_seed)
        random.seed(worker_seed)
        if worker_init_fn is not None:
            worker_init_fn(worker_id)
    
    # 更新DataLoader的worker初始化函数
    dataloader.worker_init_fn = seed_worker
    return dataloader


# 示例用法
if __name__ == "__main__":
    # 设置全局种子
    seed = set_global_seed(42)
    print(f"Global seed set to: {seed}")
    
    # 记录信息
    log_reproducibility_info()
    
    # 使用上下文管理器
    with ReproducibleRandom(123) as rng:
        print("在可重现随机上下文中:")
        print(f"Random: {random.random()}")
        print(f"NumPy: {np.random.random()}")
        print(f"PyTorch: {torch.rand(1).item()}")
    
    print("恢复到原始随机状态")