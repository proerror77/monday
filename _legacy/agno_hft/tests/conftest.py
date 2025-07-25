"""
简化的测试配置文件
================

提供基本的测试配置和 fixtures
"""

import pytest
import asyncio
from typing import Dict, Any


@pytest.fixture
def test_config() -> Dict[str, Any]:
    """基础测试配置"""
    return {
        "system": {
            "name": "HFT Test System",
            "version": "0.1.0",
            "mode": "test"
        },
        "risk_management": {
            "max_position_size": 1000,
            "max_daily_loss": 500,
            "stop_loss_pct": 0.02
        },
        "trading": {
            "symbols": ["BTCUSDT", "ETHUSDT"],
            "timeframe": "1m"
        }
    }


@pytest.fixture
def event_loop():
    """Event loop fixture for async tests"""
    loop = asyncio.new_event_loop()
    yield loop
    loop.close()


@pytest.fixture
def mock_agent():
    """模拟代理"""
    from agno.agent import Agent
    return Agent("MockAgent", "A mock agent for testing")


@pytest.fixture
def mock_team():
    """模拟团队"""
    from agno.team import Team
    from agno.agent import Agent
    
    team = Team("MockTeam", "A mock team for testing")
    agent1 = Agent("Agent1", "First test agent")
    agent2 = Agent("Agent2", "Second test agent")
    
    team.add_agent(agent1)
    team.add_agent(agent2)
    
    return team


@pytest.fixture
def mock_workflow():
    """模拟工作流"""
    from agno.workflow import Workflow
    from agno.agent import Agent
    
    workflow = Workflow("MockWorkflow", "A mock workflow for testing")
    agent = Agent("WorkflowAgent", "Agent for workflow testing")
    
    workflow.add_step("Step1", agent, "First step")
    workflow.add_step("Step2", agent, "Second step")
    
    return workflow