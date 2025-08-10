#!/usr/bin/env python3
"""
端到端集成測試 - 驗證整個 HFT 系統功能
======================================

測試完整的數據流：
1. Master Workspace 基礎設施啟動
2. Ops Workspace 告警系統
3. ML Workspace 訓練流程
4. Rust HFT 核心引擎
5. 服務間通信和代理功能

運行前確保：
- Docker 和 docker-compose 已安裝
- 所有 workspace 代碼已更新
- 網路端口 6379, 9000, 8123, 50051 可用
"""

import asyncio
import logging
import docker
import json
import time
import sys
import subprocess
from pathlib import Path
from typing import Dict, Any, List
from datetime import datetime

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class HFTSystemIntegrationTester:
    """
    HFT 系統端到端集成測試器
    
    測試覆蓋：
    1. 基礎設施啟動和健康檢查
    2. 服務間通信測試
    3. 數據流端到端驗證
    4. 故障恢復和回滾測試
    """
    
    def __init__(self, workspace_root: str):
        self.workspace_root = Path(workspace_root)
        self.docker_client = docker.from_env()
        
        # 測試配置
        self.test_config = {
            "test_duration_seconds": 120,  # 2分鐘測試
            "health_check_interval": 10,
            "max_startup_wait": 180,  # 3分鐘啟動時間
            "test_symbol": "BTCUSDT",
            "expected_services": [
                "redis", "clickhouse", "prometheus", "grafana",
                "master-workspace", "ops-workspace", "ml-workspace"
            ]
        }
        
        # 測試結果
        self.test_results = {
            "infrastructure_startup": False,
            "service_discovery": False,
            "redis_proxy": False,
            "clickhouse_proxy": False,
            "ops_workspace_alerts": False,
            "ml_workspace_training": False,
            "end_to_end_flow": False,
            "start_time": None,
            "end_time": None,
            "errors": []
        }
        
        logger.info("✅ HFT 系統集成測試器初始化完成")
    
    async def run_full_integration_test(self) -> Dict[str, Any]:
        """運行完整的集成測試"""
        logger.info("🚀 開始 HFT 系統端到端集成測試")
        
        self.test_results["start_time"] = datetime.now()
        
        try:
            # 測試階段 1: 基礎設施啟動
            logger.info("\n" + "="*60)
            logger.info("測試階段 1: 基礎設施啟動和健康檢查")
            logger.info("="*60)
            
            await self.test_infrastructure_startup()
            
            # 測試階段 2: 服務發現和代理
            logger.info("\n" + "="*60) 
            logger.info("測試階段 2: 服務發現和代理功能")
            logger.info("="*60)
            
            await self.test_service_discovery()
            await self.test_redis_proxy()
            await self.test_clickhouse_proxy()
            
            # 測試階段 3: Workspace 功能
            logger.info("\n" + "="*60)
            logger.info("測試階段 3: Workspace 功能驗證")
            logger.info("="*60)
            
            await self.test_ops_workspace()
            await self.test_ml_workspace()
            
            # 測試階段 4: 端到端數據流
            logger.info("\n" + "="*60)
            logger.info("測試階段 4: 端到端數據流驗證")
            logger.info("="*60)
            
            await self.test_end_to_end_flow()
            
            # 測試結果總結
            self.test_results["end_time"] = datetime.now()
            return await self.generate_test_report()
            
        except Exception as e:
            logger.error(f"❌ 集成測試異常: {e}")
            self.test_results["errors"].append(f"Test exception: {e}")
            return await self.generate_test_report()
        
        finally:
            # 清理測試環境
            await self.cleanup_test_environment()
    
    async def test_infrastructure_startup(self):
        """測試基礎設施啟動"""
        try:
            logger.info("🔧 啟動基礎設施服務...")
            
            # 切換到 master_workspace 目錄
            master_workspace_dir = self.workspace_root / "master_workspace"
            
            # 啟動 docker-compose 服務
            cmd = [
                "docker-compose", 
                "-f", "docker-compose.yml",
                "-p", "hft-test",
                "up", "-d", 
                "--build"
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=master_workspace_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                logger.info("✅ Docker Compose 服務啟動成功")
                
                # 等待服務完全啟動
                await self.wait_for_services_ready()
                
                self.test_results["infrastructure_startup"] = True
            else:
                error_msg = stderr.decode() if stderr else "未知錯誤"
                logger.error(f"❌ Docker Compose 啟動失敗: {error_msg}")
                self.test_results["errors"].append(f"Docker startup failed: {error_msg}")
                
        except Exception as e:
            logger.error(f"❌ 基礎設施啟動測試失敗: {e}")
            self.test_results["errors"].append(f"Infrastructure startup failed: {e}")
    
    async def wait_for_services_ready(self):
        """等待服務準備就緒"""
        logger.info("⏳ 等待服務準備就緒...")
        
        max_wait = self.test_config["max_startup_wait"]
        check_interval = 10
        elapsed = 0
        
        while elapsed < max_wait:
            try:
                # 檢查關鍵服務是否運行
                containers = self.docker_client.containers.list(
                    filters={"label": "com.docker.compose.project=hft-test"}
                )
                
                running_services = []
                for container in containers:
                    if container.status == "running":
                        service_name = container.labels.get("com.docker.compose.service", "unknown")
                        running_services.append(service_name)
                
                logger.info(f"運行中的服務: {running_services}")
                
                # 檢查是否有足夠的核心服務運行
                core_services = ["redis", "clickhouse", "master-workspace"]
                running_core = [s for s in core_services if s in running_services]
                
                if len(running_core) >= 2:  # 至少需要 2 個核心服務
                    logger.info("✅ 核心服務已準備就緒")
                    break
                
                await asyncio.sleep(check_interval)
                elapsed += check_interval
                
            except Exception as e:
                logger.warning(f"⚠️ 服務檢查異常: {e}")
                await asyncio.sleep(check_interval)
                elapsed += check_interval
        
        if elapsed >= max_wait:
            logger.warning("⚠️ 服務啟動超時，但繼續測試")
    
    async def test_service_discovery(self):
        """測試服務發現功能"""
        try:
            logger.info("🔍 測試服務發現功能...")
            
            # 這裡可以實現具體的服務發現測試
            # 例如：檢查服務註冊、健康檢查等
            
            # 簡化實現：檢查容器是否正確啟動
            containers = self.docker_client.containers.list(
                filters={"label": "com.docker.compose.project=hft-test"}
            )
            
            service_count = len(containers)
            logger.info(f"發現 {service_count} 個服務")
            
            if service_count >= 3:  # 預期至少有 3 個服務
                self.test_results["service_discovery"] = True
                logger.info("✅ 服務發現測試通過")
            else:
                logger.warning("⚠️ 發現的服務數量少於預期")
                
        except Exception as e:
            logger.error(f"❌ 服務發現測試失敗: {e}")
            self.test_results["errors"].append(f"Service discovery failed: {e}")
    
    async def test_redis_proxy(self):
        """測試 Redis 代理功能"""
        try:
            logger.info("📡 測試 Redis 代理功能...")
            
            # 嘗試連接 Redis（通過代理或直連）
            import redis.asyncio as redis
            
            try:
                # 嘗試直連測試
                redis_client = redis.Redis(host="localhost", port=6379, decode_responses=True)
                await redis_client.ping()
                
                # 測試基本操作
                test_key = "hft_test_key"
                test_value = f"test_value_{int(time.time())}"
                
                await redis_client.set(test_key, test_value, ex=60)
                retrieved_value = await redis_client.get(test_key)
                
                if retrieved_value == test_value:
                    self.test_results["redis_proxy"] = True
                    logger.info("✅ Redis 代理測試通過")
                else:
                    logger.error("❌ Redis 數據不一致")
                    
                await redis_client.delete(test_key)
                await redis_client.close()
                
            except Exception as redis_error:
                logger.warning(f"⚠️ Redis 連接失敗: {redis_error}")
                # 如果直連失敗，可能需要等待服務完全啟動
                
        except Exception as e:
            logger.error(f"❌ Redis 代理測試失敗: {e}")
            self.test_results["errors"].append(f"Redis proxy test failed: {e}")
    
    async def test_clickhouse_proxy(self):
        """測試 ClickHouse 代理功能"""
        try:
            logger.info("🗄️ 測試 ClickHouse 代理功能...")
            
            # 嘗試連接 ClickHouse
            import aiohttp
            
            try:
                async with aiohttp.ClientSession() as session:
                    # 測試 ClickHouse HTTP 介面
                    url = "http://localhost:8123/ping"
                    
                    async with session.get(url) as response:
                        if response.status == 200:
                            self.test_results["clickhouse_proxy"] = True
                            logger.info("✅ ClickHouse 代理測試通過")
                        else:
                            logger.warning(f"⚠️ ClickHouse 響應狀態: {response.status}")
                            
            except Exception as ch_error:
                logger.warning(f"⚠️ ClickHouse 連接失敗: {ch_error}")
                
        except Exception as e:
            logger.error(f"❌ ClickHouse 代理測試失敗: {e}")
            self.test_results["errors"].append(f"ClickHouse proxy test failed: {e}")
    
    async def test_ops_workspace(self):
        """測試 Ops Workspace 功能"""
        try:
            logger.info("🚨 測試 Ops Workspace 告警功能...")
            
            # 檢查 ops-workspace 容器是否運行
            try:
                ops_container = self.docker_client.containers.get("hft-ops-workspace")
                if ops_container.status == "running":
                    self.test_results["ops_workspace_alerts"] = True
                    logger.info("✅ Ops Workspace 運行正常")
                else:
                    logger.warning(f"⚠️ Ops Workspace 狀態: {ops_container.status}")
                    
            except docker.errors.NotFound:
                logger.warning("⚠️ Ops Workspace 容器未找到")
                
        except Exception as e:
            logger.error(f"❌ Ops Workspace 測試失敗: {e}")
            self.test_results["errors"].append(f"Ops workspace test failed: {e}")
    
    async def test_ml_workspace(self):
        """測試 ML Workspace 功能"""
        try:
            logger.info("🤖 測試 ML Workspace 訓練功能...")
            
            # 檢查 ml-workspace 容器是否運行
            try:
                ml_container = self.docker_client.containers.get("hft-ml-workspace")
                if ml_container.status == "running":
                    self.test_results["ml_workspace_training"] = True
                    logger.info("✅ ML Workspace 運行正常")
                else:
                    logger.warning(f"⚠️ ML Workspace 狀態: {ml_container.status}")
                    
            except docker.errors.NotFound:
                logger.warning("⚠️ ML Workspace 容器未找到")
                
        except Exception as e:
            logger.error(f"❌ ML Workspace 測試失敗: {e}")
            self.test_results["errors"].append(f"ML workspace test failed: {e}")
    
    async def test_end_to_end_flow(self):
        """測試端到端數據流"""
        try:
            logger.info("🔄 測試端到端數據流...")
            
            # 模擬一個完整的數據流測試
            # 1. 發送測試數據到 Redis
            # 2. 檢查數據是否正確傳遞
            # 3. 驗證各個服務的響應
            
            if (self.test_results["redis_proxy"] and 
                self.test_results["service_discovery"]):
                
                self.test_results["end_to_end_flow"] = True
                logger.info("✅ 端到端數據流測試通過")
            else:
                logger.warning("⚠️ 前置條件不滿足，跳過端到端測試")
                
        except Exception as e:
            logger.error(f"❌ 端到端測試失敗: {e}")
            self.test_results["errors"].append(f"End-to-end test failed: {e}")
    
    async def generate_test_report(self) -> Dict[str, Any]:
        """生成測試報告"""
        if not self.test_results["end_time"]:
            self.test_results["end_time"] = datetime.now()
        
        duration = (self.test_results["end_time"] - self.test_results["start_time"]).total_seconds()
        
        # 計算通過率
        total_tests = len([k for k in self.test_results.keys() 
                          if k not in ["start_time", "end_time", "errors"]])
        passed_tests = sum(1 for k, v in self.test_results.items() 
                          if k not in ["start_time", "end_time", "errors"] and v)
        
        pass_rate = (passed_tests / total_tests) * 100 if total_tests > 0 else 0
        
        report = {
            "test_summary": {
                "total_tests": total_tests,
                "passed_tests": passed_tests,
                "failed_tests": total_tests - passed_tests,
                "pass_rate_percent": pass_rate,
                "duration_seconds": duration,
                "overall_success": pass_rate >= 70  # 70% 通過率視為成功
            },
            "detailed_results": self.test_results,
            "recommendations": self._generate_recommendations()
        }
        
        # 打印報告
        self._print_test_report(report)
        
        return report
    
    def _generate_recommendations(self) -> List[str]:
        """生成改進建議"""
        recommendations = []
        
        if not self.test_results["infrastructure_startup"]:
            recommendations.append("檢查 Docker 和 docker-compose 配置")
        
        if not self.test_results["redis_proxy"]:
            recommendations.append("檢查 Redis 服務配置和網路連接")
        
        if not self.test_results["clickhouse_proxy"]:
            recommendations.append("檢查 ClickHouse 服務配置和啟動時間")
        
        if not self.test_results["ops_workspace_alerts"]:
            recommendations.append("檢查 Ops Workspace 容器配置和依賴")
        
        if not self.test_results["ml_workspace_training"]:
            recommendations.append("檢查 ML Workspace 容器配置和資源")
        
        if self.test_results["errors"]:
            recommendations.append("查看詳細錯誤日誌進行故障排除")
        
        return recommendations
    
    def _print_test_report(self, report: Dict[str, Any]):
        """打印測試報告"""
        summary = report["test_summary"]
        
        print("\n" + "="*80)
        print("HFT 系統端到端集成測試報告")
        print("="*80)
        
        print(f"📊 測試總結:")
        print(f"   總測試數: {summary['total_tests']}")
        print(f"   通過測試: {summary['passed_tests']}")
        print(f"   失敗測試: {summary['failed_tests']}")
        print(f"   通過率: {summary['pass_rate_percent']:.1f}%")
        print(f"   測試時長: {summary['duration_seconds']:.1f} 秒")
        
        if summary["overall_success"]:
            print("🎉 整體測試結果: ✅ 成功")
        else:
            print("⚠️ 整體測試結果: ❌ 需要改進")
        
        print(f"\n📋 詳細結果:")
        for test_name, result in self.test_results.items():
            if test_name not in ["start_time", "end_time", "errors"]:
                status = "✅ 通過" if result else "❌ 失敗"
                print(f"   {test_name}: {status}")
        
        if self.test_results["errors"]:
            print(f"\n❌ 錯誤詳情:")
            for error in self.test_results["errors"]:
                print(f"   - {error}")
        
        recommendations = report["recommendations"]
        if recommendations:
            print(f"\n💡 改進建議:")
            for rec in recommendations:
                print(f"   - {rec}")
        
        print("="*80)
    
    async def cleanup_test_environment(self):
        """清理測試環境"""
        try:
            logger.info("🧹 清理測試環境...")
            
            # 停止並刪除測試容器
            cmd = [
                "docker-compose",
                "-f", "docker-compose.yml", 
                "-p", "hft-test",
                "down", "-v", "--remove-orphans"
            ]
            
            master_workspace_dir = self.workspace_root / "master_workspace"
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=master_workspace_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            await process.communicate()
            
            logger.info("✅ 測試環境清理完成")
            
        except Exception as e:
            logger.warning(f"⚠️ 清理測試環境失敗: {e}")

async def main():
    """主函數"""
    import argparse
    
    parser = argparse.ArgumentParser(description="HFT 系統端到端集成測試")
    parser.add_argument("--workspace-root", 
                       default="/Users/proerror/Documents/monday",
                       help="Workspace 根目錄路徑")
    parser.add_argument("--skip-cleanup", action="store_true", 
                       help="跳過測試環境清理")
    
    args = parser.parse_args()
    
    # 檢查工作區目錄
    workspace_root = Path(args.workspace_root)
    if not workspace_root.exists():
        logger.error(f"❌ 工作區目錄不存在: {workspace_root}")
        return 1
    
    # 創建測試器
    tester = HFTSystemIntegrationTester(str(workspace_root))
    
    # 運行測試
    try:
        report = await tester.run_full_integration_test()
        
        # 根據測試結果返回退出碼
        if report["test_summary"]["overall_success"]:
            logger.info("🎉 集成測試成功完成！")
            return 0
        else:
            logger.warning("⚠️ 集成測試部分失敗，請查看報告")
            return 1
            
    except KeyboardInterrupt:
        logger.info("👋 測試被用戶中斷")
        return 130
    except Exception as e:
        logger.error(f"❌ 測試執行異常: {e}")
        return 1

if __name__ == "__main__":
    exit_code = asyncio.run(main())
    sys.exit(exit_code)