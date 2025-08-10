import asyncio

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import check_password

from ui.css import CUSTOM_CSS
from ui.utils import about_agno, footer

nest_asyncio.apply()

st.set_page_config(
    page_title="HFT ML Workspace",
    page_icon="🤖",
    layout="wide",
)
st.markdown(CUSTOM_CSS, unsafe_allow_html=True)


async def header():
    st.markdown("<h1 class='heading'>HFT ML Workspace</h1>", unsafe_allow_html=True)
    st.markdown(
        "<p class='subheading'>Machine Learning & Model Training Center for High-Frequency Trading</p>",
        unsafe_allow_html=True,
    )


async def body():
    st.markdown("### 🤖 Available ML Agents")

    col1, col2 = st.columns(2)
    with col1:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>🧠 Model Trainer</h3>
            <p>Deep learning specialist for HFT model development, architecture design, and training workflows.</p>
            <p>Expertise in LSTM/Transformer architectures and financial time series modeling.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch Model Trainer", key="model_trainer_button"):
            st.switch_page("pages/1_Model_Trainer.py")

        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>📈 Backtest Agent</h3>
            <p>Rigorous backtesting and performance evaluation specialist.</p>
            <p>Walk-forward analysis, transaction cost modeling, and risk metrics calculation.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch Backtest Agent", key="backtest_button"):
            st.switch_page("pages/3_Backtest_Agent.py")

    with col2:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>⚙️ Hyperopt Agent</h3>
            <p>Automated hyperparameter optimization using Bayesian methods.</p>
            <p>Multi-objective optimization for IC, IR, and Sharpe ratio maximization.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch Hyperopt Agent", key="hyperopt_button"):
            st.switch_page("pages/2_Hyperopt_Agent.py")

        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>💾 Data Engineer</h3>
            <p>Financial data processing and feature engineering specialist.</p>
            <p>Real-time feature extraction from order book data and ETL pipeline optimization.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch Data Engineer", key="data_engineer_button"):
            st.switch_page("pages/4_Data_Engineer.py")

    st.markdown("### 📊 System Status")
    
    col1, col2, col3 = st.columns(3)
    
    with col1:
        st.metric(
            label="Training Jobs",
            value="2 Active",
            delta="+1 since yesterday"
        )
    
    with col2:
        st.metric(
            label="Model Accuracy",
            value="87.3%",
            delta="+2.1% improvement"
        )
    
    with col3:
        st.metric(
            label="Data Quality",
            value="98.7%",
            delta="-0.3% from target"
        )

    st.markdown("### 🔄 Training Workflows")
    
    col1, col2 = st.columns(2)
    with col1:
        if st.button("🔄 Start Training Pipeline", key="start_training", use_container_width=True):
            st.info("Training pipeline initiated. Check logs for progress.")
        
        if st.button("🛑 Stop All Training", key="stop_training", use_container_width=True):
            st.warning("Training jobs stopped. Models will be saved at current checkpoint.")
    
    with col2:
        if st.button("🔍 Hyperparameter Optimization", key="start_hyperopt", use_container_width=True):
            st.info("Hyperparameter optimization started with Bayesian search.")
        
        if st.button("🧪 Run Backtest", key="run_backtest", use_container_width=True):
            st.info("Backtesting initiated on latest model checkpoints.")


async def main():
    await header()
    await body()
    await footer()
    await about_agno()


if __name__ == "__main__":
    if check_password():
        asyncio.run(main())