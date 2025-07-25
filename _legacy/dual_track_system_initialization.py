#!/usr/bin/env python3
"""
雙軌運行系統初始化管理器
=========================

基於您描述的成熟生產架構：
- 軌道一：24/7 Rust 核心服務 (熱備狀態)
- 軌道二：按需 Python MLOps 工作流 (冷備狀態)

系統初始化流程：
1. Rust 核心：啟動 → 加載配置 → 加載生產模型 → 建立連接 → 進入 24/7 循環
2. Python MLOps：靜默狀態 → 等待觸發 (定時/手動/事件)

符合 Agno Workflows v2 框架的企業級系統管理
"""

import asyncio
import json
import time
import logging
import subprocess
import signal
import sys
from pathlib import Path
from typing import Dict, Any, Optional, List, Callable
from enum import Enum
from dataclasses import dataclass, field
import yaml
import requests
import redis

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# ==========================================
# 系統狀態和配置定義
# ==========================================

class SystemState(Enum):
    """系統狀態"""
    INITIALIZING = "initializing"
    RUST_CORE_STARTING = "rust_core_starting"
    RUST_CORE_READY = "rust_core_ready"      # 熱備狀態
    MLOPS_IDLE = "mlops_idle"                # 冷備狀態
    FULLY_OPERATIONAL = "fully_operational"   # 雙軌就緒
    DEGRADED = "degraded"
    FAILED = "failed"
    SHUTTING_DOWN = "shutting_down"

class TrackStatus(Enum):
    """軌道狀態"""
    OFFLINE = "offline"
    STARTING = "starting"
    READY = "ready"           # 熱備 (Rust) 或 冷備 (MLOps)
    ACTIVE = "active"         # 正在執行
    ERROR = "error"
    MAINTENANCE = "maintenance"

@dataclass
class SystemConfig:
    """系統配置"""
    # Rust 核心配置
    rust_hft_dir: str = "./rust_hft"
    rust_config_dir: str = "./rust_hft/config"
    production_models_dir: str = "/app/production_models"
    rust_api_port: int = 8080
    rust_health_check_endpoint: str = "http://localhost:8080/health"
    
    # Python MLOps 配置
    python_mlops_module: str = "agno_v2_mlops_workflow"
    mlops_trigger_schedule: str = "0 3 * * *"  # 每天凌晨3點
    mlops_max_concurrent_runs: int = 1
    
    # 基礎設施配置
    clickhouse_url: str = "http://localhost:8123"
    redis_url: str = "redis://localhost:6379"
    
    # 監控和告警
    health_check_interval: int = 30
    startup_timeout: int = 300
    
    # 生產環境安全設置
    enable_trading: bool = False  # 默認關閉交易
    emergency_stop_enabled: bool = True
    max_startup_retries: int = 3

@dataclass
class TrackState:
    """軌道狀態"""
    name: str
    status: TrackStatus = TrackStatus.OFFLINE
    pid: Optional[int] = None
    start_time: Optional[float] = None
    last_health_check: Optional[float] = None
    error_count: int = 0
    last_error: Optional[str] = None
    metadata: Dict[str, Any] = field(default_factory=dict)

# ==========================================
# 雙軌系統初始化管理器
# ==========================================

