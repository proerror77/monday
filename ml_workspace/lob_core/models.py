#!/usr/bin/env python3
"""
LOB模型接口和实现
包括DeepLOB、Transformer、以及传统ML模型的统一接口
"""

import torch
import torch.nn as nn
import torch.nn.functional as F
import numpy as np
import pandas as pd
from typing import Dict, List, Tuple, Optional, Union, Any
from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum
import warnings
from pathlib import Path

from sklearn.ensemble import RandomForestClassifier, GradientBoostingClassifier
from sklearn.linear_model import LogisticRegression
from sklearn.svm import SVC
from sklearn.metrics import classification_report, confusion_matrix
import lightgbm as lgb
import xgboost as xgb


class ModelType(Enum):
    """模型类型"""
    DEEPLOB = "deeplob"
    TRANSFORMER = "transformer"
    TCN = "tcn"
    HYBRID_TCN_DEEPLOB = "hybrid_tcn_deeplob"
    LIGHTGBM = "lightgbm"
    XGBOOST = "xgboost"
    RANDOM_FOREST = "random_forest"
    LOGISTIC = "logistic"


@dataclass
class ModelConfig:
    """模型配置"""
    model_type: ModelType
    input_dim: int
    output_dim: int = 3  # -1, 0, 1
    sequence_length: int = 100
    batch_size: int = 32
    learning_rate: float = 0.001
    hidden_dims: List[int] = None
    dropout_rate: float = 0.2
    use_attention: bool = True
    model_params: Dict[str, Any] = None

    def __post_init__(self):
        if self.hidden_dims is None:
            self.hidden_dims = [128, 64, 32]
        if self.model_params is None:
            self.model_params = {}


class BaseLOBModel(ABC):
    """LOB模型基类"""

    def __init__(self, config: ModelConfig):
        self.config = config
        self.is_trained = False
        self.feature_importance_ = None

    @abstractmethod
    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:
        """训练模型"""
        pass

    @abstractmethod
    def predict(self, X: np.ndarray) -> np.ndarray:
        """预测"""
        pass

    @abstractmethod
    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        """预测概率"""
        pass

    @abstractmethod
    def save_model(self, path: str) -> None:
        """保存模型"""
        pass

    @abstractmethod
    def load_model(self, path: str) -> None:
        """加载模型"""
        pass

    def evaluate(self, X: np.ndarray, y: np.ndarray) -> Dict[str, Any]:
        """评估模型"""
        y_pred = self.predict(X)
        y_proba = self.predict_proba(X)

        # 基础指标
        accuracy = np.mean(y_pred == y)

        # 分类报告
        report = classification_report(y, y_pred, output_dict=True)

        # 混淆矩阵
        cm = confusion_matrix(y, y_pred)

        return {
            'accuracy': accuracy,
            'classification_report': report,
            'confusion_matrix': cm.tolist(),
            'predictions': y_pred,
            'probabilities': y_proba
        }


