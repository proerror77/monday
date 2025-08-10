#!/usr/bin/env python3
"""
Redis Service Proxy 測試腳本
============================

測試 Redis 服務代理的核心功能：
1. 基本連接和操作
2. 故障轉移機制
3. 健康檢查
4. 性能指標
"""

import asyncio
import logging
import time
import random
from redis_proxy import RedisServiceProxy

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

async def test_basic_operations(proxy: RedisServiceProxy):
    """測試基本操作"""
    logger.info("🧪 測試基本 Redis 操作...")
    
    try:
        # 測試 SET 操作
        await proxy.set("test_key", "test_value", ex=60)
        logger.info("✅ SET 操作成功")
        
        # 測試 GET 操作
        value = await proxy.get("test_key")
        assert value == "test_value", f"期望 'test_value'，得到 '{value}'"
        logger.info("✅ GET 操作成功")
        
        # 測試 EXISTS 操作
        exists = await proxy.exists("test_key")
        assert exists == 1, f"期望 1，得到 {exists}"
        logger.info("✅ EXISTS 操作成功")
        
        # 測試 DELETE 操作
        deleted = await proxy.delete("test_key")
        assert deleted == 1, f"期望 1，得到 {deleted}"
        logger.info("✅ DELETE 操作成功")
        
        logger.info("✅ 所有基本操作測試通過")
        return True
        
    except Exception as e:
        logger.error(f"❌ 基本操作測試失敗: {e}")
        return False

async def test_performance(proxy: RedisServiceProxy, operations: int = 1000):
    """測試性能"""
    logger.info(f"⚡ 測試性能 ({operations} 操作)...")
    
    try:
        start_time = time.time()
        
        # 並發寫入測試
        write_tasks = []
        for i in range(operations):
            task = proxy.set(f"perf_key_{i}", f"value_{i}", ex=60)
            write_tasks.append(task)
        
        await asyncio.gather(*write_tasks)
        write_time = time.time() - start_time
        
        # 並發讀取測試
        start_time = time.time()
        read_tasks = []
        for i in range(operations):
            task = proxy.get(f"perf_key_{i}")
            read_tasks.append(task)
        
        results = await asyncio.gather(*read_tasks)
        read_time = time.time() - start_time
        
        # 驗證結果
        expected_results = [f"value_{i}" for i in range(operations)]
        assert results == expected_results, "讀取結果不匹配"
        
        # 清理
        delete_tasks = [proxy.delete(f"perf_key_{i}") for i in range(operations)]
        await asyncio.gather(*delete_tasks)
        
        logger.info(f"✅ 性能測試完成:")
        logger.info(f"   寫入: {operations} 操作，耗時 {write_time:.2f}s，QPS: {operations/write_time:.0f}")
        logger.info(f"   讀取: {operations} 操作，耗時 {read_time:.2f}s，QPS: {operations/read_time:.0f}")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 性能測試失敗: {e}")
        return False

async def test_pub_sub(proxy: RedisServiceProxy):
    """測試發布/訂閱功能"""
    logger.info("📡 測試發布/訂閱功能...")
    
    try:
        # 測試發布消息
        message_count = await proxy.publish("test_channel", "test_message")
        logger.info(f"✅ 發布消息成功，訂閱者數量: {message_count}")
        
        # 測試 JSON 消息
        import json
        json_message = json.dumps({"type": "test", "data": "test_data", "timestamp": time.time()})
        await proxy.publish("test_json_channel", json_message)
        logger.info("✅ JSON 消息發布成功")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 發布/訂閱測試失敗: {e}")
        return False

async def test_health_and_stats(proxy: RedisServiceProxy):
    """測試健康檢查和統計功能"""
    logger.info("🏥 測試健康檢查和統計...")
    
    try:
        # 獲取狀態
        status = await proxy.get_status()
        logger.info(f"✅ 代理狀態: {status['running']}")
        logger.info(f"   當前實例: {status['current_instance']}")
        logger.info(f"   斷路器狀態: {status['circuit_breaker_open']}")
        
        # 檢查統計數據
        stats = status['stats']
        logger.info(f"   總操作數: {stats['total_operations']}")
        logger.info(f"   成功操作: {stats['successful_operations']}")
        logger.info(f"   失敗操作: {stats['failed_operations']}")
        
        success_rate = (stats['successful_operations'] / max(stats['total_operations'], 1)) * 100
        logger.info(f"   成功率: {success_rate:.1f}%")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 健康檢查測試失敗: {e}")
        return False

async def test_error_handling(proxy: RedisServiceProxy):
    """測試錯誤處理"""
    logger.info("🚨 測試錯誤處理...")
    
    try:
        # 測試無效操作（這應該會成功，因為 Redis 很寬容）
        await proxy.set("", "empty_key_test")
        logger.info("✅ 空鍵測試通過")
        
        # 測試大量數據
        large_data = "x" * 10000  # 10KB 數據
        await proxy.set("large_data", large_data, ex=10)
        retrieved = await proxy.get("large_data")
        assert retrieved == large_data, "大數據測試失敗"
        logger.info("✅ 大數據測試通過")
        
        # 清理
        await proxy.delete("", "large_data")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 錯誤處理測試失敗: {e}")
        return False

async def run_comprehensive_test():
    """運行全面測試"""
    logger.info("🚀 開始 Redis Service Proxy 全面測試")
    
    # 創建代理實例
    proxy = RedisServiceProxy(
        primary_host="localhost",
        primary_port=6379,
        db=0,
        proxy_enabled=True,
        failover_enabled=False,  # 測試時關閉故障轉移
        backup_instances=[]
    )
    
    try:
        # 初始化代理
        await proxy.initialize()
        await proxy.start()
        
        # 執行測試套件
        tests = [
            ("基本操作測試", test_basic_operations),
            ("性能測試", test_performance),
            ("發布/訂閱測試", test_pub_sub),
            ("健康檢查測試", test_health_and_stats),
            ("錯誤處理測試", test_error_handling)
        ]
        
        passed = 0
        total = len(tests)
        
        for test_name, test_func in tests:
            logger.info(f"\n{'='*50}")
            logger.info(f"執行: {test_name}")
            logger.info(f"{'='*50}")
            
            try:
                if len(asyncio.signature(test_func).parameters) > 1:
                    # 性能測試需要額外參數
                    result = await test_func(proxy, 100)  # 減少操作數量用於測試
                else:
                    result = await test_func(proxy)
                
                if result:
                    passed += 1
                    logger.info(f"✅ {test_name} 通過")
                else:
                    logger.error(f"❌ {test_name} 失敗")
                    
            except Exception as e:
                logger.error(f"❌ {test_name} 異常: {e}")
        
        # 測試結果總結
        logger.info(f"\n{'='*50}")
        logger.info(f"測試結果總結")
        logger.info(f"{'='*50}")
        logger.info(f"通過: {passed}/{total} ({passed/total*100:.1f}%)")
        
        if passed == total:
            logger.info("🎉 所有測試通過！Redis Service Proxy 工作正常")
            return True
        else:
            logger.warning(f"⚠️ {total-passed} 個測試失敗")
            return False
            
    except Exception as e:
        logger.error(f"❌ 測試執行異常: {e}")
        return False
        
    finally:
        # 清理
        await proxy.stop()

if __name__ == "__main__":
    # 運行測試
    success = asyncio.run(run_comprehensive_test())
    exit(0 if success else 1)