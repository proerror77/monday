"""
ML 訓練組件
==========

提供可重用的訓練基礎設施：
- 統一的訓練器基類
- 自動化超參數調優
- 模型檢查點管理
- 分佈式訓練支持
- 實驗跟踪集成
"""

import logging
import time
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Dict, Any, Optional, Tuple, List, Callable
import json
import warnings

import torch
import torch.nn as nn
import torch.optim as optim
from torch.utils.data import DataLoader
from torch.utils.tensorboard import SummaryWriter
import numpy as np
from tqdm import tqdm

# 可選依賴
try:
    import wandb
    WANDB_AVAILABLE = True
except ImportError:
    WANDB_AVAILABLE = False
    warnings.warn("wandb not available. Install with: pip install wandb")

try:
    from ray import tune
    from ray.tune.schedulers import ASHAScheduler
    RAY_AVAILABLE = True
except ImportError:
    RAY_AVAILABLE = False
    warnings.warn("Ray Tune not available. Install with: pip install ray[tune]")

logger = logging.getLogger(__name__)


class EarlyStopping:
    """早停機制"""
    
    def __init__(self, patience: int = 7, min_delta: float = 0.0, mode: str = 'min'):
        self.patience = patience
        self.min_delta = min_delta
        self.mode = mode
        self.counter = 0
        self.best_score = None
        self.early_stop = False
        
    def __call__(self, score: float) -> bool:
        if self.best_score is None:
            self.best_score = score
        elif self._is_better(score):
            self.best_score = score
            self.counter = 0
        else:
            self.counter += 1
            if self.counter >= self.patience:
                self.early_stop = True
                
        return self.early_stop
        
    def _is_better(self, score: float) -> bool:
        if self.mode == 'min':
            return score < self.best_score - self.min_delta
        else:
            return score > self.best_score + self.min_delta


class MetricsTracker:
    """指標跟踪器"""
    
    def __init__(self):
        self.metrics = {}
        self.history = {}
        
    def update(self, metrics: Dict[str, float], step: Optional[int] = None):
        """更新指標"""
        for key, value in metrics.items():
            if key not in self.history:
                self.history[key] = []
            self.history[key].append(value)
            self.metrics[key] = value
            
    def get_average(self, key: str, last_n: Optional[int] = None) -> float:
        """獲取平均值"""
        if key not in self.history:
            return 0.0
        values = self.history[key]
        if last_n:
            values = values[-last_n:]
        return np.mean(values)
        
    def get_best(self, key: str, mode: str = 'min') -> float:
        """獲取最佳值"""
        if key not in self.history:
            return float('inf') if mode == 'min' else float('-inf')
        values = self.history[key]
        return min(values) if mode == 'min' else max(values)


