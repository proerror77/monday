import asyncio
from uuid import uuid4

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import chat_with_agent, check_password

from agents.model_trainer import get_backtest_agent

nest_asyncio.apply()

st.set_page_config(
    page_title="Backtest Agent",
    page_icon="📈",
    layout="wide",
)

def get_user_id_and_session_id():
    """Get user ID and session ID for agent interactions"""
    user_id = "hft_ml_user"
    session_id = str(uuid4())[:8]
    return user_id, session_id

async def main():
    st.title("📈 HFT Backtest Agent")
    st.markdown("**Rigorous backtesting and performance evaluation specialist**")
    
    # Sample commands section
    with st.expander("💡 Sample Commands", expanded=False):
        st.markdown("""
        **Backtesting Framework:**
        - "Run walk-forward validation on the latest model"
        - "Perform out-of-sample testing on 2024 data"
        - "Execute Monte Carlo simulation with 1000 runs"
        - "Backtest with realistic transaction costs and slippage"
        
        **Performance Analysis:**
        - "Calculate Sharpe ratio and maximum drawdown"
        - "Analyze Information Coefficient and Information Ratio"
        - "Generate attribution analysis by market regime"
        - "Compare model performance vs benchmark"
        
        **Risk Metrics:**
        - "Calculate Value at Risk (VaR) and Expected Shortfall"
        - "Perform stress testing under extreme scenarios"
        - "Analyze correlation with market factors"
        - "Generate risk-adjusted performance report"
        
        **Reporting:**
        - "Create comprehensive backtest report with visualizations"
        - "Export results to PDF for stakeholder review"
        - "Generate executive summary of key findings"
        """)
    
    # Get user and session IDs
    user_id, session_id = get_user_id_and_session_id()
    
    # Create the agent
    agent = get_backtest_agent(user_id=user_id, session_id=session_id, debug_mode=True)
    
    # Chat interface
    await chat_with_agent(agent=agent)

if __name__ == "__main__":
    if check_password():
        asyncio.run(main())