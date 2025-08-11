"""
Provisional minimal Python stub for hft_control gRPC service client.
Replace with generated code from grpcio-tools in production.
"""

class _Response:
    def __init__(self, success: bool = True):
        self.success = success


class HFTControlServiceStub:
    def __init__(self, channel=None):  # channel unused in provisional
        pass

    def EmergencyStop(self, request):
        return _Response(True)

    def LoadModel(self, request):
        return _Response(True)

