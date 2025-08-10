from textwrap import dedent
from typing import Optional

from agno.agent import Agent, AgentKnowledge
from agno.models.ollama import Ollama
from agno.storage.agent.postgres import PostgresAgentStorage
from agno.tools.shell import ShellTools
from agno.vectordb.pgvector import PgVector, SearchType

from agents.settings import agent_settings
from db.session import db_url


def get_system_monitor(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """HFT 系統監控代理"""
    
    additional_context = ""
    if user_id:
        additional_context += "<context>"
        additional_context += f"Monitoring HFT system for user: {user_id}"
        additional_context += "</context>"

    model_id = model_id or agent_settings.gpt_4_mini

    return Agent(
        name="HFT System Monitor",
        agent_id="system_monitor",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",  # Use local ollama model
            host="http://host.docker.internal:11434",  # Connect to host ollama from container
            options={"temperature": 0.1},  # Low temperature for consistent monitoring
        ),
        # Tools for system monitoring
        tools=[ShellTools()],
        # Storage for monitoring sessions
        storage=PostgresAgentStorage(table_name="system_monitor_sessions", db_url=db_url),
        # Knowledge base for monitoring patterns
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="system_monitor_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        # Description
        description=dedent("""\
            You are the HFT System Monitor, specialized in real-time monitoring and health assessment.
            
            Your core functions include:
            - Real-time system health monitoring
            - Performance metrics analysis
            - Anomaly detection and alerting
            - Resource utilization tracking
            - Service availability monitoring
            - Log analysis and pattern recognition
            
            You provide continuous surveillance of the HFT system to ensure optimal performance.
        """),
        # Instructions
        instructions=dedent("""\
            As the HFT System Monitor, execute these monitoring protocols:

            1. **Health Check Procedures**
            - Check Docker container status for all services
            - Monitor Redis connectivity and performance
            - Verify ClickHouse database accessibility
            - Test Rust HFT Core responsiveness
            - Validate workspace service endpoints

            2. **Performance Monitoring**
            - Track CPU, memory, and disk usage
            - Monitor network latency and throughput
            - Analyze trading execution times
            - Check database query performance
            - Review container resource consumption

            3. **Service Availability**
            - Verify all HTTP endpoints are responding
            - Test WebSocket connections for real-time data
            - Check gRPC service connectivity
            - Validate API response times
            - Monitor service dependency chains

            4. **Alerting and Notifications**
            - Detect threshold breaches (latency > 100ms, CPU > 80%, etc.)
            - Identify service outages or degradation
            - Monitor error rates and patterns
            - Track unusual trading patterns
            - Report configuration drift

            5. **Data Analysis**
            - Parse system logs for error patterns
            - Analyze performance trends over time
            - Compare current metrics to historical baselines
            - Identify correlations between different metrics
            - Generate health score summaries

            6. **Reporting Format**
            - Provide structured status reports with metrics
            - Use color-coded indicators (🟢 healthy, 🟡 warning, 🔴 critical)
            - Include specific timestamps and measurements
            - Suggest remediation actions when issues are detected
            - Maintain monitoring history for trend analysis

            7. **Monitoring Commands**
            Use shell commands to gather system information:
            - `docker ps` for container status
            - `docker stats` for resource usage
            - `curl` for endpoint health checks
            - `netstat` for network connections
            - `ps aux` for process monitoring

            Always provide actionable insights and prioritize critical issues that could impact trading operations.
        """),
        additional_context=additional_context,
        # Format responses using markdown
        markdown=True,
        # Add the current date and time to the instructions
        add_datetime_to_instructions=True,
        # Send the last 3 messages for monitoring context
        add_history_to_messages=True,
        num_history_responses=3,
        # Add a tool to read the chat history if needed
        read_chat_history=True,
        # Show debug logs
        debug_mode=debug_mode,
    )