"""
系統集成單元測試
===============

測試 HFTSystemIntegration 類的核心功能
"""

import pytest
import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

from system_integration import HFTSystemIntegration, SystemMode, TradingState
from workflows.workflow_manager_v2 import WorkflowPriority


@pytest.mark.unit
class TestHFTSystemIntegration:
    """HFT 系統集成測試類"""
    
    @pytest.mark.asyncio
    async def test_system_initialization(self, mock_hft_system):
        """測試系統初始化"""
        # 執行初始化
        result = await mock_hft_system.initialize_system()
        
        # 驗證結果
        assert result["success"] is True
        assert "system_state" in result
        assert "initialized_symbols" in result
        assert mock_hft_system.system_state == TradingState.WARMING_UP
    
    @pytest.mark.asyncio
    async def test_start_trading_success(self, mock_hft_system):
        """測試交易啟動成功"""
        # 設置系統狀態
        mock_hft_system.system_state = TradingState.WARMING_UP
        
        # Mock workflow manager
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.create_comprehensive_trading_workflow = AsyncMock(
                return_value="workflow_123"
            )
            
            # 執行交易啟動
            result = await mock_hft_system.start_trading(["BTCUSDT"], "gradual")
            
            # 驗證結果
            assert result["success"] is True
            assert result["system_state"] == TradingState.ACTIVE.value
            assert "BTCUSDT" in result["started_symbols"]
            assert mock_hft_system.system_state == TradingState.ACTIVE
    
    @pytest.mark.asyncio
    async def test_start_trading_wrong_state(self, mock_hft_system):
        """測試錯誤狀態下啟動交易"""
        # 設置錯誤狀態
        mock_hft_system.system_state = TradingState.STOPPED
        
        # 執行交易啟動
        result = await mock_hft_system.start_trading(["BTCUSDT"], "gradual")
        
        # 驗證失敗
        assert result["success"] is False
        assert "Cannot start trading" in result["error"]
    
    @pytest.mark.asyncio
    async def test_emergency_stop(self, mock_hft_system):
        """測試緊急停止"""
        # 設置活躍策略
        mock_hft_system.active_strategies = {
            "BTCUSDT": {
                "workflow_id": "workflow_123",
                "start_time": "2024-01-01T00:00:00",
                "status": "active"
            }
        }
        
        # Mock workflow manager
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.emergency_stop_workflow = AsyncMock(
                return_value={"success": True}
            )
            
            # 執行緊急停止
            result = await mock_hft_system.emergency_stop("測試原因")
            
            # 驗證結果
            assert result["success"] is True
            assert result["emergency_event"]["reason"] == "測試原因"
            assert mock_hft_system.system_state == TradingState.EMERGENCY_STOP
    
    @pytest.mark.asyncio
    async def test_optimize_strategies(self, mock_hft_system):
        """測試策略優化"""
        # 設置活躍策略
        mock_hft_system.active_strategies = {
            "BTCUSDT": {
                "workflow_id": "workflow_123",
                "start_time": "2024-01-01T00:00:00"
            }
        }
        
        # Mock workflow manager
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.create_model_optimization_workflow = AsyncMock(
                return_value="opt_workflow_456"
            )
            
            # 執行策略優化
            result = await mock_hft_system.optimize_strategies(
                ["BTCUSDT"], "sharpe_ratio"
            )
            
            # 驗證結果
            assert result["success"] is True
            assert "BTCUSDT" in result["optimization_results"]
            assert result["optimization_results"]["BTCUSDT"]["success"] is True
    
    @pytest.mark.asyncio
    async def test_get_system_status(self, mock_hft_system):
        """測試獲取系統狀態"""
        # Mock workflow manager
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.get_comprehensive_statistics = MagicMock(return_value={
                "total_workflows": 5,
                "active_sessions": 2,
                "success_rate": 85.5
            })
            mock_wm.get_session_status = MagicMock(return_value={
                "found": True,
                "session": {"status": "running"}
            })
            
            # 設置活躍策略
            mock_hft_system.active_strategies = {
                "BTCUSDT": {
                    "workflow_id": "workflow_123",
                    "start_time": "2024-01-01T00:00:00"
                }
            }
            
            # 獲取系統狀態
            status = await mock_hft_system.get_system_status()
            
            # 驗證結果
            assert "system_state" in status
            assert "workflow_statistics" in status
            assert "strategy_status" in status
            assert "performance_metrics" in status
    
    def test_calculate_uptime(self, mock_hft_system):
        """測試運行時間計算"""
        uptime = mock_hft_system._calculate_uptime()
        assert isinstance(uptime, float)
        assert uptime >= 0
    
    def test_should_trigger_optimization(self, mock_hft_system):
        """測試是否應該觸發優化的判斷邏輯"""
        # 測試低 Sharpe 比率觸發優化
        low_sharpe_performance = {"sharpe_ratio": 0.8, "max_drawdown": -0.05}
        assert mock_hft_system._should_trigger_optimization(low_sharpe_performance) is True
        
        # 測試高回撤觸發優化
        high_drawdown_performance = {"sharpe_ratio": 1.5, "max_drawdown": -0.12}
        assert mock_hft_system._should_trigger_optimization(high_drawdown_performance) is True
        
        # 測試正常性能不觸發優化
        good_performance = {"sharpe_ratio": 1.5, "max_drawdown": -0.05}
        assert mock_hft_system._should_trigger_optimization(good_performance) is False
    
    def test_check_risk_limits_breached(self, mock_hft_system):
        """測試風險限額突破檢查"""
        # 測試正常風險指標
        normal_risk = {"total_exposure": 8000, "var_95": -400}
        assert mock_hft_system._check_risk_limits_breached(normal_risk) is False
        
        # 測試敞口超限
        high_exposure_risk = {"total_exposure": 150000, "var_95": -400}
        assert mock_hft_system._check_risk_limits_breached(high_exposure_risk) is True
        
        # 測試 VaR 超限
        high_var_risk = {"total_exposure": 8000, "var_95": -6000}
        assert mock_hft_system._check_risk_limits_breached(high_var_risk) is True


