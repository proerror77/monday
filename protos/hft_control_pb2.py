"""
Provisional minimal Python stub for hft_control.proto messages.
Replace with generated code from grpcio-tools in production.
"""

from dataclasses import dataclass
from typing import Optional


@dataclass
class EmergencyStopRequest:
    reason: str = ""


@dataclass
class ModelConfig:
    sequence_length: int = 0
    prediction_horizon: int = 0
    confidence_threshold: float = 0.0
    enable_risk_adjustment: bool = False


@dataclass
class LoadModelRequest:
    model_path: str = ""
    symbol: str = ""
    config: Optional[ModelConfig] = None
    replace_existing: bool = True