class BaseTrainer(ABC):
    """訓練器基類"""
    
    def __init__(
        self,
        model: nn.Module,
        train_loader: DataLoader,
        val_loader: Optional[DataLoader] = None,
        optimizer: Optional[optim.Optimizer] = None,
        scheduler: Optional[optim.lr_scheduler._LRScheduler] = None,
        criterion: Optional[nn.Module] = None,
        device: Optional[torch.device] = None,
        output_dir: str = './outputs',
        config: Optional[Dict] = None
    ):
        self.model = model
        self.train_loader = train_loader
        self.val_loader = val_loader
        self.optimizer = optimizer
        self.scheduler = scheduler
        self.criterion = criterion
        self.device = device or torch.device('cuda' if torch.cuda.is_available() else 'cpu')
        self.output_dir = Path(output_dir)
        self.config = config or {}
        
        # 創建輸出目錄
        self.output_dir.mkdir(parents=True, exist_ok=True)
        
        # 移動模型到設備
        self.model.to(self.device)
        
        # 初始化跟踪器
        self.metrics_tracker = MetricsTracker()
        self.early_stopping = None
        
        # TensorBoard
        if self.config.get('use_tensorboard', True):
            self.writer = SummaryWriter(self.output_dir / 'tensorboard')
        else:
            self.writer = None
            
        # Wandb
        if self.config.get('use_wandb', False) and WANDB_AVAILABLE:
            wandb.init(
                project=self.config.get('wandb_project', 'ml-training'),
                config=self.config,
                dir=str(self.output_dir)
            )
            self.use_wandb = True
        else:
            self.use_wandb = False
            
        # 訓練狀態
        self.current_epoch = 0
        self.global_step = 0
        self.best_metric = float('inf')
        self.best_model_path = None
        
    @abstractmethod
    def train_step(self, batch: Any) -> Dict[str, float]:
        """單步訓練（子類實現）"""
        pass
        
    @abstractmethod
    def validation_step(self, batch: Any) -> Dict[str, float]:
        """單步驗證（子類實現）"""
        pass
        
    def train_epoch(self) -> Dict[str, float]:
        """訓練一個 epoch"""
        self.model.train()
        epoch_metrics = {}
        
        pbar = tqdm(self.train_loader, desc=f'Epoch {self.current_epoch + 1}')
        
        for batch_idx, batch in enumerate(pbar):
            # 訓練步驟
            step_metrics = self.train_step(batch)
            
            # 更新全局步數
            self.global_step += 1
            
            # 累積指標
            for key, value in step_metrics.items():
                if key not in epoch_metrics:
                    epoch_metrics[key] = []
                epoch_metrics[key].append(value)
                
            # 更新進度條
            current_metrics = {k: np.mean(v) for k, v in epoch_metrics.items()}
            pbar.set_postfix(current_metrics)
            
            # 記錄到 TensorBoard
            if self.writer and batch_idx % self.config.get('log_interval', 100) == 0:
                for key, value in step_metrics.items():
                    self.writer.add_scalar(f'Train/Batch_{key}', value, self.global_step)
                    
            # 記錄到 Wandb
            if self.use_wandb:
                wandb.log({f'train/batch_{k}': v for k, v in step_metrics.items()}, 
                         step=self.global_step)
                
        # 計算 epoch 平均指標
        avg_metrics = {key: np.mean(values) for key, values in epoch_metrics.items()}
        
        return avg_metrics
        
    def validate_epoch(self) -> Dict[str, float]:
        """驗證一個 epoch"""
        if self.val_loader is None:
            return {}
            
        self.model.eval()
        epoch_metrics = {}
        
        with torch.no_grad():
            for batch in tqdm(self.val_loader, desc='Validation'):
                step_metrics = self.validation_step(batch)
                
                for key, value in step_metrics.items():
                    if key not in epoch_metrics:
                        epoch_metrics[key] = []
                    epoch_metrics[key].append(value)
                    
        # 計算平均指標
        avg_metrics = {key: np.mean(values) for key, values in epoch_metrics.items()}
        
        return avg_metrics
        
    def save_checkpoint(
        self, 
        epoch: int, 
        metrics: Dict[str, float], 
        is_best: bool = False,
        save_optimizer: bool = True
    ):
        """保存檢查點"""
        checkpoint = {
            'epoch': epoch,
            'model_state_dict': self.model.state_dict(),
            'metrics': metrics,
            'config': self.config,
            'global_step': self.global_step
        }
        
        if save_optimizer and self.optimizer:
            checkpoint['optimizer_state_dict'] = self.optimizer.state_dict()
            
        if self.scheduler:
            checkpoint['scheduler_state_dict'] = self.scheduler.state_dict()
            
        # 保存最新檢查點
        latest_path = self.output_dir / 'latest_checkpoint.pt'
        torch.save(checkpoint, latest_path)
        
        # 保存最佳模型
        if is_best:
            best_path = self.output_dir / 'best_model.pt'
            torch.save(checkpoint, best_path)
            self.best_model_path = str(best_path)
            logger.info(f"New best model saved: {metrics}")
            
        # 定期保存 epoch 檢查點
        if self.config.get('save_epoch_checkpoints', False):
            epoch_path = self.output_dir / f'checkpoint_epoch_{epoch}.pt'
            torch.save(checkpoint, epoch_path)
            
    def load_checkpoint(self, checkpoint_path: str, load_optimizer: bool = True):
        """加載檢查點"""
        logger.info(f"Loading checkpoint from {checkpoint_path}")
        
        checkpoint = torch.load(checkpoint_path, map_location=self.device)
        
        self.model.load_state_dict(checkpoint['model_state_dict'])
        
        if load_optimizer and self.optimizer and 'optimizer_state_dict' in checkpoint:
            self.optimizer.load_state_dict(checkpoint['optimizer_state_dict'])
            
        if self.scheduler and 'scheduler_state_dict' in checkpoint:
            self.scheduler.load_state_dict(checkpoint['scheduler_state_dict'])
            
        self.current_epoch = checkpoint.get('epoch', 0)
        self.global_step = checkpoint.get('global_step', 0)
        
        logger.info(f"Resumed from epoch {self.current_epoch}")
        
    def train(
        self,
        epochs: int,
        early_stopping_patience: Optional[int] = None,
        early_stopping_metric: str = 'val_loss',
        early_stopping_mode: str = 'min'
    ) -> Dict[str, Any]:
        """完整訓練流程"""
        logger.info(f"Starting training for {epochs} epochs")
        
        # 設置早停
        if early_stopping_patience:
            self.early_stopping = EarlyStopping(
                patience=early_stopping_patience,
                mode=early_stopping_mode
            )
            
        start_time = time.time()
        
        for epoch in range(self.current_epoch, epochs):
            self.current_epoch = epoch
            
            # 訓練
            train_metrics = self.train_epoch()
            
            # 驗證
            val_metrics = self.validate_epoch()
            
            # 合併指標
            all_metrics = {
                **{f'train_{k}': v for k, v in train_metrics.items()},
                **{f'val_{k}': v for k, v in val_metrics.items()}
            }
            
            # 更新指標跟踪器
            self.metrics_tracker.update(all_metrics, epoch)
            
            # 學習率調度
            if self.scheduler:
                if hasattr(self.scheduler, 'step'):
                    if isinstance(self.scheduler, optim.lr_scheduler.ReduceLROnPlateau):
                        monitor_metric = val_metrics.get('loss', train_metrics.get('loss', 0))
                        self.scheduler.step(monitor_metric)
                    else:
                        self.scheduler.step()
                        
            # 記錄指標
            logger.info(f"Epoch {epoch + 1}/{epochs} - {all_metrics}")
            
            # TensorBoard 記錄
            if self.writer:
                for key, value in all_metrics.items():
                    self.writer.add_scalar(key.replace('_', '/'), value, epoch)
                    
                # 記錄學習率
                if self.optimizer:
                    current_lr = self.optimizer.param_groups[0]['lr']
                    self.writer.add_scalar('learning_rate', current_lr, epoch)
                    
            # Wandb 記錄
            if self.use_wandb:
                wandb.log(all_metrics, step=epoch)
                
            # 保存檢查點
            monitor_metric = val_metrics.get('loss', train_metrics.get('loss', float('inf')))
            is_best = monitor_metric < self.best_metric
            if is_best:
                self.best_metric = monitor_metric
                
            self.save_checkpoint(epoch, all_metrics, is_best)
            
            # 早停檢查
            if self.early_stopping:
                early_stop_metric = all_metrics.get(early_stopping_metric, float('inf'))
                if self.early_stopping(early_stop_metric):
                    logger.info(f"Early stopping triggered at epoch {epoch + 1}")
                    break
                    
        training_time = time.time() - start_time
        
        # 訓練總結
        final_results = {
            'training_time': training_time,
            'best_metric': self.best_metric,
            'final_epoch': self.current_epoch,
            'metrics_history': self.metrics_tracker.history
        }
        
        # 保存訓練結果
        results_path = self.output_dir / 'training_results.json'
        with open(results_path, 'w') as f:
            json.dump(final_results, f, indent=2, default=str)
            
        if self.writer:
            self.writer.close()
            
        if self.use_wandb:
            wandb.finish()
            
        logger.info(f"Training completed in {training_time:.2f} seconds")
        
        return final_results


