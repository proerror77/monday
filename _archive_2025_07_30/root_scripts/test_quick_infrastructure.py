#!/usr/bin/env python3
"""
快速基礎設施測試
=================
"""

import asyncio
import logging
import time

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

async def test_all_services():
    """快速測試所有服務"""
    logger.info("🚀 開始快速服務測試")
    
    # 測試 Redis
    try:
        import redis
        client = redis.Redis(host="localhost", port=6379, decode_responses=True)
        response = client.ping()
        if response:
            logger.info("✅ Redis 連接測試通過")
            
            # 測試基本操作
            test_key = f"hft_test_{int(time.time())}"
            test_value = "test_success"
            client.set(test_key, test_value, ex=60)
            retrieved = client.get(test_key)
            
            if retrieved == test_value:
                logger.info("✅ Redis 數據操作測試通過")
            client.delete(test_key)
        else:
            logger.error("❌ Redis ping 失敗")
    except Exception as e:
        logger.error(f"❌ Redis 測試失敗: {e}")
    
    # 測試 ClickHouse
    try:
        import aiohttp
        async with aiohttp.ClientSession() as session:
            async with session.get("http://localhost:8123/ping") as response:
                if response.status == 200:
                    logger.info("✅ ClickHouse ping 測試通過")
                    
                    # 測試查詢
                    async with session.get("http://localhost:8123/?query=SELECT 1") as query_response:
                        if query_response.status == 200:
                            result = await query_response.text()
                            logger.info(f"✅ ClickHouse 查詢測試通過: {result.strip()}")
    except Exception as e:
        logger.error(f"❌ ClickHouse 測試失敗: {e}")
    
    # 測試 Prometheus
    try:
        import aiohttp
        async with aiohttp.ClientSession() as session:
            async with session.get("http://localhost:9090/-/healthy") as response:
                if response.status == 200:
                    logger.info("✅ Prometheus 健康檢查通過")
    except Exception as e:
        logger.error(f"❌ Prometheus 測試失敗: {e}")
    
    # 測試 Grafana
    try:
        import aiohttp
        async with aiohttp.ClientSession() as session:
            async with session.get("http://localhost:3000/api/health") as response:
                if response.status == 200:
                    logger.info("✅ Grafana 健康檢查通過")
    except Exception as e:
        logger.error(f"❌ Grafana 測試失敗: {e}")
    
    logger.info("🎉 所有基礎設施服務測試完成!")

if __name__ == "__main__":
    asyncio.run(test_all_services())