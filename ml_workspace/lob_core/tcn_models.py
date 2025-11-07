#!/usr/bin/env python3
"""
TCN (Temporal Convolutional Network) 模型实现
专为LOB时间序列预测设计的因果卷积网络
"""

import torch
import torch.nn as nn
import torch.nn.functional as F
import numpy as np
from typing import List, Tuple, Optional, Dict, Any
from dataclasses import dataclass

from .models import BaseLOBModel, ModelConfig, ModelType


class CausalConv1d(nn.Module):
    """因果卷积层 - 确保不使用未来信息"""

    def __init__(self, in_channels: int, out_channels: int,
                 kernel_size: int, dilation: int = 1, dropout: float = 0.2):
        super().__init__()
        self.padding = (kernel_size - 1) * dilation
        # 使用显式左侧填充避免未来信息泄露
        self.conv = nn.Conv1d(in_channels, out_channels, kernel_size,
                              padding=0, dilation=dilation)
        self.dropout = nn.Dropout(dropout)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        if self.padding > 0:
            # 仅在左侧填充，确保卷积不会看到未来时间步
            x = F.pad(x, (self.padding, 0))

        x = self.conv(x)
        return self.dropout(x)


class TemporalBlock(nn.Module):
    """TCN的基本构建块"""

    def __init__(self, n_inputs: int, n_outputs: int, kernel_size: int,
                 dilation: int, dropout: float = 0.2):
        super().__init__()

        # 第一个因果卷积
        self.conv1 = CausalConv1d(n_inputs, n_outputs, kernel_size,
                                 dilation, dropout)

        # 第二个因果卷积
        self.conv2 = CausalConv1d(n_outputs, n_outputs, kernel_size,
                                 dilation, dropout)

        # 批归一化
        self.bn1 = nn.BatchNorm1d(n_outputs)
        self.bn2 = nn.BatchNorm1d(n_outputs)

        # 残差连接的降维
        self.downsample = nn.Conv1d(n_inputs, n_outputs, 1) if n_inputs != n_outputs else None

        # 激活函数
        self.relu = nn.ReLU()

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        # 第一个卷积块
        out = self.conv1(x)
        out = self.bn1(out)
        out = self.relu(out)

        # 第二个卷积块
        out = self.conv2(out)
        out = self.bn2(out)

        # 残差连接
        res = x if self.downsample is None else self.downsample(x)

        # 确保维度匹配
        if out.size(2) != res.size(2):
            res = res[:, :, :out.size(2)]

        return self.relu(out + res)


class TemporalConvNet(nn.Module):
    """完整的TCN网络"""

    def __init__(self, num_inputs: int, num_channels: List[int],
                 kernel_size: int = 2, dropout: float = 0.2):
        super().__init__()

        layers = []
        num_levels = len(num_channels)

        for i in range(num_levels):
            dilation_size = 2 ** i  # 指数增长的膨胀率
            in_channels = num_inputs if i == 0 else num_channels[i-1]
            out_channels = num_channels[i]

            layers += [TemporalBlock(in_channels, out_channels, kernel_size,
                                   dilation_size, dropout)]

        self.network = nn.Sequential(*layers)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.network(x)


