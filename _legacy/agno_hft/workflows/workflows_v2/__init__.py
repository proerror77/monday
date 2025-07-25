"""
重新設計的工作流架構 v2
=======================

基於真實 HFT 業務流程的合理工作流設計：

**研發工作流 (離線)**：
- ResearchWorkflow: 市場研究和策略開發工作流
- ModelDevelopmentWorkflow: ML 模型開發和部署工作流

**運營工作流 (準實時)**：
- OperationalWorkflow: 日常運營管理工作流
- ComplianceWorkflow: 合規監控和審計工作流

**應急工作流 (實時響應)**：
- IncidentResponseWorkflow: 系統故障應急響應工作流
- RiskEmergencyWorkflow: 風險緊急處理工作流

**核心原則**：
- 工作流專注於協調管理，不直接執行交易
- 清晰的離線/準實時/實時響應分層
- 與 Rust 底層系統的明確接口分工
"""

from .research_workflow import ResearchWorkflow
from .model_development_workflow import ModelDevelopmentWorkflow
from .operational_workflow import OperationalWorkflow

__all__ = [
    'ResearchWorkflow',           # 市場研究工作流
    'ModelDevelopmentWorkflow',   # 模型開發工作流
    'OperationalWorkflow',        # 運營管理工作流
]