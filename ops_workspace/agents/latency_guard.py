#!/usr/bin/env python3
"""
延遲監控Agent - Ops Workspace專業化Agent
======================================

基於PRD第3.2.1節的延遲監控需求：
- 監控閾值：25μs (微秒級)
- 響應時間：<100ms
- 處理策略：保守模式切換 + 緊急停止
- 專業領域：交易執行延遲控制

職責分離：
- LatencyGuard：純粹的延遲監控和響應
- DrawdownGuard：財務風險控制  
- SystemMonitor：系統健康監控
"""

import asyncio
import logging
from typing import Dict, Any, List, Optional
from datetime import datetime, timedelta
from enum import Enum
import statistics
import numpy as np

from agno import Agent
from agno.models import Ollama

logger = logging.getLogger(__name__)

class LatencyLevel(Enum):
    """延遲等級定義"""
    NORMAL = "normal"          # < 10μs
    ELEVATED = "elevated"      # 10-20μs
    HIGH = "high"             # 20-25μs
    CRITICAL = "critical"      # > 25μs

class LatencyAction(Enum):
    """延遲響應動作"""
    MONITOR = "monitor"                        # 持續監控
    SWITCH_CONSERVATIVE = "switch_conservative_mode"  # 切換保守模式
    REDUCE_FREQUENCY = "reduce_frequency"      # 降低交易頻率
    EMERGENCY_STOP = "emergency_stop"          # 緊急停止

