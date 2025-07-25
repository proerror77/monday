#!/usr/bin/env python3
"""
PRD第3部分對齊：災難回復（DR）系統實施
==============================

基於PRD要求實現完整的災難回復機制：
- Redis宕機 → Rust Sentinel本地寫回退Cache (RTO: 0s)
- ClickHouse節點故障 → 讀Replica (RTO: 5min) 
- Supabase Storage CDN失效 → 本地模型回退 (RTO: 30s)
"""

import asyncio
import logging
import time
import json
import redis
import requests
from typing import Dict, Any, Optional, List
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class DRScenario(Enum):
    """災難場景類型"""
    REDIS_DOWN = "redis_down"
    CLICKHOUSE_DOWN = "clickhouse_down" 
    SUPABASE_CDN_DOWN = "supabase_cdn_down"
    NETWORK_PARTITION = "network_partition"

@dataclass
class DREvent:
    """災難事件"""
    scenario: DRScenario
    timestamp: float
    severity: str  # CRITICAL, HIGH, MEDIUM
    component: str
    message: str
    recovery_action: Optional[str] = None

class LocalCacheManager:
    """本地緩存管理器（Redis宕機回退）"""
    
    def __init__(self, cache_dir: str = "./dr_cache"):
        self.cache_dir = Path(cache_dir)
        self.cache_dir.mkdir(exist_ok=True)
        self.local_cache = {}
        
    def write_local_cache(self, key: str, data: Dict[str, Any]) -> bool:
        """寫入本地緩存"""
        try:
            cache_file = self.cache_dir / f"{key}.json"
            with open(cache_file, 'w') as f:
                json.dump(data, f)
            
            # 同時寫入內存緩存
            self.local_cache[key] = data
            logger.info(f"💾 本地緩存寫入成功: {key}")
            return True
            
        except Exception as e:
            logger.error(f"❌ 本地緩存寫入失敗: {e}")
            return False
    
    def read_local_cache(self, key: str) -> Optional[Dict[str, Any]]:
        """讀取本地緩存"""
        try:
            # 先檢查內存緩存
            if key in self.local_cache:
                return self.local_cache[key]
            
            # 再檢查文件緩存
            cache_file = self.cache_dir / f"{key}.json"
            if cache_file.exists():
                with open(cache_file, 'r') as f:
                    data = json.load(f)
                self.local_cache[key] = data
                return data
                
            return None
            
        except Exception as e:
            logger.error(f"❌ 本地緩存讀取失敗: {e}")
            return None

class ModelBackupManager:
    """模型備份管理器（Supabase CDN失效回退）"""
    
    def __init__(self, backup_dir: str = "./models/backup"):
        self.backup_dir = Path(backup_dir)
        self.backup_dir.mkdir(parents=True, exist_ok=True)
        
    def ensure_local_backup(self, model_url: str, model_version: str) -> str:
        """確保本地模型備份存在"""
        local_path = self.backup_dir / f"{model_version}.pt"
        
        if local_path.exists():
            logger.info(f"✅ 本地模型備份已存在: {local_path}")
            return str(local_path)
        
        try:
            # 從Supabase下載並備份
            response = requests.get(model_url, timeout=30)
            if response.status_code == 200:
                with open(local_path, 'wb') as f:
                    f.write(response.content)
                logger.info(f"📥 模型備份下載成功: {local_path}")
                return str(local_path)
            else:
                logger.error(f"❌ 模型下載失敗: {response.status_code}")
                return None
                
        except Exception as e:
            logger.error(f"❌ 模型備份失敗: {e}")
            return None
    
    def get_fallback_model(self) -> Optional[str]:
        """獲取回退模型"""
        backup_files = list(self.backup_dir.glob("*.pt"))
        if backup_files:
            # 返回最新的備份模型
            latest_backup = max(backup_files, key=lambda f: f.stat().st_mtime)
            logger.info(f"🔄 使用回退模型: {latest_backup}")
            return str(latest_backup)
        
        logger.error("❌ 沒有可用的回退模型")
        return None

