# Ploy Read-Only Heartbeat

Check only these canonical endpoints. Never mutate deployments, orders, governance, or files.

- `GET $PLOY_API_BASE/health`
- `GET $PLOY_API_BASE/api/system/status`
- `GET $PLOY_API_BASE/api/system/alerts`
- `GET $PLOY_API_BASE/api/deployments`
- `GET $PLOY_API_BASE/api/trading/state`

Use the configured read-only authentication header. If every request succeeds and no alert is critical, reply `HEARTBEAT_OK`. Otherwise report the failing endpoint or active critical alert; do not attempt remediation.