class DualTrackSystemManager:
    """
    雙軌運行系統管理器
    
    職責：
    1. 管理 Rust 核心服務的生命週期 (24/7 熱備)
    2. 管理 Python MLOps 工作流的調度 (按需冷備)
    3. 監控系統整體健康狀態
    4. 處理優雅停機和錯誤恢復
    """
    
    def __init__(self, config: Optional[SystemConfig] = None):
        self.config = config or SystemConfig()
        self.logger = logging.getLogger("DualTrackManager")
        
        # 系統狀態
        self.system_state = SystemState.INITIALIZING
        self.initialization_start_time = time.time()
        
        # 軌道狀態
        self.rust_track = TrackState("RustCore")
        self.mlops_track = TrackState("MLOpsWorkflow")
        
        # 運行時狀態
        self.is_shutting_down = False
        self.health_check_task = None
        self.mlops_scheduler_task = None
        
        # Redis 通信
        try:
            self.redis_client = redis.from_url(self.config.redis_url)
            self.redis_available = True
        except Exception as e:
            self.logger.warning(f"⚠️  Redis 連接失敗: {e}")
            self.redis_available = False
        
        self.logger.info("🏗️  雙軌系統管理器初始化完成")
    
    async def initialize_system(self) -> Dict[str, Any]:
        """
        系統初始化入口點
        
        執行完整的雙軌系統初始化流程
        """
        self.logger.info("🚀 開始雙軌系統初始化...")
        self.logger.info("=" * 80)
        
        initialization_result = {
            "start_time": self.initialization_start_time,
            "stages": {},
            "final_state": None,
            "error": None
        }
        
        try:
            # 階段 1: 預檢查
            self.logger.info("📋 階段 1: 系統預檢查...")
            precheck_result = await self._perform_system_precheck()
            initialization_result["stages"]["precheck"] = precheck_result
            
            if not precheck_result["passed"]:
                raise Exception(f"系統預檢查失敗: {precheck_result['issues']}")
            
            # 階段 2: 啟動 Rust 核心 (軌道一)
            self.logger.info("🦀 階段 2: 啟動 Rust 核心服務...")
            rust_result = await self._initialize_rust_core_track()
            initialization_result["stages"]["rust_core"] = rust_result
            
            if not rust_result["success"]:
                raise Exception(f"Rust 核心啟動失敗: {rust_result['error']}")
            
            # 階段 3: 準備 MLOps 軌道 (軌道二)
            self.logger.info("🧠 階段 3: 準備 MLOps 工作流軌道...")
            mlops_result = await self._prepare_mlops_track()
            initialization_result["stages"]["mlops_preparation"] = mlops_result
            
            # 階段 4: 啟動系統監控
            self.logger.info("👁️  階段 4: 啟動系統監控...")
            monitoring_result = await self._start_system_monitoring()
            initialization_result["stages"]["monitoring"] = monitoring_result
            
            # 階段 5: 最終狀態確認
            self.logger.info("✅ 階段 5: 最終狀態確認...")
            final_check_result = await self._perform_final_state_check()
            initialization_result["stages"]["final_check"] = final_check_result
            
            # 設置最終狀態
            if all(stage.get("success", False) for stage in initialization_result["stages"].values()):
                self.system_state = SystemState.FULLY_OPERATIONAL
                initialization_result["final_state"] = "success"
                
                self.logger.info("🎉 雙軌系統初始化成功！")
                self.logger.info(f"🦀 Rust 核心：{self.rust_track.status.value} (熱備狀態)")
                self.logger.info(f"🧠 MLOps 工作流：{self.mlops_track.status.value} (冷備狀態)")
                
            else:
                self.system_state = SystemState.DEGRADED
                initialization_result["final_state"] = "degraded"
                self.logger.warning("⚠️  系統初始化完成，但處於降級狀態")
            
        except Exception as e:
            self.logger.error(f"❌ 系統初始化失敗: {e}")
            self.system_state = SystemState.FAILED
            initialization_result["final_state"] = "failed"
            initialization_result["error"] = str(e)
            
            # 清理已啟動的組件
            await self._cleanup_on_failure()
        
        initialization_result["end_time"] = time.time()
        initialization_result["total_duration"] = initialization_result["end_time"] - self.initialization_start_time
        
        self.logger.info("=" * 80)
        self.logger.info(f"🏁 系統初始化完成 (耗時: {initialization_result['total_duration']:.1f}秒)")
        
        return initialization_result
    
    async def _perform_system_precheck(self) -> Dict[str, Any]:
        """執行系統預檢查"""
        checks = {
            "rust_hft_directory": Path(self.config.rust_hft_dir).exists(),
            "rust_config_files": (Path(self.config.rust_config_dir) / "comprehensive_test.yaml").exists(),
            "production_models_dir": True,  # 會自動創建
            "clickhouse_connection": False,
            "redis_connection": self.redis_available,
            "required_ports_available": False
        }
        
        # 檢查 ClickHouse 連接
        try:
            response = requests.get(f"{self.config.clickhouse_url}/ping", timeout=5)
            checks["clickhouse_connection"] = response.status_code == 200
        except Exception:
            pass
        
        # 檢查端口可用性
        try:
            # 簡單檢查 Rust API 端口是否被佔用
            import socket
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.settimeout(1)
            result = sock.connect_ex(('localhost', self.config.rust_api_port))
            checks["required_ports_available"] = result != 0  # 端口未被佔用
            sock.close()
        except Exception:
            checks["required_ports_available"] = True
        
        # 評估檢查結果
        critical_checks = ["rust_hft_directory", "rust_config_files", "required_ports_available"]
        critical_passed = all(checks[check] for check in critical_checks)
        
        issues = [f"{check}: {'✅' if status else '❌'}" for check, status in checks.items()]
        
        return {
            "passed": critical_passed,
            "checks": checks,
            "critical_passed": critical_passed,
            "issues": issues,
            "warning_count": sum(1 for check in ["clickhouse_connection", "redis_connection"] if not checks[check])
        }
    
    async def _initialize_rust_core_track(self) -> Dict[str, Any]:
        """
        初始化 Rust 核心軌道 (24/7 熱備狀態)
        
        步驟：
        1. 確保生產模型目錄存在
        2. 啟動 Rust 核心進程
        3. 等待 API 服務就緒
        4. 驗證核心功能
        """
        
        self.rust_track.status = TrackStatus.STARTING
        self.rust_track.start_time = time.time()
        
        try:
            # 1. 準備生產模型目錄
            production_models_path = Path(self.config.production_models_dir)
            production_models_path.mkdir(parents=True, exist_ok=True)
            
            # 檢查是否有現有的生產模型
            existing_models = list(production_models_path.glob("*.safetensors"))
            if not existing_models:
                self.logger.info("⚠️  未找到現有生產模型，將使用安全默認策略")
                # 這裡可以創建一個默認的"只看不做"策略模型
            else:
                self.logger.info(f"📦 找到 {len(existing_models)} 個現有生產模型")
            
            # 2. 構建 Rust 核心啟動命令
            rust_cmd = [
                "cargo", "run", "--release", "--bin", "hft_core",
                "--", 
                "--config", f"{self.config.rust_config_dir}/production.yaml",
                "--models-dir", str(production_models_path),
                "--api-port", str(self.config.rust_api_port),
                "--mode", "production"
            ]
            
            # 3. 啟動 Rust 核心進程
            self.logger.info("🚀 啟動 Rust 核心進程...")
            self.logger.info(f"   命令: {' '.join(rust_cmd)}")
            
            # 使用配置文件啟動（如果存在的話）
            config_file = Path(self.config.rust_config_dir) / "comprehensive_test.yaml"
            if config_file.exists():
                rust_cmd = [
                    "cargo", "run", "--release", "--example", "comprehensive_db_stress_test",
                    "--", "--config", str(config_file), "--mode", "production"
                ]
            
            # 啟動進程
            process = await asyncio.create_subprocess_exec(
                *rust_cmd,
                cwd=self.config.rust_hft_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            self.rust_track.pid = process.pid
            self.logger.info(f"📍 Rust 核心進程已啟動 (PID: {process.pid})")
            
            # 4. 等待 API 服務就緒
            self.logger.info("⏳ 等待 Rust API 服務就緒...")
            api_ready = await self._wait_for_rust_api_ready()
            
            if api_ready:
                self.rust_track.status = TrackStatus.READY
                self.rust_track.last_health_check = time.time()
                
                # 5. 驗證核心功能
                verification_result = await self._verify_rust_core_functionality()
                
                return {
                    "success": True,
                    "pid": self.rust_track.pid,
                    "startup_time": time.time() - self.rust_track.start_time,
                    "api_ready": api_ready,
                    "verification": verification_result,
                    "status": "hot_standby"  # 熱備狀態
                }
            else:
                raise Exception("Rust API 服務啟動超時")
                
        except Exception as e:
            self.rust_track.status = TrackStatus.ERROR
            self.rust_track.last_error = str(e)
            self.rust_track.error_count += 1
            
            return {
                "success": False,
                "error": str(e),
                "pid": self.rust_track.pid
            }
    
    async def _wait_for_rust_api_ready(self, timeout: int = None) -> bool:
        """等待 Rust API 服務就緒"""
        timeout = timeout or self.config.startup_timeout
        start_time = time.time()
        
        while time.time() - start_time < timeout:
            try:
                response = requests.get(self.config.rust_health_check_endpoint, timeout=2)
                if response.status_code == 200:
                    self.logger.info("✅ Rust API 服務已就緒")
                    return True
            except requests.exceptions.RequestException:
                pass
            
            await asyncio.sleep(2)
            self.logger.info(f"⏳ 等待 Rust API 就緒... ({time.time() - start_time:.0f}s)")
        
        self.logger.error(f"❌ Rust API 服務啟動超時 ({timeout}s)")
        return False
    
    async def _verify_rust_core_functionality(self) -> Dict[str, Any]:
        """驗證 Rust 核心功能"""
        verifications = {
            "api_health": False,
            "database_connection": False,
            "model_loading": False,
            "data_processing": False
        }
        
        try:
            # 檢查 API 健康狀態
            response = requests.get(f"{self.config.rust_health_check_endpoint}", timeout=5)
            if response.status_code == 200:
                verifications["api_health"] = True
                
                # 解析健康檢查響應
                health_data = response.json()
                verifications["database_connection"] = health_data.get("database", False)
                verifications["model_loading"] = health_data.get("models_loaded", False)
                verifications["data_processing"] = health_data.get("data_processing", False)
        
        except Exception as e:
            self.logger.warning(f"⚠️  Rust 核心功能驗證部分失敗: {e}")
        
        verification_score = sum(verifications.values()) / len(verifications)
        
        return {
            "verifications": verifications,
            "score": verification_score,
            "passed": verification_score >= 0.75  # 75% 的功能正常即可
        }
    
    async def _prepare_mlops_track(self) -> Dict[str, Any]:
        """
        準備 MLOps 工作流軌道 (冷備狀態)
        
        不啟動實際進程，只確保環境準備就緒
        """
        
        self.mlops_track.status = TrackStatus.STARTING
        
        try:
            # 1. 檢查 Python 環境和依賴
            python_env_check = await self._check_python_mlops_environment()
            
            # 2. 準備工作目錄
            work_dirs = ["./models", "./outputs", "./logs"]
            for dir_path in work_dirs:
                Path(dir_path).mkdir(parents=True, exist_ok=True)
            
            # 3. 加載 MLOps 配置
            mlops_config = await self._load_mlops_configuration()
            
            # 4. 設置調度器 (但不立即運行)
            scheduler_setup = await self._setup_mlops_scheduler()
            
            # MLOps 軌道進入冷備狀態
            self.mlops_track.status = TrackStatus.READY
            self.mlops_track.metadata = {
                "mode": "cold_standby",
                "next_scheduled_run": "待定時任務觸發",
                "python_env": python_env_check,
                "config_loaded": mlops_config["success"]
            }
            
            return {
                "success": True,
                "status": "cold_standby",  # 冷備狀態
                "python_environment": python_env_check,
                "configuration": mlops_config,
                "scheduler": scheduler_setup,
                "ready_for_trigger": True
            }
            
        except Exception as e:
            self.mlops_track.status = TrackStatus.ERROR
            self.mlops_track.last_error = str(e)
            
            return {
                "success": False,
                "error": str(e)
            }
    
    async def _check_python_mlops_environment(self) -> Dict[str, Any]:
        """檢查 Python MLOps 環境"""
        checks = {
            "agno_workflows_available": False,
            "required_modules": False,
            "mlops_workflow_module": False
        }
        
        try:
            # 檢查 Agno Workflows v2
            from agno_v2_mlops_workflow import AgnoV2MLOpsWorkflow
            checks["agno_workflows_available"] = True
            
            # 檢查必要模塊
            import asyncio, json, logging
            checks["required_modules"] = True
            
            # 檢查 MLOps 工作流模塊
            workflow = AgnoV2MLOpsWorkflow()
            checks["mlops_workflow_module"] = True
            
        except ImportError as e:
            self.logger.warning(f"⚠️  Python 環境檢查發現問題: {e}")
        
        return {
            "checks": checks,
            "all_passed": all(checks.values()),
            "missing_dependencies": [k for k, v in checks.items() if not v]
        }
    
    async def _load_mlops_configuration(self) -> Dict[str, Any]:
        """加載 MLOps 配置"""
        try:
            # 這裡可以從配置文件加載 MLOps 特定配置
            config = {
                "default_symbols": ["BTCUSDT", "ETHUSDT"],
                "training_schedule": self.config.mlops_trigger_schedule,
                "max_concurrent_runs": self.config.mlops_max_concurrent_runs,
                "auto_deployment": False,  # 默認需要手動批准部署
                "hyperparameter_tuning": {
                    "enabled": True,
                    "max_iterations": 10
                }
            }
            
            return {
                "success": True,
                "config": config
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e)
            }
    
    async def _setup_mlops_scheduler(self) -> Dict[str, Any]:
        """設置 MLOps 調度器 (但不立即啟動)"""
        try:
            # 這裡設置調度器的配置，但實際的定時任務由外部 cron 管理
            scheduler_config = {
                "type": "external_cron",
                "schedule": self.config.mlops_trigger_schedule,
                "command": f"python -m {self.config.python_mlops_module}",
                "max_runtime": 3600 * 4,  # 4小時最大運行時間
                "retry_on_failure": True,
                "notification_channels": ["redis", "logs"]
            }
            
            # 可選：如果需要內部調度器，可以在這裡設置
            # self.mlops_scheduler_task = asyncio.create_task(self._internal_mlops_scheduler())
            
            return {
                "success": True,
                "config": scheduler_config,
                "internal_scheduler": False,  # 使用外部 cron
                "next_trigger": "由 cron 作業決定"
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e)
            }
    
    async def _start_system_monitoring(self) -> Dict[str, Any]:
        """啟動系統監控"""
        try:
            # 啟動健康檢查任務
            self.health_check_task = asyncio.create_task(self._continuous_health_monitor())
            
            # 設置信號處理器，用於優雅停機
            if not sys.platform.startswith('win'):
                signal.signal(signal.SIGTERM, self._signal_handler)
                signal.signal(signal.SIGINT, self._signal_handler)
            
            return {
                "success": True,
                "health_monitoring": True,
                "signal_handlers": True,
                "monitoring_interval": self.config.health_check_interval
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e)
            }
    
    async def _perform_final_state_check(self) -> Dict[str, Any]:
        """執行最終狀態檢查"""
        
        final_state = {
            "rust_core": {
                "status": self.rust_track.status.value,
                "ready": self.rust_track.status == TrackStatus.READY,
                "mode": "hot_standby"
            },
            "mlops_workflow": {
                "status": self.mlops_track.status.value,
                "ready": self.mlops_track.status == TrackStatus.READY,
                "mode": "cold_standby"
            },
            "system_integration": {
                "redis_available": self.redis_available,
                "monitoring_active": self.health_check_task is not None,
                "emergency_stop_enabled": self.config.emergency_stop_enabled
            }
        }
        
        # 計算整體就緒分數
        ready_components = sum([
            final_state["rust_core"]["ready"],
            final_state["mlops_workflow"]["ready"],
            final_state["system_integration"]["redis_available"],
            final_state["system_integration"]["monitoring_active"]
        ])
        
        total_components = 4
        readiness_score = ready_components / total_components
        
        return {
            "success": readiness_score >= 0.75,  # 75% 組件就緒即可
            "final_state": final_state,
            "readiness_score": readiness_score,
            "ready_components": ready_components,
            "total_components": total_components,
            "system_operational": readiness_score >= 0.75
        }
    
    async def _continuous_health_monitor(self):
        """持續健康監控"""
        while not self.is_shutting_down:
            try:
                await self._perform_health_check()
                await asyncio.sleep(self.config.health_check_interval)
            except Exception as e:
                self.logger.error(f"❌ 健康監控異常: {e}")
                await asyncio.sleep(10)
    
    async def _perform_health_check(self):
        """執行健康檢查"""
        current_time = time.time()
        
        # 檢查 Rust 核心健康狀態
        if self.rust_track.status == TrackStatus.READY:
            try:
                response = requests.get(self.config.rust_health_check_endpoint, timeout=5)
                if response.status_code == 200:
                    self.rust_track.last_health_check = current_time
                    
                    # 解析健康數據
                    health_data = response.json()
                    self.rust_track.metadata.update({
                        "last_health_data": health_data,
                        "uptime": current_time - self.rust_track.start_time
                    })
                else:
                    self.logger.warning(f"⚠️  Rust 核心健康檢查異常: HTTP {response.status_code}")
                    
            except Exception as e:
                self.logger.error(f"❌ Rust 核心健康檢查失敗: {e}")
                self.rust_track.error_count += 1
                self.rust_track.last_error = str(e)
        
        # 檢查 MLOps 軌道狀態 (冷備狀態正常)
        if self.mlops_track.status == TrackStatus.READY:
            self.mlops_track.last_health_check = current_time
        
        # 發布健康狀態 (如果 Redis 可用)
        if self.redis_available:
            await self._publish_health_status()
    
    async def _publish_health_status(self):
        """發布健康狀態到 Redis"""
        try:
            health_status = {
                "timestamp": time.time(),
                "system_state": self.system_state.value,
                "rust_core": {
                    "status": self.rust_track.status.value,
                    "pid": self.rust_track.pid,
                    "uptime": time.time() - self.rust_track.start_time if self.rust_track.start_time else 0,
                    "error_count": self.rust_track.error_count
                },
                "mlops_workflow": {
                    "status": self.mlops_track.status.value,
                    "mode": "cold_standby",
                    "ready": self.mlops_track.status == TrackStatus.READY
                }
            }
            
            self.redis_client.setex(
                "hft_system:health_status",
                60,  # 60秒過期
                json.dumps(health_status)
            )
            
        except Exception as e:
            self.logger.warning(f"⚠️  發布健康狀態失敗: {e}")
    
    def _signal_handler(self, signum, frame):
        """信號處理器，用於優雅停機"""
        self.logger.info(f"📡 接收到信號: {signum}")
        asyncio.create_task(self.shutdown_system())
    
    async def shutdown_system(self):
        """優雅停機"""
        if self.is_shutting_down:
            return
        
        self.is_shutting_down = True
        self.system_state = SystemState.SHUTTING_DOWN
        
        self.logger.info("🛑 開始系統優雅停機...")
        
        try:
            # 停止健康監控
            if self.health_check_task:
                self.health_check_task.cancel()
            
            # 停止 MLOps 調度器
            if self.mlops_scheduler_task:
                self.mlops_scheduler_task.cancel()
            
            # 停止 Rust 核心進程
            if self.rust_track.pid:
                self.logger.info(f"🦀 停止 Rust 核心進程 (PID: {self.rust_track.pid})")
                try:
                    import os
                    os.kill(self.rust_track.pid, signal.SIGTERM)
                    await asyncio.sleep(5)  # 等待優雅停機
                except Exception as e:
                    self.logger.warning(f"⚠️  停止 Rust 進程時出現問題: {e}")
            
            self.logger.info("✅ 系統優雅停機完成")
            
        except Exception as e:
            self.logger.error(f"❌ 停機過程中出現異常: {e}")
    
    async def _cleanup_on_failure(self):
        """失敗時清理資源"""
        self.logger.info("🧹 清理失敗啟動的資源...")
        
        if self.rust_track.pid:
            try:
                import os
                os.kill(self.rust_track.pid, signal.SIGKILL)
            except Exception:
                pass
        
        if self.health_check_task:
            self.health_check_task.cancel()
    
    def get_system_status(self) -> Dict[str, Any]:
        """獲取當前系統狀態"""
        return {
            "timestamp": time.time(),
            "system_state": self.system_state.value,
            "initialization_duration": time.time() - self.initialization_start_time,
            "tracks": {
                "rust_core": {
                    "status": self.rust_track.status.value,
                    "mode": "hot_standby" if self.rust_track.status == TrackStatus.READY else "offline",
                    "pid": self.rust_track.pid,
                    "uptime": time.time() - self.rust_track.start_time if self.rust_track.start_time else 0,
                    "error_count": self.rust_track.error_count,
                    "last_health_check": self.rust_track.last_health_check
                },
                "mlops_workflow": {
                    "status": self.mlops_track.status.value,
                    "mode": "cold_standby" if self.mlops_track.status == TrackStatus.READY else "offline",
                    "ready_for_trigger": self.mlops_track.status == TrackStatus.READY,
                    "metadata": self.mlops_track.metadata
                }
            },
            "infrastructure": {
                "redis_available": self.redis_available,
                "monitoring_active": self.health_check_task is not None and not self.health_check_task.done()
            }
        }

