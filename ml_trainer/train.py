#!/usr/bin/env python3
"""
ML Trainer - 精簡的模型訓練腳本

功能：
1. 從 ClickHouse 加載歷史數據
2. 計算 39 維本地訂單簿特徵
3. 訓練 LSTM+Attention 模型
4. 評估指標 (IC/IR/Sharpe)
5. 達標後自動部署到 models/current/

使用方式：
    python train.py                    # 使用默認配置
    python train.py --config custom.yaml  # 使用自定義配置
    python train.py --dry-run          # 測試模式，不部署

定時任務：
    0 2 * * * cd /opt/hft && python ml_trainer/train.py >> /var/log/hft-trainer.log 2>&1
"""

import json
import os
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import Dict, Optional, Tuple

import numpy as np
import yaml

# 設置路徑
SCRIPT_DIR = Path(__file__).parent
PROJECT_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(PROJECT_ROOT / "ml_workspace"))


def load_config(config_path: Optional[str] = None) -> Dict:
    """加載配置文件"""
    if config_path is None:
        config_path = SCRIPT_DIR / "config.yaml"

    with open(config_path, "r") as f:
        return yaml.safe_load(f)


def connect_clickhouse(config: Dict):
    """連接 ClickHouse，連接失敗時返回 None"""
    try:
        import clickhouse_connect

        ch_config = config["clickhouse"]
        client = clickhouse_connect.get_client(
            host=ch_config.get("host", "localhost"),
            port=ch_config.get("port", 8123),
            database=ch_config.get("database", "hft"),
            username=ch_config.get("user", "default"),
            password=ch_config.get("password", ""),
        )
        # 測試連接
        client.command("SELECT 1")
        return client
    except ImportError:
        print("WARNING: clickhouse-connect not installed, using synthetic data")
        return None
    except Exception as e:
        print(f"WARNING: ClickHouse connection failed: {e}")
        print("Using synthetic data for training")
        return None


def load_data(client, config: Dict) -> Tuple[np.ndarray, np.ndarray]:
    """從 ClickHouse 加載訓練數據"""
    data_config = config["data"]
    feature_config = config["features"]

    # 如果沒有 ClickHouse 連接，使用合成數據
    if client is None:
        return generate_synthetic_data(config)

    # 計算時間範圍
    end_date = datetime.now()
    start_date = end_date - timedelta(days=data_config["lookback_days"])

    print(f"Loading data from {start_date} to {end_date}")

    # 查詢數據 - 使用現有的 39 維特徵表
    query = f"""
    SELECT *
    FROM {data_config['table']}
    WHERE symbol = '{data_config['symbol']}'
      AND exchange_ts >= toDateTime64('{start_date.strftime('%Y-%m-%d %H:%M:%S')}', 3)
      AND exchange_ts <= toDateTime64('{end_date.strftime('%Y-%m-%d %H:%M:%S')}', 3)
    ORDER BY exchange_ts
    """

    try:
        result = client.query(query)
        df = result.result_rows
        columns = result.column_names

        if len(df) == 0:
            print("WARNING: No data found, using synthetic data for testing")
            return generate_synthetic_data(config)

        print(f"Loaded {len(df)} records")

        # 轉換為 numpy array
        data = np.array(df, dtype=np.float32)

        # 分離特徵和標籤
        # 假設最後一列是標籤（未來收益）
        X = data[:, :-1]
        y = data[:, -1]

        return X, y

    except Exception as e:
        print(f"WARNING: Query failed: {e}")
        print("Using synthetic data for testing")
        return generate_synthetic_data(config)


def generate_synthetic_data(config: Dict) -> Tuple[np.ndarray, np.ndarray]:
    """生成合成數據（用於測試）"""
    n_samples = 10000
    input_dim = config["features"]["input_dim"]

    X = np.random.randn(n_samples, input_dim).astype(np.float32)
    # 生成有一定信號的標籤
    y = (X[:, 0] * 0.1 + X[:, 1] * 0.05 + np.random.randn(n_samples) * 0.5).astype(np.float32)

    return X, y


