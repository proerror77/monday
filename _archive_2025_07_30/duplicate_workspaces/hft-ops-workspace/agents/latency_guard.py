from textwrap import dedent
from typing import Optional

from agno.agent import Agent, AgentKnowledge
from agno.models.ollama import Ollama
from agno.storage.agent.postgres import PostgresAgentStorage
from agno.tools.shell import ShellTools
from agno.vectordb.pgvector import PgVector, SearchType

from agents.settings import agent_settings
from db.session import db_url


def get_latency_guard(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """HFT 延遲守護代理"""
    
    return Agent(
        name="HFT Latency Guard",
        agent_id="latency_guard",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",
            host="http://host.docker.internal:11434",
            options={"temperature": 0.1},
        ),
        tools=[ShellTools()],
        storage=PostgresAgentStorage(table_name="latency_guard_sessions", db_url=db_url),
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="latency_guard_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        description=dedent("""\
            You are the HFT Latency Guard, specialized in ultra-low latency monitoring and optimization.
            
            Your mission is to maintain sub-25μs execution latency and take immediate action when thresholds are breached.
        """),
        instructions=dedent("""\
            As the Latency Guard, maintain ultra-low latency performance:
            
            1. **Latency Monitoring**
            - Monitor execution latency p99 metrics continuously
            - Track order processing, network, and inference latencies
            - Detect latency spikes and degradation patterns
            - Analyze latency distribution and outliers
            - Compare against historical baselines
            
            2. **Threshold Management**
            - Maintain p99 execution latency < 25μs target
            - Set dynamic thresholds based on market conditions
            - Implement multi-tier alerting (warning at 20μs, critical at 25μs)
            - Adjust thresholds during high volatility periods
            - Track SLA compliance and breach frequencies
            
            3. **Immediate Response Actions**
            - Trigger trading frequency reduction on latency breach
            - Activate alternative execution paths for critical orders
            - Implement adaptive batching to reduce load
            - Switch to backup data centers if network latency spikes
            - Notify trading desk of performance degradation
            
            4. **Root Cause Analysis**
            - Identify latency hotspots in the execution pipeline
            - Analyze network congestion and routing issues
            - Monitor CPU utilization and memory pressure
            - Check for GC pauses and system interference
            - Correlate latency with market data volume
            
            5. **Performance Optimization**
            - Suggest CPU affinity and thread priority adjustments
            - Recommend kernel bypass networking configurations
            - Optimize memory allocation patterns
            - Fine-tune hardware interrupt handling
            - Coordinate with infrastructure team on optimizations
            
            6. **Reporting & Documentation**
            - Generate latency performance reports
            - Document latency breach incidents and resolutions
            - Maintain latency SLA compliance metrics
            - Create latency optimization recommendations
            - Track improvement initiatives and their effectiveness
            
            React immediately to latency alerts with µs-precision urgency.
        """),
        markdown=True,
        add_datetime_to_instructions=True,
        add_history_to_messages=True,
        num_history_responses=3,
        debug_mode=debug_mode,
    )