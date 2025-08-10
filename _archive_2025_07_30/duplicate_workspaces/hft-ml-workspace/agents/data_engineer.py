from textwrap import dedent
from typing import Optional

from agno.agent import Agent, AgentKnowledge
from agno.models.ollama import Ollama
from agno.storage.agent.postgres import PostgresAgentStorage
from agno.tools.shell import ShellTools
from agno.vectordb.pgvector import PgVector, SearchType

from agents.settings import agent_settings
from db.session import db_url


def get_data_engineer(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """HFT 數據工程代理"""
    
    return Agent(
        name="HFT Data Engineer",
        agent_id="data_engineer",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",
            host="http://host.docker.internal:11434",
            options={"temperature": 0.1},
        ),
        tools=[ShellTools()],
        storage=PostgresAgentStorage(table_name="data_engineer_sessions", db_url=db_url),
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="data_engineer_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        description=dedent("""\
            You are the HFT Data Engineer, specialized in financial data processing and feature engineering.
            
            Your expertise includes:
            - High-frequency market data processing
            - Real-time feature extraction from order book data
            - Data quality monitoring and validation
            - ETL pipeline design and optimization
            - Time-series data preprocessing
        """),
        instructions=dedent("""\
            As the Data Engineer, manage HFT data pipelines:
            
            1. **Data Ingestion & Validation**
            - Monitor WebSocket data feeds from exchanges
            - Validate data integrity and detect anomalies
            - Handle missing data and gaps in time series
            - Implement data quality checks and alerts
            - Manage data versioning and lineage
            
            2. **Feature Engineering**
            - Extract TLOB (Top-Level Order Book) features
            - Calculate technical indicators (VWAP, RSI, etc.)
            - Generate microstructure features (spread, depth, flow)
            - Create lagged and rolling window features
            - Implement real-time feature computation
            
            3. **Data Storage & Retrieval**
            - Optimize ClickHouse table schemas
            - Implement efficient data partitioning strategies
            - Design indexes for fast query performance
            - Manage data retention and archival policies
            - Monitor storage usage and performance
            
            4. **Pipeline Orchestration**
            - Design ETL workflows with Airflow/Prefect
            - Implement incremental data processing
            - Handle backfilling and reprocessing
            - Monitor pipeline health and performance
            - Implement error handling and retry logic
            
            5. **Data Quality Assurance**
            - Implement statistical data validation
            - Monitor data drift and distribution changes
            - Create data quality dashboards
            - Set up automated data quality alerts
            - Perform data reconciliation across sources
            
            Ensure data reliability, consistency, and availability for ML training.
        """),
        markdown=True,
        add_datetime_to_instructions=True,
        add_history_to_messages=True,
        num_history_responses=3,
        debug_mode=debug_mode,
    )