class LatencyGuardAgent(Agent):
    """
    延遲監控守護Agent
    
    PRD對應：第3.2.1節 延遲監控要求
    - 閾值：25μs P99延遲
    - 目標：保持<10μs平均延遲
    - 策略：分級響應機制
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="LatencyGuard",
            model=Ollama(model_name="qwen2.5:3b"),
            instructions=self._get_latency_guard_instructions(),
            markdown=True,
            session_id=session_id,
            show_tool_calls=True
        )
        
        # 延遲監控配置
        self.thresholds = {
            "normal_us": 10.0,      # 正常延遲閾值
            "elevated_us": 20.0,    # 提高延遲閾值  
            "high_us": 25.0,        # 高延遲閾值 - PRD要求
            "critical_us": 50.0     # 危險延遲閾值
        }
        
        # 監控狀態
        self.current_level = LatencyLevel.NORMAL
        self.recent_latencies = []  # 最近的延遲數據
        self.alert_history = []     # 告警歷史
        self.conservative_mode = False
        
        # 統計數據
        self.stats = {
            "total_measurements": 0,
            "alerts_triggered": 0,
            "false_positives": 0,
            "avg_latency_us": 0.0,
            "p99_latency_us": 0.0,
            "max_latency_us": 0.0
        }
        
        logger.info("✅ LatencyGuard Agent初始化完成")
    
    def _get_latency_guard_instructions(self) -> str:
        """獲取延遲監控Agent指令"""
        return f"""
        你是一位專精於高頻交易延遲監控的專家Agent。

        🎯 核心職責：
        1. **延遲監控**: 實時監控交易執行延遲，確保<25μs閾值
        2. **分級響應**: 根據延遲等級執行不同的響應策略
        3. **性能優化**: 分析延遲模式，提供優化建議
        4. **緊急控制**: 極端情況下執行緊急停止

        📊 延遲等級標準：
        - 正常 (NORMAL): < 10μs - 理想運行狀態
        - 提高 (ELEVATED): 10-20μs - 需要關注
        - 高延遲 (HIGH): 20-25μs - 警告狀態
        - 危險 (CRITICAL): > 25μs - 立即行動

        🚨 響應策略：
        - NORMAL: 持續監控，記錄性能基線
        - ELEVATED: 增加監控頻率，分析延遲原因
        - HIGH: 切換保守模式，降低交易頻率
        - CRITICAL: 緊急停止交易，保護系統

        💡 分析維度：
        - 延遲趨勢分析
        - 異常模式識別
        - 系統瓶頸定位
        - 網路延遲影響

        請始終基於實際延遲數據進行決策，避免誤報和過度反應。
        """
    
    async def handle_latency_breach(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """
        處理延遲突破告警
        
        Args:
            alert_data: 告警數據包含延遲值和相關信息
            
        Returns:
            處理結果和後續行動建議
        """
        latency_us = alert_data.get("latency_us", 0)
        component = alert_data.get("component", "unknown")
        timestamp = alert_data.get("timestamp", datetime.now().isoformat())
        
        logger.warning(f"⚡ 處理延遲突破: {latency_us}μs from {component}")
        
        try:
            # 1. 記錄延遲數據
            self._record_latency_measurement(latency_us, alert_data)
            
            # 2. 評估延遲等級
            latency_level = self._assess_latency_level(latency_us)
            
            # 3. 分析延遲趋势
            trend_analysis = self._analyze_latency_trend()
            
            # 4. 決定響應動作
            recommended_action = self._determine_response_action(
                latency_level, trend_analysis
            )
            
            # 5. 生成詳細分析報告
            analysis_report = await self._generate_latency_analysis(
                latency_us, latency_level, trend_analysis, alert_data
            )
            
            # 6. 執行響應動作
            action_result = await self._execute_latency_response(
                recommended_action, alert_data
            )
            
            # 7. 更新統計數據
            self._update_latency_stats(latency_us, latency_level)
            
            return {
                "success": True,
                "latency_us": latency_us,
                "level": latency_level.value,
                "action": recommended_action.value,
                "analysis": analysis_report,
                "action_result": action_result,
                "timestamp": datetime.now().isoformat()
            }
            
        except Exception as e:
            logger.error(f"❌ 延遲告警處理失敗: {e}")
            return {
                "success": False,
                "error": str(e),
                "latency_us": latency_us
            }
    
    def _record_latency_measurement(self, latency_us: float, alert_data: Dict[str, Any]):
        """記錄延遲測量數據"""
        measurement = {
            "latency_us": latency_us,
            "timestamp": datetime.now(),
            "component": alert_data.get("component", "unknown"),
            "metadata": alert_data
        }
        
        self.recent_latencies.append(measurement)
        
        # 保持最近1小時的數據
        cutoff_time = datetime.now() - timedelta(hours=1)
        self.recent_latencies = [
            m for m in self.recent_latencies 
            if m["timestamp"] > cutoff_time
        ]
        
        self.stats["total_measurements"] += 1
    
    def _assess_latency_level(self, latency_us: float) -> LatencyLevel:
        """評估延遲等級"""
        if latency_us < self.thresholds["normal_us"]:
            return LatencyLevel.NORMAL
        elif latency_us < self.thresholds["elevated_us"]:
            return LatencyLevel.ELEVATED
        elif latency_us < self.thresholds["high_us"]:
            return LatencyLevel.HIGH
        else:
            return LatencyLevel.CRITICAL
    
    def _analyze_latency_trend(self) -> Dict[str, Any]:
        """分析延遲趨勢"""
        if len(self.recent_latencies) < 10:
            return {"trend": "insufficient_data", "confidence": 0.0}
        
        # 提取最近的延遲值
        recent_values = [m["latency_us"] for m in self.recent_latencies[-50:]]
        
        # 計算統計指標
        avg_latency = statistics.mean(recent_values)
        p99_latency = np.percentile(recent_values, 99)
        max_latency = max(recent_values)
        
        # 趨勢分析
        if len(recent_values) >= 20:
            first_half = recent_values[:len(recent_values)//2]
            second_half = recent_values[len(recent_values)//2:]
            
            first_avg = statistics.mean(first_half)
            second_avg = statistics.mean(second_half)
            
            if second_avg > first_avg * 1.2:
                trend = "increasing"
                confidence = min((second_avg - first_avg) / first_avg, 1.0)
            elif second_avg < first_avg * 0.8:
                trend = "decreasing"
                confidence = min((first_avg - second_avg) / first_avg, 1.0)
            else:
                trend = "stable"
                confidence = 0.8
        else:
            trend = "unknown"
            confidence = 0.0
        
        return {
            "trend": trend,
            "confidence": confidence,
            "avg_latency_us": avg_latency,
            "p99_latency_us": p99_latency,
            "max_latency_us": max_latency,
            "sample_size": len(recent_values)
        }
    
    def _determine_response_action(
        self, 
        latency_level: LatencyLevel, 
        trend_analysis: Dict[str, Any]
    ) -> LatencyAction:
        """決定響應動作"""
        
        # 危險延遲：立即緊急停止
        if latency_level == LatencyLevel.CRITICAL:
            return LatencyAction.EMERGENCY_STOP
        
        # 高延遲且趨勢惡化：切換保守模式
        if (latency_level == LatencyLevel.HIGH and 
            trend_analysis.get("trend") == "increasing"):
            return LatencyAction.SWITCH_CONSERVATIVE
        
        # 持續高延遲：降低頻率
        if (latency_level == LatencyLevel.HIGH and 
            trend_analysis.get("avg_latency_us", 0) > self.thresholds["elevated_us"]):
            return LatencyAction.REDUCE_FREQUENCY
        
        # 其他情況：持續監控
        return LatencyAction.MONITOR
    
    async def _generate_latency_analysis(
        self,
        current_latency: float,
        latency_level: LatencyLevel,
        trend_analysis: Dict[str, Any],
        alert_data: Dict[str, Any]
    ) -> str:
        """生成延遲分析報告"""
        
        analysis_context = f"""
        延遲突破事件分析：
        
        📊 當前延遲情況：
        - 當前延遲: {current_latency:.2f}μs
        - 延遲等級: {latency_level.value.upper()}
        - 組件: {alert_data.get('component', 'unknown')}
        - 閾值狀態: {'❌ 超標' if current_latency > self.thresholds['high_us'] else '✅ 正常'}
        
        📈 趨勢分析：
        - 趨勢方向: {trend_analysis.get('trend', 'unknown')}
        - 置信度: {trend_analysis.get('confidence', 0):.2f}
        - 平均延遲: {trend_analysis.get('avg_latency_us', 0):.2f}μs
        - P99延遲: {trend_analysis.get('p99_latency_us', 0):.2f}μs
        - 樣本數量: {trend_analysis.get('sample_size', 0)}
        
        🎯 PRD要求對比：
        - 目標延遲: < 10μs (平均)
        - 警告閾值: 25μs (P99)
        - 當前狀態: {'超標' if current_latency > 25 else '達標'}
        
        請分析：
        1. 延遲突破的可能原因
        2. 對交易性能的影響評估
        3. 建議的優化措施
        4. 是否需要立即干預
        """
        
        response = await self.arun(analysis_context)
        return response.content
    
    async def _execute_latency_response(
        self,
        action: LatencyAction,
        alert_data: Dict[str, Any]
    ) -> Dict[str, Any]:
        """執行延遲響應動作"""
        
        logger.info(f"🎯 執行延遲響應動作: {action.value}")
        
        try:
            if action == LatencyAction.EMERGENCY_STOP:
                return await self._execute_emergency_stop(alert_data)
                
            elif action == LatencyAction.SWITCH_CONSERVATIVE:
                return await self._execute_conservative_mode(alert_data)
                
            elif action == LatencyAction.REDUCE_FREQUENCY:
                return await self._execute_frequency_reduction(alert_data)
                
            elif action == LatencyAction.MONITOR:
                return await self._execute_enhanced_monitoring(alert_data)
            
            else:
                return {"success": False, "error": f"Unknown action: {action}"}
                
        except Exception as e:
            logger.error(f"❌ 響應動作執行失敗: {e}")
            return {"success": False, "error": str(e)}
    
    async def _execute_emergency_stop(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """執行緊急停止"""
        logger.critical("🚨 延遲過高，執行緊急停止")
        
        # 這裡應該調用實際的緊急停止機制
        # 在實際實現中，這會通過gRPC或Redis通知Rust引擎
        
        self.stats["alerts_triggered"] += 1
        
        return {
            "success": True,
            "action": "emergency_stop",
            "reason": f"延遲{alert_data.get('latency_us')}μs超過危險閾值{self.thresholds['critical_us']}μs",
            "timestamp": datetime.now().isoformat()
        }
    
    async def _execute_conservative_mode(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """執行保守模式切換"""
        logger.warning("⚠️ 切換到保守交易模式")
        
        self.conservative_mode = True
        
        # 實際實現中，這會通知Rust引擎調整交易策略
        
        return {
            "success": True,
            "action": "conservative_mode",
            "reason": f"延遲{alert_data.get('latency_us')}μs超過高延遲閾值",
            "previous_mode": "normal",
            "new_mode": "conservative"
        }
    
    async def _execute_frequency_reduction(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """執行交易頻率降低"""
        logger.info("📉 降低交易頻率以減少延遲")
        
        # 實際實現中，這會調整交易引擎的頻率參數
        
        return {
            "success": True,
            "action": "reduce_frequency",
            "frequency_reduction": 0.5,  # 降低50%頻率
            "reason": "持續高延遲"
        }
    
    async def _execute_enhanced_monitoring(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """執行增強監控"""
        logger.info("👁️ 啟動增強延遲監控")
        
        return {
            "success": True,
            "action": "enhanced_monitoring",
            "monitoring_interval": 1,  # 1秒間隔
            "duration": 300  # 持續5分鐘
        }
    
    def _update_latency_stats(self, latency_us: float, level: LatencyLevel):
        """更新延遲統計數據"""
        # 更新平均延遲
        total = self.stats["total_measurements"]
        current_avg = self.stats["avg_latency_us"]
        self.stats["avg_latency_us"] = (current_avg * (total - 1) + latency_us) / total
        
        # 更新最大延遲
        if latency_us > self.stats["max_latency_us"]:
            self.stats["max_latency_us"] = latency_us
        
        # 更新P99延遲
        if len(self.recent_latencies) >= 100:
            recent_values = [m["latency_us"] for m in self.recent_latencies[-100:]]
            self.stats["p99_latency_us"] = np.percentile(recent_values, 99)
        
        # 如果觸發告警
        if level in [LatencyLevel.HIGH, LatencyLevel.CRITICAL]:
            self.stats["alerts_triggered"] += 1
    
    async def get_latency_status(self) -> Dict[str, Any]:
        """獲取延遲監控狀態"""
        return {
            "current_level": self.current_level.value,
            "conservative_mode": self.conservative_mode,
            "thresholds": self.thresholds,
            "stats": self.stats,
            "recent_measurements": len(self.recent_latencies),
            "last_update": datetime.now().isoformat()
        }
    
    async def update_thresholds(self, new_thresholds: Dict[str, float]) -> Dict[str, Any]:
        """更新延遲閾值"""
        old_thresholds = self.thresholds.copy()
        
        for key, value in new_thresholds.items():
            if key in self.thresholds:
                self.thresholds[key] = value
        
        logger.info(f"🔧 延遲閾值已更新: {old_thresholds} -> {self.thresholds}")
        
        return {
            "success": True,
            "old_thresholds": old_thresholds,
            "new_thresholds": self.thresholds
        }

# ==========================================
# 測試和驗證函數
# ==========================================

async def test_latency_guard():
    """測試LatencyGuard Agent"""
    guard = LatencyGuardAgent()
    
    # 模擬延遲告警
    test_alerts = [
        {"latency_us": 8.5, "component": "orderbook"},      # NORMAL
        {"latency_us": 15.2, "component": "strategy"},      # ELEVATED
        {"latency_us": 22.7, "component": "execution"},     # HIGH
        {"latency_us": 35.1, "component": "network"}        # CRITICAL
    ]
    
    print("🧪 測試LatencyGuard Agent")
    print("=" * 50)
    
    for i, alert_data in enumerate(test_alerts, 1):
        print(f"\n測試 {i}: 延遲 {alert_data['latency_us']}μs")
        
        result = await guard.handle_latency_breach(alert_data)
        
        print(f"  等級: {result['level']}")
        print(f"  動作: {result['action']}")
        print(f"  成功: {result['success']}")
    
    # 獲取狀態
    status = await guard.get_latency_status()
    print(f"\n📊 最終狀態:")
    print(f"  總測量數: {status['stats']['total_measurements']}")
    print(f"  告警數: {status['stats']['alerts_triggered']}")
    print(f"  平均延遲: {status['stats']['avg_latency_us']:.2f}μs")
    print(f"  最大延遲: {status['stats']['max_latency_us']:.2f}μs")

if __name__ == "__main__":
    asyncio.run(test_latency_guard())