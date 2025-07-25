#!/usr/bin/env python3
"""
ML Workflow Agent - DL系統工作流程管理一級代理
==========================================

職責：數據收集、模型訓練、評估部署、實驗管理
架構：管理4個二級代理的一級代理

基於 ClickHouse 數據湖 + TLOB Transformer + 智能工作流程編排
整合 real_clickhouse_collector.py + production_agno_workflow_v2.py 的功能
"""

import asyncio
import logging
import time
import subprocess
import os
import json
from datetime import datetime, timedelta
from typing import Dict, List, Any, Optional
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

# 導入 Agno Framework
from agno.agent import Agent
from agno.models.ollama import Ollama

# 導入現有的工具
import sys
sys.path.append(os.path.join(os.path.dirname(__file__), 'agno_hft'))
from rust_hft_tools import RustHFTTools

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class ExecutionMode(Enum):
    """執行模式 (來自 genuine_hft_pipeline.py 的設計)"""
    REAL = "real"      # 真實模式：真正調用 Rust 系統
    DEMO = "demo"      # 演示模式：明確的演示
    RESEARCH = "research"  # 研究模式：專注於實驗

@dataclass
class MLTask:
    """ML 任務定義"""
    task_id: str
    task_type: str  # data_collection, training, evaluation, deployment
    symbol: str
    parameters: Dict[str, Any]
    status: str = "pending"
    created_at: float = None
    completed_at: float = None
    result: Dict[str, Any] = None