class TLOBTrainerImplementation(BaseTrainer):
    """TLOB 模型專用訓練器實現"""
    
    def train_step(self, batch: Tuple[torch.Tensor, torch.Tensor]) -> Dict[str, float]:
        """TLOB 訓練步驟"""
        sequences, labels = batch
        sequences, labels = sequences.to(self.device), labels.to(self.device)
        
        # 前向傳播
        self.optimizer.zero_grad()
        outputs = self.model(sequences)
        loss = self.model.compute_loss(outputs['logits'], labels)
        
        # 反向傳播
        loss.backward()
        
        # 梯度裁剪
        if self.config.get('max_grad_norm'):
            torch.nn.utils.clip_grad_norm_(
                self.model.parameters(),
                self.config['max_grad_norm']
            )
            
        self.optimizer.step()
        
        # 計算指標
        predictions = outputs['predictions']
        accuracy = (predictions == labels).float().mean()
        
        return {
            'loss': loss.item(),
            'accuracy': accuracy.item()
        }
        
    def validation_step(self, batch: Tuple[torch.Tensor, torch.Tensor]) -> Dict[str, float]:
        """TLOB 驗證步驟"""
        sequences, labels = batch
        sequences, labels = sequences.to(self.device), labels.to(self.device)
        
        # 前向傳播
        outputs = self.model(sequences)
        loss = self.model.compute_loss(outputs['logits'], labels)
        
        # 計算指標
        predictions = outputs['predictions']
        accuracy = (predictions == labels).float().mean()
        
        return {
            'loss': loss.item(),
            'accuracy': accuracy.item()
        }


