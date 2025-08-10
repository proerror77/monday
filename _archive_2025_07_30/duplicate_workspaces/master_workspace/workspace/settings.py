#!/usr/bin/env python3
"""
Master Workspace Settings - Agno Official Standard
=================================================

按照 Agno 官方文檔標準配置的 Master workspace 設置
https://docs.agno.com/workspaces/agent-app/local
"""

import os
from pathlib import Path
from agno.workspace.settings import WorkspaceSettings

# Master Workspace Settings (Official Agno Standard)
ws_settings = WorkspaceSettings(
    # === Workspace Core Settings ===
    ws_name="hft-master-workspace",
    ws_root=Path(__file__).parent.parent,
    
    # === Environment Configuration ===
    # 支援 ag ws up, ag ws up dev, ag ws up prd
    default_env="dev",
    
    # === Container Infrastructure ===
    # Docker Desktop 本地開發 (官方推薦)
    default_container_infra="docker",
    
    # === App Configuration ===
    # 按照官方標準：Streamlit UI + FastAPI 後端
    dev_app_enabled=True,
    prd_app_enabled=True,
    
    # === Docker Build Settings ===
    # 簡化命名避免 repository format 錯誤
    image_repo="local",
    image_name="hft-master",
    build_images=True,
    
    # === Auto-confirmation Settings ===
    # 減少手動確認
    auto_create_resources=False,  # 保持安全性
    
    # === Development Settings ===
    # 本地開發優化
    dev_mode=True,
    reload_on_change=True,
    
    # === Production Settings ===
    # AWS 部署配置
    aws_region="us-east-1",
)

# 向後兼容
workspace_settings = ws_settings

# Export for Agno
__all__ = ["ws_settings", "workspace_settings"]