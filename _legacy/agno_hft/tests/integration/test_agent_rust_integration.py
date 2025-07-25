#!/usr/bin/env python3
"""
測試 Agno Agent 系統調用 Rust HFT 功能
展示高性能 PyO3 集成
"""

import asyncio
import time
from typing import Dict, Any
import rust_hft_py  # 直接導入 Rust 模塊
from agno import Agent, Team, Workflow, WorkflowStep, WorkflowEvaluator, Tool

# ===== 定義 Rust HFT 工具 =====

class RustHFTEngine(Tool):
    """直接調用 Rust HFT 引擎的工具"""
    
    def __init__(self):
        super().__init__(
            name="rust_hft_engine",
            description="High-performance Rust HFT engine for ultra-low latency trading"
        )
        self.engine = rust_hft_py.HftEngine()
        print(f"[INFO] Initialized Rust HFT Engine v{rust_hft_py.__version__}")
    
    async def execute(self, symbol: str, action: str, **kwargs) -> Dict[str, Any]:
        """執行 HFT 操作"""
        start_time = time.perf_counter()
        
        if action == "analyze_orderbook":
            # 模擬訂單簿分析
            result = self.engine.analyze_market(symbol, depth=kwargs.get("depth", 20))
        elif action == "calculate_signals":
            # 計算交易信號
            result = self.engine.calculate_signals(symbol)
        elif action == "execute_strategy":
            # 執行策略
            result = self.engine.execute_strategy(symbol, kwargs.get("strategy", "market_making"))
        else:
            result = {"error": f"Unknown action: {action}"}
        
        latency_us = (time.perf_counter() - start_time) * 1_000_000
        return {
            "action": action,
            "symbol": symbol,
            "result": result,
            "latency_us": latency_us,
            "rust_performance": "ultra_low_latency"
        }

class RustModelTrainer(Tool):
    """使用 Rust 加速的模型訓練工具"""
    
    def __init__(self):
        super().__init__(
            name="rust_model_trainer",
            description="High-performance ML model training using Rust"
        )
        self.trainer = rust_hft_py.ModelTrainer()
    
    async def execute(self, symbol: str, hours: int = 24) -> Dict[str, Any]:
        """訓練模型"""
        print(f"[RUST] Training model for {symbol} with {hours} hours of data...")
        # 調用 Rust 訓練函數
        success = self.trainer.train(symbol, hours)
        return {
            "symbol": symbol,
            "hours": hours,
            "success": success,
            "backend": "rust_accelerated"
        }

class RustDataProcessor(Tool):
    """Rust 加速的數據處理工具"""
    
    def __init__(self):
        super().__init__(
            name="rust_data_processor",
            description="Process market data using Rust for maximum performance"
        )
        self.processor = rust_hft_py.DataProcessor(buffer_size=10000)
    
    async def execute(self, symbol: str, data_type: str = "orderbook") -> Dict[str, Any]:
        """處理市場數據"""
        features = self.processor.extract_features(symbol)
        return {
            "symbol": symbol,
            "data_type": data_type,
            "features": features,
            "processing_engine": "rust_simd_optimized"
        }

# ===== 定義智能 Agent =====

class TradingStrategist(Agent):
    """交易策略 Agent - 使用 Rust 進行高頻決策"""
    
    def __init__(self):
        super().__init__(
            name="Trading Strategist",
            description="Execute trading strategies with Rust-powered ultra-low latency",
            tools=[RustHFTEngine()]
        )
    
    async def reason(self, query: str, context: Dict = None) -> str:
        """策略推理"""
        return f"Analyzing market conditions for optimal HFT strategy execution..."

class ModelEngineer(Agent):
    """模型工程師 Agent - 使用 Rust 加速訓練"""
    
    def __init__(self):
        super().__init__(
            name="Model Engineer",
            description="Train and optimize ML models using Rust acceleration",
            tools=[RustModelTrainer(), RustDataProcessor()]
        )
    
    async def reason(self, query: str, context: Dict = None) -> str:
        """模型開發推理"""
        return f"Preparing high-performance model training pipeline..."

# ===== 定義工作流 =====