@pytest.mark.unit
@pytest.mark.performance
class TestSystemPerformance:
    """系統性能測試"""
    
    @pytest.mark.asyncio
    async def test_initialization_performance(self, mock_hft_system):
        """測試初始化性能"""
        import time
        
        start_time = time.time()
        result = await mock_hft_system.initialize_system()
        end_time = time.time()
        
        # 初始化應該在 5 秒內完成
        initialization_time = end_time - start_time
        assert initialization_time < 5.0
        assert result["success"] is True
    
    @pytest.mark.asyncio
    async def test_status_query_performance(self, mock_hft_system):
        """測試狀態查詢性能"""
        import time
        
        # Mock dependencies
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.get_comprehensive_statistics = MagicMock(return_value={})
            mock_wm.get_session_status = MagicMock(return_value={"found": False})
            
            start_time = time.time()
            status = await mock_hft_system.get_system_status()
            end_time = time.time()
            
            # 狀態查詢應該在 1 秒內完成
            query_time = end_time - start_time
            assert query_time < 1.0
            assert status is not None


@pytest.mark.unit
@pytest.mark.slow
class TestSystemReliability:
    """系統可靠性測試"""
    
    @pytest.mark.asyncio
    async def test_initialization_retry_on_failure(self, test_config):
        """測試初始化失敗時的重試機制"""
        system = HFTSystemIntegration(test_config)
        
        # Mock 初始化失敗然後成功
        call_count = 0
        async def mock_health_check():
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {"passed": False, "reason": "Temporary failure"}
            return {"passed": True, "health_score": 0.95}
        
        system._conduct_system_health_check = mock_health_check
        system._initialize_rust_engine = AsyncMock(return_value={"success": True})
        
        # 第一次初始化應該失敗
        result1 = await system.initialize_system()
        assert result1["success"] is False
        
        # 第二次初始化應該成功
        result2 = await system.initialize_system()
        assert result2["success"] is True
    
    @pytest.mark.asyncio
    async def test_emergency_stop_reliability(self, mock_hft_system):
        """測試緊急停止的可靠性"""
        # 設置多個活躍策略
        mock_hft_system.active_strategies = {
            "BTCUSDT": {"workflow_id": "wf1"},
            "ETHUSDT": {"workflow_id": "wf2"},
            "ADAUSDT": {"workflow_id": "wf3"}
        }
        
        # Mock 部分失敗的情況
        def mock_stop_workflow(workflow_id):
            if workflow_id == "wf2":
                return AsyncMock(return_value={"success": False, "error": "Network timeout"})()
            return AsyncMock(return_value={"success": True})()
        
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.emergency_stop_workflow = mock_stop_workflow
            
            # 執行緊急停止
            result = await mock_hft_system.emergency_stop("系統測試")
            
            # 即使部分失敗，整體應該仍然成功
            assert result["success"] is True
            assert mock_hft_system.system_state == TradingState.EMERGENCY_STOP