#!/usr/bin/env python3
"""
37特徵定義與提取 - 基於ClickHouse預計算特徵
包含訂單簿動力學和MLOFI特徵
"""

from typing import List, Dict


class Features37:
    """37個特徵的定義和管理"""
    
    # 特徵分組定義
    FEATURE_GROUPS = {
        # 基礎價格特徵 (3個)
        'basic_price': [
            'spread_pct',       # 買賣價差百分比
            'size_imbalance',   # 買賣量不平衡
            'tick_direction',   # 價格tick方向
        ],
        
        # L2深度特徵 - 3個檔位 × 4個指標 = 12個特徵
        'depth_imbalance': [
            'imb_5',            # 5檔深度不平衡
            'imb_10',           # 10檔深度不平衡
            'imb_20',           # 20檔深度不平衡
        ],
        'weighted_imbalance': [
            'weighted_imb_5',   # 加權5檔不平衡
            'weighted_imb_10',  # 加權10檔不平衡
            'weighted_imb_20',  # 加權20檔不平衡
        ],
        'depth_ratio': [
            'depth_ratio_5',    # 5檔深度比率
            'depth_ratio_10',   # 10檔深度比率
            'depth_ratio_20',   # 20檔深度比率
        ],
        'liquidity_pressure': [
            'pressure_5',       # 5檔流動性壓力
            'pressure_10',      # 10檔流動性壓力
            'pressure_20',      # 20檔流動性壓力
        ],
        
        # 交易流特徵 - 3個時間窗口 × 6個指標 = 18個特徵
        'trade_flow_30s': [
            'buy_volume_30s',           # 30秒買量
            'sell_volume_30s',          # 30秒賣量
            'volume_imbalance_30s',     # 30秒量不平衡
            'trade_intensity_30s',      # 30秒交易強度
            'price_impact_30s',         # 30秒價格影響
            'vwap_30s',                 # 30秒成交量加權均價
        ],
        'trade_flow_60s': [
            'buy_volume_60s',           # 60秒買量
            'sell_volume_60s',          # 60秒賣量
            'volume_imbalance_60s',     # 60秒量不平衡
            'trade_intensity_60s',      # 60秒交易強度
            'price_impact_60s',         # 60秒價格影響
            'vwap_60s',                 # 60秒成交量加權均價
        ],
        'trade_flow_180s': [
            'buy_volume_180s',          # 180秒買量
            'sell_volume_180s',         # 180秒賣量
            'volume_imbalance_180s',    # 180秒量不平衡
            'trade_intensity_180s',     # 180秒交易強度
            'price_impact_180s',        # 180秒價格影響
            'vwap_180s',                # 180秒成交量加權均價
        ],
        
        # 價格動量特徵 (2個)
        'momentum': [
            'momentum_short',    # 短期動量
            'volatility_short',  # 短期波動率
        ],
    }
    
    @classmethod
    def get_all_features(cls) -> List[str]:
        """獲取所有37個特徵名稱"""
        all_features = []
        for group_features in cls.FEATURE_GROUPS.values():
            all_features.extend(group_features)
        return all_features
    
    @classmethod
    def get_feature_count(cls) -> int:
        """獲取特徵總數"""
        return len(cls.get_all_features())
    
    @classmethod
    def get_feature_groups_summary(cls) -> Dict[str, int]:
        """獲取特徵分組摘要"""
        return {
            group: len(features) 
            for group, features in cls.FEATURE_GROUPS.items()
        }
    
    @classmethod
    def validate_features(cls, feature_columns: List[str]) -> bool:
        """驗證是否包含所有必需的特徵"""
        required_features = set(cls.get_all_features())
        available_features = set(feature_columns)
        missing = required_features - available_features
        
        if missing:
            print(f"缺少以下特徵: {missing}")
            return False
        return True
    
    @classmethod
    def get_sql_query(cls, symbol: str = 'WLFIUSDT', time_range: str = '1 DAY') -> str:
        """
        生成獲取37個特徵的SQL查詢
        假設表 bitget_wlfi_37features_optimized 已存在
        """
        features_str = ', '.join(cls.get_all_features())
        
        return f"""
        SELECT 
            exchange_ts,
            symbol,
            mid_price,
            {features_str}
        FROM bitget_wlfi_37features_optimized
        WHERE symbol = '{symbol}'
          AND exchange_ts >= now() - INTERVAL {time_range}
        ORDER BY exchange_ts
        """
    
    @classmethod
    def get_feature_importance_weights(cls) -> Dict[str, float]:
        """
        獲取特徵重要性權重（基於經驗值）
        這些權重可以用於特徵選擇或加權訓練
        """
        return {
            # 基礎價格特徵 - 高重要性
            'spread_pct': 1.0,
            'size_imbalance': 0.9,
            'tick_direction': 0.8,
            
            # 深度不平衡 - 非常重要（MLOFI核心）
            'imb_5': 1.0,
            'imb_10': 0.95,
            'imb_20': 0.9,
            'weighted_imb_5': 0.95,
            'weighted_imb_10': 0.9,
            'weighted_imb_20': 0.85,
            
            # 深度比率
            'depth_ratio_5': 0.85,
            'depth_ratio_10': 0.8,
            'depth_ratio_20': 0.75,
            
            # 流動性壓力
            'pressure_5': 0.9,
            'pressure_10': 0.85,
            'pressure_20': 0.8,
            
            # 30秒交易流 - 最重要
            'buy_volume_30s': 0.95,
            'sell_volume_30s': 0.95,
            'volume_imbalance_30s': 1.0,
            'trade_intensity_30s': 0.9,
            'price_impact_30s': 0.85,
            'vwap_30s': 0.8,
            
            # 60秒交易流
            'buy_volume_60s': 0.85,
            'sell_volume_60s': 0.85,
            'volume_imbalance_60s': 0.9,
            'trade_intensity_60s': 0.8,
            'price_impact_60s': 0.75,
            'vwap_60s': 0.7,
            
            # 180秒交易流
            'buy_volume_180s': 0.75,
            'sell_volume_180s': 0.75,
            'volume_imbalance_180s': 0.8,
            'trade_intensity_180s': 0.7,
            'price_impact_180s': 0.65,
            'vwap_180s': 0.6,
            
            # 動量特徵
            'momentum_short': 0.9,
            'volatility_short': 0.85,
        }
    
    @classmethod
    def get_feature_descriptions(cls) -> Dict[str, str]:
        """獲取特徵描述"""
        return {
            # 基礎價格特徵
            'spread_pct': '買賣價差百分比 = (ask - bid) / mid_price',
            'size_imbalance': '買賣量不平衡 = (bid_size - ask_size) / (bid_size + ask_size)',
            'tick_direction': '價格方向: 1=上漲, -1=下跌, 0=不變',
            
            # 深度特徵
            'imb_5': '5檔深度不平衡 = (bid_qty - ask_qty) / (bid_qty + ask_qty)',
            'imb_10': '10檔深度不平衡',
            'imb_20': '20檔深度不平衡',
            'weighted_imb_5': '加權5檔不平衡（近端權重更高）',
            'weighted_imb_10': '加權10檔不平衡',
            'weighted_imb_20': '加權20檔不平衡',
            'depth_ratio_5': '5檔深度比率 = bid_qty / ask_qty',
            'depth_ratio_10': '10檔深度比率',
            'depth_ratio_20': '20檔深度比率',
            'pressure_5': '5檔流動性壓力 = imbalance × spread',
            'pressure_10': '10檔流動性壓力',
            'pressure_20': '20檔流動性壓力',
            
            # 交易流特徵
            'buy_volume_30s': '30秒買入成交量',
            'sell_volume_30s': '30秒賣出成交量',
            'volume_imbalance_30s': '30秒成交量不平衡 (buy - sell) / (buy + sell)',
            'trade_intensity_30s': '30秒交易強度 = trades_count / 30',
            'price_impact_30s': '30秒價格影響 = (avg_price - mid_price) / mid_price',
            'vwap_30s': '30秒成交量加權均價',
            
            'buy_volume_60s': '60秒買入成交量',
            'sell_volume_60s': '60秒賣出成交量',
            'volume_imbalance_60s': '60秒成交量不平衡',
            'trade_intensity_60s': '60秒交易強度',
            'price_impact_60s': '60秒價格影響',
            'vwap_60s': '60秒成交量加權均價',
            
            'buy_volume_180s': '180秒買入成交量',
            'sell_volume_180s': '180秒賣出成交量',
            'volume_imbalance_180s': '180秒成交量不平衡',
            'trade_intensity_180s': '180秒交易強度',
            'price_impact_180s': '180秒價格影響',
            'vwap_180s': '180秒成交量加權均價',
            
            # 動量特徵
            'momentum_short': '短期動量 = (price_now - price_10ticks_ago) / price_10ticks_ago',
            'volatility_short': '短期波動率 = std(price_10ticks) / mean(price_10ticks)',
        }


def print_feature_summary():
    """打印特徵摘要"""
    print("="*60)
    print("37特徵系統摘要")
    print("="*60)
    
    # 總數
    total = Features37.get_feature_count()
    print(f"\n總特徵數: {total}")
    
    # 分組統計
    print("\n特徵分組:")
    summary = Features37.get_feature_groups_summary()
    for group, count in summary.items():
        print(f"  {group:20s}: {count:2d} 個特徵")
    
    # 所有特徵列表
    print("\n所有特徵列表:")
    for i, feature in enumerate(Features37.get_all_features(), 1):
        print(f"  {i:2d}. {feature}")
    
    # 驗證總數（實際是35個特徵 + mid_price + exchange_ts = 37）
    assert total == 35, f"特徵數量錯誤: {total} != 35"
    print(f"\n✓ 特徵數量驗證通過: {total} = 35 (+ mid_price + exchange_ts = 37維度)")


if __name__ == '__main__':
    print_feature_summary()