"""
系統性能測試
===========

測試系統在各種負載條件下的性能表現
"""

import pytest
import asyncio
import time
import psutil
import statistics
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import AsyncMock, MagicMock, patch

from system_integration import HFTSystemIntegration, SystemMode
from workflows.workflow_manager_v2 import WorkflowManagerV2, WorkflowPriority


@pytest.mark.performance
class TestSystemPerformance:
    """系統性能測試類"""
    
    @pytest.mark.asyncio
    async def test_initialization_performance(self, test_config):
        """測試系統初始化性能"""
        hft_system = HFTSystemIntegration(test_config)
        
        # Mock 外部依賴
        hft_system._initialize_rust_engine = AsyncMock(return_value={"success": True})
        hft_system._conduct_system_health_check = AsyncMock(return_value={"passed": True})
        
        # 測量初始化時間
        start_time = time.perf_counter()
        result = await hft_system.initialize_system()
        end_time = time.perf_counter()
        
        initialization_time = end_time - start_time
        
        # 驗證性能要求
        assert result["success"] is True
        assert initialization_time < 2.0  # 初始化應在2秒內完成
        
        print(f"System initialization time: {initialization_time:.3f}s")
    
    @pytest.mark.asyncio
    async def test_workflow_creation_performance(self):
        """測試工作流創建性能"""
        manager = WorkflowManagerV2()
        
        strategy_config = {
            "model_config": {"hidden_size": 256},
            "risk_profile": "moderate"
        }
        
        # 測量多個工作流創建時間
        creation_times = []
        
        for i in range(10):
            start_time = time.perf_counter()
            session_id = await manager.create_comprehensive_trading_workflow(
                f"SYMBOL_{i}", strategy_config, WorkflowPriority.HIGH
            )
            end_time = time.perf_counter()
            
            creation_times.append(end_time - start_time)
            assert session_id is not None
        
        # 分析性能統計
        avg_creation_time = statistics.mean(creation_times)
        max_creation_time = max(creation_times)
        
        # 驗證性能要求
        assert avg_creation_time < 0.1  # 平均創建時間應低於100ms
        assert max_creation_time < 0.2  # 最大創建時間應低於200ms
        
        print(f"Average workflow creation time: {avg_creation_time:.3f}s")
        print(f"Max workflow creation time: {max_creation_time:.3f}s")
    
    @pytest.mark.asyncio
    async def test_concurrent_workflow_creation(self):
        """測試並發工作流創建性能"""
        manager = WorkflowManagerV2()
        
        strategy_config = {
            "model_config": {"hidden_size": 128},
            "risk_profile": "conservative"
        }
        
        # 並發創建多個工作流
        async def create_workflow(symbol_id):
            return await manager.create_comprehensive_trading_workflow(
                f"CONCURRENT_{symbol_id}", strategy_config, WorkflowPriority.MEDIUM
            )
        
        # 測量並發創建時間
        start_time = time.perf_counter()
        
        tasks = [create_workflow(i) for i in range(20)]
        session_ids = await asyncio.gather(*tasks)
        
        end_time = time.perf_counter()
        concurrent_creation_time = end_time - start_time
        
        # 驗證結果
        assert len(session_ids) == 20
        assert all(session_id is not None for session_id in session_ids)
        assert concurrent_creation_time < 2.0  # 20個並發工作流應在2秒內創建完成
        
        print(f"Concurrent workflow creation time (20 workflows): {concurrent_creation_time:.3f}s")
    
    @pytest.mark.asyncio
    async def test_status_query_performance(self, test_config):
        """測試狀態查詢性能"""
        hft_system = HFTSystemIntegration(test_config)
        
        # Mock 外部依賴
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.get_comprehensive_statistics = MagicMock(return_value={
                "total_workflows": 50,
                "active_sessions": 10,
                "success_rate": 85.5
            })
            mock_wm.get_session_status = MagicMock(return_value={
                "found": True,
                "session": {"status": "running"}
            })
            
            # 設置系統狀態
            hft_system.active_strategies = {
                f"SYMBOL_{i}": {"workflow_id": f"wf_{i}", "start_time": "2024-01-01"}
                for i in range(10)
            }
            
            # 測量狀態查詢時間
            query_times = []
            
            for _ in range(100):
                start_time = time.perf_counter()
                status = await hft_system.get_system_status()
                end_time = time.perf_counter()
                
                query_times.append(end_time - start_time)
                assert status is not None
            
            # 分析查詢性能
            avg_query_time = statistics.mean(query_times)
            p95_query_time = statistics.quantiles(query_times, n=20)[18]  # P95
            
            # 驗證性能要求
            assert avg_query_time < 0.05  # 平均查詢時間應低於50ms
            assert p95_query_time < 0.1   # P95查詢時間應低於100ms
            
            print(f"Average status query time: {avg_query_time*1000:.1f}ms")
            print(f"P95 status query time: {p95_query_time*1000:.1f}ms")
    
    @pytest.mark.asyncio
    async def test_memory_usage_under_load(self):
        """測試負載下的內存使用情況"""
        manager = WorkflowManagerV2()
        
        # 記錄初始內存使用
        process = psutil.Process()
        initial_memory = process.memory_info().rss / 1024 / 1024  # MB
        
        strategy_config = {"model_config": {"hidden_size": 64}}
        
        # 創建大量工作流
        session_ids = []
        for i in range(100):
            session_id = await manager.create_comprehensive_trading_workflow(
                f"LOAD_TEST_{i}", strategy_config, WorkflowPriority.LOW
            )
            session_ids.append(session_id)
            
            # 每10個工作流檢查一次內存
            if i % 10 == 0:
                current_memory = process.memory_info().rss / 1024 / 1024
                memory_increase = current_memory - initial_memory
                
                # 內存增長不應超過500MB
                assert memory_increase < 500, f"Memory usage increased by {memory_increase:.1f}MB"
        
        # 最終內存檢查
        final_memory = process.memory_info().rss / 1024 / 1024
        total_memory_increase = final_memory - initial_memory
        
        print(f"Initial memory usage: {initial_memory:.1f}MB")
        print(f"Final memory usage: {final_memory:.1f}MB")
        print(f"Memory increase: {total_memory_increase:.1f}MB")
        
        # 驗證內存使用合理
        assert total_memory_increase < 200  # 總內存增長不應超過200MB
        assert len(session_ids) == 100
    
    @pytest.mark.asyncio
    async def test_evaluator_performance_under_load(self):
        """測試評估器在高負載下的性能"""
        from workflows.evaluators_v2.system_health_evaluator import SystemHealthEvaluator
        from workflows.evaluators_v2.model_quality_evaluator import ModelQualityEvaluator
        
        health_evaluator = SystemHealthEvaluator()
        model_evaluator = ModelQualityEvaluator()
        
        # 準備測試數據
        health_context = {
            "health_status": {
                "system_health": {
                    "cpu_usage": 45,
                    "memory_usage": 60
                }
            }
        }
        
        model_context = {
            "training_result": {
                "accuracy": 0.75,
                "f1_macro": 0.68
            }
        }
        
        # 測試並發評估性能
        async def run_evaluations():
            tasks = []
            
            # 創建大量並發評估任務
            for i in range(50):
                tasks.append(health_evaluator.evaluate(health_context))
                tasks.append(model_evaluator.evaluate(model_context))
            
            start_time = time.perf_counter()
            results = await asyncio.gather(*tasks)
            end_time = time.perf_counter()
            
            return results, end_time - start_time
        
        # 執行評估
        results, evaluation_time = await run_evaluations()
        
        # 驗證結果
        assert len(results) == 100  # 50個健康評估 + 50個模型評估
        assert all(result is not None for result in results)
        assert evaluation_time < 5.0  # 100個評估應在5秒內完成
        
        print(f"100 concurrent evaluations completed in: {evaluation_time:.3f}s")
        print(f"Average evaluation time: {evaluation_time/100*1000:.1f}ms")


