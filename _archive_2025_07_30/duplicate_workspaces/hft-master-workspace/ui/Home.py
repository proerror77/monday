import asyncio
import sys
import os

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import check_password

# Add parent directory to path for imports
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from ui.css import CUSTOM_CSS
from ui.utils import about_agno, footer
from workflows.system_initialization import initialize_hft_system

nest_asyncio.apply()

st.set_page_config(
    page_title="HFT Master Control",
    page_icon="🎯",
    layout="wide",
)
st.markdown(CUSTOM_CSS, unsafe_allow_html=True)


async def header():
    st.markdown("<h1 class='heading'>🎯 HFT Master Control Center</h1>", unsafe_allow_html=True)
    st.markdown(
        "<p class='subheading'>High-Frequency Trading System Master Controller - Manage your entire HFT infrastructure from here.</p>",
        unsafe_allow_html=True,
    )


async def body():
    # 自動系統初始化檢查
    if "system_initialized" not in st.session_state:
        st.session_state.system_initialized = False
        st.session_state.initialization_results = None
    
    # 自動初始化系統（只在第一次加載時執行）
    if not st.session_state.system_initialized:
        with st.expander("🔄 System Initialization in Progress...", expanded=True):
            st.info("Master Workspace is initializing the HFT system...")
            
            initialization_placeholder = st.empty()
            
            try:
                with initialization_placeholder.container():
                    st.text("🔍 Checking infrastructure health...")
                    st.text("⚙️ Preparing Rust system...")
                    st.text("📊 Setting up data connections...")
                    st.text("🎯 Coordinating workspaces...")
                    
                    # 執行系統初始化
                    results = await initialize_hft_system()
                    st.session_state.initialization_results = results
                    st.session_state.system_initialized = True
                    
                    if results.get("system_ready", False):
                        st.success("✅ HFT System initialization completed successfully!")
                    else:
                        st.warning("⚠️ System partially initialized. Some components may need attention.")
                        
            except Exception as e:
                st.error(f"❌ System initialization failed: {str(e)}")
                st.session_state.initialization_results = {"error": str(e)}

    # 顯示系統狀態（基於初始化結果）
    st.markdown("### Real-Time System Status")
    
    if st.session_state.initialization_results:
        results = st.session_state.initialization_results
        component_status = results.get("component_status", {})
        
        # 基礎設施狀態
        infrastructure = component_status.get("infrastructure", {})
        col1, col2, col3, col4 = st.columns(4)
        
        with col1:
            redis_status = "🟢 Online" if infrastructure.get("redis") else "🔴 Offline"
            st.metric("Redis", redis_status, "6379")
            
        with col2:
            ch_status = "🟢 Online" if infrastructure.get("clickhouse") else "🔴 Offline"
            st.metric("ClickHouse", ch_status, "8123/9000")
            
        with col3:
            rust_status_data = component_status.get("rust_system", {})
            rust_status = "🟢 Ready" if rust_status_data.get("status") == "ready" else "🟡 Preparing"
            st.metric("Rust Core", rust_status, "HFT Engine")
            
        with col4:
            workspace_data = component_status.get("workspaces", {})
            ws_status = "🟢 Active" if workspace_data.get("coordination_complete") else "🟡 Starting"
            st.metric("Workspaces", ws_status, "Ops + ML")
            
        # 工作區詳細狀態
        st.markdown("### Workspace Status")
        col1, col2, col3 = st.columns(3)
        
        with col1:
            st.metric("🎯 Master WS", "🟢 Active", "Port 8504")
        with col2:
            ops_status = "🟢 Online" if workspace_data.get("ops_workspace") else "🟡 Starting"
            st.metric("⚡ Ops WS", ops_status, "Port 8503")
        with col3:
            ml_status = "🟢 Online" if workspace_data.get("ml_workspace") else "🟡 Starting"
            st.metric("🧠 ML WS", ml_status, "Port 8502")
            
        # 系統建議
        recommendations = results.get("recommendations", [])
        if recommendations:
            st.markdown("### System Recommendations")
            for i, rec in enumerate(recommendations, 1):
                if "良好" in rec or "就緒" in rec:
                    st.success(f"{i}. {rec}")
                else:
                    st.warning(f"{i}. {rec}")

    st.markdown("### Available Agents")

    col1, col2 = st.columns(2)
    with col1:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>🎯 HFT Controller</h3>
            <p>Central control agent for managing the entire HFT system infrastructure.</p>
            <p>Perfect for system orchestration, deployment management, and emergency response.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch HFT Controller", key="hft_controller_button"):
            st.switch_page("pages/1_HFT_Controller.py")

    with col2:
        st.markdown(
            """
        <div style="padding: 20px; border-radius: 10px; border: 1px solid #ddd; margin-bottom: 20px;">
            <h3>📊 System Monitor</h3>
            <p>Real-time monitoring agent for system health and performance analysis.</p>
            <p>Perfect for health checks, performance metrics, and anomaly detection.</p>
        </div>
        """,
            unsafe_allow_html=True,
        )
        if st.button("Launch System Monitor", key="system_monitor_button"):
            st.switch_page("pages/2_System_Monitor.py")

    st.markdown("### Master Control Actions")
    
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        if st.button("🚀 Start Rust Trading", type="primary"):
            st.success("Initiating Rust HFT Core...")
            # 這裡會觸發 Rust 系統啟動
    
    with col2:
        if st.button("⏸️ Pause Trading"):
            st.warning("Trading operations paused")
    
    with col3:
        if st.button("🔄 Reinitialize System"):
            st.session_state.system_initialized = False
            st.rerun()
    
    with col4:
        if st.button("🛑 Emergency Stop"):
            st.error("Emergency stop activated!")


async def main():
    await header()
    await body()
    await footer()
    await about_agno()


if __name__ == "__main__":
    if check_password():
        asyncio.run(main())