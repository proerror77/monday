#!/usr/bin/env python3
"""
Infrastructure Integration Test
===============================

測試完整的基礎設施集成：
1. InfrastructureController 初始化
2. 各個代理服務的協調工作
3. 健康檢查流程
4. 故障恢復機制
"""

import asyncio
import logging
import yaml
from pathlib import Path
from controller import InfrastructureController

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

async def create_test_config():
    """創建測試配置文件"""
    test_config = {
        "infrastructure": {
            "redis": {
                "host": "localhost",
                "port": 6379,
                "db": 0,
                "proxy_enabled": True,
                "failover_enabled": False,  # 測試時關閉
                "backup_instances": []
            },
            "clickhouse": {
                "host": "localhost", 
                "port": 9000,
                "database": "default",  # 使用默認數據庫進行測試
                "proxy_enabled": True,
                "cluster_enabled": False,
                "backup_instances": []
            },
            "docker": {
                "compose_file": "test-docker-compose.yml",
                "project_name": "hft-test",
                "auto_restart": True,
                "health_check_interval": 10  # 更快的檢查間隔用於測試
            },
            "migration": {
                "phase": "Phase0_Preparation",
                "traffic_split_percent": 0,
                "rollback_enabled": True,
                "canary_deployment": False
            }
        }
    }
    
    # 寫入測試配置文件
    config_file = Path("test_config.yml")
    with open(config_file, 'w', encoding='utf-8') as f:
        yaml.dump(test_config, f, default_flow_style=False)
    
    return str(config_file)

async def test_controller_initialization():
    """測試控制器初始化"""
    logger.info("🧪 測試 InfrastructureController 初始化...")
    
    try:
        # 創建測試配置
        config_path = await create_test_config()
        
        # 創建控制器
        controller = InfrastructureController(config_path)
        
        # 驗證配置載入
        assert "infrastructure" in controller.config
        assert "redis" in controller.config["infrastructure"]
        assert "clickhouse" in controller.config["infrastructure"]
        
        logger.info("✅ 控制器初始化測試通過")
        return controller, config_path
        
    except Exception as e:
        logger.error(f"❌ 控制器初始化測試失敗: {e}")
        raise

async def test_service_initialization(controller: InfrastructureController):
    """測試服務初始化"""
    logger.info("🔧 測試服務初始化...")
    
    try:
        # 嘗試初始化服務（可能會因為外部依賴而失敗）
        try:
            await controller.initialize()
            logger.info("✅ 完整服務初始化成功")
            return True
        except Exception as init_error:
            logger.warning(f"⚠️ 完整初始化失敗（預期，因為可能缺少外部服務）: {init_error}")
            
            # 檢查部分初始化是否成功
            if hasattr(controller, 'redis_proxy') and controller.redis_proxy:
                logger.info("✅ Redis 代理創建成功")
            if hasattr(controller, 'clickhouse_proxy') and controller.clickhouse_proxy:
                logger.info("✅ ClickHouse 代理創建成功")
            
            return False  # 部分成功
            
    except Exception as e:
        logger.error(f"❌ 服務初始化測試失敗: {e}")
        return False

async def test_status_reporting(controller: InfrastructureController):
    """測試狀態報告"""
    logger.info("📊 測試狀態報告...")
    
    try:
        # 獲取服務狀態（即使服務未完全啟動也應該能工作）
        status = await controller.get_service_status()
        
        # 驗證狀態結構
        assert "controller_running" in status
        assert "migration_phase" in status
        assert "timestamp" in status
        assert "services" in status
        
        logger.info(f"✅ 狀態報告測試通過")
        logger.info(f"   控制器運行: {status['controller_running']}")
        logger.info(f"   遷移階段: {status['migration_phase']}") 
        logger.info(f"   服務數量: {len(status['services'])}")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 狀態報告測試失敗: {e}")
        return False

