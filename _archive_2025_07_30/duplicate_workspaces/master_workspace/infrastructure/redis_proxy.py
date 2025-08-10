#!/usr/bin/env python3
"""
Redis Service Proxy - 統一 Redis 服務代理
=========================================

為所有 Workspace 提供統一的 Redis 訪問入口：
1. 透明代理模式 - 無縫遷移現有服務
2. 故障轉移支援 - 自動切換備用實例
3. 流量分配控制 - 支援漸進式遷移
4. 連接池管理 - 優化資源使用

設計模式：Proxy Pattern + Circuit Breaker
遷移策略：Strangler Fig 模式
"""

import asyncio
import redis.asyncio as redis
import logging
from typing import Dict, Any, List, Optional, Union
from datetime import datetime, timedelta
import json
import time

logger = logging.getLogger(__name__)

class RedisServiceProxy:
    """
    Redis 服務代理
    
    功能：
    - 透明代理現有 Redis 操作
    - 支援主備切換和故障轉移
    - 流量分配（新舊服務間）
    - 連接健康監控
    """
    
    def __init__(
        self,
        primary_host: str = "localhost",
        primary_port: int = 6379,
        db: int = 0,
        proxy_enabled: bool = True,
        failover_enabled: bool = True,
        backup_instances: List[Dict[str, Any]] = None
    ):
        self.primary_host = primary_host
        self.primary_port = primary_port
        self.db = db
        self.proxy_enabled = proxy_enabled
        self.failover_enabled = failover_enabled
        self.backup_instances = backup_instances or []
        
        # 連接池
        self.primary_pool: Optional[redis.ConnectionPool] = None
        self.backup_pools: List[redis.ConnectionPool] = []
        self.current_pool: Optional[redis.ConnectionPool] = None
        
        # 運行狀態
        self.is_running = False
        self.current_instance = "primary"
        self.traffic_split_percent = 0  # 新服務流量百分比
        self.circuit_breaker_open = False
        self.last_health_check = datetime.now()
        
        # 統計數據
        self.stats = {
            "total_operations": 0,
            "successful_operations": 0,
            "failed_operations": 0,
            "failover_count": 0,
            "last_failover": None,
            "circuit_breaker_trips": 0
        }
        
        logger.info(f"✅ Redis 服務代理初始化: {primary_host}:{primary_port}")
    
    async def initialize(self):
        """初始化連接池"""
        try:
            # 主要連接池
            self.primary_pool = redis.ConnectionPool(
                host=self.primary_host,
                port=self.primary_port,
                db=self.db,
                max_connections=20,
                retry_on_timeout=True,
                socket_timeout=5,
                socket_connect_timeout=5
            )
            
            # 備用連接池
            for backup in self.backup_instances:
                backup_pool = redis.ConnectionPool(
                    host=backup["host"],
                    port=backup["port"],
                    db=backup.get("db", self.db),
                    max_connections=10,
                    retry_on_timeout=True,
                    socket_timeout=5,
                    socket_connect_timeout=5
                )
                self.backup_pools.append(backup_pool)
            
            # 設置當前使用的連接池
            self.current_pool = self.primary_pool
            
            # 測試連接
            await self._test_connection()
            
            logger.info("✅ Redis 連接池初始化完成")
            
        except Exception as e:
            logger.error(f"❌ Redis 連接池初始化失敗: {e}")
            raise
    
    async def start(self):
        """啟動代理服務"""
        if not self.current_pool:
            await self.initialize()
        
        self.is_running = True
        
        # 啟動健康檢查任務
        asyncio.create_task(self._health_check_loop())
        
        logger.info("✅ Redis 服務代理已啟動")
    
    async def stop(self):
        """停止代理服務"""
        self.is_running = False
        
        # 關閉連接池
        if self.primary_pool:
            await self.primary_pool.disconnect()
        
        for pool in self.backup_pools:
            await pool.disconnect()
        
        logger.info("✅ Redis 服務代理已停止")
    
    async def _test_connection(self):
        """測試連接"""
        try:
            client = redis.Redis(connection_pool=self.current_pool)
            await client.ping()
            logger.info("✅ Redis 連接測試成功")
            
        except Exception as e:
            logger.error(f"❌ Redis 連接測試失敗: {e}")
            raise
    
    async def _health_check_loop(self):
        """健康檢查循環"""
        while self.is_running:
            try:
                await self._perform_health_check()
                await asyncio.sleep(30)  # 30秒檢查一次
                
            except Exception as e:
                logger.error(f"❌ Redis 健康檢查異常: {e}")
                await asyncio.sleep(60)  # 異常時延長間隔
    
    async def _perform_health_check(self):
        """執行健康檢查"""
        try:
            client = redis.Redis(connection_pool=self.current_pool)
            start_time = time.time()
            
            # 執行基本操作測試
            await client.ping()
            await client.set("health_check", datetime.now().isoformat(), ex=10)
            result = await client.get("health_check")
            
            latency = (time.time() - start_time) * 1000  # 毫秒
            
            if result and latency < 100:  # 延遲小於100ms視為健康
                self.last_health_check = datetime.now()
                
                # 如果斷路器開啟，嘗試恢復
                if self.circuit_breaker_open:
                    self.circuit_breaker_open = False
                    logger.info("✅ Redis 連接恢復，斷路器關閉")
            else:
                raise Exception(f"健康檢查失敗或延遲過高: {latency}ms")
                
        except Exception as e:
            logger.warning(f"⚠️ Redis 健康檢查失敗: {e}")
            
            # 如果故障轉移啟用，嘗試切換
            if self.failover_enabled and not self.circuit_breaker_open:
                await self._trigger_circuit_breaker()
    
    async def _trigger_circuit_breaker(self):
        """觸發斷路器"""
        self.circuit_breaker_open = True
        self.stats["circuit_breaker_trips"] += 1
        
        logger.warning("⚠️ Redis 斷路器開啟，嘗試故障轉移")
        
        if self.backup_pools:
            await self.trigger_failover()
    
    async def trigger_failover(self) -> bool:
        """手動觸發故障轉移"""
        if not self.failover_enabled or not self.backup_pools:
            logger.warning("⚠️ 故障轉移未啟用或無備用實例")
            return False
        
        logger.warning("🔄 開始 Redis 故障轉移...")
        
        try:
            # 嘗試每個備用實例
            for i, backup_pool in enumerate(self.backup_pools):
                try:
                    # 測試備用連接
                    test_client = redis.Redis(connection_pool=backup_pool)
                    await test_client.ping()
                    
                    # 切換到備用實例
                    old_pool = self.current_pool
                    self.current_pool = backup_pool
                    self.current_instance = f"backup_{i}"
                    
                    # 關閉舊連接
                    if old_pool != self.primary_pool:
                        await old_pool.disconnect()
                    
                    self.stats["failover_count"] += 1
                    self.stats["last_failover"] = datetime.now().isoformat()
                    
                    logger.info(f"✅ Redis 故障轉移成功: {self.current_instance}")
                    return True
                    
                except Exception as e:
                    logger.warning(f"⚠️ 備用實例 {i} 不可用: {e}")
                    continue
            
            logger.error("❌ 所有備用實例均不可用")
            return False
            
        except Exception as e:
            logger.error(f"❌ Redis 故障轉移失敗: {e}")
            return False
    
    async def switch_to_legacy(self):
        """切換回遺留服務（用於回滾）"""
        logger.info("🔄 切換到遺留 Redis 服務...")
        
        self.traffic_split_percent = 0
        
        # 如果當前不是主實例，切換回主實例
        if self.current_instance != "primary":
            self.current_pool = self.primary_pool
            self.current_instance = "primary"
        
        logger.info("✅ 已切換到遺留 Redis 服務")
    
    async def update_traffic_split(self, percent: int):
        """更新流量分配百分比"""
        self.traffic_split_percent = max(0, min(100, percent))
        logger.info(f"✅ Redis 流量分配更新: {self.traffic_split_percent}%")
    
    # ==========================================
    # Redis 操作代理方法
    # ==========================================
    
    async def get_client(self) -> redis.Redis:
        """獲取 Redis 客戶端"""
        if self.circuit_breaker_open:
            raise Exception("Redis 服務不可用 (斷路器開啟)")
        
        return redis.Redis(connection_pool=self.current_pool)
    
    async def get(self, key: str) -> Optional[str]:
        """代理 GET 操作"""
        return await self._execute_operation("get", key)
    
    async def set(self, key: str, value: Union[str, bytes], ex: Optional[int] = None) -> bool:
        """代理 SET 操作"""
        return await self._execute_operation("set", key, value, ex=ex)
    
    async def delete(self, *keys: str) -> int:
        """代理 DELETE 操作"""
        return await self._execute_operation("delete", *keys)
    
    async def exists(self, *keys: str) -> int:
        """代理 EXISTS 操作"""
        return await self._execute_operation("exists", *keys)
    
    async def publish(self, channel: str, message: Union[str, bytes]) -> int:
        """代理 PUBLISH 操作"""
        return await self._execute_operation("publish", channel, message)
    
    async def _execute_operation(self, operation: str, *args, **kwargs):
        """執行 Redis 操作並處理錯誤"""
        self.stats["total_operations"] += 1
        
        try:
            client = await self.get_client()
            method = getattr(client, operation)
            result = await method(*args, **kwargs)
            
            self.stats["successful_operations"] += 1
            return result
            
        except Exception as e:
            self.stats["failed_operations"] += 1
            logger.error(f"❌ Redis {operation} 操作失敗: {e}")
            
            # 如果故障率過高，觸發斷路器
            failure_rate = self.stats["failed_operations"] / self.stats["total_operations"]
            if failure_rate > 0.1:  # 10% 故障率
                await self._trigger_circuit_breaker()
            
            raise
    
    async def get_status(self) -> Dict[str, Any]:
        """獲取代理狀態"""
        return {
            "proxy_enabled": self.proxy_enabled,
            "running": self.is_running,
            "current_instance": self.current_instance,
            "traffic_split_percent": self.traffic_split_percent,
            "circuit_breaker_open": self.circuit_breaker_open,
            "last_health_check": self.last_health_check.isoformat(),
            "stats": self.stats,
            "backup_instances_count": len(self.backup_pools)
        }