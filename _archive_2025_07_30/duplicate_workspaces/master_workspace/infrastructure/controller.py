#!/usr/bin/env python3
"""
Infrastructure Controller - Master Workspace核心基礎設施控制器
===========================================================

負責統一管理和協調所有基礎設施服務：
1. Redis 服務代理管理
2. ClickHouse 服務代理管理
3. Docker Compose 服務生命週期
4. 跨服務健康檢查和故障恢復

遷移策略：Strangler Fig 模式
- 逐步將服務路由到新架構
- 保持舊架構向後相容
- 零停機遷移能力
"""

import asyncio
import logging
import yaml
from typing import Dict, Any, List, Optional
from datetime import datetime
from pathlib import Path

from .redis_proxy import RedisServiceProxy
from .clickhouse_proxy import ClickHouseServiceProxy
from .docker_manager import DockerComposeManager
from .health_checker import InfrastructureHealthChecker

logger = logging.getLogger(__name__)

class InfrastructureController:
    """
    基礎設施控制器 - Master Workspace的基礎設施管理核心
    
    職責：
    - 統一管理Redis、ClickHouse代理服務
    - 協調Docker Compose服務
    - 提供健康檢查和自動恢復
    - 支援漸進式遷移策略
    """
    
    def __init__(self, config_path: str):
        self.config_path = config_path
        self.config = self._load_config()
        
        # 核心服務組件
        self.redis_proxy: Optional[RedisServiceProxy] = None
        self.clickhouse_proxy: Optional[ClickHouseServiceProxy] = None
        self.docker_manager: Optional[DockerComposeManager] = None
        self.health_checker: Optional[InfrastructureHealthChecker] = None
        
        # 運行狀態
        self.is_running = False
        self.services_status = {}
        self.migration_phase = "Phase0_Preparation"
        
        logger.info("✅ InfrastructureController 初始化完成")
    
    def _load_config(self) -> Dict[str, Any]:
        """載入基礎設施配置"""
        try:
            config_file = Path(self.config_path)
            if not config_file.exists():
                logger.warning(f"配置文件不存在，使用默認配置: {self.config_path}")
                return self._get_default_config()
            
            with open(config_file, 'r', encoding='utf-8') as f:
                config = yaml.safe_load(f)
            
            logger.info(f"✅ 載入基礎設施配置: {self.config_path}")
            return config
            
        except Exception as e:
            logger.error(f"❌ 載入配置失敗: {e}")
            return self._get_default_config()
    
    def _get_default_config(self) -> Dict[str, Any]:
        """獲取默認配置"""
        return {
            "infrastructure": {
                "redis": {
                    "host": "localhost",
                    "port": 6379,
                    "db": 0,
                    "proxy_enabled": True,
                    "failover_enabled": True,
                    "backup_instances": []
                },
                "clickhouse": {
                    "host": "localhost", 
                    "port": 9000,
                    "database": "hft",
                    "proxy_enabled": True,
                    "cluster_enabled": False,
                    "backup_instances": []
                },
                "docker": {
                    "compose_file": "docker-compose.yml",
                    "project_name": "hft-master",
                    "auto_restart": True,
                    "health_check_interval": 30
                },
                "migration": {
                    "phase": "Phase0_Preparation",
                    "traffic_split_percent": 0,
                    "rollback_enabled": True,
                    "canary_deployment": False
                }
            }
        }
    
    async def initialize(self):
        """初始化所有基礎設施服務"""
        logger.info("🚀 開始初始化基礎設施服務...")
        
        try:
            # 1. 初始化 Redis 代理
            await self._initialize_redis_proxy()
            
            # 2. 初始化 ClickHouse 代理  
            await self._initialize_clickhouse_proxy()
            
            # 3. 初始化 Docker 管理器
            await self._initialize_docker_manager()
            
            # 4. 初始化健康檢查器
            await self._initialize_health_checker()
            
            self.is_running = True
            logger.info("✅ 基礎設施服務初始化完成")
            
        except Exception as e:
            logger.error(f"❌ 基礎設施初始化失敗: {e}")
            await self.shutdown()
            raise
    
    async def _initialize_redis_proxy(self):
        """初始化 Redis 服務代理"""
        redis_config = self.config["infrastructure"]["redis"]
        
        self.redis_proxy = RedisServiceProxy(
            primary_host=redis_config["host"],
            primary_port=redis_config["port"],
            db=redis_config["db"],
            proxy_enabled=redis_config["proxy_enabled"],
            failover_enabled=redis_config["failover_enabled"],
            backup_instances=redis_config.get("backup_instances", [])
        )
        
        await self.redis_proxy.initialize()
        self.services_status["redis_proxy"] = "running"
        logger.info("✅ Redis 服務代理初始化完成")
    
    async def _initialize_clickhouse_proxy(self):
        """初始化 ClickHouse 服務代理"""
        ch_config = self.config["infrastructure"]["clickhouse"]
        
        self.clickhouse_proxy = ClickHouseServiceProxy(
            primary_host=ch_config["host"],
            primary_port=ch_config["port"], 
            database=ch_config["database"],
            proxy_enabled=ch_config["proxy_enabled"],
            cluster_enabled=ch_config["cluster_enabled"],
            backup_instances=ch_config.get("backup_instances", [])
        )
        
        await self.clickhouse_proxy.initialize()
        self.services_status["clickhouse_proxy"] = "running"
        logger.info("✅ ClickHouse 服務代理初始化完成")
    
    async def _initialize_docker_manager(self):
        """初始化 Docker Compose 管理器"""
        docker_config = self.config["infrastructure"]["docker"]
        
        self.docker_manager = DockerComposeManager(
            compose_file=docker_config["compose_file"],
            project_name=docker_config["project_name"],
            auto_restart=docker_config["auto_restart"]
        )
        
        await self.docker_manager.initialize()
        self.services_status["docker_manager"] = "running"
        logger.info("✅ Docker Compose 管理器初始化完成")
    
    async def _initialize_health_checker(self):
        """初始化健康檢查器"""
        health_config = self.config["infrastructure"]["docker"]
        
        self.health_checker = InfrastructureHealthChecker(
            redis_proxy=self.redis_proxy,
            clickhouse_proxy=self.clickhouse_proxy,
            docker_manager=self.docker_manager,
            check_interval=health_config["health_check_interval"]
        )
        
        await self.health_checker.initialize()
        self.services_status["health_checker"] = "running"
        logger.info("✅ 健康檢查器初始化完成")
    
    async def start_services(self):
        """啟動所有基礎設施服務"""
        if not self.is_running:
            await self.initialize()
        
        logger.info("🚀 啟動基礎設施服務...")
        
        # 啟動服務順序很重要
        startup_tasks = [
            self.docker_manager.start_infrastructure_services(),
            self.redis_proxy.start(),
            self.clickhouse_proxy.start(),
            self.health_checker.start_monitoring()
        ]
        
        try:
            await asyncio.gather(*startup_tasks)
            logger.info("✅ 所有基礎設施服務啟動完成")
            return True
            
        except Exception as e:
            logger.error(f"❌ 服務啟動失敗: {e}")
            await self.shutdown()
            raise
    
    async def stop_services(self):
        """停止所有基礎設施服務""" 
        logger.info("🛑 正在停止基礎設施服務...")
        
        # 按相反順序停止服務
        stop_tasks = []
        
        if self.health_checker:
            stop_tasks.append(self.health_checker.stop_monitoring())
            
        if self.clickhouse_proxy:
            stop_tasks.append(self.clickhouse_proxy.stop())
            
        if self.redis_proxy:
            stop_tasks.append(self.redis_proxy.stop())
            
        if self.docker_manager:
            stop_tasks.append(self.docker_manager.stop_services())
        
        try:
            await asyncio.gather(*stop_tasks, return_exceptions=True)
            logger.info("✅ 所有基礎設施服務已停止")
            
        except Exception as e:
            logger.error(f"❌ 服務停止過程中出現錯誤: {e}")
    
    async def get_service_status(self) -> Dict[str, Any]:
        """獲取所有服務狀態"""
        status = {
            "controller_running": self.is_running,
            "migration_phase": self.migration_phase,
            "timestamp": datetime.now().isoformat(),
            "services": {}
        }
        
        # 檢查各個服務狀態
        if self.redis_proxy:
            status["services"]["redis"] = await self.redis_proxy.get_status()
            
        if self.clickhouse_proxy:
            status["services"]["clickhouse"] = await self.clickhouse_proxy.get_status()
            
        if self.docker_manager:
            status["services"]["docker"] = await self.docker_manager.get_status()
            
        if self.health_checker:
            status["services"]["health"] = await self.health_checker.get_status()
        
        return status
    
    async def perform_health_check(self) -> Dict[str, Any]:
        """執行完整的健康檢查"""
        if not self.health_checker:
            return {"healthy": False, "error": "Health checker not initialized"}
        
        return await self.health_checker.perform_comprehensive_check()
    
    async def trigger_failover(self, service: str) -> bool:
        """觸發指定服務的故障轉移"""
        logger.warning(f"🔄 觸發 {service} 服務故障轉移...")
        
        try:
            if service == "redis" and self.redis_proxy:
                return await self.redis_proxy.trigger_failover()
                
            elif service == "clickhouse" and self.clickhouse_proxy:
                return await self.clickhouse_proxy.trigger_failover()
                
            else:
                logger.error(f"❌ 不支援的服務故障轉移: {service}")
                return False
                
        except Exception as e:
            logger.error(f"❌ {service} 故障轉移失敗: {e}")
            return False
    
    async def update_migration_phase(self, phase: str, traffic_percent: int = 0):
        """更新遷移階段"""
        logger.info(f"🔄 更新遷移階段: {self.migration_phase} -> {phase}")
        
        self.migration_phase = phase
        self.config["infrastructure"]["migration"]["phase"] = phase
        self.config["infrastructure"]["migration"]["traffic_split_percent"] = traffic_percent
        
        # 更新各代理的遷移配置
        if self.redis_proxy:
            await self.redis_proxy.update_traffic_split(traffic_percent)
            
        if self.clickhouse_proxy:
            await self.clickhouse_proxy.update_traffic_split(traffic_percent)
        
        logger.info(f"✅ 遷移階段更新完成: {phase} (流量分配: {traffic_percent}%)")
    
    async def rollback_migration(self):
        """回滾遷移"""
        logger.warning("⚠️ 執行遷移回滾...")
        
        try:
            # 將流量切回舊服務
            await self.update_migration_phase("Phase0_Preparation", 0)
            
            # 停止新服務
            if self.redis_proxy:
                await self.redis_proxy.switch_to_legacy()
                
            if self.clickhouse_proxy:
                await self.clickhouse_proxy.switch_to_legacy()
            
            logger.info("✅ 遷移回滾完成")
            return True
            
        except Exception as e:
            logger.error(f"❌ 遷移回滾失敗: {e}")
            return False
    
    async def shutdown(self):
        """優雅關閉所有服務"""
        logger.info("🛑 開始優雅關閉基礎設施控制器...")
        
        self.is_running = False
        
        await self.stop_services()
        
        # 清理資源
        self.redis_proxy = None
        self.clickhouse_proxy = None  
        self.docker_manager = None
        self.health_checker = None
        
        logger.info("✅ 基礎設施控制器已優雅關閉")

# 工廠函數
async def create_infrastructure_controller(config_path: str) -> InfrastructureController:
    """創建並初始化基礎設施控制器"""
    controller = InfrastructureController(config_path)
    await controller.initialize()
    return controller