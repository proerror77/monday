from agno import Agent
from typing import Dict, Any
from enum import Enum


class LatencyLevel(Enum):
    NORMAL = "normal"
    ELEVATED = "elevated"
    HIGH = "high"
    CRITICAL = "critical"


class LatencyGuardAgent(Agent):
    def __init__(self, name: str = "LatencyGuardAgent", tools: list | None = None):
        super().__init__(name=name, tools=tools or [])

    def assess(self, metrics: Dict[str, Any]) -> LatencyLevel:
        p99 = float(metrics.get("p99_us", 0))
        if p99 > 25:
            return LatencyLevel.CRITICAL
        if p99 > 20:
            return LatencyLevel.HIGH
        if p99 > 10:
            return LatencyLevel.ELEVATED
        return LatencyLevel.NORMAL

