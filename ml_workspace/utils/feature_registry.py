"""
特征注册表 - 单一真相源
管理系统中所有特征定义、版本控制和合约验证
"""

from typing import List, Dict, Any, Optional, Tuple
import hashlib
import json
from pathlib import Path
from dataclasses import dataclass, asdict
from datetime import datetime


@dataclass
class FeatureDefinition:
    """特征定义"""
    name: str
    description: str
    data_type: str
    category: str  # 'basic', 'derived', 'engineered'
    dependencies: List[str]  # 依赖的其他特征
    computation: Optional[str] = None  # 计算逻辑描述


@dataclass
class FeatureSet:
    """特征集合定义"""
    name: str
    version: str
    description: str
    features: List[FeatureDefinition]
    created_at: str
    hash_signature: str


class FeatureRegistry:
    """特征注册表 - 系统中所有特征的权威定义"""

    def __init__(self, registry_path: Optional[str] = None):
        self.registry_path = Path(registry_path or "feature_registry.json")
        self.feature_sets: Dict[str, FeatureSet] = {}
        self._load_registry()

    def _load_registry(self):
        """加载注册表"""
        if self.registry_path.exists():
            with open(self.registry_path, 'r', encoding='utf-8') as f:
                data = json.load(f)
                for name, fs_data in data.get('feature_sets', {}).items():
                    features = [FeatureDefinition(**fd) for fd in fs_data['features']]
                    fs_data['features'] = features
                    self.feature_sets[name] = FeatureSet(**fs_data)

    def _save_registry(self):
        """保存注册表"""
        data = {
            'feature_sets': {},
            'last_updated': datetime.now().isoformat()
        }

        for name, fs in self.feature_sets.items():
            fs_dict = asdict(fs)
            data['feature_sets'][name] = fs_dict

        with open(self.registry_path, 'w', encoding='utf-8') as f:
            json.dump(data, f, indent=2, ensure_ascii=False)

    def _compute_hash(self, features: List[FeatureDefinition]) -> str:
        """计算特征集的哈希签名"""
        feature_names = [f.name for f in features]
        content = json.dumps(sorted(feature_names), sort_keys=True)
        return hashlib.sha256(content.encode()).hexdigest()[:16]

    def register_feature_set(self, name: str, features: List[FeatureDefinition],
                           version: str, description: str = "") -> str:
        """注册特征集合"""
        hash_signature = self._compute_hash(features)

        feature_set = FeatureSet(
            name=name,
            version=version,
            description=description,
            features=features,
            created_at=datetime.now().isoformat(),
            hash_signature=hash_signature
        )

        self.feature_sets[name] = feature_set
        self._save_registry()

        return hash_signature

    def get_feature_set(self, name: str) -> Optional[FeatureSet]:
        """获取特征集合"""
        return self.feature_sets.get(name)

    def list_feature_sets(self) -> List[str]:
        """列出所有特征集合名称"""
        return list(self.feature_sets.keys())

    def validate_dimensions(self, feature_set_name: str, actual_dim: int) -> bool:
        """验证特征维度"""
        fs = self.get_feature_set(feature_set_name)
        if not fs:
            return False
        return len(fs.features) == actual_dim

    def get_feature_names(self, feature_set_name: str) -> List[str]:
        """获取特征名称列表"""
        fs = self.get_feature_set(feature_set_name)
        if not fs:
            return []
        return [f.name for f in fs.features]

    def create_migration_plan(self, from_features: List[str],
                            to_feature_set: str) -> Dict[str, Any]:
        """创建特征迁移计划"""
        target_fs = self.get_feature_set(to_feature_set)
        if not target_fs:
            raise ValueError(f"目标特征集 {to_feature_set} 不存在")

        target_features = [f.name for f in target_fs.features]

        missing = set(target_features) - set(from_features)
        extra = set(from_features) - set(target_features)
        common = set(from_features) & set(target_features)

        return {
            "from_dimension": len(from_features),
            "to_dimension": len(target_features),
            "common_features": list(common),
            "missing_features": list(missing),
            "extra_features": list(extra),
            "migration_type": self._determine_migration_type(missing, extra),
            "migration_steps": self._generate_migration_steps(missing, extra)
        }

