#!/usr/bin/env python3
"""
System Startup Workflow - Agno Workflows 2.0
============================================

HFT 系統統一啟動工作流
基於 Agno Workflows 2.0 框架實現
"""

import asyncio
import logging
from typing import Dict, Any, List
from datetime import datetime

# 模擬 Agno 框架結構 (兼容模式)
class Workflow:
    def __init__(self, name, description):
        self.name = name
        self.description = description
        self.steps = []
        self.routers = []
        self.loops = []
    
    def add_step(self, step):
        self.steps.append(step)
    
    def add_router(self, router):
        self.routers.append(router)
    
    def add_loop(self, loop):
        self.loops.append(loop)
    
    async def run(self):
        # 改進的工作流執行邏輯，支持路由
        context = {}
        step_names = [step.name for step in self.steps]
        router_map = {router.name: router for router in self.routers}
        
        # 執行前5個步驟（啟動流程）
        for i, step in enumerate(self.steps[:5]):  # infrastructure_check, infrastructure_setup, rust_startup, workspace_startup, system_evaluation
            try:
                result = await step.function(context)
                context.update(result)
            except Exception as e:
                logger.error(f"Step {step.name} failed: {e}")
                # 失敗時直接跳到失敗處理
                for failure_step in self.steps:
                    if failure_step.name == "startup_failure":
                        await failure_step.function(context)
                        return
                break
        
        # 執行路由決策
        for router in self.routers:
            if router.name == "startup_result_router":
                try:
                    next_step_name = await router.function(context)
                    # 找到對應的步驟並執行
                    for step in self.steps:
                        if step.name == next_step_name:
                            await step.function(context)
                            return
                except Exception as e:
                    logger.error(f"Router {router.name} failed: {e}")
                    # 路由失敗時執行失敗處理
                    for failure_step in self.steps:
                        if failure_step.name == "startup_failure":
                            await failure_step.function(context)
                            return

class Step:
    def __init__(self, name, function, description):
        self.name = name
        self.function = function
        self.description = description

class Router:
    def __init__(self, name, function, description):
        self.name = name
        self.function = function
        self.description = description

class Loop:
    def __init__(self, name, function, condition, description):
        self.name = name
        self.function = function
        self.condition = condition
        self.description = description

# 導入設置和代理
import sys
from pathlib import Path
sys.path.append(str(Path(__file__).parent.parent))

