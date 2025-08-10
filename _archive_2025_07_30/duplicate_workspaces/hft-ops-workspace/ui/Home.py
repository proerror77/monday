import asyncio

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import check_password

from ui.css import CUSTOM_CSS
from ui.utils import about_agno, footer

nest_asyncio.apply()

st.set_page_config(
    page_title="HFT Ops Control",
    page_icon="⚡",
    layout="wide",
)
st.markdown(CUSTOM_CSS, unsafe_allow_html=True)


async def header():
    st.markdown("<h1 class='heading'>⚡ HFT Operations Control</h1>", unsafe_allow_html=True)
    st.markdown(
        "<p class='subheading'>Real-time operations monitoring, alert management, and risk control for high-frequency trading systems.</p>",
        unsafe_allow_html=True,
    )


async def body():
    st.markdown("### Available Agents")

    col1, col2 = st.columns(2)
    with col1:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>⚠️ Alert Manager</h3>
            <p>Central alert processing and coordination system for all HFT operational alerts.</p>
            <p>Perfect for alert processing, notification routing, and escalation management.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch Alert Manager", key="alert_manager_button"):
            st.switch_page("pages/1_Alert_Manager.py")

        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>🛡️ Drawdown Guard</h3>
            <p>Real-time drawdown monitoring and risk protection system with kill-switch capabilities.</p>
            <p>Perfect for risk monitoring, kill-switch activation, and position management.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch DD Guard", key="dd_guard_button"):
            st.switch_page("pages/3_DD_Guard.py")

    with col2:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>⚡ Latency Guard</h3>
            <p>Ultra-low latency monitoring and protection system for execution performance.</p>
            <p>Perfect for latency monitoring, threshold management, and degradation protocols.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch Latency Guard", key="latency_guard_button"):
            st.switch_page("pages/2_Latency_Guard.py")

        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>📊 System Monitor</h3>
            <p>Comprehensive system health monitoring and performance analysis for HFT infrastructure.</p>
            <p>Perfect for health checks, performance metrics, and resource monitoring.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch System Monitor", key="system_monitor_button"):
            st.switch_page("pages/4_System_Monitor.py")

    st.markdown("### Operational Status")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        st.metric("⚠️ Alert Manager", "Active", "Processing")
    with col2:
        st.metric("⚡ Latency Guard", "Monitoring", "< 25µs")
    with col3:
        st.metric("🛡️ DD Guard", "Armed", "< 3% DD")
    with col4:
        st.metric("📊 System Monitor", "Online", "All Systems")

    st.markdown("### Real-time Metrics")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        st.metric("📊 Active Alerts", "12", "↑ 2")
    with col2:
        st.metric("⚡ Avg Latency", "18.5µs", "↓ 0.3µs")
    with col3:
        st.metric("🛡️ Current DD", "1.2%", "↓ 0.1%")
    with col4:
        st.metric("🔧 System Health", "98.5%", "↑ 0.2%")


async def main():
    await header()
    await body()
    await footer()
    await about_agno()


if __name__ == "__main__":
    if check_password():
        asyncio.run(main())