# --- Default feature sets helpers (non-persistent) ---

DEFAULT_MS_FEATURES: list[str] = [
    # 核心BBO特征
    'spread_abs', 'spread_pct', 'best_bid_size', 'best_ask_size', 'size_imbalance',
    # 即时交易特征
    'instant_trades', 'instant_volume', 'instant_buy_volume', 'instant_sell_volume',
    'instant_volume_imbalance', 'trade_aggression', 'trade_price_deviation_bps',
    # 价格变化特征 (订单簿动态)
    'price_change', 'bid_change', 'ask_change', 'bid_size_change', 'ask_size_change',
    # 即时衍生特征
    'avg_trade_size_raw', 'volume_per_spread_raw', 'depth_imbalance_raw',
    'trade_intensity', 'liquidity_ratio',
]

def get_default_ms_feature_names() -> list[str]:
    """Return the default millisecond-level feature names used across SQL builders and FeatureEngineer.

    This list must exclude metadata/label columns like exchange_ts, symbol, mid_price.
    """
    return list(DEFAULT_MS_FEATURES)

    def _determine_migration_type(self, missing: set, extra: set) -> str:
        """确定迁移类型"""
        if not missing and not extra:
            return "no_change"
        elif missing and not extra:
            return "expansion"
        elif not missing and extra:
            return "reduction"
        else:
            return "transformation"

    def _generate_migration_steps(self, missing: set, extra: set) -> List[str]:
        """生成迁移步骤"""
        steps = []

        if extra:
            steps.append(f"移除多余特征: {list(extra)}")

        if missing:
            steps.append(f"添加缺失特征: {list(missing)}")
            steps.append("重新计算和存储新特征")

        steps.append("验证迁移结果")
        return steps


