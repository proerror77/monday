#!/usr/bin/env python3
"""
测试 Redis 集成 - 验证 Python 代理能否从 Redis 获取快速 Rust 数据
"""

import redis
import json
import time
import rust_hft_py as rust_hft

def main():
    print("🧪 测试 Rust→Redis→Python 快速数据集成")
    
    # 1. 连接 Redis
    try:
        redis_client = redis.Redis(host='localhost', port=6379, db=0)
        redis_client.ping()
        print("✅ Redis 连接成功")
    except Exception as e:
        print(f"❌ Redis 连接失败: {e}")
        return
    
    # 2. 模拟 Rust 处理器发布的快速数据
    mock_rust_data = {
        'symbol': 'BTCUSDT',
        'mid_price': 97150.5,
        'best_bid': 97150.0,
        'best_ask': 97151.0,
        'spread': 1.0,
        'spread_bps': 0.103,
        'bid_levels': 25,
        'ask_levels': 25,
        'last_update': int(time.time() * 1000000),
        'last_sequence': 12345,
        'is_valid': True,
        'data_quality_score': 0.95,
        'timestamp_us': int(time.time() * 1000000),
        'source': 'rust_fast_processor_simulation'
    }
    
    # 3. 发布到 Redis (模拟 Rust 处理器)
    redis_key = 'hft:orderbook:BTCUSDT'
    redis_data = json.dumps(mock_rust_data)
    redis_client.set(redis_key, redis_data, ex=10)  # 10秒过期
    print(f"📡 已发布模拟 Rust 数据到 Redis: {redis_key}")
    
    # 4. 测试 Python 代理提取 (模拟 Python 代理)
    processor = rust_hft.DataProcessor()
    result = processor.initialize_real_data('BTCUSDT')
    print(f"🚀 Python 初始化结果: {result}")
    
    try:
        # 调用新的 Redis 集成方法
        features = processor.extract_real_features('BTCUSDT')
        print(f"✅ 成功获取 Redis 快速数据!")
        print(f"  📊 特征数量: {len(features)}")
        print(f"  💰 中间价: ${features.get('mid_price', 0):.2f}")
        print(f"  📈 买卖价差: ${features.get('spread', 0):.2f}")
        print(f"  ⚡ 获取延迟: {features.get('latency_ms', 0):.3f}ms")
        print(f"  🎯 数据质量: {features.get('data_quality_score', 0):.2f}")
        print(f"  🔗 数据源: {features.get('source', '未知')}")
        print(f"  ✅ 数据有效: {features.get('is_valid', False) == 1.0}")
        
        # 检查是否是真实快速数据 (不是模拟数据)
        if features.get('data_source') == 1.0:
            print("🚀 确认: 成功获取真实快速数据!")
        else:
            print("⚠️  警告: 仍在使用模拟数据")
            
    except Exception as e:
        print(f"❌ Redis 提取失败: {e}")
        import traceback
        traceback.print_exc()
    
    print("\n🎯 测试完成!")

if __name__ == "__main__":
    main()