class DeepLOBModel(BaseLOBModel):
    """DeepLOB模型实现"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.device = torch.device('cuda' if torch.cuda.is_available() else 'cpu')
        self.model = self._build_model()
        self.optimizer = None
        self.criterion = nn.CrossEntropyLoss()

    def _build_model(self) -> nn.Module:
        """构建DeepLOB网络"""
        return DeepLOBNet(
            input_dim=self.config.input_dim,
            output_dim=self.config.output_dim,
            hidden_dims=self.config.hidden_dims,
            dropout_rate=self.config.dropout_rate
        ).to(self.device)

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:
        """训练DeepLOB模型"""

        # 数据预处理
        X_tensor = torch.FloatTensor(X).to(self.device)
        y_tensor = torch.LongTensor(y + 1).to(self.device)  # 将-1,0,1转为0,1,2

        # 验证集
        if X_val is not None and y_val is not None:
            X_val_tensor = torch.FloatTensor(X_val).to(self.device)
            y_val_tensor = torch.LongTensor(y_val + 1).to(self.device)

        # 优化器
        self.optimizer = torch.optim.Adam(
            self.model.parameters(),
            lr=self.config.learning_rate
        )

        # 训练循环
        train_losses = []
        val_losses = []
        val_accuracies = []

        epochs = self.config.model_params.get('epochs', 100)
        patience = self.config.model_params.get('patience', 10)
        best_val_loss = float('inf')
        patience_counter = 0

        for epoch in range(epochs):
            # 训练模式
            self.model.train()
            total_loss = 0
            num_batches = 0

            # Mini-batch训练
            for i in range(0, len(X_tensor), self.config.batch_size):
                batch_X = X_tensor[i:i+self.config.batch_size]
                batch_y = y_tensor[i:i+self.config.batch_size]

                self.optimizer.zero_grad()
                outputs = self.model(batch_X)
                loss = self.criterion(outputs, batch_y)
                loss.backward()
                self.optimizer.step()

                total_loss += loss.item()
                num_batches += 1

            avg_train_loss = total_loss / num_batches
            train_losses.append(avg_train_loss)

            # 验证
            if X_val is not None:
                self.model.eval()
                with torch.no_grad():
                    val_outputs = self.model(X_val_tensor)
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

        self.is_trained = True

        return {
            'train_losses': train_losses,
            'val_losses': val_losses,
            'val_accuracies': val_accuracies,
            'final_epoch': epoch
        }

    def predict(self, X: np.ndarray) -> np.ndarray:
        """预测类别"""
        proba = self.predict_proba(X)
        return np.argmax(proba, axis=1) - 1  # 转回-1,0,1

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        """预测概率"""
        if not self.is_trained:
            raise ValueError("Model must be trained before prediction")

        self.model.eval()
        X_tensor = torch.FloatTensor(X).to(self.device)

        with torch.no_grad():
            outputs = self.model(X_tensor)
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


class DeepLOBNet(nn.Module):
    """DeepLOB网络架构"""

    def __init__(self, input_dim: int, output_dim: int,
                 hidden_dims: List[int], dropout_rate: float = 0.2):
        super().__init__()
        self.input_dim = input_dim
        self.output_dim = output_dim

        # CNN layers for spatial patterns
        self.conv1 = nn.Conv2d(1, 32, kernel_size=(1, 2), stride=(1, 2))
        self.conv2 = nn.Conv2d(32, 32, kernel_size=(4, 1), padding=(1, 0))
        self.conv3 = nn.Conv2d(32, 32, kernel_size=(4, 1), padding=(1, 0))

        # LSTM layers for temporal patterns
        lstm_input_size = self._get_conv_output_size()
        self.lstm1 = nn.LSTM(lstm_input_size, 64, batch_first=True)
        self.lstm2 = nn.LSTM(64, 64, batch_first=True)

        # Attention mechanism
        self.attention = nn.MultiheadAttention(64, 8, batch_first=True)

        # Classification layers
        self.dropout = nn.Dropout(dropout_rate)
        self.fc_layers = nn.ModuleList()

        prev_dim = 64
        for hidden_dim in hidden_dims:
            self.fc_layers.append(nn.Linear(prev_dim, hidden_dim))
            prev_dim = hidden_dim

        self.output_layer = nn.Linear(prev_dim, output_dim)

    def _get_conv_output_size(self) -> int:
        """计算卷积层输出尺寸"""
        # 假设输入是 (batch, 1, T, input_dim)
        # 经过conv层后的特征维度
        return 32

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        batch_size, seq_len, features = x.shape

        # 重塑为4D用于CNN
        x = x.unsqueeze(1)  # (batch, 1, seq_len, features)

        # CNN layers
        x = F.relu(self.conv1(x))
        x = F.relu(self.conv2(x))
        x = F.relu(self.conv3(x))

        # 重塑为3D用于RNN
        x = x.squeeze(-1).transpose(1, 2)  # (batch, seq_len, channels)

        # LSTM layers
        x, _ = self.lstm1(x)
        x, _ = self.lstm2(x)

        # Attention
        x, _ = self.attention(x, x, x)

        # 使用最后时间步
        x = x[:, -1, :]

        # Fully connected layers
        for fc_layer in self.fc_layers:
            x = F.relu(fc_layer(x))
            x = self.dropout(x)

        x = self.output_layer(x)
        return x


class TransformerLOBModel(BaseLOBModel):
    """基于Transformer的LOB模型"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.device = torch.device('cuda' if torch.cuda.is_available() else 'cpu')
        self.model = self._build_model()
        self.criterion = nn.CrossEntropyLoss()

    def _build_model(self) -> nn.Module:
        """构建Transformer网络"""
        return TransformerNet(
            input_dim=self.config.input_dim,
            output_dim=self.config.output_dim,
            d_model=self.config.hidden_dims[0],
            nhead=8,
            num_layers=4,
            dropout_rate=self.config.dropout_rate
        ).to(self.device)

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:
        """训练Transformer模型"""
        # 实现与DeepLOB类似的训练逻辑
        pass

    def predict(self, X: np.ndarray) -> np.ndarray:
        pass

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        pass

    def save_model(self, path: str) -> None:
        pass

    def load_model(self, path: str) -> None:
        pass


