#!/usr/bin/env python3
"""
ML Training Workflow v3.0 - Agno Workflow 整合版本
=================================================

將現有的 TLOB 訓練流程重構為 Agno Workflow 模式
採用聲明式步驟定義 + 條件分支 + 循環迭代
與 24/7 Rust HFT 系統無縫整合
"""

import asyncio
import time
import json
import logging
from datetime import datetime, timedelta
from typing import Dict, List, Any, Optional, Tuple, Union
from dataclasses import dataclass, asdict
from pathlib import Path
from enum import Enum

import torch
import torch.nn as nn
import pandas as pd
import numpy as np
from torch.utils.data import DataLoader

# 導入核心定義
from .definitions import (
    Workflow, WorkflowStatus, WorkflowPriority,
    StepInput, StepOutput, TrainingStepResult,
    create_step_input, create_step_output
)

# 導入現有模塊（使用相對路徑模擬）
try:
    from ..ml.train import TLOBTrainer, TLOBDataset
    from ..ml.models.tlob import TLOB, TLOBConfig
    from ..ml.data.data_loader import DataLoader as HFTDataLoader
    from ..ml.export import ModelExporter
except ImportError:
    # 如果無法導入，使用模擬類
    class TLOBTrainer:
        def __init__(self, config): pass
        def train(self): return {"success": True}
    
    class TLOBDataset:
        def __init__(self, data): pass
    
    class TLOB:
        def __init__(self, config): pass
    
    class TLOBConfig:
        def __init__(self): pass
    
    class HFTDataLoader:
        def __init__(self): pass
    
    class ModelExporter:
        def __init__(self): pass

# 設置日誌
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# 所有核心定義已從 definitions.py 導入

