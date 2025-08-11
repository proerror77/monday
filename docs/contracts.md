# Control Plane Contracts

- Redis channels
  - `ops.alert`: { event, severity, scope, payload, timestamp, version }
  - `kill-switch`: { event: "emergency_stop", reason, scope, timestamp, version }
  - `ml.deploy`: { event: "model_ready", symbol, model_path, metrics, timestamp, version }
  - `ml.reject`: { event: "model_reject", symbol, reason, metrics, timestamp, version }
  - `model.status`: { event: "model_status", symbol, status, model_path, metrics, timestamp, version }
  - `system.metrics`: { event: "metrics", scope, values, window, timestamp, version }

- gRPC endpoints (per protos/hft_control.proto)
  - StartTrading/StopTrading/EmergencyStop
  - LoadModel/UnloadModel/GetModelStatus
  - GetSystemStatus/SubscribeAlerts/TriggerAlert/GetMetrics

- Versioning
  - All events must include `version` for forward/backward compatibility.

- Rollout statuses (model.status)
  - `canary`: model loaded for canary stage
  - `promoted`: canary passed health window and promoted
  - `rolled_back`: canary failed or critical alert; rolled back
  - `failed`: load failed or control-plane error

- Health window
  - Duration: `rollout_policy.health_timeout_seconds` (config/control_plane.yaml)
  - Critical ops alerts during window trigger rollback
  - If no critical alerts, promotion occurs automatically at window end
