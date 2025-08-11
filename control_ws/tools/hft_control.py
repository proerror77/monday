from typing import Optional, Dict, Any


class HFTControlTool:
    """gRPC control client wrapper to interact with the Rust HFT engine.

    This is a minimal stub. Wire it to generated stubs when available.
    """

    def __init__(self, host: str, port: int):
        self.host = host
        self.port = port
        self._client = None

    def _ensure(self):
        if self._client is not None:
            return
        try:
            import grpc  # type: ignore
            from protos import hft_control_pb2, hft_control_pb2_grpc  # type: ignore

            channel = grpc.insecure_channel(f"{self.host}:{self.port}")
            self._client = hft_control_pb2_grpc.HFTControlServiceStub(channel)
            self._pb = hft_control_pb2
        except Exception:
            # Stubs not available; require generation via scripts/gen_protos.py
            self._client = None
            self._pb = None

    def ping(self) -> bool:
        self._ensure()
        return self._client is not None

    # Define minimal control surface; replace bodies with actual RPC calls.
    def emergency_stop(self, reason: str) -> bool:
        self._ensure()
        if not self._client or not self._pb:
            return False
        try:
            req = self._pb.EmergencyStopRequest(reason=reason)
        except Exception:
            # Fallback if schema differs
            req = object()
        try:
            resp = self._client.EmergencyStop(req)
            return getattr(resp, "success", True)
        except Exception:
            return False

    def load_model(self, symbol: str, model_path: str, config: Optional[Dict[str, Any]] = None) -> bool:
        self._ensure()
        if not self._client or not self._pb:
            return False
        try:
            model_cfg = None
            if config:
                try:
                    model_cfg = self._pb.ModelConfig(
                        sequence_length=int(config.get("sequence_length", 0)),
                        prediction_horizon=int(config.get("prediction_horizon", 0)),
                        confidence_threshold=float(config.get("confidence_threshold", 0)),
                        enable_risk_adjustment=bool(config.get("enable_risk_adjustment", False)),
                    )
                except Exception:
                    model_cfg = None
            req = self._pb.LoadModelRequest(
                model_path=model_path,
                symbol=symbol,
                config=model_cfg,
                replace_existing=True,
            )
        except Exception:
            req = object()
        try:
            resp = self._client.LoadModel(req)
            return getattr(resp, "success", True)
        except Exception:
            return False
