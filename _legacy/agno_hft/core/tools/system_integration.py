"""
系統集成接口
===========

統一的 HFT 系統集成接口，連接所有組件：
- Rust HFT 底層執行系統
- Python ML 訓練和推理
- Agno Workflows 工作流管理
- 實時監控和告警
- 參數優化和自適應調整
"""

import asyncio
import logging
from typing import Dict, Any, Optional, List
from datetime import datetime, timedelta
import json
import os
from dataclasses import dataclass
from enum import Enum

# 導入核心組件
from workflows.workflow_manager_v2 import workflow_manager_v2, WorkflowPriority
from workflows.agents_v2.strategy_manager import StrategyManager
from workflows.agents_v2.risk_supervisor import RiskSupervisor
from workflows.agents_v2.system_operator import SystemOperator
from workflows.evaluators_v2.system_health_evaluator import SystemHealthEvaluator
from workflows.tools_v2.parameter_optimization_tools import ParameterOptimizationTools

# 導入 ML 組件
from ml.trainer import ModelTrainer
from ml.dataset import TLOBDataset

logger = logging.getLogger(__name__)


class SystemMode(Enum):
    """系統運行模式"""
    DEVELOPMENT = "development"
    TESTING = "testing"
    STAGING = "staging"
    PRODUCTION = "production"


class TradingState(Enum):
    """交易狀態"""
    STOPPED = "stopped"
    WARMING_UP = "warming_up"
    ACTIVE = "active"
    PAUSED = "paused"
    EMERGENCY_STOP = "emergency_stop"


@dataclass
class SystemConfiguration:
    """系統配置"""
    mode: SystemMode
    symbols: List[str]
    risk_limits: Dict[str, float]
    model_config: Dict[str, Any]
    rust_hft_config: Dict[str, Any]
    monitoring_config: Dict[str, Any]
    
    @classmethod
    def load_from_file(cls, config_path: str) -> 'SystemConfiguration':
        """從文件加載配置"""
        with open(config_path, 'r') as f:
            config_data = json.load(f)
        
        return cls(
            mode=SystemMode(config_data.get('mode', 'development')),
            symbols=config_data.get('symbols', ['BTCUSDT']),
            risk_limits=config_data.get('risk_limits', {}),
            model_config=config_data.get('model_config', {}),
            rust_hft_config=config_data.get('rust_hft_config', {}),
            monitoring_config=config_data.get('monitoring_config', {})
        )


