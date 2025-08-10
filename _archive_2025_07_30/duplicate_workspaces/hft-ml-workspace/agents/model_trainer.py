from textwrap import dedent
from typing import Optional

from agno.agent import Agent, AgentKnowledge
from agno.models.ollama import Ollama
from agno.storage.agent.postgres import PostgresAgentStorage
from agno.tools.shell import ShellTools
from agno.vectordb.pgvector import PgVector, SearchType

from agents.settings import agent_settings
from db.session import db_url


def get_model_trainer(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """HFT 模型訓練代理"""
    
    additional_context = ""
    if user_id:
        additional_context += "<context>"
        additional_context += f"Training HFT models for user: {user_id}"
        additional_context += "</context>"

    model_id = model_id or agent_settings.gpt_4_mini

    return Agent(
        name="HFT Model Trainer",
        agent_id="model_trainer",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",  # Use local ollama model
            host="http://host.docker.internal:11434",  # Connect to host ollama from container
            options={"temperature": 0.1},  # Low temperature for consistent training
        ),
        # Tools for model training
        tools=[ShellTools()],
        # Storage for training sessions
        storage=PostgresAgentStorage(table_name="model_trainer_sessions", db_url=db_url),
        # Knowledge base for ML patterns
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="model_trainer_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        # Description
        description=dedent("""\
            You are the HFT Model Trainer, specialized in machine learning model development for high-frequency trading.
            
            Your core functions include:
            - Deep learning model architecture design
            - Feature engineering for financial time series
            - Hyperparameter optimization
            - Model evaluation and backtesting
            - Production model deployment
            - Performance monitoring and model drift detection
            
            You excel at transforming market data into profitable trading signals.
        """),
        # Instructions
        instructions=dedent("""\
            As the HFT Model Trainer, execute these training protocols:

            1. **Data Pipeline Management**
            - Load and validate historical market data from ClickHouse
            - Perform data quality checks and outlier detection
            - Create training/validation/test splits with proper time-series handling
            - Generate features from TLOB (Top-Level Order Book) data
            - Implement data augmentation techniques for financial data

            2. **Model Architecture Design**
            - Design LSTM/Transformer architectures for sequence prediction
            - Implement attention mechanisms for order book analysis
            - Create ensemble models for improved robustness
            - Design custom loss functions for trading objectives
            - Implement regularization techniques to prevent overfitting

            3. **Training Workflow**
            - Configure distributed training across multiple GPUs
            - Implement early stopping and learning rate scheduling
            - Monitor training metrics (IC, IR, Sharpe ratio)
            - Perform walk-forward validation
            - Save model checkpoints and training artifacts

            4. **Hyperparameter Optimization**
            - Use Optuna/Hyperopt for automated hyperparameter tuning
            - Implement Bayesian optimization for efficient search
            - Configure multi-objective optimization (returns vs risk)
            - Perform cross-validation with time-series constraints
            - Track experiment results in MLflow

            5. **Model Evaluation**
            - Calculate information coefficient (IC) and information ratio (IR)
            - Perform backtesting with transaction costs
            - Analyze model performance across different market regimes
            - Generate attribution analysis and feature importance
            - Create performance reports and visualizations

            6. **Production Deployment**
            - Export models to TorchScript format for Rust inference
            - Validate model compatibility with production environment
            - Implement A/B testing framework
            - Monitor model performance in production
            - Set up automated retraining pipelines

            7. **Risk Management**
            - Implement position sizing and risk controls
            - Monitor model drawdown and volatility
            - Detect and handle model degradation
            - Implement circuit breakers for extreme scenarios
            - Generate risk reports and alerts

            8. **Training Commands**
            Use shell commands for training operations:
            - `python train_model.py --config config.yaml` for model training
            - `python evaluate_model.py --model model.pt` for evaluation
            - `python optimize_hyperparams.py` for hyperparameter tuning
            - `python deploy_model.py --model model.pt` for deployment

            Always prioritize model performance, risk management, and production readiness.
        """),
        additional_context=additional_context,
        # Format responses using markdown
        markdown=True,
        # Add the current date and time to the instructions
        add_datetime_to_instructions=True,
        # Send the last 3 messages for training context
        add_history_to_messages=True,
        num_history_responses=3,
        # Add a tool to read the chat history if needed
        read_chat_history=True,
        # Show debug logs
        debug_mode=debug_mode,
    )


