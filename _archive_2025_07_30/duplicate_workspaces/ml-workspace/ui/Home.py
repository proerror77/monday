import asyncio

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import check_password

from ui.css import CUSTOM_CSS
from ui.utils import about_agno, footer

nest_asyncio.apply()

st.set_page_config(
    page_title="HFT ML Center",
    page_icon="🧠",
    layout="wide",
)
st.markdown(CUSTOM_CSS, unsafe_allow_html=True)


async def header():
    st.markdown("<h1 class='heading'>🧠 HFT Machine Learning Center</h1>", unsafe_allow_html=True)
    st.markdown(
        "<p class='subheading'>Advanced model training, optimization, and deployment for high-frequency trading.</p>",
        unsafe_allow_html=True,
    )


async def training_status():
    """訓練狀態監控"""
    st.markdown("### 🏋️ Training Status")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        st.metric("🎯 Latest IC", "0.031", "+0.005")
    with col2:
        st.metric("📊 Latest IR", "1.28", "+0.15")
    with col3:
        st.metric("📉 MDD", "4.1%", "-0.8%")
    with col4:
        st.metric("⏱️ Training Time", "1.8h", "Last run")


async def ml_agents():
    """機器學習代理們"""
    st.markdown("### 🤖 ML Training Agents")

    col1, col2, col3 = st.columns(3)
    
    with col1:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f0f8ff;">
            <h3>🏋️ Model Trainer</h3>
            <p>Deep learning model training with PyTorch Lightning and advanced feature engineering.</p>
            <p><strong>Status:</strong> Ready for training</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("🚀 Launch Model Trainer", key="model_trainer_button", type="primary"):
            st.switch_page("pages/1_Model_Trainer.py")

    with col2:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f0f8ff;">
            <h3>⚙️ Hyperopt Agent</h3>
            <p>Intelligent hyperparameter optimization using Bayesian search and TPE algorithms.</p>
            <p><strong>Status:</strong> Optimization ready</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("📈 Launch Hyperopt Agent", key="hyperopt_agent_button"):
            st.switch_page("pages/2_Hyperopt_Agent.py")
    
    with col3:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f0f8ff;">
            <h3>📊 Backtest Agent</h3>
            <p>Comprehensive model backtesting with risk analysis and performance evaluation.</p>
            <p><strong>Status:</strong> Backtest ready</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("🎯 Launch Backtest Agent", key="backtest_agent_button"):
            st.switch_page("pages/3_Backtest_Agent.py")


async def training_workflow():
    """訓練工作流程"""
    st.markdown("### 🔄 Training Workflow")
    
    st.markdown(
        """
    <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #fff8e1;">
        <h3>🕐 Daily Training Pipeline</h3>
        <p>Automated training pipeline that runs daily at 05:00 UTC+8 with the following steps:</p>
        <ul>
            <li><strong>Data Loading:</strong> Extract T-day market data from ClickHouse</li>
            <li><strong>Feature Engineering:</strong> Generate 120-dimensional feature vectors</li>
            <li><strong>Hyperparameter Optimization:</strong> Up to 60 trials with early stopping</li>
            <li><strong>Model Training:</strong> PyTorch Lightning with GPU acceleration</li>
            <li><strong>Backtesting:</strong> Out-of-sample performance evaluation</li>
            <li><strong>Deployment:</strong> Publish to ml.deploy channel if criteria met</li>
        </ul>
        <p><strong>Target Criteria:</strong> IC ≥ 0.03, IR ≥ 1.2, MDD ≤ 5%, Sharpe ≥ 1.5</p>
    </div>
    """,
        unsafe_allow_html=True,
    )
    
    col1, col2, col3 = st.columns(3)
    with col1:
        if st.button("▶️ Start Training Now", type="primary"):
            st.success("Training workflow started - this may take up to 2 hours...")
    with col2:
        if st.button("📊 View Training History"):
            st.info("Loading training history dashboard...")
    with col3:
        if st.button("⚙️ Configure Pipeline"):
            st.warning("Opening pipeline configuration...")


async def model_deployment():
    """模型部署狀態"""
    st.markdown("### 🚀 Model Deployment")
    
    col1, col2 = st.columns(2)
    
    with col1:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f0fff0;">
            <h3>📦 Current Production Model</h3>
            <p><strong>Version:</strong> 20250726-ic031-ir128</p>
            <p><strong>Performance:</strong> IC=0.031, IR=1.28, MDD=4.1%</p>
            <p><strong>Deployed:</strong> 2025-07-26 05:30:00</p>
            <p><strong>Status:</strong> 🟢 Active in Rust Core</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
    
    with col2:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #fff0f5;">
            <h3>🔄 Model Repository</h3>
            <p><strong>Total Models:</strong> 42 trained models</p>
            <p><strong>Success Rate:</strong> 73% meet criteria</p>
            <p><strong>Storage:</strong> Supabase + TorchScript</p>
            <p><strong>Avg Training Time:</strong> 1.6 hours</p>
        </div>
        """,
            unsafe_allow_html=True,
        )


async def quick_actions():
    """快速機器學習操作"""
    st.markdown("### ⚡ Quick ML Operations")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        if st.button("🔄 Retrain Model"):
            st.info("Starting emergency model retraining...")
    
    with col2:
        if st.button("📊 Performance Report"):
            st.success("Generating comprehensive ML performance report...")
    
    with col3:
        if st.button("🔍 Model Analysis"):
            st.warning("Opening model interpretation dashboard...")
    
    with col4:
        if st.button("⚠️ Rollback Model"):
            st.error("Rolling back to previous model version...")


async def main():
    await header()
    await training_status()
    st.divider()
    await ml_agents()
    st.divider()  
    await training_workflow()
    st.divider()
    await model_deployment()
    st.divider()
    await quick_actions()
    await footer()
    await about_agno()


if __name__ == "__main__":
    if check_password():
        asyncio.run(main())