class TransformerNet(nn.Module):
    """Transformer网络架构"""

    def __init__(self, input_dim: int, output_dim: int, d_model: int,
                 nhead: int, num_layers: int, dropout_rate: float = 0.1):
        super().__init__()

        self.input_projection = nn.Linear(input_dim, d_model)
        self.positional_encoding = PositionalEncoding(d_model, dropout_rate)

        encoder_layer = nn.TransformerEncoderLayer(
            d_model=d_model,
            nhead=nhead,
            dropout=dropout_rate,
            batch_first=True
        )
        self.transformer = nn.TransformerEncoder(encoder_layer, num_layers)

        self.output_projection = nn.Linear(d_model, output_dim)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        x = self.input_projection(x)
        x = self.positional_encoding(x)
        x = self.transformer(x)
        x = x.mean(dim=1)  # Global average pooling
        x = self.output_projection(x)
        return x


class PositionalEncoding(nn.Module):
    """位置编码"""

    def __init__(self, d_model: int, dropout: float = 0.1, max_len: int = 5000):
        super().__init__()
        self.dropout = nn.Dropout(p=dropout)

        pe = torch.zeros(max_len, d_model)
        position = torch.arange(0, max_len, dtype=torch.float).unsqueeze(1)
        div_term = torch.exp(torch.arange(0, d_model, 2).float() * (-np.log(10000.0) / d_model))
        pe[:, 0::2] = torch.sin(position * div_term)
        pe[:, 1::2] = torch.cos(position * div_term)
        pe = pe.unsqueeze(0).transpose(0, 1)
        self.register_buffer('pe', pe)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        x = x + self.pe[:x.size(0), :].transpose(0, 1)
        return self.dropout(x)


class LightGBMLOBModel(BaseLOBModel):
    """LightGBM模型实现"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.model = None

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:

        # 转换标签：-1,0,1 -> 0,1,2
        y_transformed = y + 1

        # LightGBM参数
        params = {
            'objective': 'multiclass',
            'num_class': 3,
            'metric': 'multi_logloss',
            'boosting_type': 'gbdt',
            'num_leaves': 31,
            'learning_rate': self.config.learning_rate,
            'feature_fraction': 0.9,
            'bagging_fraction': 0.8,
            'bagging_freq': 5,
            'verbose': 0
        }
        params.update(self.config.model_params)

        # 训练数据
        train_data = lgb.Dataset(X, label=y_transformed)
        valid_sets = [train_data]

        # 验证数据
        if X_val is not None and y_val is not None:
            y_val_transformed = y_val + 1
            valid_data = lgb.Dataset(X_val, label=y_val_transformed, reference=train_data)
            valid_sets.append(valid_data)

        # 训练模型
        self.model = lgb.train(
            params,
            train_data,
            num_boost_round=params.get('num_boost_round', 100),
            valid_sets=valid_sets,
            callbacks=[lgb.early_stopping(50), lgb.log_evaluation(0)]
        )

        # 特征重要性
        self.feature_importance_ = self.model.feature_importance()
        self.is_trained = True

        return {
            'best_iteration': self.model.best_iteration,
            'best_score': self.model.best_score,
            'feature_importance': self.feature_importance_
        }

    def predict(self, X: np.ndarray) -> np.ndarray:
        if not self.is_trained:
            raise ValueError("Model must be trained before prediction")

        y_proba = self.model.predict(X)
        return np.argmax(y_proba, axis=1) - 1  # 转回-1,0,1

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        if not self.is_trained:
            raise ValueError("Model must be trained before prediction")

        return self.model.predict(X)

    def save_model(self, path: str) -> None:
        if self.model:
            self.model.save_model(path)

    def load_model(self, path: str) -> None:
        self.model = lgb.Booster(model_file=path)
        self.is_trained = True


class ModelFactory:
    """模型工厂"""

    @staticmethod
    def create_model(config: ModelConfig) -> BaseLOBModel:
        """根据配置创建模型"""
        if config.model_type == ModelType.DEEPLOB:
            return DeepLOBModel(config)
        elif config.model_type == ModelType.TRANSFORMER:
            return TransformerLOBModel(config)
        elif config.model_type == ModelType.TCN:
            from .tcn_models import TCNLOBModel
            return TCNLOBModel(config)
        elif config.model_type == ModelType.HYBRID_TCN_DEEPLOB:
            from .tcn_models import TCNLOBModel
            # 使用混合架构的特殊配置
            config.model_params = config.model_params or {}
            config.model_params['use_hybrid'] = True
            return TCNLOBModel(config)
        elif config.model_type == ModelType.LIGHTGBM:
            return LightGBMLOBModel(config)
        elif config.model_type == ModelType.XGBOOST:
            return XGBoostLOBModel(config)
        elif config.model_type == ModelType.RANDOM_FOREST:
            return RandomForestLOBModel(config)
        elif config.model_type == ModelType.LOGISTIC:
            return LogisticLOBModel(config)
        else:
            raise ValueError(f"Unsupported model type: {config.model_type}")


class XGBoostLOBModel(BaseLOBModel):
    """XGBoost模型（简化实现）"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.model = None

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:

        # XGBoost分类器
        self.model = xgb.XGBClassifier(
            objective='multi:softprob',
            num_class=3,
            learning_rate=self.config.learning_rate,
            n_estimators=self.config.model_params.get('n_estimators', 100),
            max_depth=self.config.model_params.get('max_depth', 6),
            random_state=42
        )

        # 训练
        eval_set = None
        if X_val is not None and y_val is not None:
            eval_set = [(X_val, y_val + 1)]

        self.model.fit(
            X, y + 1,  # 转换标签
            eval_set=eval_set,
            early_stopping_rounds=50,
            verbose=False
        )

        self.feature_importance_ = self.model.feature_importances_
        self.is_trained = True

        return {'feature_importance': self.feature_importance_}

    def predict(self, X: np.ndarray) -> np.ndarray:
        return self.model.predict(X) - 1

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        return self.model.predict_proba(X)

    def save_model(self, path: str) -> None:
        import joblib
        joblib.dump(self.model, path)

    def load_model(self, path: str) -> None:
        import joblib
        self.model = joblib.load(path)
        self.is_trained = True


