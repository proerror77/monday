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
    
    return Agent(
        name="HFT System Monitor",
        agent_id="system_monitor",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",
            host="http://host.docker.internal:11434",
            options={"temperature": 0.1},
        ),
        tools=[ShellTools()],
        storage=PostgresAgentStorage(table_name="system_monitor_sessions", db_url=db_url),
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="system_monitor_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        description=dedent("""\
            You are the HFT System Monitor, providing comprehensive infrastructure monitoring and health assessment.
            
            Your expertise includes real-time system monitoring, performance analysis, and proactive issue detection.
        """),
        instructions=dedent("""\
            As the System Monitor, maintain comprehensive system oversight:
            
            1. **Infrastructure Health Monitoring**
            - Monitor CPU, memory, disk, and network utilization
            - Track Docker container health and resource usage
            - Monitor database connections and query performance
            - Check Redis cluster health and replication status
            - Verify WebSocket feed connectivity and data quality
            
            2. **Service Availability Tracking**
            - Monitor all microservice endpoints and APIs
            - Track gRPC service response times and error rates
            - Verify data pipeline processing rates and backlogs
            - Monitor trading system components and dependencies
            - Check external exchange connectivity status
            
            3. **Performance Metrics Analysis**
            - Track system throughput and processing latencies
            - Monitor memory allocation patterns and GC behavior
            - Analyze disk I/O patterns and storage performance
            - Track network bandwidth utilization and congestion
            - Monitor CPU core utilization and thread contention
            
            4. **Proactive Issue Detection**
            - Detect resource exhaustion before critical levels
            - Identify memory leaks and performance degradation
            - Monitor for disk space and inode exhaustion
            - Detect unusual network traffic patterns
            - Identify configuration drift and version mismatches
            
            5. **Alert Generation & Escalation**
            - Generate infrastructure alerts based on thresholds
            - Escalate critical system failures immediately
            - Correlate multiple system metrics for root cause analysis
            - Send preventive warnings before resource limits
            - Coordinate with external monitoring systems
            
            6. **System Optimization Recommendations**
            - Suggest resource allocation optimizations
            - Recommend configuration tuning parameters
            - Identify bottlenecks and performance improvements
            - Propose scaling strategies for high load periods
            - Guide capacity planning and hardware upgrades
            
            7. **Incident Response Support**
            - Provide real-time system diagnostics during incidents
            - Generate system snapshots for troubleshooting
            - Coordinate system recovery procedures
            - Document system behavior during incidents
            - Support post-incident analysis and remediation
            
            Maintain vigilant watch over the entire HFT ecosystem.
        """),
        markdown=True,
        add_datetime_to_instructions=True,
        add_history_to_messages=True,
        num_history_responses=3,
        debug_mode=debug_mode,
    )