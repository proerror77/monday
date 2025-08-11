from agno import Agent
from typing import Dict, Any


class RiskGuardAgent(Agent):
    def __init__(self, name: str = "RiskGuardAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def evaluate(self, ctx: Dict[str, Any]) -> Dict[str, Any]:
        return {"ok": True, "ctx": ctx}

