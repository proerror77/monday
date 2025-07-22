"""
基础功能测试
===========

测试核心组件的基本功能，不依赖复杂的外部模块
"""

import pytest
import asyncio
from datetime import datetime
from agno.agent import Agent
from agno.team import Team
from agno.workflow import Workflow


@pytest.mark.unit
class TestBasicComponents:
    """基础组件测试"""
    
    def test_agent_creation(self):
        """测试代理创建"""
        agent = Agent("TestAgent", "A test agent")
        
        assert agent.name == "TestAgent"
        assert agent.description == "A test agent"
        assert isinstance(agent.tools, list)
        assert len(agent.tools) == 0
        assert isinstance(agent.memory, dict)
        assert len(agent.memory) == 0
    
    @pytest.mark.asyncio
    async def test_agent_run(self):
        """测试代理运行"""
        agent = Agent("TestAgent", "A test agent")
        
        result = await agent.run("Test prompt", {"key": "value"})
        
        assert result["success"] is True
        assert "Task completed by TestAgent" in result["result"]
        assert result["prompt"] == "Test prompt"
        assert result["context"]["key"] == "value"
    
    def test_team_creation(self):
        """测试团队创建"""
        team = Team("TestTeam", "A test team")
        
        assert team.name == "TestTeam"
        assert team.description == "A test team"
        assert len(team.agents) == 0
        assert team.tasks_completed == 0
    
    def test_team_add_agent(self):
        """测试向团队添加代理"""
        team = Team("TestTeam")
        agent1 = Agent("Agent1")
        agent2 = Agent("Agent2")
        
        team.add_agent(agent1)
        team.add_agent(agent2)
        
        assert len(team.agents) == 2
        assert team.get_agent("Agent1") == agent1
        assert team.get_agent("Agent2") == agent2
        assert "Agent1" in team.list_agents()
        assert "Agent2" in team.list_agents()
    
    @pytest.mark.asyncio
    async def test_team_run_task(self):
        """测试团队运行任务"""
        team = Team("TestTeam")
        agent = Agent("TestAgent")
        team.add_agent(agent)
        
        result = await team.run_task("TestTask", "TestAgent", "Test prompt")
        
        assert result["success"] is True
        assert result["task"] == "TestTask"
        assert result["agent"] == "TestAgent"
        assert result["team"] == "TestTeam"
        assert team.tasks_completed == 1
    
    def test_workflow_creation(self):
        """测试工作流创建"""
        workflow = Workflow("TestWorkflow", "A test workflow")
        
        assert workflow.name == "TestWorkflow"
        assert workflow.description == "A test workflow"
        assert len(workflow.steps) == 0
        assert workflow.status == "pending"
    
    def test_workflow_add_step(self):
        """测试工作流添加步骤"""
        workflow = Workflow("TestWorkflow")
        agent = Agent("TestAgent")
        
        workflow.add_step("Step1", agent, "Test prompt", {"key": "value"})
        
        assert len(workflow.steps) == 1
        step = workflow.steps[0]
        assert step["name"] == "Step1"
        assert step["agent"] == agent
        assert step["prompt"] == "Test prompt"
        assert step["context"]["key"] == "value"
        assert step["status"] == "pending"
    
    @pytest.mark.asyncio
    async def test_workflow_execute(self):
        """测试工作流执行"""
        workflow = Workflow("TestWorkflow")
        agent1 = Agent("Agent1")
        agent2 = Agent("Agent2")
        
        workflow.add_step("Step1", agent1, "First task")
        workflow.add_step("Step2", agent2, "Second task")
        
        result = await workflow.execute()
        
        assert result["success"] is True
        assert result["workflow"] == "TestWorkflow"
        assert len(result["results"]) == 2
        assert result["steps_completed"] == 2
        assert workflow.status == "completed"
    
    def test_workflow_get_status(self):
        """测试获取工作流状态"""
        workflow = Workflow("TestWorkflow")
        agent = Agent("TestAgent")
        
        workflow.add_step("Step1", agent, "Test task")
        
        status = workflow.get_status()
        
        assert status["name"] == "TestWorkflow"
        assert status["status"] == "pending"
        assert status["total_steps"] == 1
        assert status["completed_steps"] == 0
        assert status["progress_pct"] == 0


@pytest.mark.unit
class TestMemoryAndTools:
    """内存和工具测试"""
    
    def test_agent_memory(self):
        """测试代理内存功能"""
        agent = Agent("TestAgent")
        
        # 设置内存
        agent.set_memory("key1", "value1")
        agent.set_memory("key2", {"nested": "value"})
        
        # 获取内存
        assert agent.get_memory("key1") == "value1"
        assert agent.get_memory("key2")["nested"] == "value"
        assert agent.get_memory("nonexistent") is None
        
        # 清空内存
        agent.clear_memory()
        assert len(agent.memory) == 0
    
    def test_agent_tools(self):
        """测试代理工具管理"""
        agent = Agent("TestAgent")
        
        # 模拟工具对象
        tool1 = {"name": "tool1", "type": "mock"}
        tool2 = {"name": "tool2", "type": "mock"}
        
        agent.add_tool(tool1)
        agent.add_tool(tool2)
        
        assert len(agent.tools) == 2
        assert tool1 in agent.tools
        assert tool2 in agent.tools


@pytest.mark.unit 
@pytest.mark.performance
class TestPerformance:
    """性能测试"""
    
    @pytest.mark.asyncio
    async def test_agent_response_time(self):
        """测试代理响应时间"""
        import time
        
        agent = Agent("FastAgent")
        
        start_time = time.time()
        await agent.run("Quick task")
        end_time = time.time()
        
        response_time = end_time - start_time
        assert response_time < 0.1  # 应该在100ms内完成
    
    @pytest.mark.asyncio
    async def test_parallel_agents(self):
        """测试并行代理执行"""
        import time
        
        team = Team("ParallelTeam")
        agents = [Agent(f"Agent_{i}") for i in range(5)]
        
        for agent in agents:
            team.add_agent(agent)
        
        tasks = [
            {"task_name": f"Task_{i}", "agent_name": f"Agent_{i}", "prompt": f"Task {i}"}
            for i in range(5)
        ]
        
        start_time = time.time()
        results = await team.run_parallel_tasks(tasks)
        end_time = time.time()
        
        execution_time = end_time - start_time
        assert len(results) == 5
        assert all(result["success"] for result in results)
        assert execution_time < 0.2  # 并行执行应该比顺序执行快