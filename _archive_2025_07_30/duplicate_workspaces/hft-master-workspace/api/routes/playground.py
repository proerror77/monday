from os import getenv

from agno.playground import Playground

from agents.hft_controller import get_hft_controller
from agents.system_monitor import get_system_monitor
from workspace.dev_resources import dev_fastapi

######################################################
## Router for the Playground Interface
######################################################

hft_controller_agent = get_hft_controller(debug_mode=True)
system_monitor_agent = get_system_monitor(debug_mode=True)

# Create a playground instance for HFT agents
playground = Playground(agents=[hft_controller_agent, system_monitor_agent], teams=[])

# Note: Playground endpoint configuration - serve method may vary by Agno version
# if getenv("RUNTIME_ENV") == "dev":
#     playground.serve(f"http://localhost:{dev_fastapi.host_port}")

playground_router = playground.get_async_router()