# ==========================================
# CLI 和測試接口
# ==========================================

async def main():
    """主函數 - 演示雙軌系統初始化"""
    print("🏗️  雙軌運行系統初始化演示")
    print("=" * 80)
    
    # 創建系統管理器
    config = SystemConfig()
    manager = DualTrackSystemManager(config)
    
    try:
        # 執行系統初始化
        result = await manager.initialize_system()
        
        print(f"\\n📊 初始化結果:")
        print(json.dumps(result, indent=2, ensure_ascii=False))
        
        if result["final_state"] == "success":
            print(f"\\n🎉 系統初始化成功！")
            print(f"🦀 Rust 核心：24/7 熱備狀態")
            print(f"🧠 MLOps 工作流：按需冷備狀態")
            
            # 顯示系統狀態
            status = manager.get_system_status()
            print(f"\\n📈 當前系統狀態:")
            print(f"   系統狀態: {status['system_state']}")
            print(f"   Rust 核心: {status['tracks']['rust_core']['mode']}")
            print(f"   MLOps 工作流: {status['tracks']['mlops_workflow']['mode']}")
            
            # 模擬運行一段時間
            print(f"\\n⏳ 系統將運行 10 秒進行演示...")
            await asyncio.sleep(10)
            
        # 優雅停機
        await manager.shutdown_system()
        
    except KeyboardInterrupt:
        print(f"\\n🛑 接收到中斷信號")
        await manager.shutdown_system()
    except Exception as e:
        print(f"\\n❌ 系統運行異常: {e}")
        await manager.shutdown_system()

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    asyncio.run(main())