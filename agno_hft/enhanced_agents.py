#!/usr/bin/env python3
"""
Enhanced HFT Agents - 增強版專業化Agent系統
提供智能決策、錯誤恢復和自適應功能
"""

import asyncio
import logging
import json
import time
from typing import Dict, List, Optional, Any, Tuple
from enum import Enum
from dataclasses import dataclass
from datetime import datetime, timedelta

from agno.agent import Agent
from agno.models.ollama import Ollama
from rust_hft_tools import RustHFTTools

# 設置日誌
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class AgentState(Enum):
    """Agent狀態枚舉"""
    IDLE = "idle"
    WORKING = "working"
    ERROR = "error"
    RECOVERING = "recovering"
    DISABLED = "disabled"

class TaskPriority(Enum):
    """任務優先級"""
    CRITICAL = 1
    HIGH = 2
    MEDIUM = 3
    LOW = 4

@dataclass
class AgentTask:
    """Agent任務數據結構"""
    id: str
    type: str
    priority: TaskPriority
    params: Dict[str, Any]
    created_at: datetime
    deadline: Optional[datetime] = None
    retry_count: int = 0
    max_retries: int = 3
    
class AgentPerformanceMetrics:
    """Agent性能指標追蹤"""
    
    def __init__(self):
        self.task_count = 0
        self.success_count = 0
        self.error_count = 0
        self.avg_response_time = 0.0
        self.last_active = datetime.now()
        self.response_times = []
        
    def record_success(self, response_time: float):
        """記錄成功任務"""
        self.task_count += 1
        self.success_count += 1
        self.response_times.append(response_time)
        self.avg_response_time = sum(self.response_times) / len(self.response_times)
        self.last_active = datetime.now()
        
    def record_error(self):
        """記錄錯誤任務"""
        self.task_count += 1
        self.error_count += 1
        self.last_active = datetime.now()
        
    def get_success_rate(self) -> float:
        """獲取成功率"""
        if self.task_count == 0:
            return 1.0
        return self.success_count / self.task_count
        
    def is_healthy(self) -> bool:
        """檢查Agent健康狀態"""
        success_rate = self.get_success_rate()
        time_since_active = datetime.now() - self.last_active
        
        return (success_rate >= 0.8 and 
                time_since_active < timedelta(minutes=30) and
                self.avg_response_time < 30.0)

