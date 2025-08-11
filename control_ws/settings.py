from dataclasses import dataclass
import os


@dataclass
class ControlSettings:
    redis_url: str = os.getenv("REDIS_URL", "redis://localhost:6379/0")
    grpc_host: str = os.getenv("GRPC_HOST", "localhost")
    grpc_port: int = int(os.getenv("GRPC_PORT", "50051"))
    log_level: str = os.getenv("LOG_LEVEL", "INFO")

    # Channels (must match docs/contracts.md)
    ch_ops_alert: str = os.getenv("CH_OPS_ALERT", "ops.alert")
    ch_kill_switch: str = os.getenv("CH_KILL_SWITCH", "kill-switch")
    ch_ml_deploy: str = os.getenv("CH_ML_DEPLOY", "ml.deploy")
    ch_ml_reject: str = os.getenv("CH_ML_REJECT", "ml.reject")
    ch_model_status: str = os.getenv("CH_MODEL_STATUS", "model.status")
    ch_system_metrics: str = os.getenv("CH_SYSTEM_METRICS", "system.metrics")


settings = ControlSettings()

