from os import getenv

from agno.playground import Playground

from agents.model_trainer import get_model_trainer, get_hyperopt_agent, get_backtest_agent
from agents.data_engineer import get_data_engineer
from workspace.dev_resources import dev_fastapi

######################################################
## Router for the Playground Interface
######################################################

model_trainer_agent = get_model_trainer(debug_mode=True)
hyperopt_agent = get_hyperopt_agent(debug_mode=True)
backtest_agent = get_backtest_agent(debug_mode=True)
data_engineer_agent = get_data_engineer(debug_mode=True)

# Create a playground instance
# Create a playground instance for HFT ML agents
playground = Playground(
    agents=[
        model_trainer_agent, 
        hyperopt_agent, 
        backtest_agent, 
        data_engineer_agent
    ], 
    teams=[]
)

# Note: Playground endpoint configuration - serve method may vary by Agno version
# if getenv("RUNTIME_ENV") == "dev":
#     playground.serve(f"http://localhost:{dev_fastapi.host_port}")

playground_router = playground.get_async_router()
