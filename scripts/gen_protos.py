#!/usr/bin/env python3
"""
Generate Python gRPC stubs from protos/hft_control.proto into protos/.
Requires: grpcio-tools
Usage: python scripts/gen_protos.py
"""

import sys
from pathlib import Path


def main() -> int:
    try:
        from grpc_tools import protoc  # type: ignore
    except Exception as e:
        print("grpcio-tools not installed: pip install grpcio-tools", file=sys.stderr)
        return 1

    repo = Path(__file__).resolve().parents[1]
    proto_dir = repo / "protos"
    proto = proto_dir / "hft_control.proto"
    if not proto.exists():
        print(f"Proto not found: {proto}", file=sys.stderr)
        return 1

    args = [
        "protoc",
        f"-I{proto_dir}",
        f"--python_out={proto_dir}",
        f"--grpc_python_out={proto_dir}",
        str(proto),
    ]
    code = protoc.main(args)
    if code != 0:
        print(f"protoc failed with exit code {code}", file=sys.stderr)
    else:
        print("Generated stubs:")
        print(" - protos/hft_control_pb2.py")
        print(" - protos/hft_control_pb2_grpc.py")
    return code


if __name__ == "__main__":
    raise SystemExit(main())