class HyperparameterTuner:
    """超參數調優器"""
    
    def __init__(self, trainer_class, base_config: Dict):
        self.trainer_class = trainer_class
        self.base_config = base_config
        
    def objective_function(self, config: Dict) -> float:
        """目標函數（Ray Tune 使用）"""
        # 合併配置
        full_config = {**self.base_config, **config}
        
        # 創建訓練器並訓練
        trainer = self.trainer_class(config=full_config)
        results = trainer.train(epochs=full_config.get('tune_epochs', 20))
        
        # 返回要優化的指標
        return results['best_metric']
        
    def tune_hyperparameters(
        self,
        search_space: Dict,
        num_samples: int = 10,
        max_epochs: int = 100
    ) -> Dict:
        """執行超參數調優"""
        if not RAY_AVAILABLE:
            raise ImportError("Ray Tune is required for hyperparameter tuning")
            
        # 配置調優
        scheduler = ASHAScheduler(
            metric="loss",
            mode="min",
            max_t=max_epochs,
            grace_period=5,
            reduction_factor=2
        )
        
        analysis = tune.run(
            self.objective_function,
            config=search_space,
            num_samples=num_samples,
            scheduler=scheduler,
            resources_per_trial={"cpu": 2, "gpu": 0.5}
        )
        
        # 獲取最佳結果
        best_trial = analysis.get_best_trial("loss", "min", "last")
        best_config = best_trial.config
        
        return {
            'best_config': best_config,
            'best_result': best_trial.last_result,
            'analysis': analysis
        }


# 便捷函數
def create_optimizer(model: nn.Module, config: Dict) -> optim.Optimizer:
    """創建優化器"""
    optimizer_type = config.get('optimizer', 'adamw').lower()
    lr = config.get('learning_rate', 1e-4)
    weight_decay = config.get('weight_decay', 0.01)
    
    if optimizer_type == 'adamw':
        return optim.AdamW(model.parameters(), lr=lr, weight_decay=weight_decay)
    elif optimizer_type == 'adam':
        return optim.Adam(model.parameters(), lr=lr)
    elif optimizer_type == 'sgd':
        momentum = config.get('momentum', 0.9)
        return optim.SGD(model.parameters(), lr=lr, momentum=momentum, weight_decay=weight_decay)
    else:
        raise ValueError(f"Unknown optimizer: {optimizer_type}")


def create_scheduler(optimizer: optim.Optimizer, config: Dict) -> Optional[optim.lr_scheduler._LRScheduler]:
    """創建學習率調度器"""
    scheduler_type = config.get('scheduler', None)
    
    if scheduler_type is None:
        return None
    elif scheduler_type == 'reduce_on_plateau':
        return optim.lr_scheduler.ReduceLROnPlateau(
            optimizer,
            mode='min',
            factor=config.get('scheduler_factor', 0.5),
            patience=config.get('scheduler_patience', 5),
            verbose=True
        )
    elif scheduler_type == 'cosine':
        return optim.lr_scheduler.CosineAnnealingLR(
            optimizer,
            T_max=config.get('cosine_t_max', 100)
        )
    elif scheduler_type == 'step':
        return optim.lr_scheduler.StepLR(
            optimizer,
            step_size=config.get('step_size', 30),
            gamma=config.get('step_gamma', 0.1)
        )
    else:
        raise ValueError(f"Unknown scheduler: {scheduler_type}")