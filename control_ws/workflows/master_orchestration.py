import asyncio
from datetime import datetime, timedelta
from typing import Dict, Any

from agno import Workflow, Step

from control_ws.agents.master import MasterTradingAgent
from control_ws.agents.ops_monitor import OpsMonitorAgent
from control_ws.agents.risk_guard import RiskGuardAgent
from control_ws.agents.model_registry import ModelRegistryAgent
from control_ws.agents.ml_pipeline import MLPipelineAgent
from control_ws.tools.redis_bus import RedisBusTool
from control_ws.tools.hft_control import HFTControlTool
from control_ws.settings import settings
from control_ws.agents.latency_guard import LatencyGuardAgent
from control_ws.agents.dd_guard import DrawdownGuardAgent
from control_ws.agents.system_monitor import SystemMonitorAgent


class MasterOrchestrationWorkflow(Workflow):
    def __init__(self):
        super().__init__(
            name="Master_Orchestration",
            description="Control plane orchestration of trading system",
        )

        # Tools
        self.bus = RedisBusTool(settings.redis_url)
        self.hft = HFTControlTool(settings.grpc_host, settings.grpc_port)

        # Agents
        self.master = MasterTradingAgent(tools=[self.bus, self.hft])
        self.ops = OpsMonitorAgent(tools=[self.bus, self.hft])
        self.risk = RiskGuardAgent(tools=[self.bus])
        self.registry = ModelRegistryAgent()
        self.ml = MLPipelineAgent(tools=[self.bus, self.hft])
        self.latency_guard = LatencyGuardAgent()
        self.drawdown_guard = DrawdownGuardAgent()
        self.system_monitor = SystemMonitorAgent()
        self._pending_rollouts: dict[str, dict] = {}

        # Steps (simplified)
        self.add_step(
            Step(
                name="observe",
                description="Observe control-plane events",
                agent=self.master,
                timeout=30,
                retries=0,
            )
        )
        self.add_step(
            Step(
                name="route_and_act",
                description="Route intent and take action",
                agent=self.master,
                depends_on=["observe"],
                timeout=30,
                retries=0,
            )
        )

    async def run_once(self) -> Dict[str, Any]:
        # Minimal one-shot loop stub. Replace with Redis subscription loop.
        event = {"event": "heartbeat", "timestamp": datetime.now().isoformat()}
        intent = self.master.classify_intent(event)
        result = self.master.act(intent, event)
        return {"success": True, "result": result}

    async def run_forever(self) -> None:
        channels = [
            settings.ch_ops_alert,
            settings.ch_kill_switch,
            settings.ch_ml_deploy,
            settings.ch_ml_reject,
            settings.ch_system_metrics,
        ]
        ps = self.bus.subscribe(channels)
        if ps is None:
            # Fallback to periodic heartbeat if Redis not available
            while True:
                await self.run_once()
                await asyncio.sleep(1.0)

        # Try load rollout policy from config (optional)
        policy = {
            "ic": 0.03,
            "ir": 1.2,
            "max_drawdown": 0.05,
            "health_timeout_seconds": 60,
            "latency_p99_us_max": 25.0,
            "error_rate_max": 0.01,
            "pnl_drawdown_max": 0.05,
        }
        try:
            import yaml  # type: ignore
            from pathlib import Path

            cfg = Path("config/control_plane.yaml")
            if cfg.exists():
                data = yaml.safe_load(cfg.read_text()) or {}
                rp = (data.get("rollout_policy") or {})
                p = rp.get("promote_threshold") or {}
                policy.update(
                    {
                        "ic": float(p.get("ic", policy["ic"])),
                        "ir": float(p.get("ir", policy["ir"])),
                        "max_drawdown": float(p.get("max_drawdown", policy["max_drawdown"])),
                        "health_timeout_seconds": int(rp.get("health_timeout_seconds", policy["health_timeout_seconds"])),
                        "latency_p99_us_max": float(rp.get("latency_p99_us_max", policy["latency_p99_us_max"])),
                        "error_rate_max": float(rp.get("error_rate_max", policy["error_rate_max"])),
                        "pnl_drawdown_max": float(rp.get("pnl_drawdown_max", policy["pnl_drawdown_max"])),
                    }
                )
        except Exception:
            pass

        while True:
            msg = self.bus.next_message(ps, timeout=1.0)
            if not msg:
                await asyncio.sleep(0.1)
                continue

            if msg.get("type") != "message":
                continue

            try:
                import json

                payload = json.loads(msg.get("data") or "{}")
            except Exception:
                payload = {"raw": msg.get("data")}

            channel = msg.get("channel")
            event_type = (payload or {}).get("event") or ""

            if channel == settings.ch_kill_switch or event_type == "emergency_stop":
                # Execute emergency stop
                self.hft.emergency_stop(reason=str(payload))
                continue

            if channel == settings.ch_ops_alert:
                # Route to ops monitor / risk guard as needed
                severity = (payload or {}).get("severity", "").upper()
                et = (payload or {}).get("event", "")
                sym = (payload or {}).get("symbol") or (payload or {}).get("scope")

                if et == "latency_breach" or severity == "CRITICAL":
                    # Let latency guard decide and potentially trigger stop
                    try:
                        self.latency_guard  # noqa: B018
                        # If guard says critical, stop
                        if severity == "CRITICAL":
                            self.hft.emergency_stop(reason=str(payload))
                            # rollback pending rollout for the symbol if any
                            if sym and sym in self._pending_rollouts:
                                ctx = self._pending_rollouts.pop(sym, None) or {}
                                self.bus.publish(
                                    settings.ch_model_status,
                                    {
                                        "event": "model_status",
                                        "symbol": sym,
                                        "status": "rolled_back",
                                        "model_path": (ctx or {}).get("model_path"),
                                        "metrics": (ctx or {}).get("metrics", {}),
                                        "timestamp": datetime.now().isoformat(),
                                        "version": 1,
                                    },
                                )
                    except Exception:
                        self.hft.emergency_stop(reason=str(payload))
                else:
                    # Otherwise just acknowledge via ops agent
                    intent = self.master.classify_intent(payload)
                    self.master.act(intent, payload)
                continue

            # Evaluate pending rollouts on every loop (timeout promote)
            now = datetime.now()
            to_promote = []
            to_drop = []
            for sym, ctx in list(self._pending_rollouts.items()):
                if now >= ctx.get("deadline", now):
                    to_promote.append(sym)
            for sym in to_promote:
                ctx = self._pending_rollouts.pop(sym, None) or {}
                self.bus.publish(
                    settings.ch_model_status,
                    {
                        "event": "model_status",
                        "symbol": sym,
                        "status": "promoted",
                        "model_path": ctx.get("model_path"),
                        "metrics": ctx.get("metrics", {}),
                        "timestamp": datetime.now().isoformat(),
                        "version": 1,
                    },
                )

            if channel == settings.ch_ml_deploy or event_type in ("model_ready", "ml.deploy"):
                # Evaluate and rollout model
                metrics = (payload or {}).get("metrics") or {}
                ic = float(metrics.get("ic", 0))
                ir = float(metrics.get("ir", 0))
                max_dd = float(metrics.get("max_drawdown", 1))
                symbol = (payload or {}).get("symbol") or "UNKNOWN"
                model_path = (payload or {}).get("model_path") or ""

                ready = ic > policy["ic"] and ir > policy["ir"] and max_dd < policy["max_drawdown"]
                if not ready:
                    # publish reject
                    self.bus.publish(
                        settings.ch_ml_reject,
                        {
                            "event": "model_reject",
                            "symbol": symbol,
                            "reason": "threshold_not_met",
                            "metrics": metrics,
                            "timestamp": datetime.now().isoformat(),
                            "version": 1,
                        },
                    )
                    continue

                # Canary stage: load model via gRPC (logical canary)
                ok = self.hft.load_model(symbol=symbol, model_path=model_path, config={"canary": True})
                status = "canary" if ok else "failed"
                self.bus.publish(
                    settings.ch_model_status,
                    {
                        "event": "model_status",
                        "symbol": symbol,
                        "status": status,
                        "model_path": model_path,
                        "metrics": metrics,
                        "timestamp": datetime.now().isoformat(),
                        "version": 1,
                    },
                )
                if ok:
                    # Track for promotion after health window
                    self._pending_rollouts[symbol] = {
                        "model_path": model_path,
                        "metrics": metrics,
                        "deadline": datetime.now() + timedelta(seconds=policy["health_timeout_seconds"]),
                    }
                continue

            if channel == settings.ch_system_metrics:
                # Evaluate health for canary symbols
                # Expect payload: { scope/symbol, values: { latency_p99_us, error_rate, pnl_drawdown } }
                scope = (payload or {}).get("symbol") or (payload or {}).get("scope")
                if scope and scope in self._pending_rollouts:
                    vals = (payload or {}).get("values") or {}
                    lat_p99 = float(vals.get("latency_p99_us", vals.get("p99_us", 0)))
                    err_rate = float(vals.get("error_rate", 0))
                    pnl_dd = float(vals.get("pnl_drawdown", vals.get("max_drawdown", 0)))
                    # Early rollback if thresholds breached
                    if (
                        lat_p99 > policy["latency_p99_us_max"]
                        or err_rate > policy["error_rate_max"]
                        or pnl_dd > policy["pnl_drawdown_max"]
                    ):
                        ctx = self._pending_rollouts.pop(scope, None) or {}
                        self.bus.publish(
                            settings.ch_model_status,
                            {
                                "event": "model_status",
                                "symbol": scope,
                                "status": "rolled_back",
                                "model_path": (ctx or {}).get("model_path"),
                                "metrics": (ctx or {}).get("metrics", {}),
                                "timestamp": datetime.now().isoformat(),
                                "version": 1,
                            },
                        )
                    # Early promote if strictly within thresholds and optional additional signals
                    elif (
                        lat_p99 <= policy["latency_p99_us_max"]
                        and err_rate <= policy["error_rate_max"]
                        and pnl_dd <= policy["pnl_drawdown_max"]
                    ):
                        # Promote immediately
                        ctx = self._pending_rollouts.pop(scope, None) or {}
                        self.bus.publish(
                            settings.ch_model_status,
                            {
                                "event": "model_status",
                                "symbol": scope,
                                "status": "promoted",
                                "model_path": (ctx or {}).get("model_path"),
                                "metrics": (ctx or {}).get("metrics", {}),
                                "timestamp": datetime.now().isoformat(),
                                "version": 1,
                            },
                        )
                continue

            # Default: classify and act
            intent = self.master.classify_intent(payload)
            self.master.act(intent, payload)


async def main():
    wf = MasterOrchestrationWorkflow()
    await wf.run_forever()


if __name__ == "__main__":
    asyncio.run(main())
