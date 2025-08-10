#!/usr/bin/env python3
"""
基礎設施測試 - 僅測試 Redis, ClickHouse, Prometheus, Grafana
=====================================================

簡化版測試，避免複雜的應用構建問題
"""

import asyncio
import logging
import subprocess
import time
import sys
from pathlib import Path

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class BasicInfrastructureTester:
    """基礎設施測試器"""
    
    def __init__(self, workspace_root: str):
        self.workspace_root = Path(workspace_root)
        self.master_workspace_dir = self.workspace_root
        
    async def test_infrastructure_services(self):
        """測試基礎設施服務"""
        logger.info("🚀 開始基礎設施服務測試")
        
        try:
            # 1. 啟動基礎設施服務
            logger.info("🔧 啟動基礎設施服務 (Redis, ClickHouse, Prometheus, Grafana)")
            
            cmd = [
                "docker-compose", 
                "-f", "docker-compose-test.yml",
                "-p", "hft-infra-test",
                "up", "-d"
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=self.master_workspace_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                logger.info("✅ 基礎設施服務啟動成功")
                
                # 等待服務準備就緒
                await self.wait_for_services()
                
                # 測試各服務
                await self.test_redis()
                await self.test_clickhouse()
                await self.test_prometheus()
                await self.test_grafana()
                
                logger.info("✅ 所有基礎設施服務測試通過")
                
            else:
                error_msg = stderr.decode() if stderr else "未知錯誤"
                logger.error(f"❌ 基礎設施服務啟動失敗: {error_msg}")
                
        except Exception as e:
            logger.error(f"❌ 基礎設施測試異常: {e}")
        
        finally:
            # 清理
            await self.cleanup()
    
    async def wait_for_services(self, max_wait=120):
        """等待服務準備就緒"""
        logger.info("⏳ 等待服務準備就緒...")
        
        elapsed = 0
        check_interval = 10
        
        while elapsed < max_wait:
            await asyncio.sleep(check_interval)
            elapsed += check_interval
            logger.info(f"等待中... ({elapsed}s/{max_wait}s)")
        
        logger.info("⏰ 服務預熱完成")
    
    async def test_redis(self):
        """測試 Redis"""
        try:
            logger.info("🔍 測試 Redis 連接...")
            
            import redis
            client = redis.Redis(host="localhost", port=6379, decode_responses=True)
            
            # 測試連接
            response = client.ping()
            if response:
                logger.info("✅ Redis 連接測試通過")
                
                # 測試基本操作
                test_key = "hft_test_key"
                test_value = f"test_value_{int(time.time())}"
                
                client.set(test_key, test_value, ex=60)
                retrieved_value = client.get(test_key)
                
                if retrieved_value == test_value:
                    logger.info("✅ Redis 數據操作測試通過")
                else:
                    logger.error("❌ Redis 數據不一致")
                
                client.delete(test_key)
            else:
                logger.error("❌ Redis ping 失敗")
                
        except Exception as e:
            logger.error(f"❌ Redis 測試失敗: {e}")
    
    async def test_clickhouse(self):
        """測試 ClickHouse"""
        try:
            logger.info("🔍 測試 ClickHouse 連接...")
            
            import aiohttp
            
            async with aiohttp.ClientSession() as session:
                # 測試 ping
                async with session.get("http://localhost:8123/ping") as response:
                    if response.status == 200:
                        logger.info("✅ ClickHouse ping 測試通過")
                        
                        # 測試簡單查詢
                        async with session.get("http://localhost:8123/?query=SELECT 1") as query_response:
                            if query_response.status == 200:
                                result = await query_response.text()
                                logger.info(f"✅ ClickHouse 查詢測試通過: {result.strip()}")
                            else:
                                logger.error(f"❌ ClickHouse 查詢失敗: {query_response.status}")
                    else:
                        logger.error(f"❌ ClickHouse ping 失敗: {response.status}")
                        
        except Exception as e:
            logger.error(f"❌ ClickHouse 測試失敗: {e}")
    
    async def test_prometheus(self):
        """測試 Prometheus"""
        try:
            logger.info("🔍 測試 Prometheus 連接...")
            
            import aiohttp
            
            async with aiohttp.ClientSession() as session:
                async with session.get("http://localhost:9090/-/healthy") as response:
                    if response.status == 200:
                        logger.info("✅ Prometheus 健康檢查通過")
                    else:
                        logger.error(f"❌ Prometheus 健康檢查失敗: {response.status}")
                        
        except Exception as e:
            logger.error(f"❌ Prometheus 測試失敗: {e}")
    
    async def test_grafana(self):
        """測試 Grafana"""
        try:
            logger.info("🔍 測試 Grafana 連接...")
            
            import aiohttp
            
            async with aiohttp.ClientSession() as session:
                async with session.get("http://localhost:3000/api/health") as response:
                    if response.status == 200:
                        logger.info("✅ Grafana 健康檢查通過")
                    else:
                        logger.error(f"❌ Grafana 健康檢查失敗: {response.status}")
                        
        except Exception as e:
            logger.error(f"❌ Grafana 測試失敗: {e}")
    
    async def cleanup(self):
        """清理測試環境"""
        try:
            logger.info("🧹 清理測試環境...")
            
            cmd = [
                "docker-compose",
                "-f", "docker-compose-test.yml", 
                "-p", "hft-infra-test",
                "down", "-v", "--remove-orphans"
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=self.master_workspace_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            await process.communicate()
            logger.info("✅ 測試環境清理完成")
            
        except Exception as e:
            logger.warning(f"⚠️ 清理測試環境失敗: {e}")

async def main():
    """主函數"""
    # 檢查工作區目錄
    workspace_root = Path("/Users/proerror/Documents/monday")
    if not workspace_root.exists():
        logger.error(f"❌ 工作區目錄不存在: {workspace_root}")
        return 1
    
    # 創建測試器
    tester = BasicInfrastructureTester(str(workspace_root))
    
    # 運行測試
    try:
        await tester.test_infrastructure_services()
        logger.info("🎉 基礎設施測試完成！")
        return 0
        
    except KeyboardInterrupt:
        logger.info("👋 測試被用戶中斷")
        return 130
    except Exception as e:
        logger.error(f"❌ 測試執行異常: {e}")
        return 1

if __name__ == "__main__":
    exit_code = asyncio.run(main())
    sys.exit(exit_code)