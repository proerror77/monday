#!/usr/bin/env python3
"""
Master Workspace Infrastructure Module
======================================

基礎設施管理模組，負責統一管理所有底層服務：
- Redis 服務代理
- ClickHouse 服務代理  
- Docker Compose 服務管理
- 健康檢查和監控

遷移階段：Phase 0 - 準備階段
- 建立代理模式架構
- 提供向後相容性
- 支援漸進式遷移
"""

from .controller import InfrastructureController
from .redis_proxy import RedisServiceProxy
from .clickhouse_proxy import ClickHouseServiceProxy
from .docker_manager import DockerComposeManager
from .health_checker import InfrastructureHealthChecker

__all__ = [
    'InfrastructureController',
    'RedisServiceProxy', 
    'ClickHouseServiceProxy',
    'DockerComposeManager',
    'InfrastructureHealthChecker'
]

__version__ = "1.0.0"
__author__ = "HFT Infrastructure Team"