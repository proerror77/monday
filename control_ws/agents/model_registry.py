from agno import Agent
from typing import Dict, Any


class ModelRegistryAgent(Agent):
    def __init__(self, name: str = "ModelRegistryAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def choose_candidate(self, metrics: Dict[str, Any]) -> Dict[str, Any]:
        return {"approved": True, "metrics": metrics}