class DataCollectionAgent:
    """二級代理：數據收集編排 (整合 real_clickhouse_collector.py 功能)"""
    
    def __init__(self, parent_agent):
        self.parent = parent_agent
        self.rust_hft_path = "./rust_hft"
        
    async def collect_market_data(self, symbol: str, duration_minutes: int, mode: ExecutionMode) -> Dict[str, Any]:
        """智能數據收集編排"""
        logger.info(f"📦 開始數據收集 - {symbol} ({duration_minutes}分鐘, 模式: {mode.value})")
        
        if mode == ExecutionMode.REAL:
            return await self._collect_real_data(symbol, duration_minutes)
        else:
            return await self._collect_demo_data(symbol, duration_minutes)
    
    async def _collect_real_data(self, symbol: str, duration_minutes: int) -> Dict[str, Any]:
        """真實數據收集 (基於 real_clickhouse_collector.py)"""
        try:
            logger.info(f"🔄 執行真實 ClickHouse 數據收集...")
            
            # 使用 comprehensive_db_stress_test 進行真實數據收集
            cmd = [
                "cargo", "run", "--release", "--example", "comprehensive_db_stress_test",
                "--", "--duration", str(duration_minutes), "--symbols", "1", "--verbose"
            ]
            
            logger.info(f"   執行命令: {' '.join(cmd)}")
            logger.info(f"   工作目錄: {self.rust_hft_path}")
            
            start_time = time.time()
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=self.rust_hft_path,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            # 實時監控數據收集進度
            collected_records = 0
            database_writes = 0
            
            while True:
                line = await process.stdout.readline()
                if not line:
                    break
                
                output = line.decode().strip()
                if "💾 写入器" in output or "記錄寫入完成" in output:
                    logger.info(f"[ClickHouse寫入] {output}")
                    database_writes += 1
                elif "消息接收" in output or "数据收集" in output:
                    logger.info(f"[數據收集] {output}")
                    
                    # 提取記錄數量
                    try:
                        import re
                        match = re.search(r'(\d+).*記錄|(\d+).*messages', output)
                        if match:
                            num = int(match.group(1) or match.group(2) or 0)
                            collected_records = max(collected_records, num)
                    except:
                        pass
            
            await process.wait()
            execution_time = time.time() - start_time
            
            # 驗證 ClickHouse 數據
            verification = await self._verify_clickhouse_data(symbol)
            
            if process.returncode == 0:
                logger.info(f"✅ 真實數據收集完成！")
                return {
                    "status": "success",
                    "mode": "real",
                    "symbol": symbol,
                    "duration_minutes": duration_minutes,
                    "records_collected": collected_records,
                    "database_writes": database_writes,
                    "execution_time_seconds": execution_time,
                    "data_verification": verification,
                    "data_source": "real_clickhouse_comprehensive_test"
                }
            else:
                return {
                    "status": "failed",
                    "error": f"Process failed with code {process.returncode}",
                    "collected_records": collected_records
                }
                
        except Exception as e:
            logger.error(f"❌ 真實數據收集異常: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def _collect_demo_data(self, symbol: str, duration_minutes: int) -> Dict[str, Any]:
        """演示數據收集"""
        logger.info(f"🎭 演示模式：模擬 {symbol} 數據收集 ({duration_minutes}分鐘)")
        
        # 模擬收集過程
        await asyncio.sleep(2)
        
        return {
            "status": "success",
            "mode": "demo",
            "symbol": symbol,
            "duration_minutes": duration_minutes,
            "records_collected": duration_minutes * 1000,  # 模擬每分鐘1000條記錄
            "database_writes": duration_minutes * 10,
            "execution_time_seconds": 2.0,
            "data_source": "demo_simulation"
        }
    
    async def _verify_clickhouse_data(self, symbol: str) -> Dict[str, Any]:
        """驗證 ClickHouse 中的數據"""
        try:
            import requests
            
            # 簡單查詢來驗證數據
            url = 'http://localhost:8123'
            query = f"SELECT count(*) FROM hft_db.enhanced_market_data WHERE symbol = '{symbol}'"
            
            response = requests.get(url, params={'query': query}, timeout=10)
            if response.status_code == 200:
                count = int(response.text.strip())
                return {
                    "verified": True,
                    "record_count": count,
                    "query_success": True
                }
            else:
                return {"verified": False, "error": f"Query failed: {response.status_code}"}
                
        except Exception as e:
            return {"verified": False, "error": str(e)}

class ModelTrainingAgent:
    """二級代理：TLOB 模型訓練管理"""
    
    def __init__(self, parent_agent):
        self.parent = parent_agent
        self.rust_tools = RustHFTTools()
        
    async def train_tlob_model(self, symbol: str, training_config: Dict[str, Any]) -> Dict[str, Any]:
        """TLOB Transformer 模型訓練"""
        logger.info(f"🤖 開始 TLOB 模型訓練 - {symbol}")
        
        try:
            # 從 ClickHouse 查詢訓練數據
            data_query_result = await self._query_training_data(symbol, training_config)
            
            if not data_query_result.get("success"):
                return {
                    "status": "failed",
                    "error": "Training data query failed",
                    "details": data_query_result
                }
            
            # 執行實際訓練
            training_hours = training_config.get("training_hours", 24)
            training_result = await self.rust_tools.train_model(symbol, training_hours)
            
            if training_result.get("success"):
                # 增強訓練結果
                enhanced_result = {
                    **training_result,
                    "model_type": "tlob_transformer",
                    "training_data_source": "clickhouse",
                    "data_records": data_query_result.get("record_count", 0),
                    "training_config": training_config
                }
                
                logger.info(f"✅ TLOB 模型訓練完成: {enhanced_result.get('model_path')}")
                return {
                    "status": "success",
                    "result": enhanced_result
                }
            else:
                return {
                    "status": "failed",
                    "error": "Rust training failed",
                    "details": training_result
                }
                
        except Exception as e:
            logger.error(f"❌ TLOB 模型訓練失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def _query_training_data(self, symbol: str, config: Dict[str, Any]) -> Dict[str, Any]:
        """從 ClickHouse 查詢訓練數據"""
        try:
            import requests
            
            # 構建數據查詢
            lookback_hours = config.get("lookback_hours", 24)
            end_time = datetime.now()
            start_time = end_time - timedelta(hours=lookback_hours)
            
            query = f"""
            SELECT count(*) as record_count, 
                   min(timestamp) as min_time, 
                   max(timestamp) as max_time,
                   avg(data_quality_score) as avg_quality
            FROM hft_db.enhanced_market_data 
            WHERE symbol = '{symbol}' 
            AND toDateTime(timestamp / 1000000) BETWEEN '{start_time.isoformat()}' AND '{end_time.isoformat()}'
            """
            
            response = requests.get(
                'http://localhost:8123',
                params={'query': query, 'format': 'JSON'},
                timeout=10
            )
            
            if response.status_code == 200:
                data = response.json()['data'][0] if response.json()['data'] else {}
                
                return {
                    "success": True,
                    "record_count": data.get('record_count', 0),
                    "time_range": {
                        "start": start_time.isoformat(),
                        "end": end_time.isoformat()
                    },
                    "data_quality": data.get('avg_quality', 0)
                }
            else:
                return {"success": False, "error": f"Query failed: {response.status_code}"}
                
        except Exception as e:
            return {"success": False, "error": str(e)}

class ModelEvaluationAgent:
    """二級代理：模型評估和部署決策"""
    
    def __init__(self, parent_agent):
        self.parent = parent_agent
        self.rust_tools = RustHFTTools()
        
    async def evaluate_model(self, model_path: str, symbol: str, evaluation_config: Dict[str, Any]) -> Dict[str, Any]:
        """多維度模型評估"""
        logger.info(f"📈 開始模型評估 - {model_path}")
        
        try:
            # 執行基礎評估
            base_evaluation = await self.rust_tools.evaluate_model(model_path, symbol)
            
            if not base_evaluation.get("success"):
                return {
                    "status": "failed",
                    "error": "Base evaluation failed",
                    "details": base_evaluation
                }
            
            # 增強評估分析
            enhanced_analysis = await self._perform_enhanced_analysis(
                base_evaluation, symbol, evaluation_config
            )
            
            # 部署可行性判斷
            deployment_decision = self._assess_deployment_readiness(
                base_evaluation, enhanced_analysis
            )
            
            return {
                "status": "success",
                "base_evaluation": base_evaluation,
                "enhanced_analysis": enhanced_analysis,
                "deployment_decision": deployment_decision,
                "evaluation_timestamp": time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ 模型評估失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def _perform_enhanced_analysis(self, base_eval: Dict, symbol: str, config: Dict) -> Dict[str, Any]:
        """執行增強分析"""
        
        # 從 ClickHouse 獲取歷史回測數據
        backtest_data = await self._get_backtest_data(symbol)
        
        # 計算風險調整收益指標
        performance_metrics = {
            "sharpe_ratio": base_eval.get("sharpe_ratio", 0),
            "max_drawdown": base_eval.get("max_drawdown", 0),
            "win_rate": base_eval.get("win_rate", 0),
            "profit_factor": base_eval.get("profit_factor", 0),
            "accuracy": base_eval.get("accuracy", 0)
        }
        
        # 市場適應性分析
        market_adaptation = {
            "volatility_adaptation": self._analyze_volatility_adaptation(backtest_data),
            "trend_following": self._analyze_trend_following(backtest_data),
            "liquidity_sensitivity": self._analyze_liquidity_sensitivity(backtest_data)
        }
        
        return {
            "performance_metrics": performance_metrics,
            "market_adaptation": market_adaptation,
            "backtest_data_quality": backtest_data.get("quality", "unknown"),
            "analysis_timestamp": time.time()
        }
    
    def _assess_deployment_readiness(self, base_eval: Dict, enhanced: Dict) -> Dict[str, Any]:
        """評估部署可行性"""
        
        # 部署標準
        criteria = {
            "min_sharpe_ratio": 1.5,
            "max_drawdown": 0.05,
            "min_accuracy": 0.60,
            "min_win_rate": 0.52
        }
        
        performance = enhanced["performance_metrics"]
        
        # 檢查每個標準
        checks = {}
        for criterion, threshold in criteria.items():
            metric_key = criterion.replace("min_", "").replace("max_", "")
            current_value = performance.get(metric_key, 0)
            
            if "min_" in criterion:
                checks[criterion] = current_value >= threshold
            else:  # max_
                checks[criterion] = current_value <= threshold
        
        # 總體決策
        all_passed = all(checks.values())
        passed_count = sum(checks.values())
        
        decision = {
            "deployment_approved": all_passed,
            "criteria_checks": checks,
            "criteria_passed": f"{passed_count}/{len(criteria)}",
            "confidence_level": passed_count / len(criteria),
            "recommendation": "DEPLOY" if all_passed else "RETRAIN"
        }
        
        if not all_passed:
            failed_criteria = [k for k, v in checks.items() if not v]
            decision["failed_criteria"] = failed_criteria
            decision["improvement_suggestions"] = self._generate_improvement_suggestions(failed_criteria)
        
        return decision
    
    def _generate_improvement_suggestions(self, failed_criteria: List[str]) -> List[str]:
        """生成改進建議"""
        suggestions = []
        
        for criterion in failed_criteria:
            if "sharpe_ratio" in criterion:
                suggestions.append("增加特徵工程的複雜性，優化風險調整收益")
            elif "drawdown" in criterion:
                suggestions.append("加強止損邏輯，減少最大回撤")
            elif "accuracy" in criterion:
                suggestions.append("增加訓練數據量，改進模型架構")
            elif "win_rate" in criterion:
                suggestions.append("調整交易信號閾值，提高勝率")
        
        return suggestions
    
    async def _get_backtest_data(self, symbol: str) -> Dict[str, Any]:
        """獲取回測數據"""
        # 簡化版本，實際可以從 ClickHouse 查詢歷史表現數據
        return {"quality": "good", "sample_size": 10000}
    
    def _analyze_volatility_adaptation(self, data: Dict) -> float:
        """分析波動性適應能力"""
        return 0.75  # 簡化版本
    
    def _analyze_trend_following(self, data: Dict) -> float:
        """分析趨勢跟隨能力"""
        return 0.68
    
    def _analyze_liquidity_sensitivity(self, data: Dict) -> float:
        """分析流動性敏感性"""
        return 0.82

class ExperimentAgent:
    """二級代理：A/B 測試和實驗管理"""
    
    def __init__(self, parent_agent):
        self.parent = parent_agent
        self.experiments = {}
        
    async def create_ab_experiment(self, experiment_config: Dict[str, Any]) -> Dict[str, Any]:
        """創建 A/B 測試實驗"""
        experiment_id = f"exp_{int(time.time())}"
        
        experiment = {
            "id": experiment_id,
            "name": experiment_config.get("name"),
            "symbol": experiment_config.get("symbol"),
            "model_a": experiment_config.get("model_a"),
            "model_b": experiment_config.get("model_b"),
            "traffic_split": experiment_config.get("traffic_split", 0.5),
            "duration_hours": experiment_config.get("duration_hours", 24),
            "status": "created",
            "created_at": time.time(),
            "metrics": {
                "model_a": {"trades": 0, "pnl": 0, "sharpe": 0},
                "model_b": {"trades": 0, "pnl": 0, "sharpe": 0}
            }
        }
        
        self.experiments[experiment_id] = experiment
        
        logger.info(f"🔬 創建 A/B 實驗: {experiment_id}")
        return {
            "status": "success",
            "experiment_id": experiment_id,
            "experiment": experiment
        }
    
    async def monitor_experiment(self, experiment_id: str) -> Dict[str, Any]:
        """監控實驗進度"""
        if experiment_id not in self.experiments:
            return {"status": "error", "error": "Experiment not found"}
        
        experiment = self.experiments[experiment_id]
        
        # 模擬實驗監控
        elapsed_hours = (time.time() - experiment["created_at"]) / 3600
        progress = min(elapsed_hours / experiment["duration_hours"], 1.0)
        
        # 更新模擬指標
        if progress > 0.1:  # 10% 進度後開始有數據
            experiment["metrics"]["model_a"]["trades"] = int(progress * 100)
            experiment["metrics"]["model_b"]["trades"] = int(progress * 95)
            experiment["metrics"]["model_a"]["pnl"] = progress * 150.5
            experiment["metrics"]["model_b"]["pnl"] = progress * 142.3
        
        return {
            "status": "success",
            "experiment_id": experiment_id,
            "progress": progress,
            "metrics": experiment["metrics"],
            "is_complete": progress >= 1.0
        }
    
    async def analyze_experiment_results(self, experiment_id: str) -> Dict[str, Any]:
        """分析實驗結果"""
        if experiment_id not in self.experiments:
            return {"status": "error", "error": "Experiment not found"}
        
        experiment = self.experiments[experiment_id]
        metrics_a = experiment["metrics"]["model_a"]
        metrics_b = experiment["metrics"]["model_b"]
        
        # 簡化的統計分析
        pnl_diff = metrics_a["pnl"] - metrics_b["pnl"]
        pnl_improvement = (pnl_diff / metrics_b["pnl"]) * 100 if metrics_b["pnl"] > 0 else 0
        
        winner = "model_a" if pnl_diff > 0 else "model_b"
        confidence = abs(pnl_improvement) / 10  # 簡化的置信度計算
        
        return {
            "status": "success",
            "experiment_id": experiment_id,
            "winner": winner,
            "pnl_improvement_pct": abs(pnl_improvement),
            "confidence_level": min(confidence, 0.95),
            "recommendation": "DEPLOY_WINNER" if confidence > 0.8 else "EXTEND_EXPERIMENT",
            "detailed_metrics": {
                "model_a": metrics_a,
                "model_b": metrics_b
            }
        }

class MLWorkflowAgent:
    """一級代理：ML 工作流程管理"""
    
    def __init__(self, mode: ExecutionMode = ExecutionMode.REAL):
        self.mode = mode
        self.agent_id = f"ml_workflow_{int(time.time())}"
        
        # 初始化 Agno Agent (simplified for testing)
        self.agent = Agent(
            name="ML Workflow Manager",
            description=self._get_instructions()
        )
        
        # 初始化二級代理
        self.data_collection_agent = DataCollectionAgent(self)
        self.training_agent = ModelTrainingAgent(self)
        self.evaluation_agent = ModelEvaluationAgent(self)
        self.experiment_agent = ExperimentAgent(self)
        
        self.tasks = []
        
        logger.info(f"🧠 ML Workflow Agent 初始化完成 (模式: {mode.value})")
    
    def _get_instructions(self) -> str:
        return f"""
        你是專業的 ML 工作流程管理專家，負責協調整個深度學習生命週期。

        🎯 核心職責：
        1. **數據收集編排**: 10分鐘市場數據 → ClickHouse 高質量存儲
        2. **TLOB 模型訓練**: Transformer 架構 + LOB 特徵工程
        3. **多維度評估**: 風險調整收益 + 市場適應性分析  
        4. **智能實驗管理**: A/B 測試 + 藍綠部署策略

        🗄️ 數據架構理解：
        - ClickHouse: 歷史數據湖 (lob_depth, trade_data, ml_features)
        - 實時特徵: SIMD 優化的 OBI、價差、流動性指標
        - 預測目標: 3s/5s/10s 未來價格變化

        🤖 TLOB 模型特性：
        - Transformer 架構適配 LOB 時間序列
        - 多時間窗口特徵融合 (30s/60s/120s/300s)
        - 目標延遲 <50μs 推理時間

        📊 評估標準：
        - Sharpe 比率 >1.5
        - 最大回撤 <5%
        - 預測準確率 >60%
        - 勝率 >52%

        💡 智能決策原則：
        - 數據質量優先於數據數量
        - 穩健性優於複雜性
        - 可解釋性與性能平衡

        當前運行模式: {self.mode.value}
        """
    
    async def execute_complete_ml_workflow(self, symbol: str, workflow_config: Dict[str, Any]) -> Dict[str, Any]:
        """執行完整的 ML 工作流程：數據收集 → 訓練 → 評估 → 部署"""
        
        workflow_id = f"ml_workflow_{symbol}_{int(time.time())}"
        logger.info(f"🚀 開始完整 ML 工作流程: {workflow_id}")
        
        workflow_result = {
            "workflow_id": workflow_id,
            "symbol": symbol,
            "config": workflow_config,
            "start_time": time.time(),
            "stages": {},
            "status": "in_progress"
        }
        
        try:
            # 階段1: 數據收集
            logger.info("📦 階段1: 智能數據收集...")
            data_collection_result = await self.data_collection_agent.collect_market_data(
                symbol=symbol,
                duration_minutes=workflow_config.get("data_collection_minutes", 10),
                mode=self.mode
            )
            workflow_result["stages"]["data_collection"] = data_collection_result
            
            if data_collection_result["status"] != "success":
                workflow_result["status"] = "failed"
                workflow_result["error"] = "Data collection failed"
                return workflow_result
            
            # 階段2: 模型訓練
            logger.info("🤖 階段2: TLOB 模型訓練...")
            training_config = {
                "training_hours": workflow_config.get("training_hours", 24),
                "lookback_hours": workflow_config.get("lookback_hours", 48),
                "model_architecture": "tlob_transformer"
            }
            
            training_result = await self.training_agent.train_tlob_model(symbol, training_config)
            workflow_result["stages"]["training"] = training_result
            
            if training_result["status"] != "success":
                workflow_result["status"] = "failed"
                workflow_result["error"] = "Model training failed"
                return workflow_result
            
            # 階段3: 模型評估
            logger.info("📈 階段3: 多維度模型評估...")
            model_path = training_result["result"]["model_path"]
            evaluation_config = {
                "backtest_hours": workflow_config.get("evaluation_hours", 168),  # 1週
                "evaluation_metrics": ["sharpe_ratio", "max_drawdown", "accuracy", "win_rate"]
            }
            
            evaluation_result = await self.evaluation_agent.evaluate_model(
                model_path, symbol, evaluation_config
            )
            workflow_result["stages"]["evaluation"] = evaluation_result
            
            if evaluation_result["status"] != "success":
                workflow_result["status"] = "failed"
                workflow_result["error"] = "Model evaluation failed"
                return workflow_result
            
            # 階段4: 部署決策
            deployment_decision = evaluation_result["deployment_decision"]
            workflow_result["stages"]["deployment_decision"] = deployment_decision
            
            if deployment_decision["deployment_approved"]:
                # 階段5: A/B 測試準備（可選）
                if workflow_config.get("enable_ab_testing", False):
                    logger.info("🔬 階段5: A/B 測試準備...")
                    
                    ab_config = {
                        "name": f"{symbol}_model_test",
                        "symbol": symbol,
                        "model_a": "current_production_model",
                        "model_b": model_path,
                        "traffic_split": 0.1,  # 10% 流量給新模型
                        "duration_hours": 24
                    }
                    
                    ab_experiment = await self.experiment_agent.create_ab_experiment(ab_config)
                    workflow_result["stages"]["ab_experiment"] = ab_experiment
                
                workflow_result["status"] = "ready_for_deployment"
                workflow_result["recommended_action"] = "DEPLOY_MODEL"
            else:
                workflow_result["status"] = "requires_improvement"
                workflow_result["recommended_action"] = "RETRAIN_MODEL"
                workflow_result["improvement_suggestions"] = deployment_decision.get("improvement_suggestions", [])
            
            workflow_result["end_time"] = time.time()
            workflow_result["total_duration"] = workflow_result["end_time"] - workflow_result["start_time"]
            
            # 智能工作流程分析
            final_analysis = await self._analyze_workflow_results(workflow_result)
            workflow_result["intelligent_analysis"] = final_analysis
            
            logger.info(f"✅ ML 工作流程完成: {workflow_result['status']} ({workflow_result['total_duration']:.1f}秒)")
            return workflow_result
            
        except Exception as e:
            logger.error(f"❌ ML 工作流程異常: {e}")
            workflow_result["status"] = "error"
            workflow_result["error"] = str(e)
            workflow_result["end_time"] = time.time()
            return workflow_result
    
    async def _analyze_workflow_results(self, workflow_result: Dict[str, Any]) -> Dict[str, Any]:
        """智能分析工作流程結果"""
        
        analysis_query = f"""
        作為 ML 工作流程專家，請分析以下完整工作流程結果：

        📊 工作流程狀態: {workflow_result.get('status')}
        📦 數據收集: {workflow_result.get('stages', {}).get('data_collection', {}).get('status')}
        🤖 模型訓練: {workflow_result.get('stages', {}).get('training', {}).get('status')}
        📈 模型評估: {workflow_result.get('stages', {}).get('evaluation', {}).get('status')}
        🚀 部署決策: {workflow_result.get('stages', {}).get('deployment_decision', {}).get('deployment_approved')}

        請提供專業分析：
        1. 工作流程執行質量評估
        2. 識別瓶頸和改進機會
        3. 下一步行動建議
        4. 風險評估和緩解措施

        分析應該基於實際數據和專業判斷。
        """
        
        try:
            response = await self.agent.run(analysis_query)
            
            return {
                "analysis_content": response.get("result", "Analysis completed"),
                "workflow_quality_score": self._calculate_workflow_quality(workflow_result),
                "bottlenecks_identified": self._identify_bottlenecks(workflow_result),
                "recommendations": self._generate_recommendations(workflow_result),
                "analysis_timestamp": time.time()
            }
            
        except Exception as e:
            logger.error(f"工作流程分析失敗: {e}")
            return {
                "analysis_content": "ERROR: 無法進行智能分析",
                "error": str(e)
            }
    
    def _calculate_workflow_quality(self, result: Dict[str, Any]) -> float:
        """計算工作流程質量分數"""
        score = 0.0
        stages = result.get("stages", {})
        
        # 數據收集質量 (25%)
        if stages.get("data_collection", {}).get("status") == "success":
            records = stages["data_collection"].get("records_collected", 0)
            score += 0.25 * min(records / 10000, 1.0)  # 假設10k記錄為滿分
        
        # 訓練成功 (25%)
        if stages.get("training", {}).get("status") == "success":
            score += 0.25
        
        # 評估質量 (25%)
        if stages.get("evaluation", {}).get("status") == "success":
            deployment_decision = stages.get("deployment_decision", {})
            confidence = deployment_decision.get("confidence_level", 0)
            score += 0.25 * confidence
        
        # 整體完成度 (25%)
        if result.get("status") in ["ready_for_deployment", "requires_improvement"]:
            score += 0.25
        
        return min(score, 1.0)
    
    def _identify_bottlenecks(self, result: Dict[str, Any]) -> List[str]:
        """識別工作流程瓶頸"""
        bottlenecks = []
        stages = result.get("stages", {})
        
        # 檢查數據收集瓶頸
        data_collection = stages.get("data_collection", {})
        if data_collection.get("execution_time_seconds", 0) > 300:  # 5分鐘
            bottlenecks.append("數據收集時間過長")
        
        # 檢查訓練瓶頸
        training = stages.get("training", {})
        if training.get("result", {}).get("training_duration", 0) > 7200:  # 2小時
            bottlenecks.append("模型訓練時間過長")
        
        # 檢查評估瓶頸
        evaluation = stages.get("evaluation", {})
        if not evaluation.get("deployment_decision", {}).get("deployment_approved"):
            bottlenecks.append("模型評估標準未達標")
        
        return bottlenecks
    
    def _generate_recommendations(self, result: Dict[str, Any]) -> List[str]:
        """生成改進建議"""
        recommendations = []
        
        if result.get("status") == "requires_improvement":
            deployment_decision = result.get("stages", {}).get("deployment_decision", {})
            suggestions = deployment_decision.get("improvement_suggestions", [])
            recommendations.extend(suggestions)
        
        # 基於瓶頸的建議
        bottlenecks = self._identify_bottlenecks(result)
        for bottleneck in bottlenecks:
            if "數據收集" in bottleneck:
                recommendations.append("考慮並行數據收集或增量收集策略")
            elif "訓練時間" in bottleneck:
                recommendations.append("優化模型架構或使用分佈式訓練")
            elif "評估標準" in bottleneck:
                recommendations.append("調整超參數或增加訓練數據量")
        
        return recommendations

# 創建全局實例
ml_workflow = MLWorkflowAgent(mode=ExecutionMode.REAL)

async def main():
    """主函數 - ML 工作流程代理測試"""
    print("🧠 ML Workflow Agent 測試")
    print("=" * 60)
    
    # 執行完整 ML 工作流程
    workflow_config = {
        "data_collection_minutes": 10,
        "training_hours": 24,
        "evaluation_hours": 168,
        "enable_ab_testing": True
    }
    
    result = await ml_workflow.execute_complete_ml_workflow("BTCUSDT", workflow_config)
    
    print(f"工作流程狀態: {result.get('status')}")
    print(f"總執行時間: {result.get('total_duration', 0):.1f}秒")
    print(f"建議行動: {result.get('recommended_action', 'NONE')}")
    
    return result

if __name__ == "__main__":
    asyncio.run(main())