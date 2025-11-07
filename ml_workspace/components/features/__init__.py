"""Feature engines/managers re-export.

优先使用根目录的稳定实现，保持兼容；
待本目录完全部署后，再切换为本地实现。
"""

try:
    # 先用根目录旧实现，兼容性最佳
    from clickhouse_feature_engine import ClickHouseFeatureEngine  # type: ignore
except Exception:
    # 回退到本地实现（当前为迁移雏形）
    from .engine import ClickHouseFeatureEngine  # type: ignore

try:
    from clickhouse_feature_manager import ClickHouseMLoFIManager  # type: ignore
except Exception:
    from .manager import ClickHouseMLoFIManager  # type: ignore