class ClickHouseReplicaManager:
    """ClickHouse副本管理器"""
    
    def __init__(self, primary_url: str = "http://localhost:8123", 
                 replica_urls: List[str] = None):
        self.primary_url = primary_url
        self.replica_urls = replica_urls or ["http://localhost:8124", "http://localhost:8125"]
        
    async def execute_query_with_fallback(self, query: str) -> Optional[str]:
        """帶回退的查詢執行"""
        
        # 先嘗試主節點
        try:
            response = requests.get(f"{self.primary_url}/?query={query}", timeout=5)
            if response.status_code == 200:
                logger.debug(f"✅ ClickHouse主節點查詢成功")
                return response.text.strip()
        except Exception as e:
            logger.warning(f"⚠️ ClickHouse主節點失敗: {e}")
        
        # 依次嘗試副本節點
        for i, replica_url in enumerate(self.replica_urls):
            try:
                response = requests.get(f"{replica_url}/?query={query}", timeout=10)
                if response.status_code == 200:
                    logger.info(f"✅ ClickHouse副本節點{i+1}查詢成功")
                    return response.text.strip()
            except Exception as e:
                logger.warning(f"⚠️ ClickHouse副本節點{i+1}失敗: {e}")
        
        logger.error("❌ 所有ClickHouse節點都不可用")
        return None

