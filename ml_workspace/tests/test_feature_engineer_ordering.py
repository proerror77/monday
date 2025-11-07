#!/usr/bin/env python3
from components.feature_engineering import order_feature_columns
from utils.feature_registry import get_default_ms_feature_names


def test_order_feature_columns_respects_registry_order():
    registry = get_default_ms_feature_names()
    # Simulate shuffled + extras + metadata
    shuffled = list(reversed(registry[:5])) + ["exchange_ts", "mid_price"] + registry[5:10] + ["extra_col_a"]
    ordered = order_feature_columns(shuffled)
    expected_prefix = registry[:10]  # intersection should be first 10 in registry order
    assert ordered[:10] == expected_prefix
    assert ordered[-1] == "extra_col_a"

