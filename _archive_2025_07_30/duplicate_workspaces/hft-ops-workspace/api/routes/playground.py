from os import getenv

from agno.playground import Playground

from agents.alert_manager import get_alert_manager
from agents.latency_guard import get_latency_guard
from agents.dd_guard import get_dd_guard
from agents.system_monitor import get_system_monitor
from workspace.dev_resources import dev_fastapi

######################################################
## Router for the Playground Interface
######################################################

alert_manager_agent = get_alert_manager(debug_mode=True)
latency_guard_agent = get_latency_guard(debug_mode=True)
dd_guard_agent = get_dd_guard(debug_mode=True)
system_monitor_agent = get_system_monitor(debug_mode=True)

# Create a playground instance for HFT Ops agents
playground = Playground(
    agents=[
        alert_manager_agent, 
        latency_guard_agent, 
        dd_guard_agent, 
        system_monitor_agent
    ], 
    teams=[]
)

# Note: Playground endpoint configuration - serve method may vary by Agno version
# if getenv("RUNTIME_ENV") == "dev":
#     playground.serve(f"http://localhost:{dev_fastapi.host_port}")

playground_router = playground.get_async_router()
