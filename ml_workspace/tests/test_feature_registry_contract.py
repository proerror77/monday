#!/usr/bin/env python3
import re

from utils.feature_registry import get_default_ms_feature_names
from utils.ch_queries import build_feature_sql


def test_registry_nonempty_and_unique():
    feats = get_default_ms_feature_names()
    assert isinstance(feats, list) and len(feats) > 5
    assert len(feats) == len(set(feats)), "feature names must be unique"


def test_millisecond_sql_contains_registry_features():
    sql = build_feature_sql(symbol="TEST", start_ms=0, end_ms=1000, use_millisecond_features=True)
    feats = get_default_ms_feature_names()
    # simple presence check; alias form in SQL should include feature names
    missing = [f for f in feats if f not in sql]
    assert not missing, f"These registry features not present in SQL builder: {missing}"