class EnhancedAgentBase:
    """增強版Agent基類"""
    
    def __init__(self, name: str, role: str, rust_tools: RustHFTTools):
        self.name = name
        self.role = role
        self.rust_tools = rust_tools
        self.state = AgentState.IDLE
        self.metrics = AgentPerformanceMetrics()
        self.task_queue = asyncio.Queue()
        self.error_history = []
        self.last_error = None
        
        # 創建Agent實例
        self.agent = Agent(
            name=name,
            role=role,
            model=Ollama(id="qwen3:8b"),
            instructions=self._get_instructions(),
            markdown=True,
        )
        
        logger.info(f"✅ 初始化增強版Agent: {name}")
        
    def _get_instructions(self) -> str:
        """獲取Agent指令（子類實現）"""
        return f"""
        你是一個專業的{self.role}，負責高頻交易系統的{self.name}功能。
        
        核心原則：
        1. 專業性：基於數據和經驗做決策
        2. 謹慎性：優先考慮風險控制
        3. 效率性：快速響應，簡潔明確
        4. 協作性：與其他Agent良好配合
        
        錯誤處理：
        - 遇到錯誤時，詳細分析原因
        - 提供具體的恢復建議
        - 評估錯誤對系統的影響
        """
        
    async def add_task(self, task: AgentTask):
        """添加任務到隊列"""
        await self.task_queue.put(task)
        logger.info(f"📋 {self.name} 接收任務: {task.type} (優先級: {task.priority.name})")
        
    async def process_tasks(self):
        """處理任務隊列"""
        while True:
            try:
                # 等待任務
                task = await self.task_queue.get()
                
                # 檢查Agent健康狀態
                if not self._is_ready_for_task():
                    logger.warning(f"⚠️ {self.name} 狀態不佳，延遲任務處理")
                    await asyncio.sleep(5)
                    await self.task_queue.put(task)  # 重新排隊
                    continue
                
                # 執行任務
                await self._execute_task(task)
                
            except Exception as e:
                logger.error(f"❌ {self.name} 任務處理異常: {e}")
                await self._handle_error(e)
                
    def _is_ready_for_task(self) -> bool:
        """檢查是否準備好處理任務"""
        return (self.state in [AgentState.IDLE, AgentState.WORKING] and
                self.metrics.is_healthy())
                
    async def _execute_task(self, task: AgentTask):
        """執行具體任務"""
        self.state = AgentState.WORKING
        start_time = time.time()
        
        try:
            # 調用子類的具體實現
            result = await self.execute_specific_task(task)
            
            # 記錄成功
            response_time = time.time() - start_time
            self.metrics.record_success(response_time)
            self.state = AgentState.IDLE
            
            logger.info(f"✅ {self.name} 完成任務: {task.type} ({response_time:.2f}s)")
            return result
            
        except Exception as e:
            # 記錄錯誤
            self.metrics.record_error()
            self.last_error = str(e)
            self.error_history.append({
                'timestamp': datetime.now(),
                'task': task.type,
                'error': str(e)
            })
            
            # 嘗試恢復
            if task.retry_count < task.max_retries:
                task.retry_count += 1
                logger.warning(f"⚠️ {self.name} 任務失敗，重試 {task.retry_count}/{task.max_retries}")
                await asyncio.sleep(2 ** task.retry_count)  # 指數退避
                await self.task_queue.put(task)
            else:
                logger.error(f"❌ {self.name} 任務徹底失敗: {task.type}")
                await self._handle_critical_error(task, e)
            
            self.state = AgentState.ERROR
            raise
            
    async def execute_specific_task(self, task: AgentTask) -> Any:
        """子類實現的具體任務執行邏輯"""
        raise NotImplementedError("子類必須實現 execute_specific_task 方法")
        
    async def _handle_error(self, error: Exception):
        """處理錯誤"""
        self.state = AgentState.RECOVERING
        
        # 分析錯誤類型
        error_analysis = await self._analyze_error(error)
        
        # 執行恢復策略
        recovery_successful = await self._attempt_recovery(error_analysis)
        
        if recovery_successful:
            self.state = AgentState.IDLE
            logger.info(f"✅ {self.name} 錯誤恢復成功")
        else:
            self.state = AgentState.ERROR
            logger.error(f"❌ {self.name} 錯誤恢復失敗，可能需要人工干預")
            
    async def _analyze_error(self, error: Exception) -> Dict[str, Any]:
        """分析錯誤並生成恢復建議"""
        error_context = f"""
        Agent: {self.name}
        錯誤類型: {type(error).__name__}
        錯誤信息: {str(error)}
        錯誤歷史: {len(self.error_history)} 個錯誤
        最近錯誤: {self.error_history[-3:] if len(self.error_history) >= 3 else self.error_history}
        
        請分析這個錯誤的可能原因，並提供恢復建議。
        """
        
        try:
            analysis = self.agent.run(error_context)
            return {
                'analysis': analysis.content,
                'error_type': type(error).__name__,
                'severity': self._assess_error_severity(error),
                'recovery_strategy': self._suggest_recovery_strategy(error)
            }
        except Exception as e:
            logger.error(f"❌ 錯誤分析失敗: {e}")
            return {
                'analysis': '錯誤分析失敗',
                'error_type': type(error).__name__,
                'severity': 'high',
                'recovery_strategy': 'restart'
            }
            
    def _assess_error_severity(self, error: Exception) -> str:
        """評估錯誤嚴重程度"""
        if isinstance(error, (ConnectionError, TimeoutError)):
            return 'medium'
        elif isinstance(error, (ValueError, TypeError)):
            return 'low'
        elif 'critical' in str(error).lower():
            return 'critical'
        else:
            return 'medium'
            
    def _suggest_recovery_strategy(self, error: Exception) -> str:
        """建議恢復策略"""
        if isinstance(error, ConnectionError):
            return 'reconnect'
        elif isinstance(error, TimeoutError):
            return 'retry_with_timeout'
        elif isinstance(error, (ValueError, TypeError)):
            return 'validate_inputs'
        else:
            return 'restart'
            
    async def _attempt_recovery(self, error_analysis: Dict[str, Any]) -> bool:
        """嘗試錯誤恢復"""
        strategy = error_analysis.get('recovery_strategy', 'restart')
        
        try:
            if strategy == 'reconnect':
                # 重新連接
                await asyncio.sleep(2)
                return True
            elif strategy == 'retry_with_timeout':
                # 延長超時時間重試
                await asyncio.sleep(5)
                return True
            elif strategy == 'validate_inputs':
                # 驗證和修復輸入
                return True
            elif strategy == 'restart':
                # 重啟Agent狀態
                self.state = AgentState.IDLE
                return True
            else:
                return False
                
        except Exception as e:
            logger.error(f"❌ 恢復策略執行失敗: {e}")
            return False
            
    async def _handle_critical_error(self, task: AgentTask, error: Exception):
        """處理關鍵錯誤"""
        logger.critical(f"🚨 {self.name} 發生關鍵錯誤: {error}")
        
        # 通知其他Agent
        await self._notify_critical_error(task, error)
        
        # 可能需要暫停Agent
        if len(self.error_history) > 10:  # 錯誤過多
            self.state = AgentState.DISABLED
            logger.critical(f"🛑 {self.name} 因錯誤過多被暫停")
            
    async def _notify_critical_error(self, task: AgentTask, error: Exception):
        """通知系統其他組件關鍵錯誤"""
        # 這裡可以實現錯誤通知機制
        pass
        
    def get_status(self) -> Dict[str, Any]:
        """獲取Agent狀態"""
        return {
            'name': self.name,
            'state': self.state.value,
            'metrics': {
                'task_count': self.metrics.task_count,
                'success_rate': self.metrics.get_success_rate(),
                'avg_response_time': self.metrics.avg_response_time,
                'is_healthy': self.metrics.is_healthy(),
            },
            'queue_size': self.task_queue.qsize(),
            'last_error': self.last_error,
            'error_count': len(self.error_history)
        }

