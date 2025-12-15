#!/usr/bin/env python3
"""
端到端測試腳本

測試完整流程：
1. 訓練模型（使用合成數據，放寬部署條件）
2. 部署模型到 models/current/
3. 驗證 metadata.json 正確生成
"""

import json
import os
import shutil
import sys
from pathlib import Path

# 設置路徑
SCRIPT_DIR = Path(__file__).parent
PROJECT_ROOT = SCRIPT_DIR.parent
MODELS_DIR = PROJECT_ROOT / "models"
CURRENT_DIR = MODELS_DIR / "current"
ARCHIVE_DIR = MODELS_DIR / "archive"


def cleanup():
    """清理測試目錄"""
    if MODELS_DIR.exists():
        shutil.rmtree(MODELS_DIR)
    print("✓ Cleaned up models directory")


def create_test_config():
    """創建測試配置（放寬部署條件）"""
    config = {
        "clickhouse": {
            "host": "localhost",
            "port": 8123,
            "database": "hft",
            "user": "default",
            "password": ""
        },
        "data": {
            "table": "hft_features_eth_local_ob",
            "symbol": "ETHUSDT",
            "lookback_days": 7,
            "sequence_length": 50,  # 縮短序列
            "label_horizon": 10
        },
        "features": {
            "input_dim": 39,
            "normalize": "standard"
        },
        "model": {
            "type": "lstm_attention",
            "hidden_dim": 32,  # 縮小模型
            "num_layers": 1,
            "dropout": 0.1,
            "attention_heads": 2
        },
        "training": {
            "batch_size": 128,
            "epochs": 5,  # 減少 epochs
            "learning_rate": 0.001,
            "early_stopping_patience": 3,
            "val_ratio": 0.2
        },
        "deployment": {
            "min_ic": -1.0,  # 放寬條件，測試用
            "min_ir": -100.0,
            "max_drawdown": 10000.0  # 非常寬鬆，合成數據 MaxDD 很大
        },
        "output": {
            "model_dir": str(CURRENT_DIR),
            "archive_dir": str(ARCHIVE_DIR),
            "metadata_file": str(MODELS_DIR / "metadata.json")
        }
    }

    config_path = SCRIPT_DIR / "test_config.yaml"
    import yaml
    with open(config_path, "w") as f:
        yaml.dump(config, f, default_flow_style=False)

    print(f"✓ Created test config at {config_path}")
    return config_path


def run_training(config_path: Path) -> bool:
    """運行訓練"""
    import subprocess

    print("\n" + "=" * 60)
    print("Running training...")
    print("=" * 60)

    result = subprocess.run(
        [sys.executable, "train.py", "--config", str(config_path)],
        cwd=SCRIPT_DIR,
        capture_output=True,
        text=True
    )

    print(result.stdout)
    if result.stderr:
        print("STDERR:", result.stderr)

    return result.returncode == 0


def verify_deployment() -> bool:
    """驗證模型部署"""
    print("\n" + "=" * 60)
    print("Verifying deployment...")
    print("=" * 60)

    # 檢查模型文件
    model_files = list(CURRENT_DIR.glob("*.pt"))
    if not model_files:
        print("❌ No model file found in models/current/")
        return False

    print(f"✓ Found model: {model_files[0]}")

    # 檢查 metadata
    metadata_path = MODELS_DIR / "metadata.json"
    if not metadata_path.exists():
        print("❌ metadata.json not found")
        return False

    with open(metadata_path) as f:
        metadata = json.load(f)

    print(f"✓ Metadata loaded:")
    print(f"  - Version: {metadata.get('current_version')}")
    print(f"  - Model type: {metadata.get('model_type')}")
    print(f"  - Trained at: {metadata.get('trained_at')}")
    print(f"  - Metrics: IC={metadata['metrics'].get('ic', 0):.4f}, IR={metadata['metrics'].get('ir', 0):.4f}")

    return True


def main():
    """主函數"""
    print("=" * 60)
    print("HFT End-to-End Test")
    print("=" * 60)

    # 清理
    cleanup()

    # 創建測試配置
    config_path = create_test_config()

    # 運行訓練
    if not run_training(config_path):
        print("\n❌ Training failed")
        return 1

    # 驗證部署
    if not verify_deployment():
        print("\n❌ Deployment verification failed")
        return 1

    print("\n" + "=" * 60)
    print("✅ End-to-end test PASSED!")
    print("=" * 60)

    # 清理測試配置
    config_path.unlink()

    return 0


if __name__ == "__main__":
    sys.exit(main())
