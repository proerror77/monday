import asyncio
from typing import Any, Dict

import nest_asyncio
import streamlit as st
from agno.tools.streamlit.components import check_password
import uuid

def get_user_id_and_session_id():
    """生成用戶 ID 和會話 ID"""
    user_id = "ops_user"
    session_id = str(uuid.uuid4())[:8]
    return user_id, session_id

from agents.latency_guard import get_latency_guard
from ui.css import CUSTOM_CSS
from ui.utils import footer

nest_asyncio.apply()

st.set_page_config(
    page_title="Latency Guard",
    page_icon="🛡️",
    layout="wide",
)
st.markdown(CUSTOM_CSS, unsafe_allow_html=True)


async def latency_monitoring():
    """延遲監控介面"""
    
    # Page header
    st.markdown("<h1 class='heading'>🛡️ HFT Latency Guard</h1>", unsafe_allow_html=True)
    st.markdown(
        "<p class='subheading'>Real-time latency monitoring and automatic throttling response system</p>",
        unsafe_allow_html=True,
    )
    
    # Latency metrics sidebar
    with st.sidebar:
        st.markdown("### 📊 Latency Metrics")
        
        st.metric("Current Latency", "23.7µs", "-1.3µs")
        st.metric("P99 Threshold", "25.0µs", "Target")
        st.metric("Alert Level", "🟢 Normal", "Below threshold")
        
        st.markdown("### ⚙️ Throttling Controls")
        throttle_enabled = st.toggle("Auto Throttling", value=True)
        if throttle_enabled:
            threshold_multiplier = st.selectbox("Alert Threshold", ["1.5x", "2.0x", "2.5x", "3.0x"], index=1)
            st.write(f"Will throttle at {threshold_multiplier} × P99")
        
        st.markdown("### 🚨 Quick Actions")
        if st.button("🔥 Manual Throttle"):
            st.session_state.auto_message = "System is experiencing high latency. Please execute manual throttling immediately."
        if st.button("📊 Latency Report"):
            st.session_state.auto_message = "Generate a detailed latency analysis report for the last hour"
        if st.button("🔍 Deep Analysis"):
            st.session_state.auto_message = "Perform deep latency analysis to identify bottlenecks"
    
    # Initialize session state
    if "latency_messages" not in st.session_state:
        st.session_state.latency_messages = []
    
    # Auto-send message from sidebar buttons
    if "auto_message" in st.session_state:
        st.session_state.latency_messages.append({
            "role": "user", 
            "content": st.session_state.auto_message
        })
        del st.session_state.auto_message
    
    # Display latency monitoring chat
    st.markdown("### 💬 Latency Guard Console")
    
    # Display chat messages
    for message in st.session_state.latency_messages:
        with st.chat_message(message["role"]):
            st.markdown(message["content"])
    
    # Chat input for latency queries
    if prompt := st.chat_input("Enter latency monitoring command..."):
        # Add user message to chat history
        st.session_state.latency_messages.append({"role": "user", "content": prompt})
        
        # Display user message
        with st.chat_message("user"):
            st.markdown(prompt)
        
        # Get response from Latency Guard agent
        with st.chat_message("assistant"):
            with st.spinner("Analyzing latency metrics..."):
                try:
                    # Get user ID and session ID
                    user_id, session_id = get_user_id_and_session_id()
                    
                    # Initialize Latency Guard agent
                    agent = get_latency_guard(
                        user_id=user_id,
                        session_id=session_id,
                        debug_mode=False
                    )
                    
                    # Get response
                    response = await agent.arun(prompt)
                    st.markdown(response.content)
                    
                    # Add assistant response to chat history
                    st.session_state.latency_messages.append({
                        "role": "assistant", 
                        "content": response.content
                    })
                    
                except Exception as e:
                    error_msg = f"Latency Guard Error: {str(e)}"
                    st.error(error_msg)
                    st.session_state.latency_messages.append({
                        "role": "assistant", 
                        "content": error_msg
                    })
    
    # Latency monitoring example queries
    st.markdown("### 📋 Common Latency Operations")
    
    latency_examples = [
        "Check current execution latency and compare with P99 threshold",
        "Analyze latency patterns for the last 15 minutes",
        "What factors are contributing to current latency?",
        "Execute throttling if latency exceeds 2x threshold",
        "Show me the latency distribution histogram",
        "Is the current latency trend acceptable for trading?",
    ]
    
    cols = st.columns(2)
    for i, example in enumerate(latency_examples):
        with cols[i % 2]:
            if st.button(example, key=f"latency_example_{i}"):
                st.session_state.latency_messages.append({"role": "user", "content": example})
                st.rerun()


async def main():
    await latency_monitoring()
    await footer()


if __name__ == "__main__":
    if check_password():
        asyncio.run(main())