def create_standard_registry() -> FeatureRegistry:
    """创建标准特征注册表"""
    registry = FeatureRegistry()

    # 定义53维统一特征集
    unified_features = [
        # 基础BBO特征 (5维)
        FeatureDefinition("spread_abs", "绝对价差", "float", "basic", []),
        FeatureDefinition("spread_pct", "相对价差", "float", "basic", ["spread_abs"]),
        FeatureDefinition("best_bid_size", "最优买单量", "float", "basic", []),
        FeatureDefinition("best_ask_size", "最优卖单量", "float", "basic", []),
        FeatureDefinition("size_imbalance", "订单量不平衡", "float", "derived", ["best_bid_size", "best_ask_size"]),

        # 即时交易特征 (7维)
        FeatureDefinition("instant_trades", "即时交易次数", "int", "basic", []),
        FeatureDefinition("instant_volume", "即时交易量", "float", "basic", []),
        FeatureDefinition("instant_buy_volume", "即时买入量", "float", "basic", []),
        FeatureDefinition("instant_sell_volume", "即时卖出量", "float", "basic", []),
        FeatureDefinition("instant_volume_imbalance", "即时量不平衡", "float", "derived", ["instant_buy_volume", "instant_sell_volume"]),
        FeatureDefinition("trade_aggression", "交易侵略性", "float", "engineered", []),
        FeatureDefinition("trade_price_deviation_bps", "交易价格偏离(bp)", "float", "engineered", []),

        # 订单簿动态特征 (6维)
        FeatureDefinition("price_change", "价格变化", "float", "derived", []),
        FeatureDefinition("bid_change", "买价变化", "float", "derived", []),
        FeatureDefinition("ask_change", "卖价变化", "float", "derived", []),
        FeatureDefinition("bid_size_change", "买量变化", "float", "derived", []),
        FeatureDefinition("ask_size_change", "卖量变化", "float", "derived", []),
        FeatureDefinition("inferred_book_impact", "推断订单簿冲击", "float", "engineered", []),

        # 累计成交量特征1分钟 (8维)
        FeatureDefinition("cum_buy_volume_1m", "1分钟累计买量", "float", "engineered", []),
        FeatureDefinition("cum_sell_volume_1m", "1分钟累计卖量", "float", "engineered", []),
        FeatureDefinition("cum_volume_imbalance_1m", "1分钟累计量不平衡", "float", "engineered", []),
        FeatureDefinition("cum_trades_1m", "1分钟累计交易次数", "int", "engineered", []),
        FeatureDefinition("cum_ask_consumption_1m", "1分钟累计卖单消耗", "float", "engineered", []),
        FeatureDefinition("cum_bid_consumption_1m", "1分钟累计买单消耗", "float", "engineered", []),
        FeatureDefinition("order_flow_imbalance_1m", "1分钟订单流不平衡", "float", "engineered", []),
        FeatureDefinition("cum_book_pressure_1m", "1分钟累计订单簿压力", "float", "engineered", []),

        # 5分钟累计特征 (3维)
        FeatureDefinition("cum_buy_volume_5m", "5分钟累计买量", "float", "engineered", []),
        FeatureDefinition("cum_sell_volume_5m", "5分钟累计卖量", "float", "engineered", []),
        FeatureDefinition("cum_volume_imbalance_5m", "5分钟累计量不平衡", "float", "engineered", []),

        # 交易活跃度分析 (3维)
        FeatureDefinition("aggressive_trades_1m", "1分钟侵略性交易", "int", "engineered", []),
        FeatureDefinition("aggressive_trade_ratio_1m", "1分钟侵略性交易比率", "float", "engineered", []),
        FeatureDefinition("trade_frequency", "交易频率", "float", "engineered", []),

        # 波动率特征 (2维)
        FeatureDefinition("price_volatility_1m", "1分钟价格波动率", "float", "engineered", []),
        FeatureDefinition("spread_volatility_1m", "1分钟价差波动率", "float", "engineered", []),

        # 高级衍生特征 (8维)
        FeatureDefinition("combined_imbalance_signal", "综合不平衡信号", "float", "engineered", []),
        FeatureDefinition("liquidity_stress_indicator", "流动性压力指标", "float", "engineered", []),
        FeatureDefinition("volatility_flow_interaction", "波动率流量交互", "float", "engineered", []),
        FeatureDefinition("depth_to_activity_ratio", "深度活跃度比率", "float", "engineered", []),
        FeatureDefinition("spread_volatility_ratio", "价差波动率比率", "float", "engineered", []),
        FeatureDefinition("information_richness_score", "信息丰富度评分", "float", "engineered", []),
        FeatureDefinition("actual_lob_depth", "实际订单簿深度", "float", "engineered", []),
        FeatureDefinition("order_flow_toxicity", "订单流毒性", "float", "engineered", []),

        # 增强特征 (3维)
        FeatureDefinition("volume_weighted_imbalance", "成交量加权不平衡", "float", "engineered", []),
        FeatureDefinition("effective_spread_adjusted", "调整后有效价差", "float", "engineered", []),
        FeatureDefinition("market_resilience", "市场韧性", "float", "engineered", []),

        # 市场节奏特征 (2维)
        FeatureDefinition("informed_trading_pressure", "知情交易压力", "float", "engineered", []),
        FeatureDefinition("market_rhythm_cos", "市场节奏(余弦)", "float", "engineered", []),

        # 最终特征 (3维)
        FeatureDefinition("market_rhythm_sin", "市场节奏(正弦)", "float", "engineered", []),
        FeatureDefinition("price_discovery_efficiency", "价格发现效率", "float", "engineered", []),
        FeatureDefinition("liquidity_shock_propagation", "流动性冲击传播", "float", "engineered", [])
    ]

    # 注册53维统一特征集
    registry.register_feature_set(
        name="unified_v2",
        features=unified_features,
        version="2.0.0",
        description="统一的53维高频交易特征集，基于BBO+交易数据的完整特征工程"
    )

    print(f"✅ 已注册统一特征集: {len(unified_features)}维特征")
    return registry


if __name__ == "__main__":
    # 创建标准注册表
    registry = create_standard_registry()

    # 显示统计信息
    fs = registry.get_feature_set("unified_v2")
    if fs:
        print(f"📊 特征统计:")
        print(f"  总特征数: {len(fs.features)}")

        categories = {}
        for f in fs.features:
            categories[f.category] = categories.get(f.category, 0) + 1

        for cat, count in categories.items():
            print(f"  {cat}: {count}维")

        print(f"  哈希签名: {fs.hash_signature}")
        print(f"✅ 特征注册表已创建: feature_registry.json")