@pytest.mark.performance
class TestLatencyBenchmarks:
    """延遲基準測試"""
    
    @pytest.mark.asyncio
    async def test_decision_making_latency(self, test_config):
        """測試決策延遲"""
        hft_system = HFTSystemIntegration(test_config)
        
        # Mock 快速決策流程
        hft_system._should_trigger_optimization = MagicMock(return_value=False)
        hft_system._get_strategy_performance = AsyncMock(return_value={
            "sharpe_ratio": 1.3,
            "max_drawdown": -0.05
        })
        
        # 測量決策延遲
        decision_times = []
        
        for _ in range(1000):
            start_time = time.perf_counter()
            
            # 模擬決策過程
            performance = await hft_system._get_strategy_performance("BTCUSDT")
            should_optimize = hft_system._should_trigger_optimization(performance)
            
            end_time = time.perf_counter()
            decision_times.append((end_time - start_time) * 1000000)  # 轉換為微秒
        
        # 分析延遲統計
        avg_latency = statistics.mean(decision_times)
        p95_latency = statistics.quantiles(decision_times, n=20)[18]  # P95
        p99_latency = statistics.quantiles(decision_times, n=100)[98]  # P99
        
        # 驗證延遲要求（目標：P95 < 10μs，P99 < 50μs）
        assert avg_latency < 5.0, f"Average latency {avg_latency:.1f}μs exceeds 5μs"
        assert p95_latency < 10.0, f"P95 latency {p95_latency:.1f}μs exceeds 10μs"
        assert p99_latency < 50.0, f"P99 latency {p99_latency:.1f}μs exceeds 50μs"
        
        print(f"Decision latency - Avg: {avg_latency:.1f}μs, P95: {p95_latency:.1f}μs, P99: {p99_latency:.1f}μs")
    
    @pytest.mark.asyncio
    async def test_emergency_stop_latency(self, test_config):
        """測試緊急停止延遲"""
        hft_system = HFTSystemIntegration(test_config)
        
        # 設置活躍策略
        hft_system.active_strategies = {
            "BTCUSDT": {"workflow_id": "wf1"},
            "ETHUSDT": {"workflow_id": "wf2"},
            "ADAUSDT": {"workflow_id": "wf3"}
        }
        
        # Mock 緊急停止操作
        with patch('system_integration.workflow_manager_v2') as mock_wm:
            mock_wm.emergency_stop_workflow = AsyncMock(return_value={"success": True})
            
            hft_system._emergency_stop_rust_trading = AsyncMock(return_value={"success": True})
            
            # 測量緊急停止延遲
            stop_times = []
            
            for _ in range(100):
                start_time = time.perf_counter()
                result = await hft_system.emergency_stop("性能測試")
                end_time = time.perf_counter()
                
                stop_times.append((end_time - start_time) * 1000)  # 轉換為毫秒
                assert result["success"] is True
                
                # 重置系統狀態以便下次測試
                hft_system.system_state = TradingState.ACTIVE
            
            # 分析停止延遲
            avg_stop_time = statistics.mean(stop_times)
            max_stop_time = max(stop_times)
            
            # 驗證緊急停止性能（目標：平均 < 100ms，最大 < 500ms）
            assert avg_stop_time < 100, f"Average emergency stop time {avg_stop_time:.1f}ms exceeds 100ms"
            assert max_stop_time < 500, f"Max emergency stop time {max_stop_time:.1f}ms exceeds 500ms"
            
            print(f"Emergency stop latency - Avg: {avg_stop_time:.1f}ms, Max: {max_stop_time:.1f}ms")


