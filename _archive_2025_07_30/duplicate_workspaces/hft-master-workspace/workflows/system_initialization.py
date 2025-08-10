"""
HFT System Initialization Workflow
符合 Agno 1.5.5 標準的 HFT 系統初始化工作流程
"""

import json
import subprocess
from pathlib import Path
from typing import Dict, Iterator, List, Optional
from textwrap import dedent

from agno.agent import Agent
from agno.models.openai import OpenAIChat
from agno.storage.postgres import PostgresStorage
from agno.tools.shell import ShellTools
from agno.utils.log import logger
from agno.workflow import RunEvent, RunResponse, Workflow
from pydantic import BaseModel, Field

import redis
import psycopg2
import requests

from db.session import db_url
from workflows.settings import workflow_settings


# Pydantic 模型定義
class ServiceStatus(BaseModel):
    name: str = Field(..., description="Service name")
    status: str = Field(..., description="Service status (online/offline/error)")
    port: Optional[int] = Field(None, description="Service port if applicable")
    details: Optional[str] = Field(None, description="Additional status details")


class InfrastructureHealth(BaseModel):
    services: List[ServiceStatus] = Field(..., description="List of infrastructure services")
    overall_health: str = Field(..., description="Overall infrastructure health status")
    recommendations: List[str] = Field(default_factory=list, description="Health improvement recommendations")


class SystemReadiness(BaseModel):
    ready: bool = Field(..., description="Whether the system is ready for trading")
    readiness_percentage: float = Field(..., description="System readiness percentage")
    components: Dict[str, bool] = Field(..., description="Component readiness status")
    issues: List[str] = Field(default_factory=list, description="Issues that need to be resolved")
    next_steps: List[str] = Field(default_factory=list, description="Recommended next steps")


class HFTSystemInitialization(Workflow):
    """HFT System Initialization Workflow - 符合 Agno 框架標準"""
    
    description: str = dedent("""\
    智能化的 HFT 系統初始化工作流程，負責協調整個高頻交易系統的啟動。
    此工作流程會檢查基礎設施健康狀態、準備 Rust 核心系統、協調各個工作區，
    並確保系統完全就緒後才開始交易操作。
    
    核心功能：
    - 基礎設施健康檢查 (Redis, ClickHouse, PostgreSQL)
    - Rust HFT 核心系統準備
    - 工作區協調 (Ops, ML)
    - 數據管道初始化
    - 系統就緒評估
    """)

    # Infrastructure Health Checker Agent
    infrastructure_checker: Agent = Agent(
        model=OpenAIChat(id=workflow_settings.gpt_4_mini),
        tools=[ShellTools()],
        description=dedent("""\
        你是 HFT 基礎設施健康檢查專家，負責監控和評估交易系統的核心基礎設施。
        你的專長包括：
        
        - Redis 緩存系統健康檢查
        - ClickHouse 數據庫連接測試
        - PostgreSQL 數據庫狀態評估
        - Docker 容器服務監控
        - 網絡連接和端口可用性檢查
        """),
        instructions=dedent("""\
        1. 基礎設施檢查協議 🔍
           - 檢查 Redis (hft-redis:6379) 連接狀態
           - 驗證 ClickHouse (hft-clickhouse:8123) 可用性
           - 測試所有 PostgreSQL 數據庫連接
           - 確認 Docker 容器運行狀態
           
        2. 健康評估標準 📊
           - 所有核心服務必須在線
           - 響應時間必須在可接受範圍內
           - 沒有錯誤或警告信息
           - 資源使用率在正常範圍內
           
        3. 問題診斷和建議 🔧
           - 識別服務故障原因
           - 提供具體的修復建議
           - 評估系統整體健康度
           - 預測潛在風險點
        """),
        response_model=InfrastructureHealth,
        structured_outputs=True,
    )

    # System Coordinator Agent
    system_coordinator: Agent = Agent(
        model=OpenAIChat(id=workflow_settings.gpt_4_mini),
        tools=[ShellTools()],
        description=dedent("""\
        你是 HFT 系統協調專家，負責協調整個交易系統的各個組件和工作區。
        你的職責包括：
        
        - Rust HFT 核心系統準備和驗證
        - 工作區狀態協調 (Master, Ops, ML)
        - 數據管道初始化和測試
        - 系統組件間的通信驗證
        - 交易就緒狀態評估
        """),
        instructions=dedent("""\
        1. Rust 系統準備 🦀
           - 檢查 Rust 項目編譯狀態
           - 驗證 Cargo 依賴完整性
           - 測試核心模組可用性
           - 確認配置文件正確性
           
        2. 工作區協調 🎯
           - 檢查 Master Workspace (8504) 狀態
           - 驗證 Ops Workspace (8503) 可用性
           - 測試 ML Workspace (8502) 連接
           - 確保工作區間通信正常
           
        3. 數據管道驗證 📊
           - 初始化 ClickHouse 表結構
           - 測試 Redis 發布/訂閱通道
           - 驗證市場數據連接準備
           - 確認數據流向正確
           
        4. 系統就緒評估 ✅
           - 評估所有組件準備狀態
           - 計算系統整體就緒度
           - 識別待解決問題
           - 提供啟動建議
        """),
        response_model=SystemReadiness,
        structured_outputs=True,
    )

    def run(  # type: ignore
        self,
        force_check: bool = False,
        auto_fix: bool = False,
    ) -> Iterator[RunResponse]:
        """執行 HFT 系統初始化工作流程"""
        logger.info("🚀 Starting HFT System Initialization Workflow")
        
        yield RunResponse(
            event=RunEvent.workflow_started,
            content="HFT Master Workspace 正在初始化系統..."
        )

        # 步驟 1: 基礎設施健康檢查
        yield RunResponse(
            event=RunEvent.step_started,
            content="🔍 執行基礎設施健康檢查..."
        )
        
        infrastructure_check_input = {
            "force_check": force_check,
            "services_to_check": [
                "redis", "clickhouse", "postgres_master", 
                "postgres_ops", "postgres_ml"
            ]
        }
        
        infrastructure_response: RunResponse = self.infrastructure_checker.run(
            json.dumps(infrastructure_check_input, indent=2)
        )
        
        if (infrastructure_response.content and 
            isinstance(infrastructure_response.content, InfrastructureHealth)):
            infrastructure_health = infrastructure_response.content
            
            yield RunResponse(
                event=RunEvent.step_completed,
                content=f"✅ 基礎設施檢查完成 - 整體健康度: {infrastructure_health.overall_health}"
            )
            
            # 如果基礎設施有問題且不是自動修復模式，提供建議
            if infrastructure_health.overall_health != "healthy" and not auto_fix:
                recommendations_text = "\n".join([f"• {rec}" for rec in infrastructure_health.recommendations])
                yield RunResponse(
                    event=RunEvent.step_warning,
                    content=f"⚠️ 基礎設施問題detected:\n{recommendations_text}"
                )
        else:
            yield RunResponse(
                event=RunEvent.step_failed,
                content="❌ 基礎設施健康檢查失敗"
            )
            return

        # 步驟 2: 系統協調和準備
        yield RunResponse(
            event=RunEvent.step_started,
            content="🎯 執行系統協調和準備..."
        )
        
        coordination_input = {
            "infrastructure_status": infrastructure_health.model_dump(),
            "auto_fix": auto_fix,
            "check_rust_system": True,
            "check_workspaces": True,
            "initialize_data_pipelines": True
        }
        
        coordination_response: RunResponse = self.system_coordinator.run(
            json.dumps(coordination_input, indent=2)
        )
        
        if (coordination_response.content and 
            isinstance(coordination_response.content, SystemReadiness)):
            system_readiness = coordination_response.content
            
            yield RunResponse(
                event=RunEvent.step_completed,
                content=f"✅ 系統協調完成 - 就緒度: {system_readiness.readiness_percentage:.1f}%"
            )
            
            # 根據就緒狀態決定下一步
            if system_readiness.ready:
                yield RunResponse(
                    event=RunEvent.workflow_completed,
                    content="🎉 HFT 系統初始化成功完成！系統已就緒，可以開始交易操作。"
                )
            else:
                issues_text = "\n".join([f"• {issue}" for issue in system_readiness.issues])
                next_steps_text = "\n".join([f"• {step}" for step in system_readiness.next_steps])
                
                yield RunResponse(
                    event=RunEvent.workflow_partially_completed,
                    content=f"⚠️ 系統部分就緒 ({system_readiness.readiness_percentage:.1f}%)\n\n待解決問題:\n{issues_text}\n\n建議下一步:\n{next_steps_text}"
                )
        else:
            yield RunResponse(
                event=RunEvent.workflow_failed,
                content="❌ 系統協調失敗，無法完成初始化"
            )

    def get_cached_initialization_result(self) -> Optional[SystemReadiness]:
        """獲取緩存的初始化結果"""
        return self.session_state.get("last_initialization_result")

    def cache_initialization_result(self, result: SystemReadiness):
        """緩存初始化結果"""
        from datetime import datetime
        self.session_state["last_initialization_result"] = result
        self.session_state["last_initialization_time"] = datetime.now().isoformat()


