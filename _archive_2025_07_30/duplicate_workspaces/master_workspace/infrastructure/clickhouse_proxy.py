#!/usr/bin/env python3
"""
ClickHouse Service Proxy - 統一 ClickHouse 服務代理
=================================================

為所有 Workspace 提供統一的 ClickHouse 訪問入口：
1. 透明代理模式 - 無縫遷移現有查詢
2. 讀寫分離支援 - 優化查詢性能
3. 連接池管理 - 資源優化
4. 查詢路由與負載均衡

設計模式：Proxy Pattern + Connection Pool
遷移策略：Database Proxy Pattern
"""

import asyncio
import asyncio_clickhouse
import logging
from typing import Dict, Any, List, Optional, Union
from datetime import datetime
import time
import pandas as pd

logger = logging.getLogger(__name__)

class ClickHouseServiceProxy:
    """
    ClickHouse 服務代理
    
    功能：
    - 透明代理 ClickHouse 查詢
    - 支援主備切換和讀寫分離
    - 查詢路由和負載均衡
    - 連接健康監控
    """
    
    def __init__(
        self,
        primary_host: str = "localhost",
        primary_port: int = 9000,
        database: str = "hft",
        proxy_enabled: bool = True,
        cluster_enabled: bool = False,
        backup_instances: List[Dict[str, Any]] = None
    ):
        self.primary_host = primary_host
        self.primary_port = primary_port
        self.database = database
        self.proxy_enabled = proxy_enabled
        self.cluster_enabled = cluster_enabled
        self.backup_instances = backup_instances or []
        
        # 連接配置
        self.connection_settings = {
            'connect_timeout': 10,
            'send_receive_timeout': 300,
            'compress': True,
            'compression_method': 'lz4'
        }
        
        # 連接池
        self.primary_client: Optional[asyncio_clickhouse.Client] = None
        self.backup_clients: List[asyncio_clickhouse.Client] = []
        self.read_replicas: List[asyncio_clickhouse.Client] = []
        self.current_client: Optional[asyncio_clickhouse.Client] = None
        
        # 運行狀態
        self.is_running = False
        self.current_instance = "primary"
        self.traffic_split_percent = 0
        self.read_write_split_enabled = True
        self.last_health_check = datetime.now()
        
        # 統計數據
        self.stats = {
            "total_queries": 0,
            "successful_queries": 0,
            "failed_queries": 0,
            "read_queries": 0,
            "write_queries": 0,
            "avg_query_time_ms": 0.0,
            "failover_count": 0,
            "last_failover": None
        }
        
        logger.info(f"✅ ClickHouse 服務代理初始化: {primary_host}:{primary_port}")
    
    async def initialize(self):
        """初始化連接池"""
        try:
            # 主要客戶端
            self.primary_client = asyncio_clickhouse.Client(
                host=self.primary_host,
                port=self.primary_port,
                database=self.database,
                **self.connection_settings
            )
            
            # 備用客戶端
            for backup in self.backup_instances:
                backup_client = asyncio_clickhouse.Client(
                    host=backup["host"],
                    port=backup["port"],
                    database=backup.get("database", self.database),
                    **self.connection_settings
                )
                self.backup_clients.append(backup_client)
                
                # 如果標記為只讀副本，加入讀副本列表
                if backup.get("read_only", False):
                    self.read_replicas.append(backup_client)
            
            # 設置當前使用的客戶端
            self.current_client = self.primary_client
            
            # 測試連接
            await self._test_connection()
            
            logger.info("✅ ClickHouse 連接池初始化完成")
            
        except Exception as e:
            logger.error(f"❌ ClickHouse 連接池初始化失敗: {e}")
            raise
    
    async def start(self):
        """啟動代理服務"""
        if not self.current_client:
            await self.initialize()
        
        self.is_running = True
        
        # 啟動健康檢查任務
        asyncio.create_task(self._health_check_loop())
        
        logger.info("✅ ClickHouse 服務代理已啟動")
    
    async def stop(self):
        """停止代理服務"""
        self.is_running = False
        
        # 關閉所有連接
        if self.primary_client:
            await self.primary_client.close()
        
        for client in self.backup_clients:
            await client.close()
        
        logger.info("✅ ClickHouse 服務代理已停止")
    
    async def _test_connection(self):
        """測試連接"""
        try:
            result = await self.current_client.execute("SELECT 1 as test")
            if result:
                logger.info("✅ ClickHouse 連接測試成功")
            else:
                raise Exception("查詢返回空結果")
                
        except Exception as e:
            logger.error(f"❌ ClickHouse 連接測試失敗: {e}")
            raise
    
    async def _health_check_loop(self):
        """健康檢查循環"""
        while self.is_running:
            try:
                await self._perform_health_check()
                await asyncio.sleep(60)  # 60秒檢查一次
                
            except Exception as e:
                logger.error(f"❌ ClickHouse 健康檢查異常: {e}")
                await asyncio.sleep(120)  # 異常時延長間隔
    
    async def _perform_health_check(self):
        """執行健康檢查"""
        try:
            start_time = time.time()
            
            # 執行簡單查詢測試
            result = await self.current_client.execute(
                "SELECT version(), uptime(), formatReadableSize(total_memory_tracker)"
            )
            
            latency = (time.time() - start_time) * 1000  # 毫秒
            
            if result and latency < 1000:  # 延遲小於1秒視為健康
                self.last_health_check = datetime.now()
                logger.debug(f"ClickHouse 健康檢查通過，延遲: {latency:.2f}ms")
            else:
                raise Exception(f"健康檢查失敗或延遲過高: {latency}ms")
                
        except Exception as e:
            logger.warning(f"⚠️ ClickHouse 健康檢查失敗: {e}")
            
            # 如果故障轉移啟用，嘗試切換
            if self.backup_clients:
                await self.trigger_failover()
    
    async def trigger_failover(self) -> bool:
        """手動觸發故障轉移"""
        if not self.backup_clients:
            logger.warning("⚠️ 無可用的 ClickHouse 備用實例")
            return False
        
        logger.warning("🔄 開始 ClickHouse 故障轉移...")
        
        try:
            # 嘗試每個備用實例
            for i, backup_client in enumerate(self.backup_clients):
                try:
                    # 測試備用連接
                    result = await backup_client.execute("SELECT 1")
                    
                    if result:
                        # 切換到備用實例
                        self.current_client = backup_client
                        self.current_instance = f"backup_{i}"
                        
                        self.stats["failover_count"] += 1
                        self.stats["last_failover"] = datetime.now().isoformat()
                        
                        logger.info(f"✅ ClickHouse 故障轉移成功: {self.current_instance}")
                        return True
                        
                except Exception as e:
                    logger.warning(f"⚠️ 備用實例 {i} 不可用: {e}")
                    continue
            
            logger.error("❌ 所有備用實例均不可用")
            return False
            
        except Exception as e:
            logger.error(f"❌ ClickHouse 故障轉移失敗: {e}")
            return False
    
    async def switch_to_legacy(self):
        """切換回遺留服務（用於回滾）"""
        logger.info("🔄 切換到遺留 ClickHouse 服務...")
        
        self.traffic_split_percent = 0
        
        # 如果當前不是主實例，切換回主實例
        if self.current_instance != "primary":
            self.current_client = self.primary_client
            self.current_instance = "primary"
        
        logger.info("✅ 已切換到遺留 ClickHouse 服務")
    
    async def update_traffic_split(self, percent: int):
        """更新流量分配百分比"""
        self.traffic_split_percent = max(0, min(100, percent))
        logger.info(f"✅ ClickHouse 流量分配更新: {self.traffic_split_percent}%")
    
    def _get_optimal_client(self, query_type: str = "read") -> asyncio_clickhouse.Client:
        """根據查詢類型選擇最佳客戶端"""
        if not self.read_write_split_enabled:
            return self.current_client
        
        # 寫操作使用主實例
        if query_type == "write":
            return self.primary_client
        
        # 讀操作優先使用讀副本
        if query_type == "read" and self.read_replicas:
            # 簡單輪詢負載均衡
            import random
            return random.choice(self.read_replicas)
        
        return self.current_client
    
    def _detect_query_type(self, query: str) -> str:
        """檢測查詢類型"""
        query_upper = query.strip().upper()
        
        if any(query_upper.startswith(cmd) for cmd in ['INSERT', 'CREATE', 'DROP', 'ALTER', 'UPDATE', 'DELETE']):
            return "write"
        else:
            return "read"
    
    # ==========================================
    # ClickHouse 操作代理方法
    # ==========================================
    
    async def execute(self, query: str, params: Optional[Dict] = None) -> List[Dict]:
        """執行 ClickHouse 查詢"""
        return await self._execute_query(query, params)
    
    async def execute_df(self, query: str, params: Optional[Dict] = None) -> pd.DataFrame:
        """執行查詢並返回 DataFrame"""
        result = await self._execute_query(query, params)
        return pd.DataFrame(result) if result else pd.DataFrame()
    
    async def insert_data(self, table: str, data: List[Dict]) -> bool:
        """插入數據到表"""
        if not data:
            return True
        
        try:
            # 構建插入語句
            columns = list(data[0].keys())
            placeholders = ', '.join([f"$({col})" for col in columns])
            query = f"INSERT INTO {table} ({', '.join(columns)}) VALUES ({placeholders})"
            
            client = self._get_optimal_client("write")
            await client.execute(query, data)
            
            logger.debug(f"✅ 成功插入 {len(data)} 條記錄到 {table}")
            return True
            
        except Exception as e:
            logger.error(f"❌ 插入數據失敗: {e}")
            return False
    
    async def bulk_insert(self, table: str, df: pd.DataFrame) -> bool:
        """批量插入 DataFrame 數據"""
        try:
            data = df.to_dict('records')
            return await self.insert_data(table, data)
            
        except Exception as e:
            logger.error(f"❌ 批量插入失敗: {e}")
            return False
    
    async def create_table_if_not_exists(self, table_name: str, schema: str) -> bool:
        """創建表（如果不存在）"""
        try:
            query = f"CREATE TABLE IF NOT EXISTS {table_name} {schema}"
            await self._execute_query(query, query_type="write")
            
            logger.info(f"✅ 表 {table_name} 創建/確認完成")
            return True
            
        except Exception as e:
            logger.error(f"❌ 創建表失敗: {e}")
            return False
    
    async def get_table_info(self, table: str) -> Dict[str, Any]:
        """獲取表信息"""
        try:
            query = f"""
            SELECT 
                count() as row_count,
                formatReadableSize(sum(bytes_on_disk)) as size_on_disk,
                min(timestamp) as min_timestamp,
                max(timestamp) as max_timestamp
            FROM {table}
            """
            
            result = await self._execute_query(query)
            return result[0] if result else {}
            
        except Exception as e:
            logger.error(f"❌ 獲取表信息失敗: {e}")
            return {}
    
    async def _execute_query(self, query: str, params: Optional[Dict] = None, query_type: Optional[str] = None) -> List[Dict]:
        """執行查詢並處理錯誤"""
        self.stats["total_queries"] += 1
        start_time = time.time()
        
        try:
            # 自動檢測查詢類型
            if query_type is None:
                query_type = self._detect_query_type(query)
            
            # 更新統計
            if query_type == "read":
                self.stats["read_queries"] += 1
            else:
                self.stats["write_queries"] += 1
            
            # 選擇最佳客戶端
            client = self._get_optimal_client(query_type)
            
            # 執行查詢
            if params:
                result = await client.execute(query, params)
            else:
                result = await client.execute(query)
            
            # 更新統計
            query_time = (time.time() - start_time) * 1000
            self.stats["successful_queries"] += 1
            
            # 更新平均查詢時間
            total_time = self.stats["avg_query_time_ms"] * (self.stats["successful_queries"] - 1) + query_time
            self.stats["avg_query_time_ms"] = total_time / self.stats["successful_queries"]
            
            logger.debug(f"✅ ClickHouse 查詢執行成功，耗時: {query_time:.2f}ms")
            
            return result
            
        except Exception as e:
            self.stats["failed_queries"] += 1
            logger.error(f"❌ ClickHouse 查詢失敗: {e}")
            logger.debug(f"失敗查詢: {query[:100]}...")
            
            raise
    
    async def get_status(self) -> Dict[str, Any]:
        """獲取代理狀態"""
        return {
            "proxy_enabled": self.proxy_enabled,
            "running": self.is_running,
            "current_instance": self.current_instance,
            "traffic_split_percent": self.traffic_split_percent,
            "read_write_split_enabled": self.read_write_split_enabled,
            "cluster_enabled": self.cluster_enabled,
            "last_health_check": self.last_health_check.isoformat(),
            "backup_instances_count": len(self.backup_clients),
            "read_replicas_count": len(self.read_replicas),
            "stats": self.stats
        }