class RandomForestLOBModel(BaseLOBModel):
    """随机森林模型（简化实现）"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.model = RandomForestClassifier(
            n_estimators=config.model_params.get('n_estimators', 100),
            max_depth=config.model_params.get('max_depth', 10),
            random_state=42,
            n_jobs=-1
        )

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:

        self.model.fit(X, y + 1)
        self.feature_importance_ = self.model.feature_importances_
        self.is_trained = True

        return {'feature_importance': self.feature_importance_}

    def predict(self, X: np.ndarray) -> np.ndarray:
        return self.model.predict(X) - 1

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        return self.model.predict_proba(X)

    def save_model(self, path: str) -> None:
        import joblib
        joblib.dump(self.model, path)

    def load_model(self, path: str) -> None:
        import joblib
        self.model = joblib.load(path)
        self.is_trained = True


class LogisticLOBModel(BaseLOBModel):
    """逻辑回归模型（简化实现）"""

    def __init__(self, config: ModelConfig):
        super().__init__(config)
        self.model = LogisticRegression(
            multi_class='multinomial',
            solver='lbfgs',
            max_iter=1000,
            random_state=42
        )

    def fit(self, X: np.ndarray, y: np.ndarray,
            X_val: Optional[np.ndarray] = None,
            y_val: Optional[np.ndarray] = None) -> Dict[str, Any]:

        self.model.fit(X, y + 1)
        self.is_trained = True

        return {}

    def predict(self, X: np.ndarray) -> np.ndarray:
        return self.model.predict(X) - 1

    def predict_proba(self, X: np.ndarray) -> np.ndarray:
        return self.model.predict_proba(X)

    def save_model(self, path: str) -> None:
        import joblib
        joblib.dump(self.model, path)

    def load_model(self, path: str) -> None:
        import joblib
        self.model = joblib.load(path)
        self.is_trained = True


# 评估工具
def compare_models(models: Dict[str, BaseLOBModel], X_test: np.ndarray,
                   y_test: np.ndarray) -> pd.DataFrame:
    """比较多个模型的性能"""
    results = []

    for name, model in models.items():
        try:
            metrics = model.evaluate(X_test, y_test)
            results.append({
                'model': name,
                'accuracy': metrics['accuracy'],
                'precision_macro': metrics['classification_report']['macro avg']['precision'],
                'recall_macro': metrics['classification_report']['macro avg']['recall'],
                'f1_macro': metrics['classification_report']['macro avg']['f1-score']
            })
        except Exception as e:
            print(f"Error evaluating {name}: {e}")
            continue

    return pd.DataFrame(results).sort_values('accuracy', ascending=False)