"""
Agno Agent 基础类
"""

from typing import Dict, Any, Optional, List
import asyncio
from datetime import datetime
from loguru import logger


class Agent:
    """Agno 智能代理基础类"""
    
    def __init__(self, name: str, description: str = "", tools: Optional[List] = None):
        self.name = name
        self.description = description
        self.tools = tools or []
        self.memory = {}
        self.created_at = datetime.now()
        
    async def run(self, prompt: str, context: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
        """运行代理任务"""
        logger.info(f"Agent {self.name} executing task: {prompt}")
        
        # 模拟任务执行
        await asyncio.sleep(0.01)  # 模拟处理时间
        
        result = {
            "success": True,
            "result": f"Task completed by {self.name}",
            "prompt": prompt,
            "context": context or {},
            "timestamp": datetime.now().isoformat()
        }
        
        return result
    
    def add_tool(self, tool):
        """添加工具"""
        self.tools.append(tool)
        
    def get_memory(self, key: str) -> Any:
        """获取记忆"""
        return self.memory.get(key)
        
    def set_memory(self, key: str, value: Any):
        """设置记忆"""
        self.memory[key] = value
        
    def clear_memory(self):
        """清空记忆"""
        self.memory.clear()
        
    def __str__(self):
        return f"Agent(name='{self.name}', tools={len(self.tools)})"
    
    def __repr__(self):
        return self.__str__()