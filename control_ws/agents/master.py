from typing import Dict, Any
from agno import Agent


class MasterTradingAgent(Agent):
    """Control-plane master agent responsible for orchestration and decisions.

    Keep this agent lightweight. It should route intents to specialized agents
    and use tools to control the Rust execution plane via gRPC/Redis.
    """

    def __init__(self, name: str = "MasterTradingAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def classify_intent(self, event: Dict[str, Any]) -> str:
        t = event.get("event", "").lower()
        if t in ("latency_breach", "drawdown_breach", "emergency_stop"):
            return "ops"
        if t in ("model_ready", "ml.deploy"):
            return "ml"
        return "other"

    def act(self, intent: str, event: Dict[str, Any]) -> Dict[str, Any]:
        return {"handled": True, "intent": intent, "event": event}