@pytest.mark.performance
@pytest.mark.slow
class TestThroughputBenchmarks:
    """吞吐量基準測試"""
    
    @pytest.mark.asyncio
    async def test_workflow_throughput(self):
        """測試工作流處理吞吐量"""
        manager = WorkflowManagerV2()
        
        # Mock 快速任務執行
        manager._execute_agent_task = AsyncMock(return_value={"success": True})
        manager._execute_evaluator_task = AsyncMock(return_value={"passed": True})
        manager._execute_optimizer_task = AsyncMock(return_value={"success": True})
        
        strategy_config = {"model_config": {"hidden_size": 64}}
        
        # 測試吞吐量
        start_time = time.perf_counter()
        
        # 並發創建工作流
        tasks = []
        for i in range(50):
            task = manager.create_comprehensive_trading_workflow(
                f"THROUGHPUT_{i}", strategy_config, WorkflowPriority.HIGH
            )
            tasks.append(task)
        
        session_ids = await asyncio.gather(*tasks)
        
        # 等待工作流開始執行
        await asyncio.sleep(1)
        
        end_time = time.perf_counter()
        total_time = end_time - start_time
        
        # 計算吞吐量
        workflows_per_second = len(session_ids) / total_time
        
        # 驗證吞吐量（目標：> 10 workflows/second）
        assert workflows_per_second > 10, f"Workflow throughput {workflows_per_second:.1f}/s below target"
        assert len(session_ids) == 50
        
        print(f"Workflow creation throughput: {workflows_per_second:.1f} workflows/second")
    
    @pytest.mark.asyncio
    async def test_evaluation_throughput(self):
        """測試評估器處理吞吐量"""
        from workflows.evaluators_v2.system_health_evaluator import SystemHealthEvaluator
        
        evaluator = SystemHealthEvaluator()
        context = {
            "health_status": {
                "system_health": {"cpu_usage": 50}
            }
        }
        
        # 測試評估吞吐量
        start_time = time.perf_counter()
        
        # 並發執行大量評估
        tasks = [evaluator.evaluate(context) for _ in range(200)]
        results = await asyncio.gather(*tasks)
        
        end_time = time.perf_counter()
        total_time = end_time - start_time
        
        # 計算吞吐量
        evaluations_per_second = len(results) / total_time
        
        # 驗證吞吐量（目標：> 50 evaluations/second）
        assert evaluations_per_second > 50, f"Evaluation throughput {evaluations_per_second:.1f}/s below target"
        assert len(results) == 200
        assert all(result is not None for result in results)
        
        print(f"Evaluation throughput: {evaluations_per_second:.1f} evaluations/second")