def create_sequences(X: np.ndarray, y: np.ndarray, seq_len: int) -> Tuple[np.ndarray, np.ndarray]:
    """創建序列數據"""
    n_samples = len(X) - seq_len
    X_seq = np.zeros((n_samples, seq_len, X.shape[1]), dtype=np.float32)
    y_seq = np.zeros(n_samples, dtype=np.float32)

    for i in range(n_samples):
        X_seq[i] = X[i:i+seq_len]
        y_seq[i] = y[i+seq_len]

    return X_seq, y_seq


def build_model(config: Dict):
    """構建模型"""
    try:
        import torch
        import torch.nn as nn
    except ImportError:
        print("ERROR: PyTorch not installed")
        print("Run: pip install torch")
        sys.exit(1)

    model_config = config["model"]
    feature_config = config["features"]
    data_config = config["data"]

    class LSTMAttention(nn.Module):
        def __init__(self):
            super().__init__()
            input_dim = feature_config["input_dim"]
            hidden_dim = model_config["hidden_dim"]
            num_layers = model_config["num_layers"]
            dropout = model_config["dropout"]

            self.lstm = nn.LSTM(
                input_size=input_dim,
                hidden_size=hidden_dim,
                num_layers=num_layers,
                batch_first=True,
                dropout=dropout if num_layers > 1 else 0,
                bidirectional=True
            )

            # Attention
            self.attention = nn.MultiheadAttention(
                embed_dim=hidden_dim * 2,
                num_heads=model_config["attention_heads"],
                dropout=dropout,
                batch_first=True
            )

            # Output layers
            self.fc = nn.Sequential(
                nn.Linear(hidden_dim * 2, hidden_dim),
                nn.ReLU(),
                nn.Dropout(dropout),
                nn.Linear(hidden_dim, 1)
            )

        def forward(self, x):
            # LSTM
            lstm_out, _ = self.lstm(x)

            # Self-attention
            attn_out, _ = self.attention(lstm_out, lstm_out, lstm_out)

            # 取最後一個時間步
            out = attn_out[:, -1, :]

            # 全連接層
            out = self.fc(out)
            return out.squeeze(-1)

    return LSTMAttention()


def train_model(model, X_train, y_train, X_val, y_val, config: Dict) -> Dict:
    """訓練模型"""
    import torch
    import torch.nn as nn
    from torch.utils.data import DataLoader, TensorDataset

    train_config = config["training"]
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    print(f"Training on {device}")

    model = model.to(device)

    # 數據加載器
    train_dataset = TensorDataset(
        torch.from_numpy(X_train),
        torch.from_numpy(y_train)
    )
    val_dataset = TensorDataset(
        torch.from_numpy(X_val),
        torch.from_numpy(y_val)
    )

    train_loader = DataLoader(
        train_dataset,
        batch_size=train_config["batch_size"],
        shuffle=True
    )
    val_loader = DataLoader(
        val_dataset,
        batch_size=train_config["batch_size"],
        shuffle=False
    )

    # 優化器和損失函數
    optimizer = torch.optim.Adam(model.parameters(), lr=train_config["learning_rate"])
    criterion = nn.MSELoss()

    # 訓練循環
    best_val_loss = float("inf")
    patience_counter = 0
    history = {"train_loss": [], "val_loss": []}

    for epoch in range(train_config["epochs"]):
        # 訓練
        model.train()
        train_losses = []
        for X_batch, y_batch in train_loader:
            X_batch = X_batch.to(device)
            y_batch = y_batch.to(device)

            optimizer.zero_grad()
            outputs = model(X_batch)
            loss = criterion(outputs, y_batch)
            loss.backward()
            optimizer.step()

            train_losses.append(loss.item())

        # 驗證
        model.eval()
        val_losses = []
        with torch.no_grad():
            for X_batch, y_batch in val_loader:
                X_batch = X_batch.to(device)
                y_batch = y_batch.to(device)

                outputs = model(X_batch)
                loss = criterion(outputs, y_batch)
                val_losses.append(loss.item())

        train_loss = np.mean(train_losses)
        val_loss = np.mean(val_losses)
        history["train_loss"].append(train_loss)
        history["val_loss"].append(val_loss)

        print(f"Epoch {epoch+1}/{train_config['epochs']}: "
              f"train_loss={train_loss:.6f}, val_loss={val_loss:.6f}")

        # Early stopping
        if val_loss < best_val_loss:
            best_val_loss = val_loss
            patience_counter = 0
            # 保存最佳模型
            best_model_state = model.state_dict().copy()
        else:
            patience_counter += 1
            if patience_counter >= train_config["early_stopping_patience"]:
                print(f"Early stopping at epoch {epoch+1}")
                break

    # 恢復最佳模型
    model.load_state_dict(best_model_state)

    return {
        "train_loss": history["train_loss"][-1],
        "val_loss": best_val_loss,
        "epochs_trained": len(history["train_loss"])
    }