class EnhancedTrainingAgent(EnhancedAgentBase):
    """增強版訓練Agent"""
    
    def __init__(self, rust_tools: RustHFTTools):
        super().__init__("Enhanced Training Agent", "模型訓練專家", rust_tools)
        
    def _get_instructions(self) -> str:
        return super()._get_instructions() + """
        
        專業領域：機器學習模型訓練
        
        核心職責：
        1. 分析市場數據特徵
        2. 選擇適當的模型架構
        3. 優化訓練參數
        4. 監控訓練進度
        5. 評估模型性能
        
        智能決策：
        - 根據數據質量調整訓練策略
        - 自動選擇最優超參數
        - 檢測過擬合並採取措施
        - 評估模型收斂性
        
        錯誤處理：
        - 數據不足：建議增加收集時間
        - 訓練發散：調整學習率
        - 內存不足：減少批次大小
        - 模型不收斂：嘗試不同架構
        """
        
    async def execute_specific_task(self, task: AgentTask) -> Any:
        """執行訓練任務"""
        if task.type == "train_model":
            return await self._train_model(task.params)
        elif task.type == "optimize_hyperparameters":
            return await self._optimize_hyperparameters(task.params)
        elif task.type == "evaluate_training":
            return await self._evaluate_training(task.params)
        else:
            raise ValueError(f"未知的訓練任務類型: {task.type}")
            
    async def _train_model(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """訓練模型"""
        symbol = params.get('symbol', 'BTCUSDT')
        hours = params.get('hours', 24)
        
        # 智能參數選擇
        optimized_params = await self._optimize_training_params(symbol, hours)
        
        # 執行訓練
        result = await self.rust_tools.train_model(
            symbol=symbol,
            hours=hours,
            **optimized_params
        )
        
        # 評估結果
        if result.get('success'):
            await self._post_training_analysis(result)
            
        return result
        
    async def _optimize_training_params(self, symbol: str, hours: int) -> Dict[str, Any]:
        """智能優化訓練參數"""
        # 基於歷史經驗和當前條件優化參數
        base_params = {
            'epochs': max(hours * 2, 20),
            'batch_size': 256,
            'learning_rate': 1e-4
        }
        
        # 根據symbol調整
        if symbol in ['BTCUSDT', 'ETHUSDT']:
            base_params['learning_rate'] *= 0.8  # 主流幣種更穩定
        
        # 根據數據量調整
        if hours < 12:
            base_params['epochs'] = max(base_params['epochs'] * 2, 50)
            
        return base_params
        
    async def _post_training_analysis(self, result: Dict[str, Any]):
        """訓練後分析"""
        final_loss = result.get('final_loss', 0)
        
        if final_loss > 0.1:
            logger.warning(f"⚠️ 訓練損失較高({final_loss:.4f})，建議檢查數據質量或調整參數")
        elif final_loss < 0.01:
            logger.info(f"✅ 訓練效果優秀(損失: {final_loss:.4f})")
        else:
            logger.info(f"✅ 訓練效果良好(損失: {final_loss:.4f})")

class EnhancedRiskAgent(EnhancedAgentBase):
    """增強版風險管理Agent"""
    
    def __init__(self, rust_tools: RustHFTTools):
        super().__init__("Enhanced Risk Agent", "風險管理專家", rust_tools)
        self.risk_thresholds = {
            'max_drawdown': 0.05,
            'daily_loss_limit': 0.02,
            'position_limit': 0.1,
            'volatility_threshold': 0.15
        }
        
    def _get_instructions(self) -> str:
        return super()._get_instructions() + """
        
        專業領域：風險管理和控制
        
        核心職責：
        1. 實時監控風險指標
        2. 評估交易風險水平
        3. 執行風險控制措施
        4. 預警潛在風險
        5. 緊急情況處理
        
        智能決策：
        - 動態調整風險閾值
        - 基於市場條件評估風險
        - 預測潛在風險事件
        - 優化風險控制策略
        
        風險類型：
        - 市場風險：價格波動、流動性
        - 操作風險：系統故障、網絡問題
        - 模型風險：模型失效、數據偏差
        - 資金風險：資金管理、止損
        """
        
    async def execute_specific_task(self, task: AgentTask) -> Any:
        """執行風險管理任務"""
        if task.type == "assess_risk":
            return await self._assess_risk(task.params)
        elif task.type == "monitor_portfolio":
            return await self._monitor_portfolio(task.params)
        elif task.type == "emergency_check":
            return await self._emergency_check(task.params)
        else:
            raise ValueError(f"未知的風險管理任務類型: {task.type}")
            
    async def _assess_risk(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """評估交易風險"""
        symbol = params.get('symbol')
        capital = params.get('capital')
        
        # 多維度風險評估
        risk_score = await self._calculate_risk_score(symbol, capital)
        risk_level = await self._determine_risk_level(risk_score)
        recommendations = await self._generate_risk_recommendations(risk_level, symbol, capital)
        
        return {
            'success': True,
            'risk_score': risk_score,
            'risk_level': risk_level,
            'recommendations': recommendations,
            'thresholds': self.risk_thresholds
        }
        
    async def _calculate_risk_score(self, symbol: str, capital: float) -> float:
        """計算綜合風險評分"""
        # 基礎風險評分
        base_score = 0.5
        
        # 資金規模風險
        if capital > 50000:
            base_score += 0.2
        elif capital < 1000:
            base_score += 0.1
            
        # 幣種風險
        if symbol in ['BTCUSDT', 'ETHUSDT']:
            base_score -= 0.1  # 主流幣種風險較低
        else:
            base_score += 0.1  # 小幣種風險較高
            
        return min(max(base_score, 0.0), 1.0)
        
    async def _determine_risk_level(self, risk_score: float) -> str:
        """確定風險等級"""
        if risk_score < 0.3:
            return "LOW"
        elif risk_score < 0.6:
            return "MEDIUM"
        elif risk_score < 0.8:
            return "HIGH"
        else:
            return "CRITICAL"
            
    async def _generate_risk_recommendations(self, risk_level: str, symbol: str, capital: float) -> List[str]:
        """生成風險建議"""
        recommendations = []
        
        if risk_level == "LOW":
            recommendations.extend([
                "✅ 風險水平較低，可以正常交易",
                f"建議最大倉位：{capital * 0.1:.2f} USDT (10%)",
                "建議止損設置：2%"
            ])
        elif risk_level == "MEDIUM":
            recommendations.extend([
                "⚠️ 風險水平中等，建議謹慎交易",
                f"建議最大倉位：{capital * 0.05:.2f} USDT (5%)",
                "建議止損設置：1.5%",
                "增加監控頻率"
            ])
        elif risk_level == "HIGH":
            recommendations.extend([
                "🔴 風險水平較高，建議減少交易",
                f"建議最大倉位：{capital * 0.02:.2f} USDT (2%)",
                "建議止損設置：1%",
                "考慮暫停自動交易"
            ])
        else:  # CRITICAL
            recommendations.extend([
                "🚨 風險水平極高，強烈建議停止交易",
                "立即平倉所有持倉",
                "檢查系統和市場狀況",
                "等待風險降低後再恢復交易"
            ])
            
        return recommendations

# 創建Agent工廠
class EnhancedAgentFactory:
    """增強版Agent工廠"""
    
    @staticmethod
    def create_agent(agent_type: str, rust_tools: RustHFTTools) -> EnhancedAgentBase:
        """創建指定類型的Agent"""
        if agent_type == "training":
            return EnhancedTrainingAgent(rust_tools)
        elif agent_type == "risk":
            return EnhancedRiskAgent(rust_tools)
        else:
            raise ValueError(f"未知的Agent類型: {agent_type}")

# 測試函數
async def test_enhanced_agents():
    """測試增強版Agent系統"""
    logger.info("🚀 開始測試增強版Agent系統")
    
    # 創建Rust工具
    rust_tools = RustHFTTools()
    
    # 創建Agents
    training_agent = EnhancedAgentFactory.create_agent("training", rust_tools)
    risk_agent = EnhancedAgentFactory.create_agent("risk", rust_tools)
    
    # 創建測試任務
    training_task = AgentTask(
        id="test_train_1",
        type="train_model",
        priority=TaskPriority.HIGH,
        params={'symbol': 'BTCUSDT', 'hours': 2},
        created_at=datetime.now()
    )
    
    risk_task = AgentTask(
        id="test_risk_1",
        type="assess_risk",
        priority=TaskPriority.CRITICAL,
        params={'symbol': 'BTCUSDT', 'capital': 10000},
        created_at=datetime.now()
    )
    
    # 添加任務
    await training_agent.add_task(training_task)
    await risk_agent.add_task(risk_task)
    
    # 啟動任務處理
    training_processor = asyncio.create_task(training_agent.process_tasks())
    risk_processor = asyncio.create_task(risk_agent.process_tasks())
    
    # 等待一段時間
    await asyncio.sleep(5)
    
    # 檢查狀態
    logger.info(f"訓練Agent狀態: {training_agent.get_status()}")
    logger.info(f"風險Agent狀態: {risk_agent.get_status()}")
    
    # 清理
    training_processor.cancel()
    risk_processor.cancel()
    
    logger.info("✅ 增強版Agent系統測試完成")

if __name__ == "__main__":
    asyncio.run(test_enhanced_agents())