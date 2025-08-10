#!/usr/bin/env python3
"""
性能和穩定性測試
測試系統在高負載和長時間運行下的性能表現和穩定性
"""

import asyncio
import json
import logging
import time
import sys
import psutil
import statistics
from pathlib import Path
from datetime import datetime, timedelta
from typing import Dict, Any, List
from concurrent.futures import ThreadPoolExecutor

# 添加工作區路徑
sys.path.append(str(Path(__file__).parent / "ops_workspace"))
sys.path.append(str(Path(__file__).parent / "ml_workspace"))

from ops_workspace.connectors.rust_hft_connector import RustHFTConnector
from ops_workspace.agents.real_latency_guard import RealLatencyGuardAgent

logger = logging.getLogger(__name__)

class PerformanceStabilityTest:
    """性能和穩定性測試套件"""
    
    def __init__(self):
        self.connector = None
        self.ops_agent = None
        self.test_results = {}
        self.performance_metrics = []
        self.error_count = 0
        self.start_time = None
        
    async def setup(self):
        """初始化測試環境"""
        print("🔧 初始化性能測試環境...")
        
        self.connector = RustHFTConnector()
        self.ops_agent = RealLatencyGuardAgent()
        
        # 連接系統
        connected = await self.connector.connect()
        if not connected:
            raise ConnectionError("無法連接到系統")
        
        self.start_time = time.time()
        print("✅ 性能測試環境初始化完成")
        return True
    
    async def test_connection_latency(self, iterations: int = 100) -> Dict[str, Any]:
        """測試連接延遲"""
        print(f"\n🔍 測試連接延遲 ({iterations} 次測試)...")
        
        latencies = []
        errors = 0
        
        for i in range(iterations):
            try:
                start = time.perf_counter()
                
                # 測試 Redis ping
                await self.connector.redis_client.ping()
                
                end = time.perf_counter()
                latency_ms = (end - start) * 1000
                latencies.append(latency_ms)
                
                if i % 20 == 0:
                    print(f"  進度: {i+1}/{iterations} - 當前延遲: {latency_ms:.2f}ms")
                
            except Exception as e:
                errors += 1
                logger.error(f"連接測試失敗 #{i}: {e}")
        
        if latencies:
            result = {
                "test": "connection_latency",
                "iterations": iterations,
                "success_rate": (iterations - errors) / iterations * 100,
                "avg_latency_ms": statistics.mean(latencies),
                "min_latency_ms": min(latencies),
                "max_latency_ms": max(latencies),
                "p95_latency_ms": statistics.quantiles(latencies, n=20)[18] if len(latencies) > 20 else max(latencies),
                "p99_latency_ms": statistics.quantiles(latencies, n=100)[98] if len(latencies) > 100 else max(latencies),
                "std_dev": statistics.stdev(latencies) if len(latencies) > 1 else 0,
                "errors": errors
            }
            
            print(f"✅ 連接延遲測試完成:")
            print(f"  - 成功率: {result['success_rate']:.1f}%")
            print(f"  - 平均延遲: {result['avg_latency_ms']:.2f}ms") 
            print(f"  - P95 延遲: {result['p95_latency_ms']:.2f}ms")
            print(f"  - P99 延遲: {result['p99_latency_ms']:.2f}ms")
            
            self.test_results["connection_latency"] = result
            return result
        else:
            return {"test": "connection_latency", "error": "所有測試失敗"}
    
    async def test_message_throughput(self, duration_seconds: int = 30) -> Dict[str, Any]:
        """測試消息吞吐量"""
        print(f"\n🔍 測試消息吞吐量 ({duration_seconds}秒 高頻發送)...")
        
        messages_sent = 0
        messages_received = 0
        errors = 0
        start_time = time.time()
        end_time = start_time + duration_seconds
        
        # 設置消息接收計數器
        received_messages = []
        
        async def message_receiver():
            """消息接收器"""
            nonlocal messages_received
            try:
                pubsub = self.connector.redis_client.pubsub()
                await pubsub.subscribe('performance.test')
                
                async for message in pubsub.listen():
                    if message['type'] == 'message':
                        messages_received += 1
                        received_messages.append(time.time())
                        
                        # 如果測試時間結束就退出
                        if time.time() > end_time:
                            break
                            
            except Exception as e:
                logger.error(f"消息接收失敗: {e}")
        
        # 啟動接收器
        receiver_task = asyncio.create_task(message_receiver())
        
        # 等待接收器啟動
        await asyncio.sleep(1)
        
        # 高頻發送消息
        try:
            while time.time() < end_time:
                try:
                    test_message = {
                        "timestamp": time.time(),
                        "sequence": messages_sent,
                        "test_id": "throughput_test"
                    }
                    
                    await self.connector.redis_client.publish(
                        'performance.test', 
                        json.dumps(test_message)
                    )
                    messages_sent += 1
                    
                    # 短暫延遲以控制發送頻率
                    await asyncio.sleep(0.001)  # 1ms 間隔
                    
                except Exception as e:
                    errors += 1
                    if errors % 10 == 0:
                        logger.error(f"發送錯誤 #{errors}: {e}")
        
        finally:
            # 停止接收器
            receiver_task.cancel()
            try:
                await receiver_task
            except asyncio.CancelledError:
                pass
        
        actual_duration = time.time() - start_time
        
        result = {
            "test": "message_throughput",
            "duration_seconds": actual_duration,
            "messages_sent": messages_sent,
            "messages_received": messages_received,
            "send_rate_per_second": messages_sent / actual_duration,
            "receive_rate_per_second": messages_received / actual_duration,
            "message_loss_rate": (messages_sent - messages_received) / messages_sent * 100 if messages_sent > 0 else 0,
            "errors": errors,
            "error_rate": errors / messages_sent * 100 if messages_sent > 0 else 0
        }
        
        print(f"✅ 消息吞吐量測試完成:")
        print(f"  - 發送速率: {result['send_rate_per_second']:.1f} msg/s")
        print(f"  - 接收速率: {result['receive_rate_per_second']:.1f} msg/s") 
        print(f"  - 消息丟失率: {result['message_loss_rate']:.1f}%")
        print(f"  - 錯誤率: {result['error_rate']:.1f}%")
        
        self.test_results["message_throughput"] = result
        return result
    
    async def test_concurrent_operations(self, num_concurrent: int = 50) -> Dict[str, Any]:
        """測試並發操作"""
        print(f"\n🔍 測試並發操作 ({num_concurrent} 個並發任務)...")
        
        async def concurrent_task(task_id: int):
            """單個並發任務"""
            try:
                start = time.perf_counter()
                
                # 執行多種操作
                operations = [
                    self.connector.redis_client.ping(),
                    self.connector.redis_client.set(f"test:{task_id}", json.dumps({"task_id": task_id, "timestamp": time.time()})),
                    self.connector.redis_client.get(f"test:{task_id}"),
                    self.connector.redis_client.publish(f"test.concurrent.{task_id}", json.dumps({"task_id": task_id}))
                ]
                
                await asyncio.gather(*operations)
                
                end = time.perf_counter()
                return {
                    "task_id": task_id,
                    "success": True,
                    "duration_ms": (end - start) * 1000
                }
                
            except Exception as e:
                return {
                    "task_id": task_id,
                    "success": False,
                    "error": str(e)
                }
        
        # 執行並發測試
        start_time = time.time()
        tasks = [concurrent_task(i) for i in range(num_concurrent)]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        end_time = time.time()
        
        # 分析結果
        successful_tasks = []
        failed_tasks = []
        
        for result in results:
            if isinstance(result, Exception):
                failed_tasks.append({"error": str(result)})
            elif result.get("success"):
                successful_tasks.append(result)
            else:
                failed_tasks.append(result)
        
        durations = [task["duration_ms"] for task in successful_tasks]
        
        concurrent_result = {
            "test": "concurrent_operations",
            "num_concurrent": num_concurrent,
            "total_duration_seconds": end_time - start_time,
            "successful_tasks": len(successful_tasks),
            "failed_tasks": len(failed_tasks),
            "success_rate": len(successful_tasks) / num_concurrent * 100,
            "avg_task_duration_ms": statistics.mean(durations) if durations else 0,
            "max_task_duration_ms": max(durations) if durations else 0,
            "min_task_duration_ms": min(durations) if durations else 0
        }
        
        print(f"✅ 並發操作測試完成:")
        print(f"  - 成功率: {concurrent_result['success_rate']:.1f}%")
        print(f"  - 平均任務時間: {concurrent_result['avg_task_duration_ms']:.2f}ms")
        print(f"  - 總執行時間: {concurrent_result['total_duration_seconds']:.2f}s")
        
        self.test_results["concurrent_operations"] = concurrent_result
        return concurrent_result
    
    async def test_memory_cpu_usage(self, duration_seconds: int = 60) -> Dict[str, Any]:
        """測試記憶體和CPU使用情況"""
        print(f"\n🔍 測試系統資源使用 ({duration_seconds}秒 監控)...")
        
        # 記錄資源使用情況
        cpu_samples = []
        memory_samples = []
        
        start_time = time.time()
        end_time = start_time + duration_seconds
        
        # 同時運行一些負載操作
        async def background_load():
            """背景負載生成器"""
            while time.time() < end_time:
                try:
                    # 模擬各種操作
                    await self.connector.redis_client.ping()
                    await self.connector.redis_client.set("load_test", json.dumps({"timestamp": time.time()}))
                    await self.connector.get_system_status()
                    await asyncio.sleep(0.1)
                except:
                    pass
        
        # 啟動背景負載
        load_task = asyncio.create_task(background_load())
        
        # 監控系統資源
        try:
            while time.time() < end_time:
                # 獲取 CPU 和記憶體使用率
                cpu_percent = psutil.cpu_percent(interval=0.1)
                memory_info = psutil.virtual_memory()
                
                cpu_samples.append(cpu_percent)
                memory_samples.append(memory_info.percent)
                
                await asyncio.sleep(1)  # 每秒採樣一次
        
        finally:
            load_task.cancel()
            try:
                await load_task
            except asyncio.CancelledError:
                pass
        
        resource_result = {
            "test": "memory_cpu_usage",
            "duration_seconds": duration_seconds,
            "cpu_usage": {
                "avg_percent": statistics.mean(cpu_samples) if cpu_samples else 0,
                "max_percent": max(cpu_samples) if cpu_samples else 0,
                "min_percent": min(cpu_samples) if cpu_samples else 0,
                "samples": len(cpu_samples)
            },
            "memory_usage": {
                "avg_percent": statistics.mean(memory_samples) if memory_samples else 0,
                "max_percent": max(memory_samples) if memory_samples else 0,
                "min_percent": min(memory_samples) if memory_samples else 0,
                "samples": len(memory_samples)
            }
        }
        
        print(f"✅ 系統資源測試完成:")
        print(f"  - 平均 CPU 使用率: {resource_result['cpu_usage']['avg_percent']:.1f}%")
        print(f"  - 最大 CPU 使用率: {resource_result['cpu_usage']['max_percent']:.1f}%")
        print(f"  - 平均記憶體使用率: {resource_result['memory_usage']['avg_percent']:.1f}%")
        print(f"  - 最大記憶體使用率: {resource_result['memory_usage']['max_percent']:.1f}%")
        
        self.test_results["memory_cpu_usage"] = resource_result
        return resource_result
    
    async def test_stability_under_stress(self, stress_duration: int = 120) -> Dict[str, Any]:
        """測試高壓力下的穩定性"""
        print(f"\n🔍 測試高壓力穩定性 ({stress_duration}秒 壓力測試)...")
        
        errors = []
        operations_completed = 0
        start_time = time.time()
        end_time = start_time + stress_duration
        
        async def stress_worker(worker_id: int):
            """壓力測試工作器"""
            nonlocal operations_completed
            worker_ops = 0
            
            while time.time() < end_time:
                try:
                    # 執行各種高頻操作
                    operations = [
                        self.connector.redis_client.ping(),
                        self.connector.redis_client.set(f"stress:{worker_id}:{worker_ops}", json.dumps({
                            "worker": worker_id,
                            "op": worker_ops,
                            "timestamp": time.time()
                        })),
                        self.connector.redis_client.publish(f"stress.worker.{worker_id}", json.dumps({
                            "worker": worker_id,
                            "operation": worker_ops
                        })),
                        self.connector.get_system_status()
                    ]
                    
                    await asyncio.gather(*operations)
                    worker_ops += 1
                    operations_completed += 1
                    
                    # 非常短的延遲
                    await asyncio.sleep(0.001)
                    
                except Exception as e:
                    errors.append({
                        "worker_id": worker_id,
                        "operation": worker_ops,
                        "error": str(e),
                        "timestamp": time.time()
                    })
        
        # 啟動多個壓力測試工作器
        num_workers = 20
        stress_tasks = [stress_worker(i) for i in range(num_workers)]
        
        # 等待所有工作器完成
        await asyncio.gather(*stress_tasks, return_exceptions=True)
        
        actual_duration = time.time() - start_time
        
        stability_result = {
            "test": "stability_under_stress",
            "duration_seconds": actual_duration,
            "num_workers": num_workers,
            "operations_completed": operations_completed,
            "operations_per_second": operations_completed / actual_duration,
            "total_errors": len(errors),
            "error_rate": len(errors) / operations_completed * 100 if operations_completed > 0 else 0,
            "errors_by_type": {}
        }
        
        # 分析錯誤類型
        error_types = {}
        for error in errors:
            error_type = type(error.get("error", "Unknown")).__name__
            error_types[error_type] = error_types.get(error_type, 0) + 1
        
        stability_result["errors_by_type"] = error_types
        
        print(f"✅ 壓力穩定性測試完成:")
        print(f"  - 操作完成數: {operations_completed}")
        print(f"  - 操作速率: {stability_result['operations_per_second']:.1f} ops/s")
        print(f"  - 錯誤率: {stability_result['error_rate']:.2f}%")
        print(f"  - 總錯誤數: {len(errors)}")
        
        self.test_results["stability_under_stress"] = stability_result
        return stability_result
    
    async def cleanup(self):
        """清理測試環境"""
        print("\n🧹 清理性能測試環境...")
        
        if self.connector:
            await self.connector.disconnect()
        
        if self.ops_agent:
            await self.ops_agent.stop_monitoring()
        
        print("✅ 性能測試環境清理完成")
    
    def generate_performance_report(self):
        """生成性能測試報告"""
        print("\n" + "="*80)
        print("📊 性能和穩定性測試報告")
        print("="*80)
        
        total_runtime = time.time() - self.start_time if self.start_time else 0
        
        print(f"🕐 總測試時間: {total_runtime:.1f} 秒")
        print(f"📈 執行的測試: {len(self.test_results)}")
        
        # 性能指標摘要
        print(f"\n📋 性能指標摘要:")
        
        for test_name, result in self.test_results.items():
            print(f"\n🔍 {test_name.replace('_', ' ').title()}:")
            
            if test_name == "connection_latency":
                print(f"  - 平均延遲: {result.get('avg_latency_ms', 0):.2f} ms")
                print(f"  - P99 延遲: {result.get('p99_latency_ms', 0):.2f} ms")
                print(f"  - 成功率: {result.get('success_rate', 0):.1f}%")
                
            elif test_name == "message_throughput":
                print(f"  - 發送速率: {result.get('send_rate_per_second', 0):.1f} msg/s")
                print(f"  - 接收速率: {result.get('receive_rate_per_second', 0):.1f} msg/s")
                print(f"  - 丟失率: {result.get('message_loss_rate', 0):.1f}%")
                
            elif test_name == "concurrent_operations":
                print(f"  - 並發任務數: {result.get('num_concurrent', 0)}")
                print(f"  - 成功率: {result.get('success_rate', 0):.1f}%")
                print(f"  - 平均執行時間: {result.get('avg_task_duration_ms', 0):.2f} ms")
                
            elif test_name == "memory_cpu_usage":
                cpu = result.get('cpu_usage', {})
                memory = result.get('memory_usage', {})
                print(f"  - 平均 CPU: {cpu.get('avg_percent', 0):.1f}%")
                print(f"  - 最大 CPU: {cpu.get('max_percent', 0):.1f}%")
                print(f"  - 平均記憶體: {memory.get('avg_percent', 0):.1f}%")
                
            elif test_name == "stability_under_stress":
                print(f"  - 操作速率: {result.get('operations_per_second', 0):.1f} ops/s")
                print(f"  - 錯誤率: {result.get('error_rate', 0):.2f}%")
                print(f"  - 總操作數: {result.get('operations_completed', 0)}")
        
        # 整體評估
        print(f"\n🎯 整體性能評估:")
        
        # 延遲評估
        latency_result = self.test_results.get("connection_latency", {})
        avg_latency = latency_result.get("avg_latency_ms", 0)
        p99_latency = latency_result.get("p99_latency_ms", 0)
        
        if avg_latency < 5:
            print("✅ 延遲表現: 優秀 (< 5ms)")
        elif avg_latency < 10:
            print("🟡 延遲表現: 良好 (< 10ms)")
        else:
            print("❌ 延遲表現: 需要優化 (> 10ms)")
        
        # 吞吐量評估
        throughput_result = self.test_results.get("message_throughput", {})
        send_rate = throughput_result.get("send_rate_per_second", 0)
        
        if send_rate > 1000:
            print("✅ 吞吐量表現: 優秀 (> 1000 msg/s)")
        elif send_rate > 500:
            print("🟡 吞吐量表現: 良好 (> 500 msg/s)")
        else:
            print("❌ 吞吐量表現: 需要優化 (< 500 msg/s)")
        
        # 穩定性評估
        stability_result = self.test_results.get("stability_under_stress", {})
        error_rate = stability_result.get("error_rate", 0)
        
        if error_rate < 1:
            print("✅ 穩定性表現: 優秀 (< 1% 錯誤率)")
        elif error_rate < 5:
            print("🟡 穩定性表現: 良好 (< 5% 錯誤率)")
        else:
            print("❌ 穩定性表現: 需要優化 (> 5% 錯誤率)")
        
        # 建議
        print(f"\n💡 優化建議:")
        
        if avg_latency > 10:
            print("- 考慮優化網絡配置和 Redis 連接池設置")
        
        if send_rate < 500:
            print("- 考慮調整消息批處理和連接複用策略")
        
        if error_rate > 1:
            print("- 增強錯誤處理和重試機制")
        
        resource_result = self.test_results.get("memory_cpu_usage", {})
        max_cpu = resource_result.get("cpu_usage", {}).get("max_percent", 0)
        max_memory = resource_result.get("memory_usage", {}).get("max_percent", 0)
        
        if max_cpu > 80:
            print("- 考慮優化 CPU 密集型操作或增加計算資源")
        
        if max_memory > 80:
            print("- 考慮優化記憶體使用或增加記憶體容量")
        
        print(f"\n📝 測試完成時間: {datetime.now().isoformat()}")
        
        return len([r for r in self.test_results.values() if r.get("success_rate", 0) > 95]) == len(self.test_results)

async def main():
    """主測試函數"""
    print("🚀 開始性能和穩定性測試")
    print("="*80)
    
    logging.basicConfig(level=logging.WARNING)  # 減少日志輸出
    
    test_suite = PerformanceStabilityTest()
    
    try:
        # 1. 初始化測試環境
        await test_suite.setup()
        
        # 2. 執行性能測試套件
        print("\n🧪 執行性能測試套件...")
        
        # 基礎性能測試
        await test_suite.test_connection_latency(100)
        await test_suite.test_message_throughput(30)
        await test_suite.test_concurrent_operations(50)
        
        # 資源使用測試
        await test_suite.test_memory_cpu_usage(60)
        
        # 穩定性壓力測試
        await test_suite.test_stability_under_stress(120)
        
        # 3. 生成測試報告
        success = test_suite.generate_performance_report()
        
        return 0 if success else 1
        
    except Exception as e:
        print(f"❌ 性能測試失敗: {e}")
        return 1
    finally:
        await test_suite.cleanup()

if __name__ == "__main__":
    exit_code = asyncio.run(main())
    exit(exit_code)