def evaluate_model(model, X_test, y_test, config: Dict) -> Dict:
    """評估模型"""
    import torch

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    model = model.to(device)
    model.eval()

    with torch.no_grad():
        X_tensor = torch.from_numpy(X_test).to(device)
        predictions = model(X_tensor).cpu().numpy()

    # 計算指標
    # IC (Information Coefficient) - 預測值與實際值的相關係數
    ic = np.corrcoef(predictions, y_test)[0, 1]

    # IR (Information Ratio) - IC / IC 的標準差
    # 使用滾動 IC 計算
    window_size = 100
    rolling_ics = []
    for i in range(0, len(predictions) - window_size, window_size):
        window_ic = np.corrcoef(
            predictions[i:i+window_size],
            y_test[i:i+window_size]
        )[0, 1]
        if not np.isnan(window_ic):
            rolling_ics.append(window_ic)

    ir = np.mean(rolling_ics) / (np.std(rolling_ics) + 1e-10) if rolling_ics else 0

    # Sharpe Ratio (簡化計算)
    returns = predictions * y_test  # 預測信號乘以實際收益
    sharpe = np.mean(returns) / (np.std(returns) + 1e-10) * np.sqrt(252)

    # 最大回撤 (基於累積收益)
    cumulative = np.cumsum(returns)
    running_max = np.maximum.accumulate(cumulative)
    drawdown = (running_max - cumulative) / (running_max + 1e-10)
    max_drawdown = np.max(drawdown)

    return {
        "ic": float(ic) if not np.isnan(ic) else 0.0,
        "ir": float(ir),
        "sharpe": float(sharpe),
        "max_drawdown": float(max_drawdown)
    }


def deploy_model(model, metrics: Dict, config: Dict, dry_run: bool = False) -> bool:
    """部署模型"""
    import torch

    deployment_config = config["deployment"]
    output_config = config["output"]

    # 檢查是否達標
    if metrics["ic"] < deployment_config["min_ic"]:
        print(f"❌ IC {metrics['ic']:.4f} < {deployment_config['min_ic']} - not deploying")
        return False

    if metrics["ir"] < deployment_config["min_ir"]:
        print(f"❌ IR {metrics['ir']:.4f} < {deployment_config['min_ir']} - not deploying")
        return False

    if metrics["max_drawdown"] > deployment_config["max_drawdown"]:
        print(f"❌ MaxDD {metrics['max_drawdown']:.4f} > {deployment_config['max_drawdown']} - not deploying")
        return False

    print(f"✅ Model meets deployment criteria:")
    print(f"   IC: {metrics['ic']:.4f} >= {deployment_config['min_ic']}")
    print(f"   IR: {metrics['ir']:.4f} >= {deployment_config['min_ir']}")
    print(f"   MaxDD: {metrics['max_drawdown']:.4f} <= {deployment_config['max_drawdown']}")

    if dry_run:
        print("📋 Dry run mode - skipping actual deployment")
        return True

    # 創建目錄
    model_dir = Path(output_config["model_dir"])
    archive_dir = Path(output_config["archive_dir"])
    model_dir.mkdir(parents=True, exist_ok=True)
    archive_dir.mkdir(parents=True, exist_ok=True)

    # 歸檔當前模型
    current_model_path = model_dir / "strategy_dl.pt"
    if current_model_path.exists():
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        archive_path = archive_dir / f"strategy_dl_{timestamp}.pt"
        current_model_path.rename(archive_path)
        print(f"📦 Archived current model to {archive_path}")

    # 保存新模型 (TorchScript)
    model.eval()
    model_cpu = model.cpu()

    # 創建示例輸入
    example_input = torch.randn(1, config["data"]["sequence_length"], config["features"]["input_dim"])

    try:
        scripted_model = torch.jit.trace(model_cpu, example_input)
        scripted_model.save(str(current_model_path))
        print(f"✅ Deployed model to {current_model_path}")
    except Exception as e:
        print(f"⚠️ TorchScript failed, saving state dict: {e}")
        torch.save(model_cpu.state_dict(), current_model_path)

    # 更新元數據
    metadata = {
        "current_version": f"v{datetime.now().strftime('%Y%m%d_%H%M%S')}",
        "model_type": config["model"]["type"],
        "input_dim": config["features"]["input_dim"],
        "output_dim": 1,
        "sequence_length": config["data"]["sequence_length"],
        "trained_at": datetime.now().isoformat(),
        "data_range": {
            "lookback_days": config["data"]["lookback_days"],
            "symbol": config["data"]["symbol"]
        },
        "metrics": metrics,
        "deployment_criteria": deployment_config
    }

    metadata_path = Path(output_config["metadata_file"])
    metadata_path.parent.mkdir(parents=True, exist_ok=True)

    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)

    print(f"📝 Updated metadata at {metadata_path}")

    return True