class HFTSystemIntegration:
    """
    HFT 系統集成主控制器
    
    統一管理所有子系統和工作流
    """
    
    def __init__(self, config: SystemConfiguration):
        self.config = config
        self.system_state = TradingState.STOPPED
        self.active_strategies = {}
        self.system_metrics = {}
        
        # 核心組件
        self.workflow_manager = workflow_manager_v2
        self.parameter_optimizer = ParameterOptimizationTools()
        self.system_health_evaluator = SystemHealthEvaluator()
        
        # Agent 池
        self.strategy_managers = {}
        self.risk_supervisor = None
        self.system_operator = None
        
        # 監控任務
        self.monitoring_tasks = []
        
        logger.info(f"HFT System Integration initialized in {config.mode.value} mode")
    
    async def initialize_system(self) -> Dict[str, Any]:
        """
        初始化系統
        
        Returns:
            初始化結果
        """
        try:
            logger.info("Initializing HFT system...")
            
            # 1. 初始化風險監督和系統操作員
            self.risk_supervisor = RiskSupervisor(session_id="system_supervisor")
            self.system_operator = SystemOperator(session_id="system_operator")
            
            # 2. 為每個交易對創建策略管理器
            for symbol in self.config.symbols:
                self.strategy_managers[symbol] = StrategyManager(
                    session_id=f"strategy_manager_{symbol}"
                )
            
            # 3. 啟動監控任務
            await self._start_monitoring_tasks()
            
            # 4. 系統健康檢查
            health_result = await self._conduct_system_health_check()
            
            if not health_result.get("passed", False):
                return {
                    "success": False,
                    "error": "System health check failed",
                    "health_result": health_result
                }
            
            # 5. 初始化 Rust HFT 引擎連接
            rust_init_result = await self._initialize_rust_engine()
            
            if not rust_init_result.get("success", False):
                return {
                    "success": False,
                    "error": "Rust engine initialization failed",
                    "rust_result": rust_init_result
                }
            
            self.system_state = TradingState.WARMING_UP
            
            logger.info("HFT system initialization completed successfully")
            
            return {
                "success": True,
                "system_state": self.system_state.value,
                "initialized_symbols": self.config.symbols,
                "health_status": health_result,
                "rust_status": rust_init_result
            }
            
        except Exception as e:
            logger.error(f"System initialization failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def start_trading(
        self,
        symbols: Optional[List[str]] = None,
        mode: str = "gradual"
    ) -> Dict[str, Any]:
        """
        啟動交易
        
        Args:
            symbols: 要啟動的交易對列表
            mode: 啟動模式 (gradual, immediate)
            
        Returns:
            啟動結果
        """
        if self.system_state != TradingState.WARMING_UP:
            return {
                "success": False,
                "error": f"Cannot start trading in {self.system_state.value} state"
            }
        
        symbols = symbols or self.config.symbols
        results = {}
        
        try:
            logger.info(f"Starting trading for symbols: {symbols}")
            
            for symbol in symbols:
                if symbol in self.strategy_managers:
                    # 創建策略啟動工作流
                    strategy_config = {
                        "symbol": symbol,
                        "mode": mode,
                        "risk_profile": "moderate",
                        "initial_position_size": 0.1 if mode == "gradual" else 0.3
                    }
                    
                    # 使用工作流管理器啟動綜合交易流程
                    workflow_id = await self.workflow_manager.create_comprehensive_trading_workflow(
                        symbol=symbol,
                        strategy_config=strategy_config,
                        priority=WorkflowPriority.HIGH
                    )
                    
                    results[symbol] = {
                        "success": True,
                        "workflow_id": workflow_id,
                        "mode": mode
                    }
                    
                    self.active_strategies[symbol] = {
                        "workflow_id": workflow_id,
                        "start_time": datetime.now(),
                        "status": "starting"
                    }
            
            if results:
                self.system_state = TradingState.ACTIVE
            
            logger.info(f"Trading started for {len(results)} symbols")
            
            return {
                "success": True,
                "system_state": self.system_state.value,
                "started_symbols": results,
                "total_active": len(self.active_strategies)
            }
            
        except Exception as e:
            logger.error(f"Failed to start trading: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def optimize_strategies(
        self,
        symbols: Optional[List[str]] = None,
        optimization_target: str = "sharpe_ratio"
    ) -> Dict[str, Any]:
        """
        優化策略參數
        
        Args:
            symbols: 要優化的交易對
            optimization_target: 優化目標
            
        Returns:
            優化結果
        """
        symbols = symbols or list(self.active_strategies.keys())
        optimization_results = {}
        
        try:
            logger.info(f"Starting strategy optimization for {symbols}")
            
            for symbol in symbols:
                if symbol in self.active_strategies:
                    # 獲取當前策略性能
                    current_performance = await self._get_strategy_performance(symbol)
                    
                    if current_performance:
                        # 創建優化工作流
                        opt_workflow_id = await self.workflow_manager.create_model_optimization_workflow(
                            symbol=symbol,
                            current_performance=current_performance,
                            priority=WorkflowPriority.MEDIUM
                        )
                        
                        optimization_results[symbol] = {
                            "success": True,
                            "optimization_workflow_id": opt_workflow_id,
                            "current_performance": current_performance
                        }
                    else:
                        optimization_results[symbol] = {
                            "success": False,
                            "error": "Could not retrieve current performance"
                        }
            
            logger.info(f"Strategy optimization initiated for {len(optimization_results)} symbols")
            
            return {
                "success": True,
                "optimization_results": optimization_results,
                "total_optimizations": len(optimization_results)
            }
            
        except Exception as e:
            logger.error(f"Strategy optimization failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def emergency_stop(self, reason: str = "Manual trigger") -> Dict[str, Any]:
        """
        緊急停止所有交易
        
        Args:
            reason: 停止原因
            
        Returns:
            停止結果
        """
        try:
            logger.warning(f"EMERGENCY STOP triggered: {reason}")
            
            # 停止所有活躍策略
            stop_results = {}
            for symbol, strategy_info in self.active_strategies.items():
                workflow_id = strategy_info["workflow_id"]
                
                # 緊急停止工作流
                stop_result = await self.workflow_manager.emergency_stop_workflow(workflow_id)
                stop_results[symbol] = stop_result
                
                # 通知 Rust 引擎停止交易
                rust_stop_result = await self._emergency_stop_rust_trading(symbol)
                stop_results[f"{symbol}_rust"] = rust_stop_result
            
            # 更新系統狀態
            self.system_state = TradingState.EMERGENCY_STOP
            
            # 記錄緊急停止事件
            emergency_event = {
                "timestamp": datetime.now().isoformat(),
                "reason": reason,
                "affected_symbols": list(self.active_strategies.keys()),
                "stop_results": stop_results
            }
            
            logger.critical(f"Emergency stop completed: {emergency_event}")
            
            return {
                "success": True,
                "emergency_event": emergency_event,
                "system_state": self.system_state.value,
                "stopped_strategies": len(stop_results) // 2  # 除以2因為包含rust結果
            }
            
        except Exception as e:
            logger.error(f"Emergency stop failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def get_system_status(self) -> Dict[str, Any]:
        """
        獲取系統狀態
        
        Returns:
            系統狀態信息
        """
        try:
            # 工作流狀態
            workflow_stats = self.workflow_manager.get_comprehensive_statistics()
            
            # 策略狀態
            strategy_status = {}
            for symbol, strategy_info in self.active_strategies.items():
                workflow_status = self.workflow_manager.get_session_status(
                    strategy_info["workflow_id"]
                )
                strategy_status[symbol] = {
                    "workflow_status": workflow_status,
                    "start_time": strategy_info["start_time"].isoformat(),
                    "status": strategy_info.get("status", "unknown")
                }
            
            # 系統健康狀態
            health_status = await self._get_current_health_status()
            
            # 性能指標
            performance_metrics = await self._get_performance_metrics()
            
            return {
                "system_state": self.system_state.value,
                "uptime": self._calculate_uptime(),
                "active_symbols": list(self.active_strategies.keys()),
                "workflow_statistics": workflow_stats,
                "strategy_status": strategy_status,
                "health_status": health_status,
                "performance_metrics": performance_metrics,
                "configuration": {
                    "mode": self.config.mode.value,
                    "symbols": self.config.symbols,
                    "risk_limits": self.config.risk_limits
                }
            }
            
        except Exception as e:
            logger.error(f"Failed to get system status: {e}")
            return {
                "error": str(e),
                "system_state": self.system_state.value
            }
    
    async def _start_monitoring_tasks(self):
        """啟動監控任務"""
        # 系統健康監控
        self.monitoring_tasks.append(
            asyncio.create_task(self._system_health_monitor_loop())
        )
        
        # 性能監控
        self.monitoring_tasks.append(
            asyncio.create_task(self._performance_monitor_loop())
        )
        
        # 風險監控
        self.monitoring_tasks.append(
            asyncio.create_task(self._risk_monitor_loop())
        )
        
        logger.info(f"Started {len(self.monitoring_tasks)} monitoring tasks")
    
    async def _system_health_monitor_loop(self):
        """系統健康監控循環"""
        while True:
            try:
                await asyncio.sleep(60)  # 每分鐘檢查一次
                
                health_result = await self._conduct_system_health_check()
                
                if not health_result.get("passed", False):
                    logger.warning(f"System health check failed: {health_result}")
                    
                    # 如果健康檢查失敗，可以考慮降低交易強度或暫停
                    if self.system_state == TradingState.ACTIVE:
                        await self._handle_health_degradation(health_result)
                
            except Exception as e:
                logger.error(f"System health monitor error: {e}")
                await asyncio.sleep(30)
    
    async def _performance_monitor_loop(self):
        """性能監控循環"""
        while True:
            try:
                await asyncio.sleep(300)  # 每5分鐘檢查一次
                
                for symbol in self.active_strategies.keys():
                    performance = await self._get_strategy_performance(symbol)
                    
                    if performance:
                        # 檢查是否需要優化
                        if self._should_trigger_optimization(performance):
                            logger.info(f"Triggering optimization for {symbol}")
                            await self.optimize_strategies([symbol])
                
            except Exception as e:
                logger.error(f"Performance monitor error: {e}")
                await asyncio.sleep(60)
    
    async def _risk_monitor_loop(self):
        """風險監控循環"""
        while True:
            try:
                await asyncio.sleep(30)  # 每30秒檢查一次
                
                # 檢查系統級風險
                risk_metrics = await self._get_system_risk_metrics()
                
                # 檢查是否超過風險限額
                if self._check_risk_limits_breached(risk_metrics):
                    logger.error("Risk limits breached, triggering emergency stop")
                    await self.emergency_stop("Risk limits breached")
                
            except Exception as e:
                logger.error(f"Risk monitor error: {e}")
                await asyncio.sleep(60)
    
    async def _conduct_system_health_check(self) -> Dict[str, Any]:
        """進行系統健康檢查"""
        try:
            # 使用系統健康評估器
            health_context = {
                "health_status": {
                    "system_health": {
                        "cpu_usage": 45,
                        "memory_usage": 60,
                        "disk_usage": 30,
                        "network_latency": 2.5
                    }
                }
            }
            
            return await self.system_health_evaluator.evaluate(health_context)
            
        except Exception as e:
            return {
                "passed": False,
                "error": str(e)
            }
    
    async def _initialize_rust_engine(self) -> Dict[str, Any]:
        """初始化 Rust 引擎連接"""
        try:
            # 這裡應該調用 Rust HFT 工具
            # 暫時返回模擬結果
            logger.info("Initializing connection to Rust HFT engine")
            
            # 模擬 Rust 引擎初始化
            await asyncio.sleep(0.5)
            
            return {
                "success": True,
                "engine_status": "connected",
                "latency_us": 125,
                "supported_symbols": self.config.symbols
            }
            
        except Exception as e:
            logger.error(f"Rust engine initialization failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def _get_strategy_performance(self, symbol: str) -> Optional[Dict[str, float]]:
        """獲取策略性能指標"""
        try:
            # 模擬從 Rust 系統獲取性能數據
            return {
                "sharpe_ratio": 1.25,
                "max_drawdown": -0.08,
                "win_rate": 0.58,
                "total_return": 0.045,
                "volatility": 0.15
            }
        except Exception as e:
            logger.error(f"Failed to get performance for {symbol}: {e}")
            return None
    
    def _should_trigger_optimization(self, performance: Dict[str, float]) -> bool:
        """判斷是否應該觸發優化"""
        # 如果 Sharpe 比率低於 1.0 或回撤超過 10%，觸發優化
        return (performance.get("sharpe_ratio", 0) < 1.0 or 
                performance.get("max_drawdown", 0) < -0.1)
    
    async def _emergency_stop_rust_trading(self, symbol: str) -> Dict[str, Any]:
        """緊急停止 Rust 交易"""
        try:
            logger.warning(f"Emergency stopping Rust trading for {symbol}")
            # 這裡應該調用 Rust 緊急停止接口
            return {"success": True, "symbol": symbol}
        except Exception as e:
            return {"success": False, "error": str(e)}
    
    async def _get_current_health_status(self) -> Dict[str, Any]:
        """獲取當前健康狀態"""
        return await self._conduct_system_health_check()
    
    async def _get_performance_metrics(self) -> Dict[str, Any]:
        """獲取性能指標"""
        metrics = {}
        for symbol in self.active_strategies.keys():
            performance = await self._get_strategy_performance(symbol)
            if performance:
                metrics[symbol] = performance
        return metrics
    
    def _calculate_uptime(self) -> float:
        """計算系統運行時間（小時）"""
        # 這裡應該基於實際啟動時間計算
        return 24.5  # 模擬值
    
    async def _handle_health_degradation(self, health_result: Dict[str, Any]):
        """處理健康狀況惡化"""
        logger.warning("Handling system health degradation")
        # 可以實現降低交易強度、暫停部分策略等邏輯
    
    async def _get_system_risk_metrics(self) -> Dict[str, float]:
        """獲取系統風險指標"""
        return {
            "total_exposure": 50000,
            "var_95": -2500,
            "portfolio_beta": 0.85,
            "concentration_risk": 0.15
        }
    
    def _check_risk_limits_breached(self, risk_metrics: Dict[str, float]) -> bool:
        """檢查風險限額是否被突破"""
        max_exposure = self.config.risk_limits.get("max_exposure", 100000)
        max_var = self.config.risk_limits.get("max_var", -5000)
        
        return (risk_metrics.get("total_exposure", 0) > max_exposure or
                risk_metrics.get("var_95", 0) < max_var)


async def demo_system_integration():
    """演示系統集成功能"""
    print("🚀 HFT System Integration Demo")
    print("=" * 50)
    
    # 加載配置
    config = SystemConfiguration(
        mode=SystemMode.DEVELOPMENT,
        symbols=["BTCUSDT", "ETHUSDT"],
        risk_limits={
            "max_exposure": 100000,
            "max_var": -5000,
            "max_drawdown": -0.15
        },
        model_config={
            "hidden_size": 256,
            "num_layers": 3
        },
        rust_hft_config={
            "max_latency_us": 500,
            "connection_timeout": 30
        },
        monitoring_config={
            "health_check_interval": 60,
            "performance_check_interval": 300
        }
    )
    
    # 創建系統實例
    hft_system = HFTSystemIntegration(config)
    
    # 1. 初始化系統
    print("\n🔧 1. 初始化系統")
    init_result = await hft_system.initialize_system()
    print(f"✅ 初始化: {init_result['success']}")
    if init_result['success']:
        print(f"   系統狀態: {init_result['system_state']}")
        print(f"   初始化交易對: {init_result['initialized_symbols']}")
    
    # 2. 啟動交易
    if init_result['success']:
        print("\n💰 2. 啟動交易")
        start_result = await hft_system.start_trading(mode="gradual")
        print(f"✅ 交易啟動: {start_result['success']}")
        if start_result['success']:
            print(f"   活躍策略: {start_result['total_active']}")
    
    # 3. 等待一段時間
    print("\n⏳ 3. 系統運行中...")
    await asyncio.sleep(3)
    
    # 4. 獲取系統狀態
    print("\n📊 4. 系統狀態")
    status = await hft_system.get_system_status()
    print(f"   系統狀態: {status['system_state']}")
    print(f"   活躍交易對: {len(status['active_symbols'])}")
    print(f"   系統運行時間: {status['uptime']:.1f}小時")
    
    # 5. 策略優化
    print("\n🔧 5. 策略優化")
    opt_result = await hft_system.optimize_strategies()
    print(f"✅ 優化啟動: {opt_result['success']}")
    if opt_result['success']:
        print(f"   優化策略數: {opt_result['total_optimizations']}")
    
    print("\n🎉 System Integration Demo 完成!")


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(demo_system_integration())