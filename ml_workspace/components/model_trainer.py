from typing import Dict, Any


class TLOBModelTrainer:
    """Minimal trainer stub returning a fake model path and metrics."""

    def train(self, features: Dict[str, Any]) -> Dict[str, Any]:
        return {
            "model_path": "models/tlob/latest.pt",
            "metrics": {"accuracy": 0.56, "ic": 0.031, "ir": 1.25, "max_drawdown": 0.04},
        }

