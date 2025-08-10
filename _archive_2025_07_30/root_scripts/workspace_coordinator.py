#!/usr/bin/env python3
"""
HFT System Workspace Coordinator
统一的workspace启动、协调和健康检查管理器
解决多workspace依赖关系和通信问题
"""

import asyncio
import logging
import time
import yaml
import redis
import aioredis
import psycopg2
import grpc
import sys
import json
from pathlib import Path
from typing import Dict, List, Optional, Any
from dataclasses import dataclass
from enum import Enum
import subprocess
import signal
import threading
from concurrent.futures import ThreadPoolExecutor, TimeoutError
import httpx

# 配置日志
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger('workspace_coordinator')

class ServiceStatus(Enum):
    PENDING = "pending"
    STARTING = "starting"
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    FAILED = "failed"
    STOPPED = "stopped"

@dataclass
class ServiceHealth:
    name: str
    status: ServiceStatus
    last_check: float
    failure_count: int
    message: str

class WorkspaceCoordinator:
    """Workspace协调管理器"""
    
    def __init__(self, config_dir: str = "shared_config"):
        self.config_dir = Path(config_dir)
        self.services_health: Dict[str, ServiceHealth] = {}
        self.startup_config = None
        self.redis_config = None
        self.grpc_config = None
        self.redis_client = None
        self.running = False
        self.executor = ThreadPoolExecutor(max_workers=10)
        
        # 加载配置
        self._load_configurations()
        
    def _load_configurations(self):
        """加载所有配置文件"""
        try:
            # 加载启动序列配置
            with open(self.config_dir / "startup_sequence.yaml", 'r', encoding='utf-8') as f:
                self.startup_config = yaml.safe_load(f)
                
            # 加载Redis通道配置
            with open(self.config_dir / "redis_channels.yaml", 'r', encoding='utf-8') as f:
                self.redis_config = yaml.safe_load(f)
                
            # 加载gRPC服务配置
            with open(self.config_dir / "grpc_services.yaml", 'r', encoding='utf-8') as f:
                self.grpc_config = yaml.safe_load(f)
                
            logger.info("配置文件加载成功")
            
        except Exception as e:
            logger.error(f"配置文件加载失败: {e}")
            raise

    async def initialize_redis_connection(self):
        """初始化Redis连接"""
        try:
            redis_host = self.redis_config['redis_config']['host'].replace('${REDIS_HOST:-hft-redis}', 'hft-redis')
            redis_port = int(self.redis_config['redis_config']['port'].replace('${REDIS_PORT:-6379}', '6379'))
            
            self.redis_client = await aioredis.from_url(
                f"redis://{redis_host}:{redis_port}",
                encoding="utf-8",
                decode_responses=True
            )
            
            # 测试连接
            await self.redis_client.ping()
            logger.info("Redis连接建立成功")
            
        except Exception as e:
            logger.error(f"Redis连接失败: {e}")
            raise

    async def check_service_health(self, service_name: str, service_config: Dict) -> bool:
        """检查单个服务健康状态"""
        try:
            health_check = service_config.get('health_check', {})
            check_type = health_check.get('type', 'http')
            timeout_ms = health_check.get('timeout_ms', 5000)
            
            if check_type == 'redis':
                return await self._check_redis_health(health_check)
            elif check_type == 'http':
                return await self._check_http_health(health_check, timeout_ms)
            elif check_type == 'postgres':
                return await self._check_postgres_health(health_check)
            elif check_type == 'grpc':
                return await self._check_grpc_health(health_check, timeout_ms)
            else:
                logger.warning(f"未知的健康检查类型: {check_type} for {service_name}")
                return False
                
        except Exception as e:
            logger.error(f"健康检查失败 {service_name}: {e}")
            return False

    async def _check_redis_health(self, config: Dict) -> bool:
        """检查Redis健康状态"""
        try:
            host = config.get('host', 'hft-redis')
            port = config.get('port', 6379)
            
            temp_client = await aioredis.from_url(
                f"redis://{host}:{port}",
                socket_connect_timeout=2,
                socket_timeout=2
            )
            
            result = await temp_client.ping()
            await temp_client.close()
            return result
            
        except Exception:
            return False

    async def _check_http_health(self, config: Dict, timeout_ms: int) -> bool:
        """检查HTTP服务健康状态"""
        try:
            url = config.get('url', '')
            expected_status = config.get('expected_status', 200)
            
            async with httpx.AsyncClient(timeout=timeout_ms/1000) as client:
                response = await client.get(url)
                return response.status_code == expected_status
                
        except Exception:
            return False

    async def _check_postgres_health(self, config: Dict) -> bool:
        """检查PostgreSQL健康状态"""
        try:
            host = config.get('host', 'localhost')
            port = config.get('port', 5432)
            database = config.get('database', 'ai')
            username = config.get('username', 'ai')
            
            # 使用线程池执行阻塞的psycopg2连接
            def check_pg():
                conn = psycopg2.connect(
                    host=host,
                    port=port,
                    database=database,
                    user=username,
                    password='ai',
                    connect_timeout=3
                )
                conn.close()
                return True
            
            loop = asyncio.get_event_loop()
            result = await loop.run_in_executor(self.executor, check_pg)
            return result
            
        except Exception:
            return False

    async def _check_grpc_health(self, config: Dict, timeout_ms: int) -> bool:
        """检查gRPC服务健康状态"""
        try:
            address = config.get('address', 'rust-hft:50051')
            
            # 创建gRPC通道并测试连接
            channel = grpc.aio.insecure_channel(address)
            
            # 使用gRPC health check protocol
            try:
                # 简单的连接测试
                await channel.channel_ready()
                await channel.close()
                return True
            except:
                await channel.close()
                return False
                
        except Exception:
            return False

    async def start_phase_services(self, phase_name: str, phase_config: Dict) -> bool:
        """启动一个阶段的所有服务"""
        logger.info(f"启动阶段: {phase_name}")
        
        services = phase_config.get('services', [])
        timeout = phase_config.get('timeout_seconds', 120)
        
        # 检查依赖
        depends_on = phase_config.get('depends_on', [])
        for dep_phase in depends_on:
            if not await self._verify_phase_health(dep_phase):
                logger.error(f"依赖阶段 {dep_phase} 未就绪，无法启动 {phase_name}")
                return False
        
        # 并行启动服务（如果配置允许）
        if self.startup_config['global_startup_config'].get('parallel_startup', True):
            tasks = []
            for service in services:
                task = asyncio.create_task(self._start_single_service(service, timeout))
                tasks.append(task)
            
            results = await asyncio.gather(*tasks, return_exceptions=True)
            
            # 检查结果
            success_count = sum(1 for r in results if r is True)
            total_count = len(services)
            
            logger.info(f"阶段 {phase_name}: {success_count}/{total_count} 服务启动成功")
            
            # 根据策略决定是否继续
            critical_services = self.startup_config['global_startup_config'].get('critical_services', [])
            failed_critical = any(
                service['name'] in critical_services and results[i] is not True
                for i, service in enumerate(services)
            )
            
            if failed_critical:
                logger.error(f"关键服务启动失败，阶段 {phase_name} 启动失败")
                return False
                
            return success_count > 0  # 至少一个服务启动成功
        else:
            # 顺序启动
            for service in services:
                success = await self._start_single_service(service, timeout)
                if not success:
                    service_name = service.get('name', 'unknown')
                    if service_name in self.startup_config['global_startup_config'].get('critical_services', []):
                        logger.error(f"关键服务 {service_name} 启动失败")
                        return False
                    else:
                        logger.warning(f"非关键服务 {service_name} 启动失败，继续启动其他服务")
            
            return True

    async def _start_single_service(self, service_config: Dict, timeout: int) -> bool:
        """启动单个服务"""
        service_name = service_config.get('name', 'unknown')
        
        try:
            logger.info(f"启动服务: {service_name}")
            
            # 更新服务状态
            self.services_health[service_name] = ServiceHealth(
                name=service_name,
                status=ServiceStatus.STARTING,
                last_check=time.time(),
                failure_count=0,
                message="Starting"
            )
            
            # 等待服务就绪
            health_check = service_config.get('health_check', {})
            retry_attempts = health_check.get('retry_attempts', 10)
            retry_interval_ms = health_check.get('retry_interval_ms', 3000)
            
            for attempt in range(retry_attempts):
                if await self.check_service_health(service_name, service_config):
                    logger.info(f"服务 {service_name} 启动成功")
                    self.services_health[service_name].status = ServiceStatus.HEALTHY
                    self.services_health[service_name].message = "Healthy"
                    return True
                
                if attempt < retry_attempts - 1:
                    await asyncio.sleep(retry_interval_ms / 1000)
            
            logger.error(f"服务 {service_name} 启动失败 - 健康检查超时")
            self.services_health[service_name].status = ServiceStatus.FAILED
            self.services_health[service_name].message = "Health check timeout"
            return False
            
        except Exception as e:
            logger.error(f"服务 {service_name} 启动异常: {e}")
            self.services_health[service_name] = ServiceHealth(
                name=service_name,
                status=ServiceStatus.FAILED,
                last_check=time.time(),
                failure_count=1,
                message=str(e)
            )
            return False

    async def _verify_phase_health(self, phase_name: str) -> bool:
        """验证阶段中所有服务的健康状态"""
        phase_config = self.startup_config['startup_phases'].get(phase_name, {})
        services = phase_config.get('services', [])
        
        for service in services:
            service_name = service.get('name', '')
            if service_name not in self.services_health:
                return False
            
            if self.services_health[service_name].status != ServiceStatus.HEALTHY:
                return False
        
        return True

    async def start_all_workspaces(self) -> bool:
        """启动所有workspace"""
        try:
            self.running = True
            logger.info("开始启动HFT系统所有workspaces")
            
            # 初始化Redis连接
            await self.initialize_redis_connection()
            
            # 按阶段顺序启动
            phases = self.startup_config['startup_phases']
            sorted_phases = sorted(phases.items(), key=lambda x: x[1].get('phase_id', 0))
            
            for phase_name, phase_config in sorted_phases:
                logger.info(f"开始启动阶段: {phase_name} (ID: {phase_config.get('phase_id', 0)})")
                
                success = await self.start_phase_services(phase_name, phase_config)
                if not success:
                    failure_strategy = self.startup_config['global_startup_config'].get('failure_strategy', 'continue')
                    if failure_strategy == 'stop_on_critical':
                        logger.error(f"阶段 {phase_name} 启动失败，停止后续启动")
                        return False
                    else:
                        logger.warning(f"阶段 {phase_name} 启动失败，继续启动后续阶段")
                
                logger.info(f"阶段 {phase_name} 启动完成")
            
            logger.info("所有workspace启动流程完成")
            
            # 启动持续健康监控
            if self.startup_config['health_monitoring'].get('continuous_monitoring', True):
                asyncio.create_task(self._continuous_health_monitoring())
            
            return True
            
        except Exception as e:
            logger.error(f"启动过程中发生异常: {e}")
            return False

    async def _continuous_health_monitoring(self):
        """持续健康监控"""
        monitoring_interval = self.startup_config['health_monitoring'].get('monitoring_interval_seconds', 30)
        
        while self.running:
            try:
                logger.debug("执行健康检查")
                
                # 检查所有服务健康状态
                for service_name, health in self.services_health.items():
                    if health.status == ServiceStatus.HEALTHY:
                        # 查找服务配置
                        service_config = self._find_service_config(service_name)
                        if service_config:
                            is_healthy = await self.check_service_health(service_name, service_config)
                            if not is_healthy:
                                health.failure_count += 1
                                health.last_check = time.time()
                                
                                failure_threshold = self.startup_config['health_monitoring']['failure_thresholds'].get('consecutive_failures', 3)
                                if health.failure_count >= failure_threshold:
                                    logger.warning(f"服务 {service_name} 健康检查连续失败 {health.failure_count} 次")
                                    health.status = ServiceStatus.DEGRADED
                                    health.message = f"Health check failed {health.failure_count} times"
                                    
                                    # 发送告警
                                    await self._send_health_alert(service_name, health)
                            else:
                                if health.failure_count > 0:
                                    logger.info(f"服务 {service_name} 健康状态恢复")
                                health.failure_count = 0
                                health.last_check = time.time()
                                health.message = "Healthy"
                
                await asyncio.sleep(monitoring_interval)
                
            except Exception as e:
                logger.error(f"健康监控异常: {e}")
                await asyncio.sleep(monitoring_interval)

    def _find_service_config(self, service_name: str) -> Optional[Dict]:
        """查找服务配置"""
        for phase_config in self.startup_config['startup_phases'].values():
            for service in phase_config.get('services', []):
                if service.get('name') == service_name:
                    return service
        return None

    async def _send_health_alert(self, service_name: str, health: ServiceHealth):
        """发送健康告警"""
        try:
            if self.redis_client:
                alert_message = {
                    "type": "health",
                    "service": service_name,
                    "status": health.status.value,
                    "message": health.message,
                    "failure_count": health.failure_count,
                    "timestamp": int(time.time())
                }
                
                await self.redis_client.publish("ops.alert", json.dumps(alert_message))
                logger.info(f"健康告警已发送: {service_name}")
                
        except Exception as e:
            logger.error(f"发送健康告警失败: {e}")

    async def stop_all_workspaces(self):
        """停止所有workspace"""
        logger.info("开始停止所有workspaces")
        self.running = False
        
        # 反向停止顺序
        phases = self.startup_config['startup_phases']
        sorted_phases = sorted(phases.items(), key=lambda x: x[1].get('phase_id', 0), reverse=True)
        
        for phase_name, phase_config in sorted_phases:
            logger.info(f"停止阶段: {phase_name}")
            services = phase_config.get('services', [])
            
            for service in services:
                service_name = service.get('name', '')
                container_name = service.get('container', '')
                
                if container_name:
                    try:
                        # 使用docker stop停止容器
                        subprocess.run(['docker', 'stop', container_name], timeout=30, check=False)
                        logger.info(f"停止容器: {container_name}")
                    except Exception as e:
                        logger.error(f"停止容器 {container_name} 失败: {e}")
        
        # 关闭Redis连接
        if self.redis_client:
            await self.redis_client.close()
        
        # 关闭线程池
        self.executor.shutdown(wait=True)

    def get_system_status(self) -> Dict:
        """获取系统状态"""
        status = {
            "overall_status": "healthy",
            "services": {},
            "timestamp": int(time.time())
        }
        
        healthy_count = 0
        total_count = len(self.services_health)
        
        for service_name, health in self.services_health.items():
            status["services"][service_name] = {
                "status": health.status.value,
                "last_check": health.last_check,
                "failure_count": health.failure_count,
                "message": health.message
            }
            
            if health.status == ServiceStatus.HEALTHY:
                healthy_count += 1
        
        # 计算整体状态
        if total_count == 0:
            status["overall_status"] = "unknown"
        elif healthy_count == total_count:
            status["overall_status"] = "healthy"
        elif healthy_count > total_count * 0.7:
            status["overall_status"] = "degraded"
        else:
            status["overall_status"] = "critical"
        
        return status

async def main():
    """主函数"""
    coordinator = WorkspaceCoordinator()
    
    def signal_handler(signum, frame):
        logger.info("收到停止信号，开始关闭系统")
        asyncio.create_task(coordinator.stop_all_workspaces())
        sys.exit(0)
    
    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)
    
    try:
        success = await coordinator.start_all_workspaces()
        if success:
            logger.info("系统启动成功，进入运行状态")
            
            # 保持运行状态
            while coordinator.running:
                await asyncio.sleep(10)
                
                # 定期输出系统状态
                status = coordinator.get_system_status()
                logger.info(f"系统状态: {status['overall_status']}")
                
        else:
            logger.error("系统启动失败")
            sys.exit(1)
            
    except KeyboardInterrupt:
        logger.info("收到中断信号")
    except Exception as e:
        logger.error(f"系统运行异常: {e}")
        sys.exit(1)
    finally:
        await coordinator.stop_all_workspaces()

if __name__ == "__main__":
    asyncio.run(main())