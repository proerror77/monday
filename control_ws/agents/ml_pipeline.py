from agno import Agent
from typing import Dict, Any


class MLPipelineAgent(Agent):
    def __init__(self, name: str = "MLPipelineAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def rollout(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        return {"rolled_out": True, "payload": payload}

