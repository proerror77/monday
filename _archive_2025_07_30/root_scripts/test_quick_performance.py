#!/usr/bin/env python3
"""
快速性能和穩定性測試
快速評估系統的基本性能指標
"""

import asyncio
import json
import logging
import time
import sys
import statistics
from pathlib import Path
from datetime import datetime

# 添加工作區路徑
sys.path.append(str(Path(__file__).parent / "ops_workspace"))

from ops_workspace.connectors.rust_hft_connector import RustHFTConnector

logger = logging.getLogger(__name__)

class QuickPerformanceTest:
    """快速性能測試"""
    
    def __init__(self):
        self.connector = None
        self.test_results = {}
        self.start_time = None
        
    async def setup(self):
        """初始化測試環境"""
        print("🔧 初始化快速性能測試...")
        
        self.connector = RustHFTConnector()
        connected = await self.connector.connect()
        if not connected:
            raise ConnectionError("無法連接到系統")
        
        self.start_time = time.time()
        print("✅ 測試環境初始化完成")
        return True
    
    async def test_basic_latency(self, iterations: int = 50) -> dict:
        """測試基本延遲"""
        print(f"\n🔍 測試基本延遲 ({iterations} 次)...")
        
        latencies = []
        errors = 0
        
        for i in range(iterations):
            try:
                start = time.perf_counter()
                await self.connector.redis_client.ping()
                end = time.perf_counter()
                
                latency_ms = (end - start) * 1000
                latencies.append(latency_ms)
                
            except Exception as e:
                errors += 1
                logger.error(f"延遲測試失敗 #{i}: {e}")
        
        if latencies:
            result = {
                "avg_latency_ms": statistics.mean(latencies),
                "min_latency_ms": min(latencies),
                "max_latency_ms": max(latencies),
                "success_rate": (iterations - errors) / iterations * 100,
                "errors": errors
            }
            
            print(f"✅ 基本延遲測試完成:")
            print(f"  - 平均延遲: {result['avg_latency_ms']:.2f}ms")
            print(f"  - 最小延遲: {result['min_latency_ms']:.2f}ms") 
            print(f"  - 最大延遲: {result['max_latency_ms']:.2f}ms")
            print(f"  - 成功率: {result['success_rate']:.1f}%")
            
            self.test_results["basic_latency"] = result
            return result
        else:
            return {"error": "所有延遲測試失敗"}
    
    async def test_message_speed(self, num_messages: int = 100) -> dict:
        """測試消息傳輸速度"""
        print(f"\n🔍 測試消息傳輸速度 ({num_messages} 條消息)...")
        
        messages_sent = 0
        errors = 0
        start_time = time.time()
        
        for i in range(num_messages):
            try:
                test_message = {
                    "id": i,
                    "timestamp": time.time(),
                    "data": f"test_message_{i}"
                }
                
                await self.connector.redis_client.publish(
                    'test.speed',
                    json.dumps(test_message)
                )
                messages_sent += 1
                
            except Exception as e:
                errors += 1
                logger.error(f"消息發送失敗 #{i}: {e}")
        
        end_time = time.time()
        duration = end_time - start_time
        
        result = {
            "messages_sent": messages_sent,
            "duration_seconds": duration,
            "messages_per_second": messages_sent / duration if duration > 0 else 0,
            "success_rate": (messages_sent / num_messages) * 100,
            "errors": errors
        }
        
        print(f"✅ 消息速度測試完成:")
        print(f"  - 發送速率: {result['messages_per_second']:.1f} msg/s")
        print(f"  - 總耗時: {result['duration_seconds']:.2f}s")
        print(f"  - 成功率: {result['success_rate']:.1f}%")
        
        self.test_results["message_speed"] = result
        return result
    
    async def test_concurrent_connections(self, num_concurrent: int = 20) -> dict:
        """測試並發連接"""
        print(f"\n🔍 測試並發連接 ({num_concurrent} 個並發)...")
        
        async def concurrent_operation(task_id: int):
            """並發操作"""
            try:
                start = time.perf_counter()
                
                # 執行基本操作
                await self.connector.redis_client.ping()
                await self.connector.redis_client.set(f"concurrent:{task_id}", f"test_data_{task_id}")
                value = await self.connector.redis_client.get(f"concurrent:{task_id}")
                
                end = time.perf_counter()
                return {
                    "task_id": task_id,
                    "success": True,
                    "duration_ms": (end - start) * 1000,
                    "data_match": value == f"test_data_{task_id}"
                }
                
            except Exception as e:
                return {
                    "task_id": task_id,
                    "success": False,
                    "error": str(e)
                }
        
        # 執行並發測試
        start_time = time.time()
        tasks = [concurrent_operation(i) for i in range(num_concurrent)]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        end_time = time.time()
        
        # 分析結果
        successful = [r for r in results if isinstance(r, dict) and r.get("success")]
        failed = [r for r in results if isinstance(r, dict) and not r.get("success")]
        exceptions = [r for r in results if isinstance(r, Exception)]
        
        durations = [r["duration_ms"] for r in successful if "duration_ms" in r]
        
        result = {
            "total_tasks": num_concurrent,
            "successful_tasks": len(successful),
            "failed_tasks": len(failed) + len(exceptions),
            "success_rate": len(successful) / num_concurrent * 100,
            "total_duration_seconds": end_time - start_time,
            "avg_task_duration_ms": statistics.mean(durations) if durations else 0,
            "max_task_duration_ms": max(durations) if durations else 0
        }
        
        print(f"✅ 並發連接測試完成:")
        print(f"  - 成功率: {result['success_rate']:.1f}%")
        print(f"  - 平均任務時間: {result['avg_task_duration_ms']:.2f}ms")
        print(f"  - 總執行時間: {result['total_duration_seconds']:.2f}s")
        
        self.test_results["concurrent_connections"] = result
        return result
    
    async def test_error_recovery(self) -> dict:
        """測試錯誤恢復能力"""
        print(f"\n🔍 測試錯誤恢復能力...")
        
        recovery_tests = []
        
        # 測試1: 無效操作恢復
        try:
            # 嘗試訪問不存在的key
            await self.connector.redis_client.get("non_existent_key_12345")
            
            # 然後執行正常操作
            await self.connector.redis_client.ping()
            recovery_tests.append({"test": "invalid_key_access", "recovered": True})
            
        except Exception as e:
            recovery_tests.append({"test": "invalid_key_access", "recovered": False, "error": str(e)})
        
        # 測試2: 快速連續操作
        try:
            operations = []
            for i in range(10):
                operations.append(self.connector.redis_client.ping())
            
            await asyncio.gather(*operations)
            recovery_tests.append({"test": "rapid_operations", "recovered": True})
            
        except Exception as e:
            recovery_tests.append({"test": "rapid_operations", "recovered": False, "error": str(e)})
        
        # 測試3: 暫停/恢復操作
        try:
            await self.connector.pause_trading("測試暫停")
            await asyncio.sleep(0.5)
            await self.connector.resume_trading()
            recovery_tests.append({"test": "pause_resume", "recovered": True})
            
        except Exception as e:
            recovery_tests.append({"test": "pause_resume", "recovered": False, "error": str(e)})
        
        successful_recoveries = len([t for t in recovery_tests if t.get("recovered")])
        
        result = {
            "total_recovery_tests": len(recovery_tests),
            "successful_recoveries": successful_recoveries,
            "recovery_rate": successful_recoveries / len(recovery_tests) * 100,
            "recovery_details": recovery_tests
        }
        
        print(f"✅ 錯誤恢復測試完成:")
        print(f"  - 恢復成功率: {result['recovery_rate']:.1f}%")
        print(f"  - 成功恢復: {successful_recoveries}/{len(recovery_tests)}")
        
        self.test_results["error_recovery"] = result
        return result
    
    async def cleanup(self):
        """清理測試環境"""
        print("\n🧹 清理測試環境...")
        
        if self.connector:
            # 清理測試數據
            try:
                keys = await self.connector.redis_client.keys("concurrent:*")
                if keys:
                    await self.connector.redis_client.delete(*keys)
            except:
                pass
            
            await self.connector.disconnect()
        
        print("✅ 測試環境清理完成")
    
    def generate_summary_report(self):
        """生成測試摘要報告"""
        print("\n" + "="*60)
        print("📊 快速性能測試報告")
        print("="*60)
        
        total_runtime = time.time() - self.start_time if self.start_time else 0
        print(f"🕐 總測試時間: {total_runtime:.1f} 秒")
        print(f"📈 完成的測試: {len(self.test_results)}")
        
        # 性能評估
        print(f"\n📋 性能指標摘要:")
        
        # 延遲評估
        latency_result = self.test_results.get("basic_latency", {})
        avg_latency = latency_result.get("avg_latency_ms", 0)
        if avg_latency > 0:
            if avg_latency < 5:
                status = "✅ 優秀"
            elif avg_latency < 10:
                status = "🟡 良好"
            else:
                status = "❌ 需要優化"
            
            print(f"  - 延遲表現: {status} ({avg_latency:.2f}ms 平均)")
        
        # 吞吐量評估
        speed_result = self.test_results.get("message_speed", {})
        msg_rate = speed_result.get("messages_per_second", 0)
        if msg_rate > 0:
            if msg_rate > 500:
                status = "✅ 優秀"
            elif msg_rate > 100:
                status = "🟡 良好"
            else:
                status = "❌ 需要優化"
            
            print(f"  - 吞吐量表現: {status} ({msg_rate:.1f} msg/s)")
        
        # 並發評估
        concurrent_result = self.test_results.get("concurrent_connections", {})
        success_rate = concurrent_result.get("success_rate", 0)
        if success_rate > 0:
            if success_rate > 95:
                status = "✅ 優秀"
            elif success_rate > 85:
                status = "🟡 良好"
            else:
                status = "❌ 需要優化"
            
            print(f"  - 並發表現: {status} ({success_rate:.1f}% 成功率)")
        
        # 恢復能力評估
        recovery_result = self.test_results.get("error_recovery", {})
        recovery_rate = recovery_result.get("recovery_rate", 0)
        if recovery_rate > 0:
            if recovery_rate > 90:
                status = "✅ 優秀"
            elif recovery_rate > 70:
                status = "🟡 良好"
            else:
                status = "❌ 需要優化"
            
            print(f"  - 恢復能力: {status} ({recovery_rate:.1f}% 恢復率)")
        
        # 整體評估
        all_success_rates = [
            latency_result.get("success_rate", 0),
            speed_result.get("success_rate", 0), 
            concurrent_result.get("success_rate", 0),
            recovery_result.get("recovery_rate", 0)
        ]
        
        overall_success = statistics.mean([rate for rate in all_success_rates if rate > 0])
        
        print(f"\n🎯 整體性能評估:")
        if overall_success > 95:
            print("✅ 系統性能優秀 - 準備投入生產使用")
        elif overall_success > 85:
            print("🟡 系統性能良好 - 可以使用但建議優化")
        else:
            print("❌ 系統性能需要改進 - 建議優化後再使用")
        
        print(f"📊 整體成功率: {overall_success:.1f}%")
        
        print(f"\n📝 測試完成: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        
        return overall_success > 85

async def main():
    """主測試函數"""
    print("🚀 開始快速性能和穩定性測試")
    print("="*60)
    
    logging.basicConfig(level=logging.ERROR)  # 只顯示錯誤日志
    
    test_suite = QuickPerformanceTest()
    
    try:
        # 1. 初始化
        await test_suite.setup()
        
        # 2. 執行快速測試套件
        await test_suite.test_basic_latency(50)
        await test_suite.test_message_speed(100)
        await test_suite.test_concurrent_connections(20)
        await test_suite.test_error_recovery()
        
        # 3. 生成報告
        success = test_suite.generate_summary_report()
        
        return 0 if success else 1
        
    except Exception as e:
        print(f"❌ 快速性能測試失敗: {e}")
        return 1
    finally:
        await test_suite.cleanup()

if __name__ == "__main__":
    exit_code = asyncio.run(main())
    exit(exit_code)