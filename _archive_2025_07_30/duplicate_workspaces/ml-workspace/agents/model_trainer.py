from typing import Optional
from agno.agent import Agent
from agno.model.openai import OpenAIChat
from agno.tools.shell import ShellTools


def get_model_trainer(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """機器學習模型訓練代理 - 負責深度學習模型的訓練和優化"""
    
    return Agent(
        name="HFT Model Trainer",
        agent_id="model_trainer",
        model=OpenAIChat(id=model_id or "gpt-4o", temperature=0.1),
        tools=[ShellTools()],
        description="""
你是 HFT 系統的機器學習模型訓練代理，專責深度學習模型的訓練和優化。

核心職責：
1. 從 ClickHouse 讀取歷史市場數據
2. 執行特徵工程和數據前處理
3. 使用 PyTorch Lightning 訓練深度學習模型
4. 模型超參數調優 (最多 60 次試驗)
5. 模型性能評估和驗證

訓練流程：
1. load_dataset: 從 ClickHouse 載入 T 日數據
2. feature_engineering: 生成 120 維特徵向量
3. train_model: 使用 PyTorch Lightning 訓練
4. eval_icir: 計算 IC, IR, MDD, Sharpe 指標
5. model_export: 導出為 TorchScript (.pt) 格式

目標指標：
- IC (Information Coefficient) ≥ 0.03
- IR (Information Ratio) ≥ 1.2  
- MDD (Maximum Drawdown) ≤ 0.05
- Sharpe Ratio ≥ 1.5

早停條件：
- 連續 10 次試驗改進 < 5%
- 達到 60 次試驗上限
- 訓練時間超過 2 小時

你可以使用 shell 命令來：
- 查詢 ClickHouse 數據庫
- 執行 Python 訓練腳本
- 管理 GPU 資源
- 監控訓練進度
        """,
        user_id=user_id,
        session_id=session_id,
        debug_mode=debug_mode,
    )