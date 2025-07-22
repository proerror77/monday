"""
Agno 工作流基础类
"""

from typing import Dict, Any, List, Optional
import asyncio
from datetime import datetime
from loguru import logger


class Workflow:
    """Agno 工作流基础类"""
    
    def __init__(self, name: str, description: str = ""):
        self.name = name
        self.description = description
        self.steps = []
        self.created_at = datetime.now()
        self.status = "pending"
        
    def add_step(self, step_name: str, agent, prompt: str, context: Optional[Dict[str, Any]] = None):
        """添加工作流步骤"""
        step = {
            "name": step_name,
            "agent": agent,
            "prompt": prompt,
            "context": context or {},
            "status": "pending"
        }
        self.steps.append(step)
        
    async def execute(self) -> Dict[str, Any]:
        """执行工作流"""
        logger.info(f"Starting workflow: {self.name}")
        self.status = "running"
        
        results = []
        
        for i, step in enumerate(self.steps):
            try:
                logger.info(f"Executing step {i+1}/{len(self.steps)}: {step['name']}")
                step["status"] = "running"
                
                result = await step["agent"].run(step["prompt"], step["context"])
                step["result"] = result
                step["status"] = "completed"
                results.append(result)
                
            except Exception as e:
                logger.error(f"Step {step['name']} failed: {e}")
                step["error"] = str(e)
                step["status"] = "failed"
                self.status = "failed"
                return {
                    "success": False,
                    "error": f"Workflow failed at step {step['name']}: {e}",
                    "results": results
                }
        
        self.status = "completed"
        logger.info(f"Workflow {self.name} completed successfully")
        
        return {
            "success": True,
            "workflow": self.name,
            "results": results,
            "steps_completed": len(results)
        }
        
    def get_status(self) -> Dict[str, Any]:
        """获取工作流状态"""
        completed_steps = sum(1 for step in self.steps if step.get("status") == "completed")
        
        return {
            "name": self.name,
            "status": self.status,
            "total_steps": len(self.steps),
            "completed_steps": completed_steps,
            "progress_pct": (completed_steps / len(self.steps) * 100) if self.steps else 0,
            "created_at": self.created_at.isoformat()
        }