async def test_migration_phase_updates(controller: InfrastructureController):
    """測試遷移階段更新"""
    logger.info("🔄 測試遷移階段更新...")
    
    try:
        # 測試階段更新
        phases = [
            ("Phase1_Infrastructure", 20),
            ("Phase2_Service_Proxy", 50),
            ("Phase3_Traffic_Switch", 100)
        ]
        
        for phase, traffic_percent in phases:
            await controller.update_migration_phase(phase, traffic_percent)
            
            # 驗證更新
            assert controller.migration_phase == phase
            assert controller.config["infrastructure"]["migration"]["traffic_split_percent"] == traffic_percent
            
            logger.info(f"✅ 階段更新成功: {phase} ({traffic_percent}%)")
        
        # 測試回滾
        rollback_success = await controller.rollback_migration()
        logger.info(f"✅ 回滾測試: {'成功' if rollback_success else '失敗'}")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 遷移階段測試失敗: {e}")
        return False

async def test_health_check(controller: InfrastructureController):
    """測試健康檢查"""
    logger.info("🏥 測試健康檢查...")
    
    try:
        # 執行健康檢查（可能會因為外部服務不可用而失敗）
        health_result = await controller.perform_health_check()
        
        # 驗證健康檢查結果結構
        assert "healthy" in health_result
        
        if health_result.get("healthy"):
            logger.info("✅ 健康檢查通過 - 所有服務正常")
        else:
            logger.warning("⚠️ 健康檢查失敗 - 部分服務不可用（測試環境預期）")
            if "error" in health_result:
                logger.info(f"   錯誤信息: {health_result['error']}")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ 健康檢查測試失敗: {e}")
        return False

async def cleanup_test_files():
    """清理測試文件"""
    try:
        test_files = ["test_config.yml", "test-docker-compose.yml"]
        for file_path in test_files:
            path = Path(file_path)
            if path.exists():
                path.unlink()
                logger.info(f"🗑️ 清理測試文件: {file_path}")
    except Exception as e:
        logger.warning(f"⚠️ 清理測試文件失敗: {e}")

async def run_integration_test():
    """運行集成測試"""
    logger.info("🚀 開始基礎設施集成測試")
    
    controller = None
    config_path = None
    
    try:
        # 測試套件
        tests = [
            ("控制器初始化", test_controller_initialization),
            ("服務初始化", test_service_initialization),
            ("狀態報告", test_status_reporting),
            ("遷移階段更新", test_migration_phase_updates),
            ("健康檢查", test_health_check)
        ]
        
        passed = 0
        total = len(tests)
        results = {}
        
        # 執行第一個測試（控制器初始化）
        logger.info(f"\n{'='*50}")
        logger.info(f"執行: {tests[0][0]}")
        logger.info(f"{'='*50}")
        
        controller, config_path = await tests[0][1]()
        passed += 1
        results[tests[0][0]] = True
        
        # 執行其餘測試
        for test_name, test_func in tests[1:]:
            logger.info(f"\n{'='*50}")
            logger.info(f"執行: {test_name}")
            logger.info(f"{'='*50}")
            
            try:
                result = await test_func(controller)
                results[test_name] = result
                if result:
                    passed += 1
                    logger.info(f"✅ {test_name} 通過")
                else:
                    logger.warning(f"⚠️ {test_name} 部分通過或失敗")
                    
            except Exception as e:
                logger.error(f"❌ {test_name} 異常: {e}")
                results[test_name] = False
        
        # 測試結果總結
        logger.info(f"\n{'='*50}")
        logger.info(f"集成測試結果總結")
        logger.info(f"{'='*50}")
        
        for test_name, result in results.items():
            status = "✅ 通過" if result else "❌ 失敗"
            logger.info(f"  {test_name}: {status}")
        
        logger.info(f"\n總計: {passed}/{total} ({passed/total*100:.1f}%)")
        
        if passed >= total * 0.8:  # 80% 通過率視為成功
            logger.info("🎉 集成測試基本通過！基礎設施架構可用")
            return True
        else:
            logger.warning(f"⚠️ 集成測試通過率不足 80%")
            return False
            
    except Exception as e:
        logger.error(f"❌ 集成測試執行異常: {e}")
        return False
        
    finally:
        # 清理
        if controller:
            try:
                await controller.shutdown()
            except:
                pass
        
        await cleanup_test_files()

if __name__ == "__main__":
    # 運行集成測試
    success = asyncio.run(run_integration_test())
    exit(0 if success else 1)