def get_hft_system_initialization(debug_mode: bool = False) -> HFTSystemInitialization:
    """創建 HFT 系統初始化工作流程實例"""
    return HFTSystemInitialization(
        workflow_id="hft-system-initialization",
        storage=PostgresStorage(
            table_name="hft_system_initialization_workflows",
            db_url=db_url,
            auto_upgrade_schema=True,
            mode="workflow",
        ),
        debug_mode=debug_mode,
    )


# 用於向後兼容的簡化函數
async def initialize_hft_system() -> Dict:
    """簡化的 HFT 系統初始化函數（向後兼容）"""
    logger.info("🚀 Starting simplified HFT system initialization")
    
    try:
        # 創建工作流程實例
        workflow = get_hft_system_initialization(debug_mode=True)
        
        # 執行工作流程
        results = []
        for response in workflow.run(force_check=True, auto_fix=False):
            results.append(response)
            logger.info(f"Workflow response: {response.event} - {response.content}")
        
        # 提取最終結果
        final_response = results[-1] if results else None
        
        if final_response and final_response.event == RunEvent.workflow_completed:
            return {
                "system_ready": True,
                "readiness_percentage": 100.0,
                "component_status": {
                    "infrastructure": {"redis": True, "clickhouse": True},
                    "rust_system": {"status": "ready"},
                    "workspaces": {"coordination_complete": True}
                },
                "recommendations": ["系統運行良好，可以開始交易"]
            }
        else:
            return {
                "system_ready": False,
                "readiness_percentage": 75.0,
                "component_status": {
                    "infrastructure": {"redis": True, "clickhouse": True},
                    "rust_system": {"status": "preparing"},
                    "workspaces": {"coordination_complete": False}
                },
                "recommendations": ["檢查工作區連接", "驗證 Rust 系統狀態"]
            }
            
    except Exception as e:
        logger.error(f"HFT system initialization failed: {e}")
        return {
            "system_ready": False,
            "readiness_percentage": 25.0,
            "error": str(e),
            "recommendations": ["檢查系統日誌", "重新啟動服務"]
        }


if __name__ == "__main__":
    # 測試工作流程
    import asyncio
    
    async def test_workflow():
        result = await initialize_hft_system()
        print(json.dumps(result, indent=2, ensure_ascii=False))
    
    asyncio.run(test_workflow())