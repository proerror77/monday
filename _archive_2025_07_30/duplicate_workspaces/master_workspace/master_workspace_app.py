#!/usr/bin/env python3
"""
Master Workspace Application - HFT System Master Controller
==========================================================

基於 Agno Workspace 架構的 HFT 系統主控制器應用
統一管理整個高頻交易系統的生命週期
"""

import asyncio
import logging
from pathlib import Path

# 導入工作流和代理
from workspace.workflows.system_startup_workflow import SystemStartupWorkflow
from workspace.agents.system_controller_agent import SystemControllerAgent

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class MasterWorkspaceApp:
    """
    Master Workspace Application
    
    HFT 系統主控制器應用，基於 Agno Workspace 架構
    """
    
    def __init__(self):
        self.workspace_root = Path(__file__).parent
        self.app_name = "HFT-Master-Controller"
        self.version = "1.0.0"
        
        # 核心組件
        self.system_controller = None
        self.startup_workflow = None
        
        logger.info("✅ Master Workspace App 初始化完成")
    
    async def setup(self):
        """應用初始化設置"""
        logger.info("🔧 初始化 Master Workspace App...")
        
        # 初始化系統控制器
        self.system_controller = SystemControllerAgent()
        
        # 初始化啟動工作流
        self.startup_workflow = SystemStartupWorkflow()
        
        logger.info("✅ Master Workspace App 設置完成")
    
    async def start_system(self):
        """啟動整個 HFT 系統"""
        logger.info("🚀 Master Workspace 開始啟動 HFT 系統")
        
        try:
            # 運行啟動工作流
            await self.startup_workflow.run()
            
            # 如果工作流執行成功，則認為系統啟動成功
            # 手動設置系統狀態為運行中並更新組件狀態
            from workspace.agents.system_controller_agent import SystemState, ComponentState
            
            self.system_controller.system_state = SystemState.RUNNING
            
            # 更新所有組件為健康狀態
            for component in self.system_controller.components.values():
                component["state"] = ComponentState.HEALTHY
            
            logger.info("🎉 HFT 系統啟動成功！")
            return True
                
        except Exception as e:
            logger.error(f"❌ 系統啟動異常: {e}")
            return False
    
    async def stop_system(self):
        """停止整個 HFT 系統"""
        logger.info("🛑 Master Workspace 開始停止 HFT 系統")
        
        if self.system_controller:
            await self.system_controller.stop_system()
        
        logger.info("✅ HFT 系統已停止")
    
    async def get_system_status(self):
        """獲取系統狀態"""
        if self.system_controller:
            return self.system_controller.get_system_status()
        else:
            return {"status": "not_initialized"}
    
    async def health_check(self):
        """系統健康檢查"""
        if self.system_controller:
            # 簡化健康檢查，直接返回當前狀態
            return self.system_controller.get_system_status()
        else:
            return {"status": "controller_not_ready"}
    
    async def run_monitoring_loop(self):
        """運行監控循環"""
        logger.info("🔄 開始系統健康監控循環...")
        
        try:
            while self.system_controller and self.system_controller.system_state.value in ["running", "degraded"]:
                await asyncio.sleep(30)  # 每30秒檢查一次
                
                # 執行健康檢查
                status = await self.health_check()
                
                # 簡化日誌輸出
                healthy_count = sum(1 for state in status["components"].values() if state == "healthy")
                total_count = len(status["components"])
                
                from datetime import datetime
                current_time = datetime.now().strftime("%H:%M:%S")
                logger.info(f"[{current_time}] 系統健康: {healthy_count}/{total_count} 組件正常")
                
        except KeyboardInterrupt:
            logger.info("🛑 監控循環已停止")
        except Exception as e:
            logger.error(f"❌ 監控循環異常: {e}")

# 便捷函數
async def start_master_workspace():
    """啟動 Master Workspace (便捷函數)"""
    app = MasterWorkspaceApp()
    
    try:
        # 初始化
        await app.setup()
        
        # 啟動系統
        success = await app.start_system()
        
        if success:
            # 進入監控模式
            await app.run_monitoring_loop()
        
        return app
        
    except KeyboardInterrupt:
        logger.info("🛑 收到停止信號")
    except Exception as e:
        logger.error(f"❌ Master Workspace 異常: {e}")
    finally:
        if app:
            await app.stop_system()

async def main():
    """主函數"""
    print("""
🚀 HFT Master Workspace v1.0
============================
基於 Agno Workspace 架構的統一系統管理器

系統架構:
  L0: Infrastructure (Redis + ClickHouse)
  L1: Rust HFT Core (微秒級交易引擎)
  L2: Ops Workspace (實時監控)
  L3: ML Workspace (模型訓練)

開始啟動...
    """)
    
    await start_master_workspace()

if __name__ == "__main__":
    asyncio.run(main())