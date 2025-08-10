#!/usr/bin/env python3
"""
Master Workspace Development Resources - Simplified
==================================================

簡化版本的開發資源配置，基於 Agno 1.7.6 API
"""

from os import getenv
from pathlib import Path

from agno.infra.app import App
from agno.infra.resources import InfraResources

from workspace.settings import ws_settings

#
# === HFT Master Workspace Development Resources ===
#

# Container 環境變量
container_env = {
    "RUNTIME_ENV": "dev",
    # HFT 系統配置
    "REDIS_URL": "redis://host.docker.internal:6379/2",
    "CLICKHOUSE_URL": "http://host.docker.internal:8123", 
    "RUST_GRPC_URL": "http://host.docker.internal:50051",
    # 路徑配置
    "RUST_HFT_PATH": "/app/../rust_hft",
    "ML_WORKSPACE_PATH": "/app/../ml_workspace",
    "OPS_WORKSPACE_PATH": "/app/../ops_workspace",
    # API Keys
    "AGNO_API_KEY": getenv("AGNO_API_KEY"),
    "OPENAI_API_KEY": getenv("OPENAI_API_KEY"),
    "OLLAMA_BASE_URL": "http://host.docker.internal:11434",
}

# Master Workspace UI (Streamlit)
master_ui = App(
    name=f"{ws_settings.ws_name}-ui",
    port=8503,
    command="streamlit run ui/master_dashboard.py --server.port=8503 --server.address=0.0.0.0 --server.headless=true",
    env=container_env,
    working_dir="/app",
    health_check="/health",
    restart_policy="unless-stopped",
)

# Master Workspace API (FastAPI)  
master_api = App(
    name=f"{ws_settings.ws_name}-api",
    port=8002,
    command="uvicorn api.main:app --host 0.0.0.0 --port 8002 --reload",
    env=container_env,
    working_dir="/app",
    health_check="/health",
    restart_policy="unless-stopped",
)

# === 開發資源配置 ===
dev_resources = InfraResources(
    name="hft-master-dev",
    env="dev",
    infra_type="docker",
    apps=[master_ui, master_api],
)

# Export for Agno
__all__ = ["dev_resources"]