class HFTTradingWorkflow(Workflow):
    """HFT 交易工作流 - 展示 Rust 集成"""
    
    def __init__(self):
        super().__init__(name="HFT Trading Workflow")
        
        # 定義工作流步驟
        self.add_step(WorkflowStep(
            name="prepare_data",
            description="Process market data using Rust",
            agent=ModelEngineer(),
            tool_name="rust_data_processor"
        ))
        
        self.add_step(WorkflowStep(
            name="train_model",
            description="Train ML model with Rust acceleration",
            agent=ModelEngineer(),
            tool_name="rust_model_trainer",
            depends_on=["prepare_data"]
        ))
        
        self.add_step(WorkflowStep(
            name="execute_trading",
            description="Execute HFT strategy with <1μs latency",
            agent=TradingStrategist(),
            tool_name="rust_hft_engine",
            depends_on=["train_model"]
        ))

class PerformanceEvaluator(WorkflowEvaluator):
    """評估 Rust 性能"""
    
    async def evaluate(self, result: Any) -> float:
        """評估延遲性能"""
        if isinstance(result, dict) and "latency_us" in result:
            # 目標: <1μs 延遲
            latency = result["latency_us"]
            if latency < 1.0:
                return 1.0  # 完美
            elif latency < 10.0:
                return 0.8  # 優秀
            elif latency < 100.0:
                return 0.6  # 良好
            else:
                return 0.3  # 需要優化
        return 0.5

# ===== 主測試函數 =====

async def test_rust_integration():
    """測試 Agent 系統與 Rust 的集成"""
    
    print("=" * 60)
    print("🚀 Rust HFT × Agno Agent Integration Test")
    print("=" * 60)
    
    # 1. 測試系統信息
    print("\n1️⃣ Testing System Info...")
    system_info = rust_hft_py.get_system_info()
    print(f"System: {system_info}")
    
    # 2. 創建 HFT 團隊
    print("\n2️⃣ Creating HFT Team...")
    team = Team(
        name="Rust-Powered HFT Team",
        agents=[TradingStrategist(), ModelEngineer()],
        workflows=[HFTTradingWorkflow()],
        evaluators=[PerformanceEvaluator()]
    )
    
    # 3. 執行工作流
    print("\n3️⃣ Executing HFT Workflow...")
    workflow = HFTTradingWorkflow()
    
    # 測試數據處理
    print("\n📊 Processing Market Data...")
    data_result = await workflow.steps[0].execute(
        symbol="BTCUSDT",
        data_type="orderbook"
    )
    print(f"Data Processing Result: {data_result}")
    
    # 測試模型訓練
    print("\n🧠 Training ML Model...")
    train_result = await workflow.steps[1].execute(
        symbol="BTCUSDT",
        hours=24
    )
    print(f"Training Result: {train_result}")
    
    # 測試策略執行
    print("\n⚡ Executing HFT Strategy...")
    trade_result = await workflow.steps[2].execute(
        symbol="BTCUSDT",
        action="execute_strategy",
        strategy="market_making"
    )
    print(f"Trading Result: {trade_result}")
    
    # 4. 性能評估
    print("\n4️⃣ Performance Evaluation...")
    evaluator = PerformanceEvaluator()
    score = await evaluator.evaluate(trade_result)
    print(f"Performance Score: {score:.2f}")
    
    if trade_result.get("latency_us", float('inf')) < 1.0:
        print("✅ Achieved <1μs latency target!")
    else:
        print(f"⚠️ Latency: {trade_result.get('latency_us', 'N/A')}μs")
    
    # 5. 測試直接 Rust 調用
    print("\n5️⃣ Direct Rust Function Calls...")
    
    # 測試交易執行器
    executor = rust_hft_py.TradingExecutor()
    exec_result = executor.start_trading("ETHUSDT", 10000.0)
    print(f"Direct Execution: {exec_result}")
    
    # 測試模型評估器
    evaluator = rust_hft_py.ModelEvaluator()
    eval_result = evaluator.evaluate("BTCUSDT")
    print(f"Model Evaluation: {eval_result}")
    
    print("\n" + "=" * 60)
    print("✅ Integration Test Completed Successfully!")
    print("=" * 60)

if __name__ == "__main__":
    # 執行測試
    asyncio.run(test_rust_integration())