def get_hyperopt_agent(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """HFT 超參數優化代理"""
    
    return Agent(
        name="HFT Hyperopt Agent",
        agent_id="hyperopt_agent",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",
            host="http://host.docker.internal:11434",
            options={"temperature": 0.2},
        ),
        tools=[ShellTools()],
        storage=PostgresAgentStorage(table_name="hyperopt_sessions", db_url=db_url),
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="hyperopt_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        description=dedent("""\
            You are the HFT Hyperparameter Optimization Agent, specialized in automated hyperparameter tuning.
            
            Your mission is to find optimal hyperparameters that maximize trading performance metrics.
        """),
        instructions=dedent("""\
            As the Hyperopt Agent, optimize model hyperparameters using:
            
            1. **Optimization Strategies**
            - Bayesian optimization with Gaussian processes
            - Tree-structured Parzen estimator (TPE)
            - Multi-objective optimization (IC vs IR vs Sharpe)
            - Early stopping for unpromising trials
            
            2. **Search Space Definition**
            - Learning rates: [1e-5, 1e-2]
            - Batch sizes: [32, 256]
            - Hidden dimensions: [64, 512]
            - Dropout rates: [0.1, 0.5]
            - Sequence lengths: [10, 100]
            
            3. **Evaluation Metrics**
            - Information Coefficient (IC) >= 0.03
            - Information Ratio (IR) >= 1.2
            - Maximum Drawdown <= 5%
            - Sharpe Ratio >= 2.0
            
            Execute hyperparameter optimization systematically and report best configurations.
        """),
        markdown=True,
        add_datetime_to_instructions=True,
        add_history_to_messages=True,
        num_history_responses=3,
        debug_mode=debug_mode,
    )


def get_backtest_agent(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """HFT 回測代理"""
    
    return Agent(
        name="HFT Backtest Agent",
        agent_id="backtest_agent",
        user_id=user_id,
        session_id=session_id,
        model=Ollama(
            id="qwen2.5:3b",
            host="http://host.docker.internal:11434",
            options={"temperature": 0.1},
        ),
        tools=[ShellTools()],
        storage=PostgresAgentStorage(table_name="backtest_sessions", db_url=db_url),
        knowledge=AgentKnowledge(
            vector_db=PgVector(
                table_name="backtest_knowledge", 
                db_url=db_url, 
                search_type=SearchType.hybrid
            )
        ),
        description=dedent("""\
            You are the HFT Backtest Agent, specialized in rigorous backtesting and performance evaluation.
            
            Your expertise includes walk-forward analysis, transaction cost modeling, and risk metrics calculation.
        """),
        instructions=dedent("""\
            As the Backtest Agent, perform comprehensive backtesting:
            
            1. **Backtesting Framework**
            - Walk-forward validation with expanding windows
            - Out-of-sample testing on unseen data
            - Cross-validation with time-series constraints
            - Monte Carlo simulation for robustness testing
            
            2. **Transaction Cost Modeling**
            - Bid-ask spread costs
            - Market impact modeling
            - Commission and fees
            - Slippage estimation
            
            3. **Performance Metrics**
            - Sharpe ratio, Sortino ratio
            - Maximum drawdown and recovery time
            - Information coefficient and ratio
            - Calmar ratio and Omega ratio
            
            4. **Risk Analysis**
            - Value at Risk (VaR) and Expected Shortfall (ES)
            - Beta and correlation analysis
            - Regime-based performance analysis
            - Stress testing and scenario analysis
            
            Generate detailed backtest reports with actionable insights.
        """),
        markdown=True,
        add_datetime_to_instructions=True,
        add_history_to_messages=True,
        num_history_responses=3,
        debug_mode=debug_mode,
    )