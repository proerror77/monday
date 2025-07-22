"""
重新設計的 Agent 架構 v2
=======================

基於真實 HFT 業務流程的合理 Agent 分工：

**研發類 (離線)**：
- ResearchAnalyst: 市場研究和策略開發
- ModelEngineer: ML 模型開發和部署

**管理類 (準實時)**：
- StrategyManager: 策略參數管理和優化
- RiskSupervisor: 風險監控和限額管理  
- SystemOperator: 系統運維和監控
- ComplianceOfficer: 合規監控和審計

**核心原則**：
- Rust 負責微秒級實時執行
- Python Agents 負責秒/分鐘級管理決策
- 清晰的職責邊界，避免重疊
"""

from .research_analyst import ResearchAnalyst
from .model_engineer import ModelEngineer
from .strategy_manager import StrategyManager
from .risk_supervisor import RiskSupervisor
from .system_operator import SystemOperator
from .compliance_officer import ComplianceOfficer

__all__ = [
    'ResearchAnalyst',    # 市場研究分析師
    'ModelEngineer',      # 模型工程師
    'StrategyManager',    # 策略管理員
    'RiskSupervisor',     # 風險監督員
    'SystemOperator',     # 系統操作員  
    'ComplianceOfficer'   # 合規官員
]