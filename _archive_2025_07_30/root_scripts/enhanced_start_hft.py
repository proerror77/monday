#!/usr/bin/env python3
"""
Enhanced HFT 系統啟動器
=====================

提供實時系統狀態監控和改進的組件檢測
"""

import asyncio
import logging
import time
import json
from datetime import datetime
from master_hft_agent import MasterHFTAgent, SystemState, ComponentState

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

class EnhancedHFTStarter:
    """增強版 HFT 系統啟動器"""
    
    def __init__(self):
        self.master = MasterHFTAgent()
        self.start_time = None
        self.startup_log = []
    
    def log_event(self, event: str, status: str = "INFO"):
        """記錄啟動事件"""
        timestamp = datetime.now().isoformat()
        self.startup_log.append({
            "timestamp": timestamp,
            "event": event,
            "status": status
        })
        
        if status == "ERROR":
            logger.error(f"❌ {event}")
        elif status == "WARNING":
            logger.warning(f"⚠️ {event}")
        else:
            logger.info(f"📝 {event}")
    
    async def pre_start_checks(self):
        """啟動前檢查"""
        print("\n🔍 系統啟動前檢查")
        print("=" * 50)
        
        # 檢查基礎設施
        self.log_event("檢查 Redis 連接...")
        redis_ok = await self.master._check_redis_connection()
        if redis_ok:
            self.log_event("Redis 連接正常")
            print("✅ Redis: 連接正常")
        else:
            self.log_event("Redis 連接失敗", "ERROR")
            print("❌ Redis: 連接失敗")
        
        self.log_event("檢查 ClickHouse 連接...")
        ch_ok = await self.master._check_clickhouse_connection()
        if ch_ok:
            self.log_event("ClickHouse 連接正常")
            print("✅ ClickHouse: 連接正常")
        else:
            self.log_event("ClickHouse 連接失敗", "ERROR")
            print("❌ ClickHouse: 連接失敗")
        
        # 檢查工作區
        ml_ok = await self.master._check_workspace_health("ml")
        ops_ok = await self.master._check_workspace_health("ops")
        
        print(f"✅ ML Workspace: {'正常' if ml_ok else '異常'}")
        print(f"✅ Ops Workspace: {'正常' if ops_ok else '異常'}")
        
        # 檢查 Rust 項目
        rust_project_ok = self.master.rust_path.exists() and (self.master.rust_path / "Cargo.toml").exists()
        print(f"✅ Rust 項目: {'存在' if rust_project_ok else '缺失'}")
        
        print(f"\n📊 預檢查完成，基礎設施就緒: {redis_ok and ch_ok}")
        return redis_ok and ch_ok
    
    async def start_with_monitoring(self):
        """帶監控的系統啟動"""
        print("""
🚀 Enhanced HFT 系統啟動器 v3.0
==========================================
具備實時監控和智能故障恢復能力

系統架構:
  L0: Redis + ClickHouse (基礎設施)
  L1: Rust HFT Core (微秒級交易引擎)  
  L2: Ops Workspace (實時監控)
  L3: ML Workspace (模型訓練)

開始啟動...
        """)
        
        self.start_time = time.time()
        
        # 啟動前檢查
        if not await self.pre_start_checks():
            print("❌ 啟動前檢查失敗，請檢查基礎設施")
            return False
        
        print("\n🎯 開始系統啟動流程...")
        self.log_event("開始系統啟動流程")
        
        try:
            # 啟動系統
            await self.master.start_system()
            
            # 額外的系統狀態報告
            await self.display_enhanced_status()
            
            return True
            
        except Exception as e:
            self.log_event(f"系統啟動異常: {e}", "ERROR")
            logger.error(f"❌ 系統啟動失敗: {e}")
            return False
    
    async def display_enhanced_status(self):
        """顯示增強的系統狀態"""
        print("\n" + "=" * 60)
        print("🎉 HFT 系統啟動完成！")
        print("=" * 60)
        
        status = self.master.get_system_status()
        
        # 系統概覽
        print(f"📊 系統狀態: {status['system_state'].upper()}")
        print(f"⏰ 啟動時間: {time.time() - self.start_time:.2f} 秒")
        print(f"🔄 重啟次數: {status['stats']['restart_count']}")
        
        # 組件詳細狀態
        print("\n🔧 組件狀態詳情:")
        for name, state in status['components'].items():
            if state == "healthy":
                icon = "🟢"
                status_text = "正常運行"
            elif state == "unhealthy":
                icon = "🟡"  
                status_text = "異常狀態"
            else:
                icon = "🔴"
                status_text = "離線"
            
            print(f"  {icon} {name.ljust(15)}: {status_text}")
        
        # Rust 核心特殊檢查
        await self.check_rust_core_activity()
        
        # 啟動日誌摘要
        print(f"\n📋 啟動日誌摘要 ({len(self.startup_log)} 個事件):")
        for log_entry in self.startup_log[-5:]:  # 顯示最後5個事件
            status_icon = "❌" if log_entry["status"] == "ERROR" else "✅"
            print(f"  {status_icon} {log_entry['event']}")
    
    async def check_rust_core_activity(self):
        """檢查 Rust 核心活動"""
        print("\n⚡ Rust HFT 核心狀態:")
        
        rust_process = self.master.components["rust_core"]["process"]
        if rust_process and rust_process.poll() is None:
            print("  🟢 進程狀態: 運行中")
            print(f"  🆔 進程 PID: {rust_process.pid}")
            
            # 檢查進程資源使用
            try:
                import psutil
                process = psutil.Process(rust_process.pid)
                cpu_percent = process.cpu_percent(interval=1)
                memory_mb = process.memory_info().rss / 1024 / 1024
                
                print(f"  💻 CPU 使用率: {cpu_percent:.1f}%")
                print(f"  🧠 內存使用: {memory_mb:.1f} MB")
                
                if cpu_percent > 0.1:
                    print("  📈 檢測到 CPU 活動，可能正在處理市場數據")
                
            except Exception as e:
                print(f"  ⚠️ 無法獲取進程資源信息: {e}")
        else:
            print("  🔴 進程狀態: 未運行")
        
        # 檢查網絡連接
        gRPC_ok = await self.master._check_port_open(50051)
        print(f"  🔌 gRPC 端口 (50051): {'開放' if gRPC_ok else '關閉'}")
    
    async def monitor_loop(self):
        """監控循環"""
        print("\n🔄 進入實時監控模式...")
        print("按 Ctrl+C 停止監控")
        
        try:
            while self.master.system_state in [SystemState.RUNNING, SystemState.DEGRADED]:
                await asyncio.sleep(30)  # 每30秒檢查一次
                
                # 靜默健康檢查
                await self.master._check_all_components_health()
                
                # 簡化狀態報告
                healthy_count = sum(1 for comp in self.master.components.values() 
                                  if comp["state"] == ComponentState.HEALTHY)
                total_count = len(self.master.components)
                
                current_time = datetime.now().strftime("%H:%M:%S")
                print(f"[{current_time}] 系統健康: {healthy_count}/{total_count} 組件正常")
                
        except KeyboardInterrupt:
            print("\n🛑 監控已停止")

async def main():
    """主函數"""
    starter = EnhancedHFTStarter()
    
    try:
        # 啟動系統
        success = await starter.start_with_monitoring()
        
        if success:
            # 進入監控模式
            await starter.monitor_loop()
        else:
            print("❌ 系統啟動失敗")
            
    except KeyboardInterrupt:
        logger.info("\n🛑 收到停止信號")
    except Exception as e:
        logger.error(f"❌ 系統異常: {e}")
    finally:
        await starter.master.stop_system()
        print("✅ 系統已安全關閉")

if __name__ == "__main__":
    asyncio.run(main())