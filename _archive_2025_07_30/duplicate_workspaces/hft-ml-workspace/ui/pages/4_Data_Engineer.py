import asyncio
from uuid import uuid4

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import chat_with_agent, check_password

from agents.data_engineer import get_data_engineer

nest_asyncio.apply()

st.set_page_config(
    page_title="Data Engineer",
    page_icon="💾",
    layout="wide",
)

def get_user_id_and_session_id():
    """Get user ID and session ID for agent interactions"""
    user_id = "hft_ml_user"
    session_id = str(uuid4())[:8]
    return user_id, session_id

async def main():
    st.title("💾 HFT Data Engineer")
    st.markdown("**Financial data processing and feature engineering specialist**")
    
    # Sample commands section
    with st.expander("💡 Sample Commands", expanded=False):
        st.markdown("""
        **Data Pipeline Management:**
        - "Check data quality for the last 24 hours"
        - "Validate TLOB data completeness and consistency"
        - "Monitor WebSocket feed health and latency"
        - "Detect and handle missing data gaps"
        
        **Feature Engineering:**
        - "Extract microstructure features from order book"
        - "Calculate VWAP, RSI, and momentum indicators"
        - "Generate rolling window features with different horizons"
        - "Create market regime classification features"
        
        **Data Storage Optimization:**
        - "Optimize ClickHouse table partitioning strategy"
        - "Review and update data retention policies"
        - "Monitor query performance and create indexes"
        - "Implement data compression for historical storage"
        
        **Pipeline Monitoring:**
        - "Show ETL pipeline status and health metrics"
        - "Analyze data processing latency and throughput"
        - "Generate data quality report for model training"
        - "Set up alerts for data anomalies"
        """)
    
    # Get user and session IDs
    user_id, session_id = get_user_id_and_session_id()
    
    # Create the agent
    agent = get_data_engineer(user_id=user_id, session_id=session_id, debug_mode=True)
    
    # Chat interface
    await chat_with_agent(agent=agent)

if __name__ == "__main__":
    if check_password():
        asyncio.run(main())