from settings import ws_settings
from agents.system_controller_agent import SystemControllerAgent, SystemState

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class SystemStartupWorkflow(Workflow):
    """
    HFT 系統啟動工作流
    
    基於 Agno Workflows 2.0 的聲明式系統啟動管理
    """
    
    def __init__(self):
        super().__init__(
            name="HFT-System-Startup",
            description="HFT 高頻交易系統統一啟動工作流"
        )
        
        # 初始化系統控制器
        self.controller = SystemControllerAgent()
        
        # 工作流狀態
        self.startup_log = []
        self.startup_start_time = None
        
        # 定義工作流步驟
        self._define_workflow_steps()
    
    def _define_workflow_steps(self):
        """定義工作流步驟"""
        
        # Step 1: 系統預檢查
        self.add_step(Step(
            name="pre_startup_check",
            function=self._pre_startup_check,
            description="啟動前系統健康檢查"
        ))
        
        # Step 2: 基礎設施啟動
        self.add_step(Step(
            name="start_infrastructure",
            function=self._start_infrastructure_step,
            description="啟動 Redis 和 ClickHouse 基礎設施"
        ))
        
        # Step 3: Rust 核心啟動
        self.add_step(Step(
            name="start_rust_core",
            function=self._start_rust_core_step,
            description="編譯並啟動 Rust HFT 核心引擎"
        ))
        
        # Step 4: 工作區啟動 (並行)
        self.add_step(Step(
            name="start_workspaces",
            function=self._start_workspaces_step,
            description="並行啟動 ML 和 Ops 工作區"
        ))
        
        # Step 5: 系統狀態評估
        self.add_step(Step(
            name="evaluate_system",
            function=self._evaluate_system_step,
            description="評估系統整體狀態並生成報告"
        ))
        
        # Router: 啟動結果處理
        self.add_router(Router(
            name="startup_result_router",
            function=self._route_startup_result,
            description="根據啟動結果決定下一步操作"
        ))
        
        # Step 6a: 啟動成功處理
        self.add_step(Step(
            name="startup_success",
            function=self._startup_success_step,
            description="啟動成功後的後續處理"
        ))
        
        # Step 6b: 啟動失敗處理
        self.add_step(Step(
            name="startup_failure",
            function=self._startup_failure_step,
            description="啟動失敗的錯誤處理和恢復"
        ))
        
        # Loop: 健康監控循環
        self.add_loop(Loop(
            name="health_monitoring_loop",
            function=self._health_monitoring_iteration,
            condition=self._should_continue_monitoring,
            description="系統健康狀態持續監控循環"
        ))
    
    async def _pre_startup_check(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """啟動前檢查"""
        logger.info("🔍 執行啟動前系統檢查...")
        
        self.startup_start_time = datetime.now()
        self._log_event("開始系統啟動工作流")
        
        # 檢查基礎設施連接
        redis_ok = await self.controller._check_redis_connection()
        clickhouse_ok = await self.controller._check_clickhouse_connection()
        
        # 檢查工作區結構
        ml_exists = self.controller.ml_path.exists()
        ops_exists = self.controller.ops_path.exists()
        rust_exists = self.controller.rust_path.exists()
        
        check_results = {
            "redis_connection": redis_ok,
            "clickhouse_connection": clickhouse_ok,
            "ml_workspace_exists": ml_exists,
            "ops_workspace_exists": ops_exists,
            "rust_project_exists": rust_exists,
            "pre_check_passed": all([redis_ok, clickhouse_ok, ml_exists, ops_exists, rust_exists])
        }
        
        if check_results["pre_check_passed"]:
            logger.info("✅ 啟動前檢查通過")
            self._log_event("啟動前檢查通過")
        else:
            logger.error("❌ 啟動前檢查失敗")
            self._log_event("啟動前檢查失敗", "ERROR")
        
        return {"pre_check_results": check_results}
    
    async def _start_infrastructure_step(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """基礎設施啟動步驟"""
        logger.info("🏗️ 啟動基礎設施...")
        
        success = await self.controller._start_infrastructure()
        
        if success:
            logger.info("✅ 基礎設施啟動成功")
            self._log_event("基礎設施啟動成功")
        else:
            logger.error("❌ 基礎設施啟動失敗")
            self._log_event("基礎設施啟動失敗", "ERROR")
        
        return {"infrastructure_started": success}
    
    async def _start_rust_core_step(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """Rust 核心啟動步驟"""
        logger.info("⚡ 啟動 Rust HFT 核心...")
        
        success = await self.controller._start_rust_core()
        
        if success:
            logger.info("✅ Rust HFT 核心啟動成功")
            self._log_event("Rust HFT 核心啟動成功")
        else:
            logger.error("❌ Rust HFT 核心啟動失敗")
            self._log_event("Rust HFT 核心啟動失敗", "ERROR")
        
        return {"rust_core_started": success}
    
    async def _start_workspaces_step(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """工作區啟動步驟 (並行)"""
        logger.info("🧠 並行啟動 ML 和 Ops 工作區...")
        
        # 並行啟動
        ml_task = asyncio.create_task(self.controller._start_ml_workspace())
        ops_task = asyncio.create_task(self.controller._start_ops_workspace())
        
        ml_result, ops_result = await asyncio.gather(ml_task, ops_task, return_exceptions=True)
        
        ml_success = not isinstance(ml_result, Exception) and ml_result
        ops_success = not isinstance(ops_result, Exception) and ops_result
        
        if ml_success:
            self._log_event("ML Workspace 啟動成功")
        else:
            self._log_event(f"ML Workspace 啟動失敗: {ml_result if isinstance(ml_result, Exception) else 'Unknown error'}", "ERROR")
        
        if ops_success:
            self._log_event("Ops Workspace 啟動成功")
        else:
            self._log_event(f"Ops Workspace 啟動失敗: {ops_result if isinstance(ops_result, Exception) else 'Unknown error'}", "ERROR")
        
        return {
            "ml_workspace_started": ml_success,
            "ops_workspace_started": ops_success,
            "workspaces_started": ml_success and ops_success
        }
    
    async def _evaluate_system_step(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """系統狀態評估步驟"""
        logger.info("📊 評估系統整體狀態...")
        
        # 評估系統狀態
        await self.controller._evaluate_system_state()
        
        # 獲取系統狀態
        system_status = self.controller.get_system_status()
        
        # AI 分析 (如果啟用)
        ai_analysis = None
        if ws_settings.enable_ai_analysis:
            try:
                ai_analysis = await self.controller.analyze_system_state(system_status)
                self._log_event("AI 系統分析完成")
            except Exception as e:
                self._log_event(f"AI 分析失敗: {e}", "WARNING")
        
        return {
            "system_status": system_status,
            "ai_analysis": ai_analysis,
            "startup_successful": self.controller.system_state in [SystemState.RUNNING, SystemState.DEGRADED]
        }
    
    async def _route_startup_result(self, context: Dict[str, Any]) -> str:
        """啟動結果路由"""
        if context.get("startup_successful", False):
            return "startup_success"
        else:
            return "startup_failure"
    
    async def _startup_success_step(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """啟動成功處理"""
        logger.info("🎉 HFT 系統啟動成功！")
        
        # 顯示啟動摘要
        elapsed_time = (datetime.now() - self.startup_start_time).total_seconds()
        
        logger.info("=" * 60)
        logger.info("🎉 HFT 系統啟動完成！")
        logger.info("=" * 60)
        logger.info(f"📊 系統狀態: {self.controller.system_state.value.upper()}")
        logger.info(f"⏰ 總啟動時間: {elapsed_time:.2f} 秒")
        logger.info(f"📋 啟動事件數: {len(self.startup_log)}")
        
        # 組件狀態
        logger.info("\n🔧 組件狀態:")
        for name, comp in self.controller.components.items():
            icon = "🟢" if comp["state"].value == "healthy" else "🔴"
            logger.info(f"  {icon} {name}: {comp['state'].value}")
        
        # AI 分析結果
        if context.get("ai_analysis"):
            logger.info(f"\n🤖 AI 分析報告:")
            logger.info(context["ai_analysis"])
        
        self._log_event(f"系統啟動成功 (耗時 {elapsed_time:.2f}s)")
        
        return {"startup_completed": True, "elapsed_time": elapsed_time}
    
    async def _startup_failure_step(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """啟動失敗處理"""
        logger.error("❌ HFT 系統啟動失敗")
        
        elapsed_time = (datetime.now() - self.startup_start_time).total_seconds()
        
        # 錯誤分析
        failed_components = [
            name for name, comp in self.controller.components.items()
            if comp["state"].value != "healthy"
        ]
        
        logger.error(f"⏰ 啟動時間: {elapsed_time:.2f} 秒")
        logger.error(f"❌ 失敗組件: {', '.join(failed_components)}")
        
        # 緊急清理
        await self.controller._emergency_cleanup()
        
        self._log_event(f"系統啟動失敗 (耗時 {elapsed_time:.2f}s)", "ERROR")
        
        return {"startup_completed": False, "failed_components": failed_components}
    
    async def _health_monitoring_iteration(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """健康監控循環迭代"""
        # 每次監控間隔
        await asyncio.sleep(ws_settings.health_check_interval_s)
        
        # 檢查所有組件健康狀態
        await self.controller._check_all_components_health()
        await self.controller._evaluate_system_state()
        
        # 簡化日誌輸出
        current_time = datetime.now().strftime("%H:%M:%S")
        healthy_count = sum(1 for comp in self.controller.components.values() 
                          if comp["state"].value == "healthy")
        total_count = len(self.controller.components)
        
        logger.info(f"[{current_time}] 系統健康監控: {healthy_count}/{total_count} 組件正常")
        
        return {"monitoring_iteration": True}
    
    def _should_continue_monitoring(self, context: Dict[str, Any]) -> bool:
        """判斷是否繼續監控"""
        return self.controller.system_state in [SystemState.RUNNING, SystemState.DEGRADED]
    
    def _log_event(self, event: str, level: str = "INFO"):
        """記錄啟動事件"""
        timestamp = datetime.now().isoformat()
        self.startup_log.append({
            "timestamp": timestamp,
            "event": event,
            "level": level
        })
        
        if level == "ERROR":
            logger.error(f"❌ {event}")
        elif level == "WARNING":
            logger.warning(f"⚠️ {event}")
        else:
            logger.info(f"📝 {event}")

# 便捷函數
async def start_hft_system() -> SystemStartupWorkflow:
    """啟動 HFT 系統 (便捷函數)"""
    workflow = SystemStartupWorkflow()
    await workflow.run()
    return workflow

async def main():
    """主函數 - 用於測試"""
    try:
        workflow = await start_hft_system()
        
        # 如果啟動成功，進入監控模式
        if workflow.controller.system_state in [SystemState.RUNNING, SystemState.DEGRADED]:
            logger.info("🔄 進入健康監控模式 (按 Ctrl+C 停止)...")
            
            try:
                while True:
                    await asyncio.sleep(30)
                    await workflow.controller._check_all_components_health()
                    await workflow.controller._evaluate_system_state()
            except KeyboardInterrupt:
                logger.info("🛑 監控已停止")
        
    except KeyboardInterrupt:
        logger.info("🛑 收到停止信號")
    except Exception as e:
        logger.error(f"❌ 工作流異常: {e}")
    finally:
        logger.info("👋 工作流結束")

if __name__ == "__main__":
    asyncio.run(main())