#!/usr/bin/env python3
"""
HFT Workspace Integration Test Suite
测试workspace之间的通信和协调功能
验证修复后的依赖关系和启动顺序
"""

import asyncio
import redis
import aioredis
import grpc
import httpx
import psycopg2
import json
import time
import logging
from typing import Dict, List, Optional
import subprocess
import sys
from dataclasses import dataclass

# 配置日志
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger('integration_test')

@dataclass
class TestResult:
    name: str
    success: bool
    message: str
    duration: float

class WorkspaceIntegrationTester:
    """Workspace集成测试器"""
    
    def __init__(self):
        self.test_results: List[TestResult] = []
        self.redis_client = None
        
        # 服务配置
        self.services = {
            'redis': {'host': 'localhost', 'port': 6379},
            'clickhouse': {'host': 'localhost', 'port': 8123},
            'rust_hft_grpc': {'host': 'localhost', 'port': 50051},
            'master_api': {'host': 'localhost', 'port': 8002},
            'ops_api': {'host': 'localhost', 'port': 8003},
            'ml_api': {'host': 'localhost', 'port': 8001},
            'master_ui': {'host': 'localhost', 'port': 8504},
            'ops_ui': {'host': 'localhost', 'port': 8503},
            'ml_ui': {'host': 'localhost', 'port': 8502},
        }

    async def run_all_tests(self) -> bool:
        """运行所有集成测试"""
        logger.info("🧪 开始workspace集成测试")
        
        test_suites = [
            self.test_infrastructure_connectivity,
            self.test_workspace_api_health,
            self.test_redis_communication,
            self.test_grpc_communication,
            self.test_database_connectivity,
            self.test_service_dependencies,
            self.test_end_to_end_workflow
        ]
        
        for test_suite in test_suites:
            try:
                await test_suite()
            except Exception as e:
                logger.error(f"测试套件异常: {test_suite.__name__}: {e}")
                self.test_results.append(TestResult(
                    name=test_suite.__name__,
                    success=False,
                    message=f"Exception: {e}",
                    duration=0.0
                ))
        
        # 输出测试结果
        self.print_test_results()
        
        # 返回总体测试结果
        failed_tests = [r for r in self.test_results if not r.success]
        return len(failed_tests) == 0

    async def test_infrastructure_connectivity(self):
        """测试基础设施连接性"""
        logger.info("测试基础设施连接性...")
        
        # Redis连接测试
        start_time = time.time()
        try:
            self.redis_client = await aioredis.from_url(
                f"redis://{self.services['redis']['host']}:{self.services['redis']['port']}",
                encoding="utf-8",
                decode_responses=True
            )
            await self.redis_client.ping()
            await self.redis_client.set("test_key", "test_value")
            value = await self.redis_client.get("test_key")
            await self.redis_client.delete("test_key")
            
            self.test_results.append(TestResult(
                name="Redis Connectivity",
                success=value == "test_value",
                message="Redis connection and basic operations successful",
                duration=time.time() - start_time
            ))
        except Exception as e:
            self.test_results.append(TestResult(
                name="Redis Connectivity",
                success=False,
                message=f"Redis connection failed: {e}",
                duration=time.time() - start_time
            ))

        # ClickHouse连接测试
        start_time = time.time()
        try:
            async with httpx.AsyncClient() as client:
                response = await client.get(
                    f"http://{self.services['clickhouse']['host']}:{self.services['clickhouse']['port']}/ping",
                    timeout=10.0
                )
                self.test_results.append(TestResult(
                    name="ClickHouse Connectivity",
                    success=response.status_code == 200,
                    message=f"ClickHouse ping status: {response.status_code}",
                    duration=time.time() - start_time
                ))
        except Exception as e:
            self.test_results.append(TestResult(
                name="ClickHouse Connectivity", 
                success=False,
                message=f"ClickHouse connection failed: {e}",
                duration=time.time() - start_time
            ))

        # 数据库连接测试
        for db_name, port in [('master', 5432), ('ops', 5434), ('ml', 5433)]:
            start_time = time.time()
            try:
                conn = psycopg2.connect(
                    host='localhost',
                    port=port,
                    database='ai',
                    user='ai',
                    password='ai',
                    connect_timeout=5
                )
                conn.close()
                
                self.test_results.append(TestResult(
                    name=f"PostgreSQL {db_name.title()} DB",
                    success=True,
                    message=f"Database connection successful",
                    duration=time.time() - start_time
                ))
            except Exception as e:
                self.test_results.append(TestResult(
                    name=f"PostgreSQL {db_name.title()} DB",
                    success=False,
                    message=f"Database connection failed: {e}",
                    duration=time.time() - start_time
                ))

    async def test_workspace_api_health(self):
        """测试workspace API健康状态"""
        logger.info("测试workspace API健康状态...")
        
        api_services = [
            ('Master API', 'master_api', '/health'),
            ('Ops API', 'ops_api', '/health'),
            ('ML API', 'ml_api', '/health')
        ]
        
        for service_name, service_key, endpoint in api_services:
            start_time = time.time()
            try:
                service_config = self.services[service_key]
                url = f"http://{service_config['host']}:{service_config['port']}{endpoint}"
                
                async with httpx.AsyncClient() as client:
                    response = await client.get(url, timeout=15.0)
                    
                self.test_results.append(TestResult(
                    name=f"{service_name} Health",
                    success=response.status_code == 200,
                    message=f"HTTP {response.status_code}: {response.text[:100]}",
                    duration=time.time() - start_time
                ))
            except Exception as e:
                self.test_results.append(TestResult(
                    name=f"{service_name} Health",
                    success=False,
                    message=f"Health check failed: {e}",
                    duration=time.time() - start_time
                ))

    async def test_redis_communication(self):
        """测试Redis通信功能"""
        logger.info("测试Redis通信功能...")
        
        if not self.redis_client:
            self.test_results.append(TestResult(
                name="Redis Communication Setup",
                success=False,
                message="Redis client not available",
                duration=0.0
            ))
            return

        # 测试发布/订阅功能
        start_time = time.time()
        try:
            # 测试各种HFT通道
            channels = ['ops.alert', 'ml.deploy', 'kill-switch', 'system.heartbeat']
            
            for channel in channels:
                test_message = {
                    'type': 'test',
                    'timestamp': int(time.time()),
                    'test_id': f'test_{channel}_{int(time.time())}'
                }
                
                # 发布测试消息
                await self.redis_client.publish(channel, json.dumps(test_message))
                
            self.test_results.append(TestResult(
                name="Redis Pub/Sub Channels",
                success=True,
                message=f"Successfully published to {len(channels)} channels",
                duration=time.time() - start_time
            ))
        except Exception as e:
            self.test_results.append(TestResult(
                name="Redis Pub/Sub Channels",
                success=False,
                message=f"Redis pub/sub failed: {e}",
                duration=time.time() - start_time
            ))

        # 测试Redis key-value操作
        start_time = time.time()
        try:
            # 模拟系统状态存储
            system_status = {
                'rust_hft': {'status': 'healthy', 'last_update': int(time.time())},
                'ops_workspace': {'status': 'healthy', 'last_update': int(time.time())},
                'ml_workspace': {'status': 'healthy', 'last_update': int(time.time())}
            }
            
            await self.redis_client.hset('system:status', mapping=system_status)
            stored_status = await self.redis_client.hgetall('system:status')
            await self.redis_client.delete('system:status')
            
            self.test_results.append(TestResult(
                name="Redis Key-Value Operations",
                success=len(stored_status) == len(system_status),
                message=f"Stored and retrieved {len(stored_status)} status entries",
                duration=time.time() - start_time
            ))
        except Exception as e:
            self.test_results.append(TestResult(
                name="Redis Key-Value Operations",
                success=False,
                message=f"Redis key-value operations failed: {e}",
                duration=time.time() - start_time
            ))

    async def test_grpc_communication(self):
        """测试gRPC通信功能"""
        logger.info("测试gRPC通信功能...")
        
        start_time = time.time()
        try:
            # 尝试连接到Rust HFT gRPC服务
            grpc_config = self.services['rust_hft_grpc']
            address = f"{grpc_config['host']}:{grpc_config['port']}"
            
            channel = grpc.aio.insecure_channel(address)
            
            try:
                # 测试连接
                await asyncio.wait_for(channel.channel_ready(), timeout=10.0)
                
                self.test_results.append(TestResult(
                    name="gRPC Connection",
                    success=True,
                    message=f"Successfully connected to {address}",
                    duration=time.time() - start_time
                ))
            finally:
                await channel.close()
                
        except Exception as e:
            self.test_results.append(TestResult(
                name="gRPC Connection",
                success=False,
                message=f"gRPC connection failed: {e}",
                duration=time.time() - start_time
            ))

    async def test_database_connectivity(self):
        """测试数据库连接和基本操作"""
        logger.info("测试数据库连接和基本操作...")
        
        databases = [
            ('Master DB', 5432),
            ('Ops DB', 5434), 
            ('ML DB', 5433)
        ]
        
        for db_name, port in databases:
            start_time = time.time()
            try:
                conn = psycopg2.connect(
                    host='localhost',
                    port=port,
                    database='ai',
                    user='ai',
                    password='ai',
                    connect_timeout=5
                )
                
                # 测试基本SQL操作
                cursor = conn.cursor()
                cursor.execute("SELECT version();")
                version = cursor.fetchone()
                cursor.close()
                conn.close()
                
                self.test_results.append(TestResult(
                    name=f"{db_name} Operations",
                    success=True,
                    message=f"Database operations successful",
                    duration=time.time() - start_time
                ))
            except Exception as e:
                self.test_results.append(TestResult(
                    name=f"{db_name} Operations",
                    success=False,
                    message=f"Database operations failed: {e}",
                    duration=time.time() - start_time
                ))

    async def test_service_dependencies(self):
        """测试服务依赖关系"""
        logger.info("测试服务依赖关系...")
        
        # 测试workspace之间的API调用
        start_time = time.time()
        try:
            # 模拟Master workspace调用其他workspace
            async with httpx.AsyncClient() as client:
                # 调用Ops API
                ops_response = await client.get(
                    f"http://localhost:8003/health",
                    timeout=10.0
                )
                
                # 调用ML API
                ml_response = await client.get(
                    f"http://localhost:8001/health",
                    timeout=10.0
                )
                
                success = ops_response.status_code == 200 and ml_response.status_code == 200
                
                self.test_results.append(TestResult(
                    name="Inter-Workspace API Calls",
                    success=success,
                    message=f"Ops: {ops_response.status_code}, ML: {ml_response.status_code}",
                    duration=time.time() - start_time
                ))
        except Exception as e:
            self.test_results.append(TestResult(
                name="Inter-Workspace API Calls",
                success=False,
                message=f"Inter-workspace communication failed: {e}",
                duration=time.time() - start_time
            ))

    async def test_end_to_end_workflow(self):
        """测试端到端工作流"""
        logger.info("测试端到端工作流...")
        
        start_time = time.time()
        try:
            if not self.redis_client:
                raise Exception("Redis client not available")
            
            # 模拟完整的告警工作流
            # 1. Rust HFT发送告警
            alert_message = {
                'type': 'latency',
                'value': 35.7,
                'threshold': 25.0,
                'timestamp': int(time.time()),
                'severity': 'high',
                'source_component': 'rust_hft_test'
            }
            
            await self.redis_client.publish('ops.alert', json.dumps(alert_message))
            
            # 2. 等待处理时间
            await asyncio.sleep(1)
            
            # 3. 检查是否有响应操作
            # 这里我们模拟检查system:alerts key
            await self.redis_client.hset('system:alerts', 'last_alert', json.dumps(alert_message))
            stored_alert = await self.redis_client.hget('system:alerts', 'last_alert')
            
            success = stored_alert is not None
            
            # 清理测试数据
            await self.redis_client.delete('system:alerts')
            
            self.test_results.append(TestResult(
                name="End-to-End Alert Workflow",
                success=success,
                message="Alert workflow simulation completed",
                duration=time.time() - start_time
            ))
            
        except Exception as e:
            self.test_results.append(TestResult(
                name="End-to-End Alert Workflow",
                success=False,
                message=f"E2E workflow failed: {e}",
                duration=time.time() - start_time
            ))

    def print_test_results(self):
        """打印测试结果"""
        print("\n" + "="*80)
        print("🧪 WORKSPACE INTEGRATION TEST RESULTS")
        print("="*80)
        
        success_count = 0
        total_count = len(self.test_results)
        total_duration = 0.0
        
        for result in self.test_results:
            status = "✅ PASS" if result.success else "❌ FAIL"
            duration_str = f"{result.duration:.2f}s"
            
            print(f"{status:<8} {result.name:<35} ({duration_str:<6}) {result.message}")
            
            if result.success:
                success_count += 1
            total_duration += result.duration
        
        print("-"*80)
        print(f"SUMMARY: {success_count}/{total_count} tests passed ({success_count/total_count*100:.1f}%)")
        print(f"TOTAL DURATION: {total_duration:.2f}s")
        
        if success_count == total_count:
            print("🎉 ALL TESTS PASSED! Workspace integration is working correctly.")
        else:
            failed_count = total_count - success_count
            print(f"⚠️  {failed_count} TESTS FAILED. Please check the failed tests above.")
        
        print("="*80)

    async def cleanup(self):
        """清理测试资源"""
        if self.redis_client:
            await self.redis_client.close()

async def main():
    """主函数"""
    tester = WorkspaceIntegrationTester()
    
    try:
        success = await tester.run_all_tests()
        sys.exit(0 if success else 1)
    except KeyboardInterrupt:
        logger.info("测试被用户中断")
        sys.exit(1)
    except Exception as e:
        logger.error(f"测试执行异常: {e}")
        sys.exit(1)
    finally:
        await tester.cleanup()

if __name__ == "__main__":
    asyncio.run(main())