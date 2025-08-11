#!/usr/bin/env python3
import asyncio
import argparse

from control_ws.workflows.master_orchestration import MasterOrchestrationWorkflow


async def main():
    parser = argparse.ArgumentParser(description="Control plane CLI")
    parser.add_argument("--once", action="store_true", help="Run one cycle and exit")
    args = parser.parse_args()

    wf = MasterOrchestrationWorkflow()
    if args.once:
        await wf.run_once()
        return
    await wf.run_forever()


if __name__ == "__main__":
    asyncio.run(main())
