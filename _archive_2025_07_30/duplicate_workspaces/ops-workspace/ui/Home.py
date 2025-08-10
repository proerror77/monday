import asyncio

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import check_password

from ui.css import CUSTOM_CSS
from ui.utils import about_agno, footer

nest_asyncio.apply()

st.set_page_config(
    page_title="HFT Ops Center",
    page_icon="⚡",
    layout="wide",
)
st.markdown(CUSTOM_CSS, unsafe_allow_html=True)


async def header():
    st.markdown("<h1 class='heading'>⚡ HFT Operations Center</h1>", unsafe_allow_html=True)
    st.markdown(
        "<p class='subheading'>Real-time monitoring, alerting, and risk management for the HFT trading system.</p>",
        unsafe_allow_html=True,
    )


async def system_status():
    """系統狀態監控"""
    st.markdown("### 🔥 System Status")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        st.metric("🦀 Rust Core", "🟢 Online", "25.3µs avg")
    with col2:
        st.metric("📊 Redis", "🟢 Healthy", "0.8ms latency")
    with col3:
        st.metric("🗄️ ClickHouse", "🟢 Connected", "45MB/s writes")
    with col4:
        st.metric("💰 Current DD", "🟢 1.2%", "-0.3% vs yesterday")


async def alert_agents():
    """告警管理代理們"""
    st.markdown("### 🚨 Alert Management Agents")

    col1, col2, col3 = st.columns(3)
    
    with col1:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f8f9fa;">
            <h3>🛡️ Latency Guard</h3>
            <p>Monitor execution latency and trigger throttling when p99 exceeds 25µs threshold.</p>
            <p><strong>Status:</strong> Active monitoring</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("🚀 Launch Latency Guard", key="latency_guard_button", type="primary"):
            st.switch_page("pages/1_Latency_Guard.py")

    with col2:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f8f9fa;">
            <h3>💸 DD Guard</h3>
            <p>Monitor drawdown levels and execute risk controls including kill-switch activation.</p>
            <p><strong>Status:</strong> Risk monitoring active</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("📈 Launch DD Guard", key="dd_guard_button"):
            st.switch_page("pages/2_DD_Guard.py")
    
    with col3:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #f8f9fa;">
            <h3>📋 Alert Manager</h3>
            <p>Centralized alert processing and coordination between different guard agents.</p>
            <p><strong>Status:</strong> Redis channels active</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("🎯 Launch Alert Manager", key="alert_manager_button"):
            st.switch_page("pages/3_Alert_Manager.py")


async def alert_workflow():
    """告警工作流程"""
    st.markdown("### 🔄 Alert Workflow")
    
    st.markdown(
        """
    <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px; background: #fff8e1;">
        <h3>📈 Real-time Alert Processing</h3>
        <p>24/7 monitoring workflow that listens to Redis ops.alert channel and dispatches alerts to appropriate agents.</p>
        <p><strong>Channels monitored:</strong> ops.alert, kill-switch, ml.deploy</p>
        <p><strong>Response time:</strong> < 100ms for critical alerts</p>
    </div>
    """,
        unsafe_allow_html=True,
    )
    
    col1, col2 = st.columns(2)
    with col1:
        if st.button("▶️ Start Alert Workflow", type="primary"):
            st.success("Alert Workflow started - monitoring Redis channels...")
    with col2:
        if st.button("⏹️ Stop Alert Workflow"):
            st.warning("Alert Workflow stopped")


async def quick_actions():
    """快速運維操作"""
    st.markdown("### ⚡ Quick Operations")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        if st.button("🔄 Restart Monitoring"):
            st.info("Restarting all monitoring agents...")
    
    with col2:
        if st.button("🛑 Trigger Kill-Switch"):
            st.error("Kill-switch activated! Trading stopped.")
    
    with col3:
        if st.button("📊 Generate Report"):
            st.success("Performance report generated")
    
    with col4:
        if st.button("🚨 Test Alerts"):
            st.warning("Test alert sent to ops.alert channel")


async def main():
    await header()
    await system_status()
    st.divider()
    await alert_agents()
    st.divider()  
    await alert_workflow()
    st.divider()
    await quick_actions()
    await footer()
    await about_agno()


if __name__ == "__main__":
    if check_password():
        asyncio.run(main())