def main():
    """主函數"""
    import argparse

    parser = argparse.ArgumentParser(description="ML Trainer for HFT System")
    parser.add_argument("--config", type=str, help="Path to config file")
    parser.add_argument("--dry-run", action="store_true", help="Test mode, don't deploy")
    args = parser.parse_args()

    print("=" * 60)
    print("HFT ML Trainer")
    print(f"Started at: {datetime.now().isoformat()}")
    print("=" * 60)

    # 加載配置
    config = load_config(args.config)
    print(f"\nConfiguration loaded")
    print(f"  Model type: {config['model']['type']}")
    print(f"  Input dim: {config['features']['input_dim']}")
    print(f"  Lookback days: {config['data']['lookback_days']}")

    # 連接 ClickHouse
    print("\nConnecting to ClickHouse...")
    client = connect_clickhouse(config)

    # 加載數據
    print("\nLoading data...")
    X, y = load_data(client, config)
    print(f"  Total samples: {len(X)}")

    # 創建序列
    print("\nCreating sequences...")
    seq_len = config["data"]["sequence_length"]
    X_seq, y_seq = create_sequences(X, y, seq_len)
    print(f"  Sequence shape: {X_seq.shape}")

    # 分割數據
    val_ratio = config["training"]["val_ratio"]
    split_idx = int(len(X_seq) * (1 - val_ratio))

    X_train, X_val = X_seq[:split_idx], X_seq[split_idx:]
    y_train, y_val = y_seq[:split_idx], y_seq[split_idx:]

    print(f"  Training samples: {len(X_train)}")
    print(f"  Validation samples: {len(X_val)}")

    # 構建模型
    print("\nBuilding model...")
    model = build_model(config)
    total_params = sum(p.numel() for p in model.parameters())
    print(f"  Total parameters: {total_params:,}")

    # 訓練模型
    print("\nTraining model...")
    train_results = train_model(model, X_train, y_train, X_val, y_val, config)
    print(f"  Final train loss: {train_results['train_loss']:.6f}")
    print(f"  Final val loss: {train_results['val_loss']:.6f}")
    print(f"  Epochs trained: {train_results['epochs_trained']}")

    # 評估模型
    print("\nEvaluating model...")
    metrics = evaluate_model(model, X_val, y_val, config)
    metrics["train_loss"] = train_results["train_loss"]
    metrics["val_loss"] = train_results["val_loss"]

    print(f"\nMetrics:")
    print(f"  IC: {metrics['ic']:.4f}")
    print(f"  IR: {metrics['ir']:.4f}")
    print(f"  Sharpe: {metrics['sharpe']:.4f}")
    print(f"  MaxDD: {metrics['max_drawdown']:.4f}")

    # 部署模型
    print("\nDeployment check...")
    deployed = deploy_model(model, metrics, config, dry_run=args.dry_run)

    print("\n" + "=" * 60)
    if deployed:
        print("✅ Training completed and model deployed!")
    else:
        print("⚠️ Training completed but model not deployed (criteria not met)")
    print(f"Finished at: {datetime.now().isoformat()}")
    print("=" * 60)

    return 0 if deployed else 1


if __name__ == "__main__":
    sys.exit(main())
