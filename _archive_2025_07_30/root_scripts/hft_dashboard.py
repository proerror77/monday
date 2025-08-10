#!/usr/bin/env python3
"""
HFT 系統狀態儀表板
==================

實時顯示 HFT 系統狀態的輕量級儀表板
"""

import asyncio
import json
import time
from datetime import datetime
from master_hft_agent import MasterHFTAgent, ComponentState

class HFTDashboard:
    """HFT 系統狀態儀表板"""
    
    def __init__(self):
        self.master = MasterHFTAgent()
    
    async def get_system_metrics(self):
        """獲取系統指標"""
        # 檢查所有組件健康狀態
        await self.master._check_all_components_health()
        await self.master._evaluate_system_state()
        
        # 獲取基本狀態
        status = self.master.get_system_status()
        
        # 獲取詳細指標
        metrics = {
            "timestamp": datetime.now().isoformat(),
            "system_state": status["system_state"],
            "components": {},
            "infrastructure": {},
            "performance": {}
        }
        
        # 組件狀態
        for name, comp in self.master.components.items():
            metrics["components"][name] = {
                "state": comp["state"].value,
                "healthy": comp["state"] == ComponentState.HEALTHY
            }
        
        # 基礎設施檢查
        redis_ok = await self.master._check_redis_connection()
        ch_ok = await self.master._check_clickhouse_connection()
        
        metrics["infrastructure"] = {
            "redis_connection": redis_ok,
            "clickhouse_connection": ch_ok,
            "redis_port": await self.master._check_port_open(6379),
            "clickhouse_port": await self.master._check_port_open(8123),
            "grpc_port": await self.master._check_port_open(50051)
        }
        
        # Rust 核心性能
        rust_process = self.master.components["rust_core"]["process"]
        if rust_process and rust_process.poll() is None:
            try:
                import psutil
                process = psutil.Process(rust_process.pid)
                metrics["performance"] = {
                    "rust_core_pid": rust_process.pid,
                    "cpu_percent": process.cpu_percent(interval=0.1),
                    "memory_mb": process.memory_info().rss / 1024 / 1024,
                    "process_status": process.status(),
                    "create_time": process.create_time()
                }
            except:
                metrics["performance"] = {"rust_core_pid": rust_process.pid, "error": "無法獲取性能數據"}
        else:
            metrics["performance"] = {"rust_core_running": False}
        
        return metrics
    
    def display_dashboard(self, metrics):
        """顯示儀表板"""
        # 清屏 (僅在支持的終端)
        print("\033[2J\033[H", end="")
        
        print("🚀 HFT 系統實時儀表板")
        print("=" * 60)
        print(f"📅 時間: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        print(f"📊 系統狀態: {self._get_status_icon(metrics['system_state'])} {metrics['system_state'].upper()}")
        print()
        
        # 組件狀態
        print("🔧 組件狀態")
        print("-" * 30)
        for name, comp in metrics["components"].items():
            icon = "🟢" if comp["healthy"] else "🔴"
            print(f"{icon} {name.ljust(15)}: {comp['state']}")
        print()
        
        # 基礎設施
        print("🏗️ 基礎設施")
        print("-" * 30)
        infra = metrics["infrastructure"]
        print(f"🔗 Redis 連接:     {'✅' if infra['redis_connection'] else '❌'}")
        print(f"🔗 ClickHouse 連接: {'✅' if infra['clickhouse_connection'] else '❌'}")
        print(f"🔌 Redis 端口:      {'✅' if infra['redis_port'] else '❌'} (6379)")
        print(f"🔌 ClickHouse 端口: {'✅' if infra['clickhouse_port'] else '❌'} (8123)")
        print(f"🔌 gRPC 端口:       {'✅' if infra['grpc_port'] else '❌'} (50051)")
        print()
        
        # 性能指標
        print("⚡ Rust 核心性能")
        print("-" * 30)
        perf = metrics["performance"]
        if "rust_core_pid" in perf:
            print(f"🆔 進程 PID:  {perf['rust_core_pid']}")
            if "cpu_percent" in perf:
                print(f"💻 CPU 使用:  {perf['cpu_percent']:.1f}%")
                print(f"🧠 內存使用:  {perf['memory_mb']:.1f} MB")
                print(f"📈 進程狀態:  {perf.get('process_status', 'unknown')}")
                
                if perf['cpu_percent'] > 0.1:
                    print("📊 狀態: 正在處理數據 🔥")
                else:
                    print("📊 狀態: 空閒中 💤")
            else:
                print(f"⚠️ {perf.get('error', '未知錯誤')}")
        else:
            print("🔴 Rust 核心未運行")
        
        print()
        print("💡 提示: 按 Ctrl+C 退出監控")
        print("=" * 60)
    
    def _get_status_icon(self, status):
        """獲取狀態圖標"""
        icons = {
            "running": "🟢",
            "degraded": "🟡", 
            "starting": "🔵",
            "failed": "🔴",
            "stopped": "⚪"
        }
        return icons.get(status, "❓")
    
    async def run_dashboard(self, refresh_interval=5):
        """運行儀表板"""
        print("🚀 啟動 HFT 系統儀表板...")
        print(f"🔄 更新間隔: {refresh_interval} 秒")
        time.sleep(2)
        
        try:
            while True:
                metrics = await self.get_system_metrics()
                self.display_dashboard(metrics)
                await asyncio.sleep(refresh_interval)
                
        except KeyboardInterrupt:
            print("\n\n🛑 儀表板已停止")
        except Exception as e:
            print(f"\n❌ 儀表板異常: {e}")

async def main():
    """主函數"""
    dashboard = HFTDashboard()
    
    try:
        await dashboard.run_dashboard(refresh_interval=3)  # 每3秒更新
    except KeyboardInterrupt:
        print("👋 再見!")

if __name__ == "__main__":
    asyncio.run(main())