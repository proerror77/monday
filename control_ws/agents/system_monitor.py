from agno import Agent
from typing import Dict, Any


class SystemMonitorAgent(Agent):
    def __init__(self, name: str = "SystemMonitorAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def heartbeat(self, ctx: Dict[str, Any]) -> Dict[str, Any]:
        return {"alive": True, "ctx": ctx}

