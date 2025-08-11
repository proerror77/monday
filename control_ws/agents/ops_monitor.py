from agno import Agent
from typing import Dict, Any


class OpsMonitorAgent(Agent):
    def __init__(self, name: str = "OpsMonitorAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def handle_alert(self, alert: Dict[str, Any]) -> Dict[str, Any]:
        return {"ack": True, "alert": alert}

