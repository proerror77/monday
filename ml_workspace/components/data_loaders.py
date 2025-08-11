from typing import Dict, Any
from datetime import datetime, timedelta


class ClickHouseDataLoader:
    """Minimal data loader stub for training workflow.

    Replace with real ClickHouse queries.
    """

    def load(self, symbol: str, hours: int) -> Dict[str, Any]:
        return {
            "symbol": symbol,
            "from": (datetime.utcnow() - timedelta(hours=hours)).isoformat(),
            "to": datetime.utcnow().isoformat(),
            "rows": 0,
            "data": [],
        }