class DisasterRecoverySystem:
    """災難回復系統"""
    
    def __init__(self):
        self.cache_manager = LocalCacheManager()
        self.model_manager = ModelBackupManager()
        self.ch_manager = ClickHouseReplicaManager()
        self.dr_events: List[DREvent] = []
        
        # DR狀態跟踪
        self.redis_available = True
        self.clickhouse_available = True
        self.supabase_available = True
        
    async def health_check_loop(self):
        """持續健康檢查循環"""
        logger.info("🏥 啟動災難回復健康檢查循環")
        
        while True:
            try:
                # Redis健康檢查
                await self._check_redis_health()
                
                # ClickHouse健康檢查  
                await self._check_clickhouse_health()
                
                # Supabase CDN健康檢查
                await self._check_supabase_health()
                
                # 每30秒檢查一次
                await asyncio.sleep(30)
                
            except Exception as e:
                logger.error(f"❌ 健康檢查循環異常: {e}")
                await asyncio.sleep(5)
    
    async def _check_redis_health(self):
        """Redis健康檢查"""
        try:
            redis_client = redis.Redis(host='localhost', port=6379, db=0)
            redis_client.ping()
            
            if not self.redis_available:
                # Redis恢復
                self.redis_available = True
                await self._handle_recovery(DRScenario.REDIS_DOWN)
                
        except Exception as e:
            if self.redis_available:
                # Redis宕機
                self.redis_available = False
                dr_event = DREvent(
                    scenario=DRScenario.REDIS_DOWN,
                    timestamp=time.time(),
                    severity="CRITICAL",
                    component="redis",
                    message=f"Redis連接失敗: {e}",
                    recovery_action="switch_to_local_cache"
                )
                await self._handle_disaster(dr_event)
    
    async def _check_clickhouse_health(self):
        """ClickHouse健康檢查"""
        try:
            test_query = "SELECT 1"
            result = await self.ch_manager.execute_query_with_fallback(test_query)
            
            if result == "1":
                if not self.clickhouse_available:
                    # ClickHouse恢復
                    self.clickhouse_available = True
                    await self._handle_recovery(DRScenario.CLICKHOUSE_DOWN)
            else:
                raise Exception("查詢返回異常結果")
                
        except Exception as e:
            if self.clickhouse_available:
                # ClickHouse宕機
                self.clickhouse_available = False
                dr_event = DREvent(
                    scenario=DRScenario.CLICKHOUSE_DOWN,
                    timestamp=time.time(),
                    severity="HIGH",
                    component="clickhouse",
                    message=f"ClickHouse不可用: {e}",
                    recovery_action="use_replica_nodes"
                )
                await self._handle_disaster(dr_event)
    
    async def _check_supabase_health(self):
        """Supabase CDN健康檢查"""
        try:
            # 測試Supabase API健康狀況
            test_url = "https://api.supabase.co/health"
            response = requests.get(test_url, timeout=5)
            
            if response.status_code == 200:
                if not self.supabase_available:
                    # Supabase恢復
                    self.supabase_available = True
                    await self._handle_recovery(DRScenario.SUPABASE_CDN_DOWN)
            else:
                raise Exception(f"HTTP {response.status_code}")
                
        except Exception as e:
            if self.supabase_available:
                # Supabase CDN失效
                self.supabase_available = False
                dr_event = DREvent(
                    scenario=DRScenario.SUPABASE_CDN_DOWN,
                    timestamp=time.time(),
                    severity="MEDIUM",
                    component="supabase_cdn",
                    message=f"Supabase CDN不可用: {e}",
                    recovery_action="use_local_model_backup"
                )
                await self._handle_disaster(dr_event)
    
    async def _handle_disaster(self, dr_event: DREvent):
        """處理災難事件"""
        logger.error(f"🛑 災難事件: {dr_event.scenario.value} - {dr_event.message}")
        
        self.dr_events.append(dr_event)
        
        if dr_event.scenario == DRScenario.REDIS_DOWN:
            await self._handle_redis_disaster()
        elif dr_event.scenario == DRScenario.CLICKHOUSE_DOWN:
            await self._handle_clickhouse_disaster()
        elif dr_event.scenario == DRScenario.SUPABASE_CDN_DOWN:
            await self._handle_supabase_disaster()
    
    async def _handle_redis_disaster(self):
        """處理Redis災難（RTO: 0s）"""
        logger.info("🔄 啟動Redis災難回復：切換到本地緩存")
        
        # 立即切換到本地緩存模式
        # 這裡Rust Sentinel會自動使用本地Cache
        
        # 保存當前狀態到本地
        current_state = {
            "timestamp": time.time(),
            "mode": "local_cache_fallback",
            "redis_down": True
        }
        
        self.cache_manager.write_local_cache("system_state", current_state)
        logger.info("✅ Redis災難回復完成 (RTO: 0s)")
    
    async def _handle_clickhouse_disaster(self):
        """處理ClickHouse災難（RTO: 5min）"""
        logger.info("🔄 啟動ClickHouse災難回復：切換到副本節點")
        
        # 測試副本節點可用性
        test_query = "SELECT count() FROM system.tables"
        result = await self.ch_manager.execute_query_with_fallback(test_query)
        
        if result:
            logger.info("✅ ClickHouse災難回復完成：副本節點可用")
        else:
            logger.error("❌ ClickHouse災難回復失敗：所有節點不可用")
            
            # 通知ML訓練跳過本次批次
            skip_notification = {
                "event": "skip_training_batch",
                "reason": "clickhouse_unavailable",
                "timestamp": time.time()
            }
            self.cache_manager.write_local_cache("training_skip", skip_notification)
    
    async def _handle_supabase_disaster(self):
        """處理Supabase災難（RTO: 30s）"""
        logger.info("🔄 啟動Supabase災難回復：切換到本地模型備份")
        
        # 獲取本地備份模型
        fallback_model = self.model_manager.get_fallback_model()
        
        if fallback_model:
            # 通知Rust引擎使用本地模型
            model_config = {
                "model_path": fallback_model,
                "fallback_mode": True,
                "timestamp": time.time()
            }
            
            self.cache_manager.write_local_cache("active_model", model_config)
            logger.info(f"✅ Supabase災難回復完成：使用本地模型 {fallback_model}")
        else:
            logger.error("❌ Supabase災難回復失敗：沒有可用的本地模型備份")
    
    async def _handle_recovery(self, scenario: DRScenario):
        """處理系統恢復"""
        logger.info(f"🎉 系統恢復: {scenario.value}")
        
        if scenario == DRScenario.REDIS_DOWN:
            logger.info("✅ Redis服務已恢復，可以恢復正常模式")
        elif scenario == DRScenario.CLICKHOUSE_DOWN:
            logger.info("✅ ClickHouse服務已恢復，可以恢復正常查詢")
        elif scenario == DRScenario.SUPABASE_CDN_DOWN:
            logger.info("✅ Supabase CDN已恢復，可以恢復正常模型下載")
    
    def get_dr_status(self) -> Dict[str, Any]:
        """獲取災難回復狀態"""
        return {
            "timestamp": time.time(),
            "system_health": {
                "redis_available": self.redis_available,
                "clickhouse_available": self.clickhouse_available,
                "supabase_available": self.supabase_available
            },
            "recent_events": [
                {
                    "scenario": event.scenario.value,
                    "timestamp": event.timestamp,
                    "severity": event.severity,
                    "message": event.message,
                    "recovery_action": event.recovery_action
                }
                for event in self.dr_events[-10:]  # 最近10個事件
            ],
            "rto_targets": {
                "redis_down": "0s",
                "clickhouse_down": "5min", 
                "supabase_cdn_down": "30s"
            }
        }

# 全局DR系統實例
dr_system = DisasterRecoverySystem()

async def main():
    """災難回復系統主函數"""
    print("🛡️ 啟動災難回復系統（PRD第3部分）")
    print("=" * 60)
    
    # 啟動健康檢查循環
    await dr_system.health_check_loop()

if __name__ == "__main__":
    asyncio.run(main())