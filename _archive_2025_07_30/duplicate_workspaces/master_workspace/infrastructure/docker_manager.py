#!/usr/bin/env python3
"""
Docker Compose Manager - Docker 服務管理器
=========================================

管理整個 HFT 系統的 Docker 容器生命週期：
1. 基礎設施服務管理 (Redis, ClickHouse, etc.)
2. 應用服務管理 (Rust HFT, Python Workspaces)
3. 健康檢查和自動重啟
4. 服務發現和網路管理

設計模式：Manager Pattern + Service Registry
"""

import asyncio
import docker
import yaml
import logging
from typing import Dict, Any, List, Optional
from datetime import datetime
from pathlib import Path
import subprocess
import json

logger = logging.getLogger(__name__)

class DockerComposeManager:
    """
    Docker Compose 服務管理器
    
    功能：
    - 管理 docker-compose 服務生命週期
    - 服務健康檢查和自動重啟
    - 網路和卷管理
    - 服務發現和註冊
    """
    
    def __init__(
        self,
        compose_file: str = "docker-compose.yml",
        project_name: str = "hft-master",
        auto_restart: bool = True
    ):
        self.compose_file = Path(compose_file)
        self.project_name = project_name
        self.auto_restart = auto_restart
        
        # Docker 客戶端
        self.docker_client: Optional[docker.DockerClient] = None
        
        # 服務狀態
        self.services_status = {}
        self.is_running = False
        self.infrastructure_services = [
            "redis", "clickhouse", "prometheus", "grafana"
        ]
        self.application_services = [
            "rust-hft", "ops-workspace", "ml-workspace"
        ]
        
        # 統計數據
        self.stats = {
            "services_started": 0,
            "services_stopped": 0,
            "restart_count": 0,
            "last_restart": None,
            "uptime_start": None
        }
        
        logger.info(f"✅ Docker Compose 管理器初始化: {compose_file}")
    
    async def initialize(self):
        """初始化 Docker 客戶端和配置"""
        try:
            # 初始化 Docker 客戶端
            self.docker_client = docker.from_env()
            
            # 驗證 Docker daemon 連接
            self.docker_client.ping()
            
            # 檢查 compose 文件
            if not self.compose_file.exists():
                logger.warning(f"⚠️ Docker Compose 文件不存在: {self.compose_file}")
                await self._create_default_compose_file()
            
            # 載入 compose 配置
            await self._load_compose_config()
            
            logger.info("✅ Docker Compose 管理器初始化完成")
            
        except Exception as e:
            logger.error(f"❌ Docker Compose 管理器初始化失敗: {e}")
            raise
    
    async def _load_compose_config(self):
        """載入 compose 配置"""
        try:
            with open(self.compose_file, 'r', encoding='utf-8') as f:
                config = yaml.safe_load(f)
            
            # 提取服務列表
            if 'services' in config:
                services = list(config['services'].keys())
                logger.info(f"✅ 發現 {len(services)} 個服務: {services}")
                
                # 更新服務分類
                self._classify_services(services)
            
        except Exception as e:
            logger.error(f"❌ 載入 compose 配置失敗: {e}")
            raise
    
    def _classify_services(self, services: List[str]):
        """分類服務"""
        infrastructure = []
        application = []
        
        for service in services:
            if any(infra in service.lower() for infra in ['redis', 'clickhouse', 'prometheus', 'grafana']):
                infrastructure.append(service)
            else:
                application.append(service)
        
        self.infrastructure_services = infrastructure
        self.application_services = application
        
        logger.info(f"基礎設施服務: {self.infrastructure_services}")
        logger.info(f"應用服務: {self.application_services}")
    
    async def _create_default_compose_file(self):
        """創建默認的 compose 文件"""
        default_compose = {
            'version': '3.8',
            'services': {
                'redis': {
                    'image': 'redis:7-alpine',
                    'ports': ['6379:6379'],
                    'volumes': ['redis_data:/data'],
                    'command': 'redis-server --save 60 1',
                    'healthcheck': {
                        'test': ['CMD', 'redis-cli', 'ping'],
                        'interval': '30s',
                        'timeout': '5s',
                        'retries': 3
                    }
                },
                'clickhouse': {
                    'image': 'clickhouse/clickhouse-server:latest',
                    'ports': ['9000:9000', '8123:8123'],
                    'volumes': [
                        'clickhouse_data:/var/lib/clickhouse',
                        './clickhouse/config.xml:/etc/clickhouse-server/config.xml:ro'
                    ],
                    'environment': {
                        'CLICKHOUSE_USER': 'hft_user',
                        'CLICKHOUSE_PASSWORD': 'hft_password',
                        'CLICKHOUSE_DB': 'hft'
                    },
                    'healthcheck': {
                        'test': ['CMD', 'wget', '--no-verbose', '--tries=1', '--spider', 'http://localhost:8123/ping'],
                        'interval': '30s',
                        'timeout': '5s',
                        'retries': 3
                    }
                },
                'prometheus': {
                    'image': 'prom/prometheus:latest',
                    'ports': ['9090:9090'],
                    'volumes': [
                        'prometheus_data:/prometheus',
                        './prometheus/prometheus.yml:/etc/prometheus/prometheus.yml:ro'
                    ],
                    'command': [
                        '--config.file=/etc/prometheus/prometheus.yml',
                        '--storage.tsdb.path=/prometheus',
                        '--web.console.libraries=/usr/share/prometheus/console_libraries',
                        '--web.console.templates=/usr/share/prometheus/consoles',
                        '--web.enable-lifecycle'
                    ]
                },
                'grafana': {
                    'image': 'grafana/grafana:latest',
                    'ports': ['3000:3000'],
                    'volumes': ['grafana_data:/var/lib/grafana'],
                    'environment': {
                        'GF_SECURITY_ADMIN_PASSWORD': 'hft_admin'
                    }
                }
            },
            'volumes': {
                'redis_data': {},
                'clickhouse_data': {},
                'prometheus_data': {},
                'grafana_data': {}
            },
            'networks': {
                'hft-network': {
                    'driver': 'bridge'
                }
            }
        }
        
        # 寫入文件
        with open(self.compose_file, 'w', encoding='utf-8') as f:
            yaml.dump(default_compose, f, default_flow_style=False)
        
        logger.info(f"✅ 創建默認 compose 文件: {self.compose_file}")
    
    async def start_infrastructure_services(self):
        """啟動基礎設施服務"""
        logger.info("🚀 啟動基礎設施服務...")
        
        try:
            # 按順序啟動基礎設施服務
            for service in self.infrastructure_services:
                await self._start_service(service)
                await asyncio.sleep(2)  # 等待服務啟動
            
            self.stats["uptime_start"] = datetime.now()
            logger.info("✅ 基礎設施服務啟動完成")
            
        except Exception as e:
            logger.error(f"❌ 基礎設施服務啟動失敗: {e}")
            raise
    
    async def start_application_services(self):
        """啟動應用服務"""
        logger.info("🚀 啟動應用服務...")
        
        try:
            # 並行啟動應用服務
            tasks = [
                self._start_service(service) 
                for service in self.application_services
            ]
            
            await asyncio.gather(*tasks, return_exceptions=True)
            
            logger.info("✅ 應用服務啟動完成")
            
        except Exception as e:
            logger.error(f"❌ 應用服務啟動失敗: {e}")
            raise
    
    async def _start_service(self, service_name: str):
        """啟動單個服務"""
        try:
            cmd = [
                "docker-compose", 
                "-f", str(self.compose_file),
                "-p", self.project_name,
                "up", "-d", service_name
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                self.services_status[service_name] = "running"
                self.stats["services_started"] += 1
                logger.info(f"✅ 服務 {service_name} 啟動成功")
            else:
                error_msg = stderr.decode() if stderr else "未知錯誤"
                logger.error(f"❌ 服務 {service_name} 啟動失敗: {error_msg}")
                self.services_status[service_name] = "failed"
                
        except Exception as e:
            logger.error(f"❌ 啟動服務 {service_name} 異常: {e}")
            self.services_status[service_name] = "error"
    
    async def stop_services(self):
        """停止所有服務"""
        logger.info("🛑 停止所有服務...")
        
        try:
            cmd = [
                "docker-compose",
                "-f", str(self.compose_file),
                "-p", self.project_name,
                "down"
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                self.is_running = False
                self.services_status = {}
                self.stats["services_stopped"] += len(self.infrastructure_services + self.application_services)
                logger.info("✅ 所有服務已停止")
            else:
                error_msg = stderr.decode() if stderr else "未知錯誤"
                logger.error(f"❌ 停止服務失敗: {error_msg}")
                
        except Exception as e:
            logger.error(f"❌ 停止服務異常: {e}")
    
    async def restart_service(self, service_name: str):
        """重啟單個服務"""
        logger.info(f"🔄 重啟服務: {service_name}")
        
        try:
            cmd = [
                "docker-compose",
                "-f", str(self.compose_file),
                "-p", self.project_name,
                "restart", service_name
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                self.services_status[service_name] = "running"
                self.stats["restart_count"] += 1
                self.stats["last_restart"] = datetime.now().isoformat()
                logger.info(f"✅ 服務 {service_name} 重啟成功")
                return True
            else:
                error_msg = stderr.decode() if stderr else "未知錯誤"
                logger.error(f"❌ 服務 {service_name} 重啟失敗: {error_msg}")
                return False
                
        except Exception as e:
            logger.error(f"❌ 重啟服務 {service_name} 異常: {e}")
            return False
    
    async def get_service_logs(self, service_name: str, tail: int = 100) -> str:
        """獲取服務日誌"""
        try:
            cmd = [
                "docker-compose",
                "-f", str(self.compose_file),
                "-p", self.project_name,
                "logs", "--tail", str(tail), service_name
            ]
            
            process = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                return stdout.decode()
            else:
                return f"獲取日誌失敗: {stderr.decode()}"
                
        except Exception as e:
            logger.error(f"❌ 獲取服務 {service_name} 日誌異常: {e}")
            return f"日誌獲取異常: {e}"
    
    async def check_service_health(self, service_name: str) -> Dict[str, Any]:
        """檢查服務健康狀態"""
        try:
            if not self.docker_client:
                return {"healthy": False, "error": "Docker client not initialized"}
            
            # 獲取容器
            container_name = f"{self.project_name}_{service_name}_1"
            try:
                container = self.docker_client.containers.get(container_name)
            except docker.errors.NotFound:
                return {"healthy": False, "error": "Container not found"}
            
            # 檢查容器狀態
            container.reload()
            status = container.status
            
            health_status = {
                "healthy": status == "running",
                "status": status,
                "container_id": container.id[:12],
                "created": container.attrs["Created"],
                "started": container.attrs["State"].get("StartedAt"),
                "restart_count": container.attrs["RestartCount"]
            }
            
            # 檢查健康檢查狀態（如果配置了）
            health_check = container.attrs["State"].get("Health")
            if health_check:
                health_status["health_check"] = {
                    "status": health_check["Status"],
                    "failing_streak": health_check["FailingStreak"],
                    "log": health_check["Log"][-1] if health_check["Log"] else None
                }
            
            return health_status
            
        except Exception as e:
            logger.error(f"❌ 檢查服務 {service_name} 健康狀態異常: {e}")
            return {"healthy": False, "error": str(e)}
    
    async def get_status(self) -> Dict[str, Any]:
        """獲取管理器狀態"""
        # 檢查所有服務健康狀態
        services_health = {}
        for service in self.infrastructure_services + self.application_services:
            services_health[service] = await self.check_service_health(service)
        
        return {
            "running": self.is_running,
            "project_name": self.project_name,
            "compose_file": str(self.compose_file),
            "auto_restart": self.auto_restart,
            "infrastructure_services": self.infrastructure_services,
            "application_services": self.application_services,
            "services_status": self.services_status,
            "services_health": services_health,
            "stats": self.stats
        }