class MLTrainingWorkflow(Workflow):
    """
    ML 訓練工作流程 - Agno Workflow 實現
    
    Features:
    - 聲明式步驟定義
    - 條件分支邏輯
    - 循環迭代優化
    - 並行模型訓練
    - 自動品質檢查
    - 與 HFT 系統整合
    """
    
    def __init__(self, config: Dict[str, Any]):
        super().__init__(
            name="ML_Training_Workflow_v3",
            description="Agno-based ML training with HFT integration"
        )
        
        self.config = config
        self.session_id = f"training_{int(time.time())}"
        self.output_dir = Path(config.get('output_dir', './outputs')) / self.session_id
        self.output_dir.mkdir(parents=True, exist_ok=True)
        
        # 工作流程狀態
        self.workflow_state = {
            "current_step": None,
            "training_iteration": 0,
            "best_model_performance": 0.0,
            "models_trained": [],
            "deployment_ready": False
        }
        
        # 定義步驟序列
        self.define_workflow_steps()
        
    def define_workflow_steps(self):
        """定義工作流程步驟 - Agno 聲明式模式"""
        self.step_definitions = {
            "data_validation": {
                "name": "數據質量驗證",
                "description": "驗證 ClickHouse 數據質量和完整性",
                "function": self.step_data_validation,
                "retry_count": 3,
                "timeout": 300,  # 5分鐘
                "critical": True
            },
            "feature_engineering": {
                "name": "特徵工程處理",
                "description": "數據預處理和特徵提取",
                "function": self.step_feature_engineering,
                "retry_count": 2,
                "timeout": 600,  # 10分鐘
                "critical": True
            },
            "parallel_model_training": {
                "name": "並行模型訓練",
                "description": "並行訓練多個 TLOB 模型變體",
                "function": self.step_parallel_model_training,
                "retry_count": 1,
                "timeout": 3600,  # 1小時
                "critical": True
            },
            "model_evaluation": {
                "name": "模型性能評估",
                "description": "評估模型性能和風險指標",
                "function": self.step_model_evaluation,
                "retry_count": 2,
                "timeout": 300,
                "critical": True
            },
            "quality_gate_check": {
                "name": "質量門檢查",
                "description": "檢查模型是否達到部署標準",
                "function": self.step_quality_gate_check,
                "retry_count": 1,
                "timeout": 60,
                "critical": True
            },
            "conditional_retraining": {
                "name": "條件式重訓練",
                "description": "如果質量不達標，進行參數調優重訓練",
                "function": self.step_conditional_retraining,
                "retry_count": 3,
                "timeout": 1800,  # 30分鐘
                "critical": False
            },
            "model_export": {
                "name": "模型導出",
                "description": "導出 TorchScript 供 Rust 系統使用",
                "function": self.step_model_export,
                "retry_count": 2,
                "timeout": 300,
                "critical": True
            },
            "hft_integration": {
                "name": "HFT 系統整合",
                "description": "將模型部署到 Rust HFT 系統",
                "function": self.step_hft_integration,
                "retry_count": 2,
                "timeout": 600,
                "critical": True
            }
        }
    
    async def step_data_validation(self, input_data: StepInput) -> StepOutput:
        """步驟 1: 數據質量驗證 - Agno Step 模式"""
        logger.info("🔍 執行數據質量驗證...")
        
        try:
            symbol = self.config.get('symbol', 'BTCUSDT')
            
            # 從 ClickHouse 載入數據
            data_loader = HFTDataLoader()
            raw_data = data_loader.load_orderbook_data(
                symbol=symbol,
                start_time=self.config['data_start_time'],
                end_time=self.config['data_end_time']
            )
            
            # 數據質量檢查
            data_quality_score = self._calculate_data_quality(raw_data)
            
            validation_results = {
                "data_samples": len(raw_data),
                "quality_score": data_quality_score,
                "symbol": symbol,
                "time_range": {
                    "start": self.config['data_start_time'],
                    "end": self.config['data_end_time']
                }
            }
            
            # Agno 條件邏輯 - 數據質量門檻
            if data_quality_score < 0.9:
                return StepOutput(
                    success=False,
                    data=validation_results,
                    error_message=f"數據質量不足: {data_quality_score:.2f} < 0.9",
                    next_step="end"  # 終止流程
                )
            
            return StepOutput(
                success=True,
                data={
                    "raw_data": raw_data,
                    "validation_results": validation_results
                },
                metadata={"quality_passed": True},
                next_step="feature_engineering"
            )
            
        except Exception as e:
            logger.error(f"數據驗證失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_feature_engineering(self, input_data: StepInput) -> StepOutput:
        """步驟 2: 特徵工程處理"""
        logger.info("⚙️ 執行特徵工程...")
        
        try:
            raw_data = input_data.data["raw_data"]
            
            # 使用現有的預處理邏輯
            from ..ml.data.labeling import LabelGenerator
            from ..ml.data.preprocessing import LOBPreprocessor
            
            # 生成標籤
            label_generator = LabelGenerator()
            labeled_data = label_generator.generate_labels(raw_data)
            
            # 分析標籤分佈
            label_analysis = label_generator.analyze_label_distribution(labeled_data)
            logger.info(f"標籤分佈: {label_analysis['label_percentages']}")
            
            # 特徵預處理
            preprocessor = LOBPreprocessor()
            
            # 數據分割
            train_size = int(len(labeled_data) * self.config.get('train_ratio', 0.7))
            val_size = int(len(labeled_data) * self.config.get('val_ratio', 0.15))
            
            train_data = labeled_data[:train_size]
            val_data = labeled_data[train_size:train_size + val_size]
            test_data = labeled_data[train_size + val_size:]
            
            # 擬合預處理器
            normalization_stats_path = self.output_dir / 'normalization_stats.json'
            preprocessor.fit(train_data, save_stats_path=str(normalization_stats_path))
            
            # 轉換數據
            train_sequences = preprocessor.transform(train_data)
            val_sequences = preprocessor.transform(val_data)
            test_sequences = preprocessor.transform(test_data)
            
            # 提取標籤
            seq_len = preprocessor.config.sequence_length
            train_labels = train_data['label'].iloc[seq_len-1:].values
            val_labels = val_data['label'].iloc[seq_len-1:].values
            test_labels = test_data['label'].iloc[seq_len-1:].values
            
            feature_engineering_results = {
                "train_samples": len(train_sequences),
                "val_samples": len(val_sequences),
                "test_samples": len(test_sequences),
                "feature_dim": train_sequences.shape[-1],
                "sequence_length": seq_len,
                "label_distribution": label_analysis
            }
            
            return StepOutput(
                success=True,
                data={
                    "train_data": (train_sequences, train_labels),
                    "val_data": (val_sequences, val_labels),
                    "test_data": (test_sequences, test_labels),
                    "preprocessor": preprocessor,
                    "feature_engineering_results": feature_engineering_results
                },
                metadata={"preprocessing_completed": True},
                next_step="parallel_model_training"
            )
            
        except Exception as e:
            logger.error(f"特徵工程失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_parallel_model_training(self, input_data: StepInput) -> StepOutput:
        """步驟 3: 並行模型訓練 - Agno 並行執行"""
        logger.info("🚀 開始並行模型訓練...")
        
        try:
            train_data = input_data.data["train_data"]
            val_data = input_data.data["val_data"]
            
            # Agno 並行執行 - 多個模型配置
            model_configs = self._generate_model_configurations()
            
            # 並行訓練任務
            training_tasks = []
            for i, model_config in enumerate(model_configs):
                task = self._train_single_model(
                    model_id=f"model_{i}",
                    config=model_config,
                    train_data=train_data,
                    val_data=val_data
                )
                training_tasks.append(task)
            
            # 等待所有訓練完成
            training_results = await asyncio.gather(*training_tasks, return_exceptions=True)
            
            # 處理訓練結果
            successful_models = []
            for i, result in enumerate(training_results):
                if isinstance(result, Exception):
                    logger.error(f"模型 {i} 訓練失敗: {result}")
                else:
                    successful_models.append(result)
            
            if not successful_models:
                return StepOutput(
                    success=False,
                    data={},
                    error_message="所有模型訓練都失敗了"
                )
            
            # 選擇最佳模型
            best_model = max(successful_models, key=lambda x: x["validation_accuracy"])
            
            parallel_training_results = {
                "models_trained": len(successful_models),
                "best_model_id": best_model["model_id"],
                "best_validation_acc": best_model["validation_accuracy"],
                "training_duration": best_model["training_duration"]
            }
            
            # 更新工作流程狀態
            self.workflow_state["models_trained"] = successful_models
            self.workflow_state["best_model_performance"] = best_model["validation_accuracy"]
            
            return StepOutput(
                success=True,
                data={
                    "trained_models": successful_models,
                    "best_model": best_model,
                    "parallel_training_results": parallel_training_results
                },
                metadata={"parallel_training_completed": True},
                next_step="model_evaluation"
            )
            
        except Exception as e:
            logger.error(f"並行訓練失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_model_evaluation(self, input_data: StepInput) -> StepOutput:
        """步驟 4: 模型性能評估"""
        logger.info("📊 執行模型性能評估...")
        
        try:
            best_model = input_data.data["best_model"]
            test_data = input_data.previous_results[1].data["test_data"]  # 從特徵工程步驟獲取
            
            # 載入最佳模型進行測試評估
            model = best_model["model"]
            test_sequences, test_labels = test_data
            
            # 創建測試數據載入器
            test_dataset = TLOBDataset(test_sequences, test_labels)
            test_loader = DataLoader(
                test_dataset,
                batch_size=self.config.get('batch_size', 64),
                shuffle=False
            )
            
            # 評估模型
            evaluation_metrics = await self._evaluate_model_performance(model, test_loader)
            
            # 風險指標評估
            risk_metrics = self._calculate_risk_metrics(evaluation_metrics)
            
            evaluation_results = {
                "test_accuracy": evaluation_metrics["accuracy"],
                "test_loss": evaluation_metrics["loss"],
                "classification_report": evaluation_metrics["classification_report"],
                "risk_metrics": risk_metrics,
                "evaluation_timestamp": datetime.now().isoformat()
            }
            
            return StepOutput(
                success=True,
                data={
                    "evaluation_results": evaluation_results,
                    "best_model": best_model
                },
                metadata={"evaluation_completed": True},
                next_step="quality_gate_check"
            )
            
        except Exception as e:
            logger.error(f"模型評估失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_quality_gate_check(self, input_data: StepInput) -> StepOutput:
        """步驟 5: 質量門檢查 - Agno 條件分支"""
        logger.info("🚪 執行質量門檢查...")
        
        try:
            evaluation_results = input_data.data["evaluation_results"]
            
            # 定義質量標準
            quality_thresholds = {
                "min_accuracy": self.config.get('min_accuracy', 0.55),
                "max_test_loss": self.config.get('max_test_loss', 1.0),
                "min_precision": self.config.get('min_precision', 0.50),
                "max_risk_score": self.config.get('max_risk_score', 0.3)
            }
            
            # 檢查每個質量指標
            quality_checks = {}
            quality_checks["accuracy_check"] = evaluation_results["test_accuracy"] >= quality_thresholds["min_accuracy"]
            quality_checks["loss_check"] = evaluation_results["test_loss"] <= quality_thresholds["max_test_loss"]
            
            # 風險檢查
            risk_score = evaluation_results["risk_metrics"]["overall_risk_score"]
            quality_checks["risk_check"] = risk_score <= quality_thresholds["max_risk_score"]
            
            # 精確度檢查
            precision = evaluation_results["classification_report"]["weighted avg"]["precision"]
            quality_checks["precision_check"] = precision >= quality_thresholds["min_precision"]
            
            # Agno 條件邏輯 - 決定下一步
            all_checks_passed = all(quality_checks.values())
            
            quality_gate_results = {
                "quality_checks": quality_checks,
                "thresholds": quality_thresholds,
                "overall_pass": all_checks_passed,
                "quality_score": sum(quality_checks.values()) / len(quality_checks)
            }
            
            if all_checks_passed:
                logger.info("✅ 質量門檢查通過，可以部署")
                self.workflow_state["deployment_ready"] = True
                return StepOutput(
                    success=True,
                    data={
                        "quality_gate_results": quality_gate_results,
                        "deployment_approved": True
                    },
                    metadata={"quality_gate_passed": True},
                    next_step="model_export"
                )
            else:
                logger.warning("⚠️ 質量門檢查失敗，需要重訓練")
                self.workflow_state["training_iteration"] += 1
                
                # Agno 條件分支 - 檢查重訓練次數
                max_retraining_attempts = self.config.get('max_retraining_attempts', 3)
                if self.workflow_state["training_iteration"] >= max_retraining_attempts:
                    return StepOutput(
                        success=False,
                        data=quality_gate_results,
                        error_message=f"達到最大重訓練次數 ({max_retraining_attempts}), 模型質量仍不達標",
                        next_step="end"
                    )
                
                return StepOutput(
                    success=True,
                    data={
                        "quality_gate_results": quality_gate_results,
                        "retraining_needed": True
                    },
                    metadata={"quality_gate_failed": True},
                    next_step="conditional_retraining"
                )
                
        except Exception as e:
            logger.error(f"質量門檢查失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_conditional_retraining(self, input_data: StepInput) -> StepOutput:
        """步驟 6: 條件式重訓練 - Agno 循環邏輯"""
        logger.info("🔄 執行條件式重訓練...")
        
        try:
            quality_gate_results = input_data.data["quality_gate_results"]
            failed_checks = [k for k, v in quality_gate_results["quality_checks"].items() if not v]
            
            logger.info(f"重訓練原因: {failed_checks}")
            
            # 根據失敗的檢查項目調整超參數
            adjusted_config = self._adjust_training_config(failed_checks)
            
            # 重新獲取訓練數據 (從之前的步驟結果)
            feature_engineering_result = input_data.previous_results[1]  # 特徵工程步驟
            train_data = feature_engineering_result.data["train_data"]
            val_data = feature_engineering_result.data["val_data"]
            
            # 重新訓練模型
            retraining_result = await self._train_single_model(
                model_id=f"retrained_model_{self.workflow_state['training_iteration']}",
                config=adjusted_config,
                train_data=train_data,
                val_data=val_data
            )
            
            retraining_results = {
                "retraining_iteration": self.workflow_state["training_iteration"],
                "adjusted_config": adjusted_config,
                "retraining_performance": retraining_result["validation_accuracy"],
                "improvement": retraining_result["validation_accuracy"] - self.workflow_state["best_model_performance"]
            }
            
            # 更新最佳模型性能
            if retraining_result["validation_accuracy"] > self.workflow_state["best_model_performance"]:
                self.workflow_state["best_model_performance"] = retraining_result["validation_accuracy"]
            
            return StepOutput(
                success=True,
                data={
                    "retrained_model": retraining_result,
                    "retraining_results": retraining_results
                },
                metadata={"retraining_completed": True},
                next_step="model_evaluation"  # 回到評估步驟
            )
            
        except Exception as e:
            logger.error(f"條件式重訓練失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_model_export(self, input_data: StepInput) -> StepOutput:
        """步驟 7: 模型導出"""
        logger.info("📦 執行模型導出...")
        
        try:
            # 獲取最佳模型 (可能來自重訓練或原始訓練)
            if "retrained_model" in input_data.data:
                best_model = input_data.data["retrained_model"]
            else:
                best_model = input_data.previous_results[2].data["best_model"]  # 從並行訓練步驟
            
            # 導出模型
            exporter = ModelExporter(str(self.output_dir / 'exported_models'))
            
            model = best_model["model"]
            symbol = self.config.get('symbol', 'BTCUSDT')
            
            # 創建範例輸入
            input_dim = model.config.input_dim
            seq_len = model.config.sequence_length
            example_input = torch.randn(1, seq_len, input_dim)
            
            # 導出為 TorchScript
            export_info = exporter.export_for_rust(
                model,
                f"tlob_{symbol}_{self.session_id}",
                example_input,
                rust_config=self.config
            )
            
            export_results = {
                "model_path": export_info["model_path"],
                "model_size_mb": export_info.get("model_size_mb", 0),
                "export_timestamp": datetime.now().isoformat(),
                "model_id": best_model["model_id"],
                "performance": best_model["validation_accuracy"]
            }
            
            return StepOutput(
                success=True,
                data={
                    "export_results": export_results,
                    "exported_model_path": export_info["model_path"]
                },
                metadata={"model_exported": True},
                next_step="hft_integration"
            )
            
        except Exception as e:
            logger.error(f"模型導出失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    async def step_hft_integration(self, input_data: StepInput) -> StepOutput:
        """步驟 8: HFT 系統整合"""
        logger.info("🔗 執行 HFT 系統整合...")
        
        try:
            export_results = input_data.data["export_results"]
            model_path = export_results["model_path"]
            
            # 通過 Redis 通知 Rust HFT 系統
            import redis
            redis_client = redis.Redis(host='localhost', port=6379, decode_responses=True)
            
            # 發送模型部署命令
            deployment_command = {
                "type": "MODEL_DEPLOYMENT",
                "model_path": model_path,
                "model_id": export_results["model_id"],
                "symbol": self.config.get('symbol', 'BTCUSDT'),
                "performance": export_results["performance"],
                "deployment_mode": "shadow",  # 先影子模式部署
                "timestamp": datetime.now().isoformat(),
                "session_id": self.session_id
            }
            
            redis_client.publish("hft:model_deployment", json.dumps(deployment_command))
            
            # 等待部署確認 (簡化版本)
            await asyncio.sleep(5)  # 實際應該等待 HFT 系統回應
            
            integration_results = {
                "deployment_command_sent": True,
                "model_path": model_path,
                "deployment_mode": "shadow",
                "integration_timestamp": datetime.now().isoformat(),
                "hft_notification_sent": True
            }
            
            # 標記部署完成
            self.workflow_state["deployment_ready"] = True
            
            return StepOutput(
                success=True,
                data={
                    "integration_results": integration_results,
                    "deployment_completed": True
                },
                metadata={"hft_integration_completed": True},
                next_step="end"  # 工作流程結束
            )
            
        except Exception as e:
            logger.error(f"HFT 系統整合失敗: {e}")
            return StepOutput(
                success=False,
                data={},
                error_message=str(e)
            )
    
    # 輔助方法
    def _calculate_data_quality(self, raw_data: pd.DataFrame) -> float:
        """計算數據質量分數"""
        if raw_data.empty:
            return 0.0
        
        # 檢查缺失值
        missing_ratio = raw_data.isnull().sum().sum() / (raw_data.shape[0] * raw_data.shape[1])
        
        # 檢查數據範圍合理性
        numeric_cols = raw_data.select_dtypes(include=[np.number]).columns
        outlier_ratio = 0.0
        
        for col in numeric_cols:
            q1 = raw_data[col].quantile(0.25)
            q3 = raw_data[col].quantile(0.75)
            iqr = q3 - q1
            lower_bound = q1 - 1.5 * iqr
            upper_bound = q3 + 1.5 * iqr
            outliers = ((raw_data[col] < lower_bound) | (raw_data[col] > upper_bound)).sum()
            outlier_ratio += outliers / len(raw_data)
        
        outlier_ratio /= len(numeric_cols) if numeric_cols.any() else 1
        
        # 計算綜合質量分數
        quality_score = 1.0 - (missing_ratio * 0.5 + outlier_ratio * 0.3)
        return max(0.0, min(1.0, quality_score))
    
    def _generate_model_configurations(self) -> List[Dict[str, Any]]:
        """生成多個模型配置進行並行訓練"""
        base_config = self.config.copy()
        
        configurations = [
            # 配置 1: 基礎配置
            {**base_config, "d_model": 256, "nhead": 8, "num_layers": 4, "dropout": 0.1},
            # 配置 2: 更大模型
            {**base_config, "d_model": 512, "nhead": 8, "num_layers": 6, "dropout": 0.15},
            # 配置 3: 更深模型
            {**base_config, "d_model": 256, "nhead": 8, "num_layers": 8, "dropout": 0.2},
            # 配置 4: 不同注意力頭數
            {**base_config, "d_model": 384, "nhead": 12, "num_layers": 4, "dropout": 0.1}
        ]
        
        return configurations
    
    async def _train_single_model(
        self, 
        model_id: str, 
        config: Dict[str, Any], 
        train_data: Tuple[np.ndarray, np.ndarray],
        val_data: Tuple[np.ndarray, np.ndarray]
    ) -> Dict[str, Any]:
        """訓練單個模型"""
        logger.info(f"開始訓練模型: {model_id}")
        
        train_sequences, train_labels = train_data
        val_sequences, val_labels = val_data
        
        # 創建數據載入器
        train_dataset = TLOBDataset(train_sequences, train_labels)
        val_dataset = TLOBDataset(val_sequences, val_labels)
        
        train_loader = DataLoader(train_dataset, batch_size=config['batch_size'], shuffle=True)
        val_loader = DataLoader(val_dataset, batch_size=config['batch_size'], shuffle=False)
        
        # 創建模型
        model_config = TLOBConfig(
            input_dim=train_sequences.shape[-1],
            sequence_length=config.get('sequence_length', 100),
            d_model=config.get('d_model', 256),
            nhead=config.get('nhead', 8),
            num_layers=config.get('num_layers', 4),
            dim_feedforward=config.get('dim_feedforward', 1024),
            num_classes=3,
            dropout=config.get('dropout', 0.1)
        )
        
        model = TLOB(model_config)
        device = torch.device('cuda' if torch.cuda.is_available() else 'cpu')
        model.to(device)
        
        # 設置訓練器
        optimizer = torch.optim.AdamW(
            model.parameters(),
            lr=config.get('learning_rate', 1e-4),
            weight_decay=config.get('weight_decay', 0.01)
        )
        criterion = nn.CrossEntropyLoss()
        
        # 訓練循環 (簡化版本)
        model.train()
        start_time = time.time()
        best_val_acc = 0.0
        
        epochs = config.get('epochs', 20)  # 並行訓練時減少 epoch
        
        for epoch in range(epochs):
            # 訓練
            total_loss = 0.0
            for sequences, labels in train_loader:
                sequences, labels = sequences.to(device), labels.to(device)
                
                optimizer.zero_grad()
                outputs = model(sequences)
                loss = model.compute_loss(outputs['logits'], labels)
                loss.backward()
                optimizer.step()
                
                total_loss += loss.item()
            
            # 驗證
            model.eval()
            val_correct = 0
            val_total = 0
            
            with torch.no_grad():
                for sequences, labels in val_loader:
                    sequences, labels = sequences.to(device), labels.to(device)
                    outputs = model(sequences)
                    predictions = outputs['predictions']
                    val_correct += (predictions == labels).sum().item()
                    val_total += labels.size(0)
            
            val_accuracy = val_correct / val_total
            if val_accuracy > best_val_acc:
                best_val_acc = val_accuracy
            
            model.train()
        
        training_duration = time.time() - start_time
        
        return {
            "model_id": model_id,
            "model": model,
            "validation_accuracy": best_val_acc,
            "training_duration": training_duration,
            "config": config
        }
    
    async def _evaluate_model_performance(self, model: TLOB, test_loader: DataLoader) -> Dict[str, Any]:
        """評估模型性能"""
        model.eval()
        device = next(model.parameters()).device
        
        total_loss = 0.0
        correct = 0
        total = 0
        all_predictions = []
        all_labels = []
        
        with torch.no_grad():
            for sequences, labels in test_loader:
                sequences, labels = sequences.to(device), labels.to(device)
                
                outputs = model(sequences)
                loss = model.compute_loss(outputs['logits'], labels)
                
                total_loss += loss.item()
                predictions = outputs['predictions']
                correct += (predictions == labels).sum().item()
                total += labels.size(0)
                
                all_predictions.extend(predictions.cpu().numpy())
                all_labels.extend(labels.cpu().numpy())
        
        # 計算詳細指標
        from sklearn.metrics import classification_report
        
        classification_report_dict = classification_report(
            all_labels,
            all_predictions,
            target_names=['Down', 'Stable', 'Up'],
            output_dict=True
        )
        
        return {
            "loss": total_loss / len(test_loader),
            "accuracy": correct / total,
            "classification_report": classification_report_dict,
            "predictions": all_predictions,
            "labels": all_labels
        }
    
    def _calculate_risk_metrics(self, evaluation_metrics: Dict[str, Any]) -> Dict[str, Any]:
        """計算風險指標"""
        classification_report = evaluation_metrics["classification_report"]
        
        # 計算類別不平衡風險
        class_supports = [classification_report[cls]["support"] for cls in ["Down", "Stable", "Up"]]
        total_support = sum(class_supports)
        class_ratios = [support / total_support for support in class_supports]
        imbalance_risk = max(class_ratios) - min(class_ratios)  # 最大與最小類別比例差
        
        # 計算精確度風險
        precisions = [classification_report[cls]["precision"] for cls in ["Down", "Stable", "Up"]]
        precision_risk = 1.0 - min(precisions)  # 最低精確度的風險
        
        # 計算召回率風險
        recalls = [classification_report[cls]["recall"] for cls in ["Down", "Stable", "Up"]]
        recall_risk = 1.0 - min(recalls)  # 最低召回率的風險
        
        # 綜合風險分數
        overall_risk_score = (imbalance_risk * 0.3 + precision_risk * 0.4 + recall_risk * 0.3)
        
        return {
            "imbalance_risk": imbalance_risk,
            "precision_risk": precision_risk,
            "recall_risk": recall_risk,
            "overall_risk_score": overall_risk_score,
            "risk_level": "high" if overall_risk_score > 0.5 else "medium" if overall_risk_score > 0.3 else "low"
        }
    
    def _adjust_training_config(self, failed_checks: List[str]) -> Dict[str, Any]:
        """根據失敗的檢查項目調整訓練配置"""
        adjusted_config = self.config.copy()
        
        if "accuracy_check" in failed_checks:
            # 增加模型複雜度
            adjusted_config["d_model"] = min(adjusted_config.get("d_model", 256) * 1.5, 512)
            adjusted_config["num_layers"] = min(adjusted_config.get("num_layers", 4) + 1, 8)
        
        if "loss_check" in failed_checks:
            # 降低學習率
            adjusted_config["learning_rate"] = adjusted_config.get("learning_rate", 1e-4) * 0.5
            # 增加訓練輪數
            adjusted_config["epochs"] = min(adjusted_config.get("epochs", 50) + 20, 100)
        
        if "risk_check" in failed_checks:
            # 增加正則化
            adjusted_config["dropout"] = min(adjusted_config.get("dropout", 0.1) + 0.05, 0.3)
            adjusted_config["weight_decay"] = min(adjusted_config.get("weight_decay", 0.01) * 2, 0.1)
        
        if "precision_check" in failed_checks:
            # 調整批次大小
            adjusted_config["batch_size"] = max(adjusted_config.get("batch_size", 64) // 2, 16)
        
        return adjusted_config
    
    async def execute_workflow(self) -> Dict[str, Any]:
        """執行完整的工作流程 - Agno Workflow 主循環"""
        logger.info(f"🚀 開始執行 ML 訓練工作流程: {self.name}")
        
        workflow_start_time = time.time()
        step_results = []
        current_step = "data_validation"
        
        # 創建初始輸入
        step_input = StepInput(
            data={"workflow_config": self.config},
            context={"session_id": self.session_id},
            previous_results=[]
        )
        
        while current_step and current_step != "end":
            if current_step not in self.step_definitions:
                logger.error(f"未定義的步驟: {current_step}")
                break
            
            step_def = self.step_definitions[current_step]
            logger.info(f"📋 執行步驟: {step_def['name']}")
            
            # 更新工作流程狀態
            self.workflow_state["current_step"] = current_step
            
            try:
                # 執行步驟
                step_start_time = time.time()
                step_output = await step_def["function"](step_input)
                step_duration = time.time() - step_start_time
                
                # 記錄步驟結果
                step_result = {
                    "step_name": current_step,
                    "step_display_name": step_def["name"],
                    "success": step_output.success,
                    "duration": step_duration,
                    "data": step_output.data,
                    "metadata": step_output.metadata,
                    "error_message": step_output.error_message,
                    "timestamp": datetime.now().isoformat()
                }
                
                step_results.append(step_result)
                
                if step_output.success:
                    logger.info(f"✅ 步驟完成: {step_def['name']} ({step_duration:.2f}s)")
                    
                    # 準備下一步的輸入
                    next_step_input = StepInput(
                        data=step_output.data,
                        context={"session_id": self.session_id, "current_step": current_step},
                        previous_results=step_results
                    )
                    step_input = next_step_input
                    
                    # 移動到下一步
                    current_step = step_output.next_step
                    
                else:
                    logger.error(f"❌ 步驟失敗: {step_def['name']} - {step_output.error_message}")
                    break
                    
            except Exception as e:
                logger.error(f"💥 步驟執行異常: {current_step} - {e}")
                step_result = {
                    "step_name": current_step,
                    "step_display_name": step_def["name"],
                    "success": False,
                    "duration": time.time() - step_start_time,
                    "error_message": str(e),
                    "timestamp": datetime.now().isoformat()
                }
                step_results.append(step_result)
                break
        
        # 工作流程完成
        workflow_duration = time.time() - workflow_start_time
        successful_steps = sum(1 for result in step_results if result["success"])
        
        workflow_results = {
            "workflow_name": self.name,
            "session_id": self.session_id,
            "success": current_step == "end",
            "total_duration": workflow_duration,
            "steps_executed": len(step_results),
            "successful_steps": successful_steps,
            "workflow_state": self.workflow_state,
            "output_directory": str(self.output_dir),
            "step_results": step_results,
            "completion_timestamp": datetime.now().isoformat()
        }
        
        # 保存工作流程結果
        results_path = self.output_dir / 'workflow_results.json'
        with open(results_path, 'w') as f:
            json.dump(workflow_results, f, indent=2, default=str)
        
        if workflow_results["success"]:
            logger.info(f"🎉 工作流程成功完成! 耗時: {workflow_duration:.2f}s")
            logger.info(f"📊 成功步驟: {successful_steps}/{len(step_results)}")
            logger.info(f"📁 結果保存至: {self.output_dir}")
        else:
            logger.error(f"💔 工作流程失敗! 耗時: {workflow_duration:.2f}s")
        
        return workflow_results

# 使用範例
async def main():
    """主函數 - 演示 Agno Workflow ML 訓練"""
    
    # 配置
    config = {
        "symbol": "BTCUSDT",
        "data_start_time": "2024-01-01 00:00:00",
        "data_end_time": "2024-01-31 23:59:59",
        "d_model": 256,
        "nhead": 8,
        "num_layers": 4,
        "dropout": 0.1,
        "epochs": 50,
        "batch_size": 64,
        "learning_rate": 1e-4,
        "weight_decay": 0.01,
        "output_dir": "./outputs/ml_training_workflow_v3",
        
        # 質量門檻
        "min_accuracy": 0.55,
        "max_test_loss": 1.0,
        "min_precision": 0.50,
        "max_risk_score": 0.3,
        "max_retraining_attempts": 3,
        
        # 數據分割
        "train_ratio": 0.7,
        "val_ratio": 0.15
    }
    
    # 創建並執行工作流程
    workflow = MLTrainingWorkflow(config)
    results = await workflow.execute_workflow()
    
    print("\n" + "="*80)
    print("🎯 ML Training Workflow v3.0 執行完成!")
    print("="*80)
    print(f"成功: {'✅' if results['success'] else '❌'}")
    print(f"耗時: {results['total_duration']:.2f} 秒")
    print(f"步驟: {results['successful_steps']}/{results['steps_executed']}")
    
    if results['success']:
        print(f"模型已部署: {'✅' if results['workflow_state']['deployment_ready'] else '❌'}")
        print(f"最佳性能: {results['workflow_state']['best_model_performance']:.4f}")
    
    return results

if __name__ == "__main__":
    result = asyncio.run(main())