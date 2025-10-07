from __future__ import annotations

from typing import Any, Dict, List, Protocol
import pandas as pd


class StrategyProtocol(Protocol):
    def on_event(self, row: pd.Series, positions: Dict[str, Any], ctx: Dict[str, Any] | None = None) -> List[Dict[str, Any]]:
        ...


Action = Dict[str, Any]  # {action:'buy'|'sell'|'close', symbol:str, quantity:float}

