import asyncio
from uuid import uuid4

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import chat_with_agent, check_password

from agents.model_trainer import get_hyperopt_agent

nest_asyncio.apply()

st.set_page_config(
    page_title="Hyperopt Agent",
    page_icon="⚙️",
    layout="wide",
)

def get_user_id_and_session_id():
    """Get user ID and session ID for agent interactions"""
    user_id = "hft_ml_user"
    session_id = str(uuid4())[:8]
    return user_id, session_id

async def main():
    st.title("⚙️ HFT Hyperopt Agent")
    st.markdown("**Automated hyperparameter optimization using Bayesian methods**")
    
    # Sample commands section
    with st.expander("💡 Sample Commands", expanded=False):
        st.markdown("""
        **Hyperparameter Optimization:**
        - "Start Bayesian optimization for LSTM learning rate and hidden size"
        - "Optimize batch size and dropout rate for best IC/IR ratio"
        - "Run multi-objective optimization for Sharpe vs maximum drawdown"
        - "Search optimal sequence length for time series prediction"
        
        **Optimization Strategies:**
        - "Use TPE (Tree-structured Parzen Estimator) for search"
        - "Set up early stopping for unpromising trials"
        - "Configure 100 trials with 2-hour time limit"
        
        **Results Analysis:**
        - "Show best hyperparameters found so far"
        - "Plot optimization history and convergence"
        - "Compare top 10 configurations by performance"
        - "Generate hyperparameter importance analysis"
        """)
    
    # Get user and session IDs
    user_id, session_id = get_user_id_and_session_id()
    
    # Create the agent
    agent = get_hyperopt_agent(user_id=user_id, session_id=session_id, debug_mode=True)
    
    # Chat interface
    await chat_with_agent(agent=agent)

if __name__ == "__main__":
    if check_password():
        asyncio.run(main())