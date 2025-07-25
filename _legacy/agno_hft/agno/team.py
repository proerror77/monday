"""
Agno Team 管理类
"""

from typing import List, Dict, Any, Optional
import asyncio
from datetime import datetime
from loguru import logger
from .agent import Agent


class Team:
    """Agent 团队管理类"""
    
    def __init__(self, name: str, description: str = ""):
        self.name = name
        self.description = description
        self.agents: List[Agent] = []
        self.created_at = datetime.now()
        self.tasks_completed = 0
        
    def add_agent(self, agent: Agent):
        """添加代理到团队"""
        self.agents.append(agent)
        logger.info(f"Added agent {agent.name} to team {self.name}")
        
    def remove_agent(self, agent_name: str) -> bool:
        """从团队中移除代理"""
        for i, agent in enumerate(self.agents):
            if agent.name == agent_name:
                removed_agent = self.agents.pop(i)
                logger.info(f"Removed agent {removed_agent.name} from team {self.name}")
                return True
        return False
        
    def get_agent(self, agent_name: str) -> Optional[Agent]:
        """根据名称获取代理"""
        for agent in self.agents:
            if agent.name == agent_name:
                return agent
        return None
        
    def list_agents(self) -> List[str]:
        """列出所有代理名称"""
        return [agent.name for agent in self.agents]
        
    async def run_task(self, task_name: str, agent_name: str, prompt: str, context: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
        """让指定代理执行任务"""
        agent = self.get_agent(agent_name)
        if not agent:
            return {
                "success": False,
                "error": f"Agent {agent_name} not found in team {self.name}"
            }
            
        try:
            result = await agent.run(prompt, context)
            self.tasks_completed += 1
            
            return {
                "success": True,
                "task": task_name,
                "agent": agent_name,
                "team": self.name,
                "result": result
            }
            
        except Exception as e:
            logger.error(f"Task {task_name} failed for agent {agent_name}: {e}")
            return {
                "success": False,
                "task": task_name,
                "agent": agent_name,
                "team": self.name,
                "error": str(e)
            }
    
    async def run_parallel_tasks(self, tasks: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """并行执行多个任务"""
        logger.info(f"Team {self.name} starting {len(tasks)} parallel tasks")
        
        async_tasks = []
        for task in tasks:
            task_name = task.get("task_name", "unnamed_task")
            agent_name = task.get("agent_name")
            prompt = task.get("prompt", "")
            context = task.get("context", {})
            
            if not agent_name:
                continue
                
            async_task = self.run_task(task_name, agent_name, prompt, context)
            async_tasks.append(async_task)
        
        results = await asyncio.gather(*async_tasks, return_exceptions=True)
        
        # 处理异常结果
        processed_results = []
        for i, result in enumerate(results):
            if isinstance(result, Exception):
                processed_results.append({
                    "success": False,
                    "task": tasks[i].get("task_name", f"task_{i}"),
                    "error": str(result)
                })
            else:
                processed_results.append(result)
        
        return processed_results
    
    def get_team_status(self) -> Dict[str, Any]:
        """获取团队状态"""
        return {
            "name": self.name,
            "description": self.description,
            "agent_count": len(self.agents),
            "agents": [{"name": agent.name, "tools": len(agent.tools)} for agent in self.agents],
            "tasks_completed": self.tasks_completed,
            "created_at": self.created_at.isoformat()
        }
    
    def __str__(self):
        return f"Team(name='{self.name}', agents={len(self.agents)}, tasks_completed={self.tasks_completed})"
    
    def __repr__(self):
        return self.__str__()