@pytest.mark.performance
class TestResourceUsage:
    """資源使用測試"""
    
    @pytest.mark.asyncio
    async def test_cpu_usage_under_load(self):
        """測試負載下的CPU使用率"""
        manager = WorkflowManagerV2()
        
        # 記錄初始CPU使用率
        initial_cpu = psutil.cpu_percent(interval=1)
        
        strategy_config = {"model_config": {"hidden_size": 32}}
        
        # 創建CPU密集型負載
        async def cpu_intensive_workflow():
            for i in range(20):
                await manager.create_comprehensive_trading_workflow(
                    f"CPU_TEST_{i}", strategy_config, WorkflowPriority.HIGH
                )
                await asyncio.sleep(0.01)  # 短暫間隔
        
        # 執行負載測試
        start_time = time.time()
        await cpu_intensive_workflow()
        end_time = time.time()
        
        # 測量CPU使用率
        final_cpu = psutil.cpu_percent(interval=1)
        
        # 驗證CPU使用合理
        cpu_increase = final_cpu - initial_cpu
        execution_time = end_time - start_time
        
        print(f"CPU usage - Initial: {initial_cpu:.1f}%, Final: {final_cpu:.1f}%")
        print(f"CPU increase: {cpu_increase:.1f}%, Execution time: {execution_time:.1f}s")
        
        # CPU使用率不應該過高（系統應保持響應）
        assert final_cpu < 90, f"CPU usage {final_cpu:.1f}% too high"
    
    def test_file_descriptor_usage(self):
        """測試文件描述符使用情況"""
        import resource
        
        # 獲取初始文件描述符數量
        initial_fds = len(psutil.Process().open_files())
        
        # 創建多個系統組件
        managers = []
        for i in range(10):
            manager = WorkflowManagerV2()
            managers.append(manager)
        
        # 檢查文件描述符增長
        final_fds = len(psutil.Process().open_files())
        fd_increase = final_fds - initial_fds
        
        print(f"File descriptors - Initial: {initial_fds}, Final: {final_fds}")
        print(f"FD increase: {fd_increase}")
        
        # 文件描述符增長應該合理
        assert fd_increase < 100, f"Too many file descriptors opened: {fd_increase}"
        
        # 清理
        del managers