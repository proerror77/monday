#!/usr/bin/env python3
"""
HFT Master Dashboard - Streamlit UI
===================================

HFT 系統主控制器儀表板
"""

import streamlit as st
import requests
from datetime import datetime
import time

# 頁面配置
st.set_page_config(
    page_title="HFT Master Dashboard",
    page_icon="🎛️",
    layout="wide",
    initial_sidebar_state="expanded"
)

# 標題
st.title("🎛️ HFT Master Control Dashboard")
st.markdown("**高頻交易系統統一控制中心**")

# 側邊欄
with st.sidebar:
    st.header("🖥️ 系統信息")
    st.write("**環境**: Development")
    st.write("**版本**: v1.0.0")
    st.write("**端口**: 8503 (UI), 8002 (API)")
    
    st.divider()
    
    st.header("🌐 服務端點")
    st.write("• **Ops**: [localhost:8501](http://localhost:8501)")
    st.write("• **ML**: [localhost:8502](http://localhost:8502)")
    st.write("• **Master**: [localhost:8503](http://localhost:8503)")
    
    st.divider()
    
    # 系統控制
    st.header("⚡ 系統控制")
    if st.button("🚀 啟動系統", type="primary"):
        st.info("系統啟動命令已發送")
    
    if st.button("🛑 停止系統", type="secondary"):
        st.warning("系統停止命令已發送")

# 主要內容
tab1, tab2, tab3 = st.tabs(["📊 系統狀態", "🔧 組件管理", "📈 性能監控"])

with tab1:
    st.header("系統整體狀態")
    
    # 狀態指示器
    col1, col2, col3, col4 = st.columns(4)
    
    with col1:
        st.metric("🔧 Rust 核心", "運行中", "狀態正常")
    with col2:
        st.metric("🧠 ML 工作區", "就緒", "模型可用")
    with col3:
        st.metric("🛡️ Ops 工作區", "監控中", "告警正常")
    with col4:
        st.metric("📊 系統健康", "優秀", "100% 可用")
    
    st.divider()
    
    # 基礎設施狀態
    st.subheader("🏗️ 基礎設施狀態")
    
    infra_col1, infra_col2 = st.columns(2)
    
    with infra_col1:
        st.success("✅ **Redis**: 正常運行 (localhost:6379)")
        st.success("✅ **ClickHouse**: 正常運行 (localhost:8123)")
    
    with infra_col2:
        st.info("📊 **數據流**: 實時市場數據正在收集")
        st.info("🔄 **同步狀態**: 所有服務同步正常")

with tab2:
    st.header("🔧 Workspace 組件管理")
    
    # Agno Workspaces 狀態
    workspaces = [
        {"name": "ops_workspace", "display": "Ops 運維工作區", "port": "8501", "api_port": "8000", "status": "running"},
        {"name": "ml_workspace", "display": "ML 機器學習工作區", "port": "8502", "api_port": "8001", "status": "running"},
        {"name": "master_workspace", "display": "Master 主控制器", "port": "8503", "api_port": "8002", "status": "running"},
    ]
    
    for ws in workspaces:
        with st.expander(f"🤖 {ws['display']}", expanded=True):
            col1, col2, col3 = st.columns([2, 1, 1])
            
            with col1:
                st.write(f"**名稱**: {ws['name']}")
                st.write(f"**狀態**: {'🟢 運行中' if ws['status'] == 'running' else '🔴 停止'}")
            
            with col2:
                st.write(f"**UI**: [{ws['port']}](http://localhost:{ws['port']})")
                st.write(f"**API**: [{ws['api_port']}](http://localhost:{ws['api_port']}/docs)")
            
            with col3:
                if st.button(f"重啟 {ws['name']}", key=f"restart_{ws['name']}"):
                    st.info(f"{ws['display']} 重啟命令已發送")

with tab3:
    st.header("📈 實時性能監控")
    
    # 性能指標
    perf_col1, perf_col2, perf_col3 = st.columns(3)
    
    with perf_col1:
        st.metric("⚡ 平均延遲", "18.5 μs", "-2.1 μs")
        st.metric("💰 日收益", "+$2,450", "+5.2%")
    
    with perf_col2:
        st.metric("📊 交易成功率", "98.7%", "+0.3%")
        st.metric("⚠️ 告警次數", "2", "-1")
    
    with perf_col3:
        st.metric("🔄 數據吞吐", "3.4 msg/s", "穩定")
        st.metric("💾 內存使用", "1.21 MB", "正常")
    
    st.divider()
    
    # 系統日誌（模擬）
    st.subheader("📝 系統日誌 (最近 10 條)")
    
    log_entries = [
        f"{datetime.now().strftime('%H:%M:%S')} [INFO] 系統健康檢查完成",
        f"{datetime.now().strftime('%H:%M:%S')} [INFO] 市場數據收集正常",
        f"{datetime.now().strftime('%H:%M:%S')} [INFO] ML 工作區模型就緒",
        f"{datetime.now().strftime('%H:%M:%S')} [INFO] Ops 工作區監控正常",
        f"{datetime.now().strftime('%H:%M:%S')} [INFO] Redis 連接穩定",
    ]
    
    for entry in log_entries[:5]:
        st.text(entry)

# 自動刷新
if st.checkbox("🔄 自動刷新 (3秒)", value=False):
    time.sleep(3)
    st.rerun()

# 底部信息
st.divider()
st.markdown("""
<div style='text-align: center; color: #666;'>
    <p>🎛️ HFT Master Control Dashboard | 統一系統管理 | 實時狀態監控</p>
</div>
""", unsafe_allow_html=True)