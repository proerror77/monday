from agno import Agent
from typing import Dict, Any


class DrawdownGuardAgent(Agent):
    def __init__(self, name: str = "DrawdownGuardAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def check(self, stats: Dict[str, Any]) -> Dict[str, Any]:
        return {"ok": True, "stats": stats}