class TCNLOBModel(BaseLOBModel):
    """基于TCN的LOB预测模型"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.device = torch.device('cuda' if torch.cuda.is_available() else 'cpu')

        # TCN参数
        self.sequence_length = config.sequence_length
        self.num_channels = config.hidden_dims or [32, 64, 128, 64, 32]
        self.kernel_size = config.model_params.get('kernel_size', 3)
        self.dropout_rate = config.dropout_rate

        # 构建模型
        self.model = self._build_model().to(self.device)
        self.criterion = nn.CrossEntropyLoss()
        self.optimizer = None

    def _build_model(self) -> nn.Module:
        """构建TCN-LOB模型"""
        return TCNLOBNet(
            input_dim=self.config.input_dim,
            output_dim=self.config.output_dim,
            num_channels=self.num_channels,
            kernel_size=self.kernel_size,
            dropout=self.dropout_rate,
            sequence_length=self.sequence_length
        )

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:
        """训练TCN模型"""

        # 重塑数据为时间序列格式
        X_seq = self._prepare_sequences(X)
        y_tensor = torch.LongTensor(y + 1).to(self.device)  # -1,0,1 -> 0,1,2

        if X_val is not None and y_val is not None:
            X_val_seq = self._prepare_sequences(X_val)
            y_val_tensor = torch.LongTensor(y_val + 1).to(self.device)

        # 优化器
        self.optimizer = torch.optim.Adam(
            self.model.parameters(),
            lr=self.config.learning_rate,
            weight_decay=1e-5
        )

        # 学习率调度器
        scheduler = torch.optim.lr_scheduler.StepLR(
            self.optimizer, step_size=30, gamma=0.5
        )

        # 训练循环
        epochs = self.config.model_params.get('epochs', 100)
        patience = self.config.model_params.get('patience', 15)

        train_losses = []
        val_losses = []
        val_accuracies = []

        best_val_loss = float('inf')
        patience_counter = 0

        for epoch in range(epochs):
            # 训练阶段
            self.model.train()
            total_loss = 0
            num_batches = 0

            for i in range(0, len(X_seq), self.config.batch_size):
                batch_X = X_seq[i:i+self.config.batch_size]
                batch_y = y_tensor[i:i+self.config.batch_size]

                self.optimizer.zero_grad()
                outputs = self.model(batch_X)
                loss = self.criterion(outputs, batch_y)
                loss.backward()

                # 梯度裁剪
                torch.nn.utils.clip_grad_norm_(self.model.parameters(), max_norm=1.0)

                self.optimizer.step()

                total_loss += loss.item()
                num_batches += 1

            avg_train_loss = total_loss / num_batches
            train_losses.append(avg_train_loss)

            # 验证阶段
            if X_val is not None:
                self.model.eval()
                with torch.no_grad():
                    val_outputs = self.model(X_val_seq)
                    val_loss = self.criterion(val_outputs, y_val_tensor).item()
                    val_pred = torch.argmax(val_outputs, dim=1)
                    val_acc = (val_pred == y_val_tensor).float().mean().item()

                    val_losses.append(val_loss)
                    val_accuracies.append(val_acc)

                    # Early stopping
                    if val_loss < best_val_loss:
                        best_val_loss = val_loss
                        patience_counter = 0
                    else:
                        patience_counter += 1

                    if patience_counter >= patience:
                        print(f"Early stopping at epoch {epoch}")
                        break

            # 更新学习率
            scheduler.step()

            if epoch % 10 == 0:
                print(f"Epoch {epoch}: Train Loss = {avg_train_loss:.4f}")
                if X_val is not None:
                    print(f"          Val Loss = {val_loss:.4f}, Val Acc = {val_acc:.4f}")

        self.is_trained = True

        return {
            'train_losses': train_losses,
            'val_losses': val_losses,
            'val_accuracies': val_accuracies,
            'best_val_loss': best_val_loss,
            'final_epoch': epoch
        }

    def _prepare_sequences(self, X: np.ndarray) -> torch.Tensor:
        """准备序列数据"""
        if len(X.shape) == 2:
            # (samples, features) -> (samples, features, sequence_length)
            n_samples, n_features = X.shape

            # 创建时间序列
            sequences = []
            for i in range(self.sequence_length, n_samples):
                seq = X[i-self.sequence_length:i].T  # (features, sequence_length)
                sequences.append(seq)

            if sequences:
                X_seq = np.stack(sequences, axis=0)  # (samples, features, sequence_length)
            else:
                # 如果数据不够长，重复最后一个样本
                X_seq = np.repeat(X[-1:].T[:, np.newaxis, :],
                                 max(1, n_samples - self.sequence_length + 1), axis=0)
        else:
            X_seq = X

        return torch.FloatTensor(X_seq).to(self.device)

    def predict(self, X: np.ndarray) -> np.ndarray:
        """预测"""
        proba = self.predict_proba(X)
        return np.argmax(proba, axis=1) - 1  # 转回-1,0,1

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        """预测概率"""
        if not self.is_trained:
            raise ValueError("Model must be trained before prediction")

        self.model.eval()
        X_seq = self._prepare_sequences(X)

        with torch.no_grad():
            outputs = self.model(X_seq)
            probabilities = F.softmax(outputs, dim=1)

        return probabilities.cpu().numpy()

    def save_model(self, path: str) -> None:
        """保存模型"""
        save_dict = {
            'model_state_dict': self.model.state_dict(),
            'config': self.config,
            'is_trained': self.is_trained
        }
        torch.save(save_dict, path)

    def load_model(self, path: str) -> None:
        """加载模型"""
        checkpoint = torch.load(path, map_location=self.device)
        self.model.load_state_dict(checkpoint['model_state_dict'])
        self.config = checkpoint['config']
        self.is_trained = checkpoint['is_trained']


class TCNLOBNet(nn.Module):
    """TCN-LOB网络架构"""

    def __init__(self, input_dim: int, output_dim: int,
                 num_channels: List[int], kernel_size: int = 3,
                 dropout: float = 0.2, sequence_length: int = 100):
        super().__init__()

        self.input_dim = input_dim
        self.output_dim = output_dim
        self.sequence_length = sequence_length

        # 输入投影层
        self.input_projection = nn.Linear(input_dim, num_channels[0])

        # TCN主体
        self.tcn = TemporalConvNet(
            num_inputs=num_channels[0],
            num_channels=num_channels[1:],
            kernel_size=kernel_size,
            dropout=dropout
        )

        # 注意力机制
        self.attention = nn.MultiheadAttention(
            embed_dim=num_channels[-1],
            num_heads=8,
            dropout=dropout,
            batch_first=True
        )

        # 分类头
        self.classifier = nn.Sequential(
            nn.Dropout(dropout),
            nn.Linear(num_channels[-1], num_channels[-1] // 2),
            nn.ReLU(),
            nn.Dropout(dropout),
            nn.Linear(num_channels[-1] // 2, output_dim)
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        batch_size, n_features, seq_len = x.shape

        # 重塑为 (batch_size * seq_len, n_features)
        x = x.transpose(1, 2).contiguous()  # (batch_size, seq_len, n_features)
        x = x.view(-1, n_features)  # (batch_size * seq_len, n_features)

        # 输入投影
        x = self.input_projection(x)  # (batch_size * seq_len, num_channels[0])

        # 重塑回TCN格式
        x = x.view(batch_size, seq_len, -1).transpose(1, 2)  # (batch_size, num_channels[0], seq_len)

        # TCN处理
        x = self.tcn(x)  # (batch_size, num_channels[-1], seq_len)

        # 转换为注意力格式
        x = x.transpose(1, 2)  # (batch_size, seq_len, num_channels[-1])

        # 自注意力
        x, _ = self.attention(x, x, x)

        # 全局池化（使用最后几个时间步）
        x = x[:, -5:, :].mean(dim=1)  # (batch_size, num_channels[-1])

        # 分类
        output = self.classifier(x)

        return output


class HybridTCNDeepLOB(nn.Module):
    """混合TCN-DeepLOB架构"""

    def __init__(self, lob_depth: int = 10, tcn_channels: List[int] = None,
                 output_dim: int = 3, dropout: float = 0.2):
        super().__init__()

        if tcn_channels is None:
            tcn_channels = [32, 64, 128, 64, 32]

        # LOB特征提取（类似DeepLOB的CNN部分）
        self.lob_conv1 = nn.Conv2d(1, 32, kernel_size=(1, 2), stride=(1, 2))
        self.lob_conv2 = nn.Conv2d(32, 32, kernel_size=(4, 1), padding=(1, 0))
        self.lob_conv3 = nn.Conv2d(32, 32, kernel_size=(4, 1), padding=(1, 0))

        # TCN时序建模
        conv_output_size = 32  # 需要根据实际计算
        self.tcn = TemporalConvNet(
            num_inputs=conv_output_size,
            num_channels=tcn_channels,
            kernel_size=3,
            dropout=dropout
        )

        # 最终分类层
        self.classifier = nn.Sequential(
            nn.Dropout(dropout),
            nn.Linear(tcn_channels[-1], tcn_channels[-1] // 2),
            nn.ReLU(),
            nn.Dropout(dropout),
            nn.Linear(tcn_channels[-1] // 2, output_dim)
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        batch_size, seq_len, features = x.shape

        # LOB处理：重塑为 (batch_size, 1, seq_len, features)
        x = x.unsqueeze(1)

        # CNN特征提取
        x = F.relu(self.lob_conv1(x))
        x = F.relu(self.lob_conv2(x))
        x = F.relu(self.lob_conv3(x))

        # 重塑为TCN格式 (batch_size, channels, seq_len)
        x = x.squeeze(-1).transpose(1, 2)
        x = x.transpose(1, 2)

        # TCN时序建模
        x = self.tcn(x)

        # 全局平均池化
        x = x.mean(dim=2)  # (batch_size, channels)

        # 分类
        output = self.classifier(x)

        return output


# 模型工厂扩展
def create_tcn_model(config: ModelConfig) -> TCNLOBModel:
    """创建TCN模型"""
    config.model_type = ModelType.TCN if hasattr(ModelType, 'TCN') else "TCN"
    return TCNLOBModel(config)


# 性能基准测试
def benchmark_tcn_vs_lstm(sequence_length: int = 100,
                         input_dim: int = 40,
                         batch_size: int = 32,
                         warmup_iters: int = 10,
                         measure_iters: int = 200) -> Dict[str, float]:
    """TCN vs LSTM性能对比

    该基准使用相同的设备、数据类型和批次，确保比较公平。
    """
    import time

    device = torch.device('cuda' if torch.cuda.is_available() else 'cpu')

    # 创建模拟数据（TCN: N,C,L / LSTM: N,L,C）
    X_tcn = torch.randn(batch_size, input_dim, sequence_length, device=device)
    X_lstm = X_tcn.transpose(1, 2).contiguous()

    # TCN模型
    tcn = TemporalConvNet(input_dim, [32, 64, 32]).to(device)
    tcn.eval()

    # LSTM模型
    lstm = nn.LSTM(input_dim, 64, 2, batch_first=True).to(device)
    lstm.eval()

    def _time_module(fn, *module_inputs) -> float:
        # 预热阶段，排除编译和缓存影响
        for _ in range(warmup_iters):
            with torch.no_grad():
                fn(*module_inputs)

        if device.type == 'cuda':
            torch.cuda.synchronize()

        start = time.perf_counter()
        for _ in range(measure_iters):
            with torch.no_grad():
                fn(*module_inputs)

        if device.type == 'cuda':
            torch.cuda.synchronize()

        elapsed = time.perf_counter() - start
        return elapsed / measure_iters

    tcn_time = _time_module(tcn, X_tcn)
    lstm_time = _time_module(lstm, X_lstm)

    return {
        'tcn_avg_time': tcn_time,
        'lstm_avg_time': lstm_time,
        'speedup': lstm_time / tcn_time if tcn_time > 0 else float('inf'),
        'tcn_per_sample': tcn_time / batch_size,
        'lstm_per_sample': lstm_time / batch_size
    }
