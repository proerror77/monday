"""
Agno 工具基础类
"""

from typing import Dict, Any, Optional
from abc import ABC, abstractmethod
from datetime import datetime
from loguru import logger


class Tool(ABC):
    """工具基础抽象类"""
    
    def __init__(self, name: str, description: str = ""):
        self.name = name
        self.description = description
        self.usage_count = 0
        self.created_at = datetime.now()
    
    @abstractmethod
    async def execute(self, **kwargs) -> Dict[str, Any]:
        """执行工具功能"""
        pass
    
    async def run(self, **kwargs) -> Dict[str, Any]:
        """运行工具并记录使用"""
        self.usage_count += 1
        logger.info(f"Executing tool: {self.name}")
        
        try:
            result = await self.execute(**kwargs)
            return {
                "success": True,
                "tool": self.name,
                "result": result,
                "usage_count": self.usage_count
            }
        except Exception as e:
            logger.error(f"Tool {self.name} failed: {e}")
            return {
                "success": False,
                "tool": self.name,
                "error": str(e),
                "usage_count": self.usage_count
            }


class MockTool(Tool):
    """模拟工具类用于测试"""
    
    def __init__(self, name: str, description: str = "", return_value: Any = None):
        super().__init__(name, description)
        self.return_value = return_value or {"status": "success"}
    
    async def execute(self, **kwargs) -> Dict[str, Any]:
        """模拟执行"""
        # 模拟一些处理时间
        import asyncio
        await asyncio.sleep(0.01)
        
        result = self.return_value.copy() if isinstance(self.return_value, dict) else self.return_value
        result.update({"input_params": kwargs, "executed_at": datetime.now().isoformat()})
        
        return result