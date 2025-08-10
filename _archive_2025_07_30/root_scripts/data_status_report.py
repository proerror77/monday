#!/usr/bin/env python3
"""
HFT 系統數據狀態報告
==================

實時數據收集和狀態監控報告
"""

import asyncio
import json
import time
from datetime import datetime, timedelta
import requests

def format_number(num):
    """格式化數字顯示"""
    if num >= 1000000:
        return f"{num/1000000:.1f}M"
    elif num >= 1000:
        return f"{num/1000:.1f}K"
    else:
        return str(int(num))

def get_clickhouse_stats():
    """獲取 ClickHouse 數據統計"""
    try:
        # 總體統計
        total_query = "SELECT COUNT(*) as total_records, MIN(timestamp) as first_record, MAX(timestamp) as last_record FROM market_data"
        total_response = requests.post("http://localhost:8123/", data=total_query)
        total_data = total_response.text.strip().split('\t')
        
        # 按交易對統計
        symbol_query = """
        SELECT 
            symbol,
            COUNT(*) as records,
            MIN(timestamp) as first_record,
            MAX(timestamp) as last_record,
            AVG(price) as avg_price,
            AVG(volume) as avg_volume
        FROM market_data 
        GROUP BY symbol 
        ORDER BY symbol
        FORMAT TabSeparated
        """
        symbol_response = requests.post("http://localhost:8123/", data=symbol_query)
        
        # 買賣分布統計
        side_query = """
        SELECT 
            side,
            COUNT(*) as records,
            COUNT(*) * 100.0 / (SELECT COUNT(*) FROM market_data) as percentage
        FROM market_data 
        GROUP BY side
        FORMAT TabSeparated
        """
        side_response = requests.post("http://localhost:8123/", data=side_query)
        
        # 最近數據采樣
        recent_query = """
        SELECT timestamp, symbol, price, volume, side 
        FROM market_data 
        ORDER BY timestamp DESC 
        LIMIT 5
        FORMAT TabSeparated
        """
        recent_response = requests.post("http://localhost:8123/", data=recent_query)
        
        return {
            'total': total_data,
            'symbols': symbol_response.text.strip().split('\n') if symbol_response.text.strip() else [],
            'sides': side_response.text.strip().split('\n') if side_response.text.strip() else [],
            'recent': recent_response.text.strip().split('\n') if recent_response.text.strip() else []
        }
    except Exception as e:
        return {'error': str(e)}

def check_redis_status():
    """檢查 Redis 狀態"""
    try:
        import redis
        client = redis.Redis(host='localhost', port=6379, decode_responses=True)
        client.ping()
        
        # 檢查活躍連接數
        info = client.info()
        connected_clients = info.get('connected_clients', 0)
        used_memory = info.get('used_memory_human', 'N/A')
        
        return {
            'status': 'connected',
            'clients': connected_clients,
            'memory': used_memory
        }
    except Exception as e:
        return {'status': 'error', 'error': str(e)}

def display_data_status():
    """顯示數據狀態報告"""
    print("\033[2J\033[H")  # 清屏
    print("📊 HFT 系統數據狀態報告")
    print("=" * 60)
    print(f"🕐 生成時間: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    print()
    
    # ClickHouse 數據統計
    print("💾 ClickHouse 數據庫狀態")
    print("-" * 30)
    
    ch_stats = get_clickhouse_stats()
    if 'error' in ch_stats:
        print(f"❌ ClickHouse 連接錯誤: {ch_stats['error']}")
    else:
        # 總體統計
        total_records, first_record, last_record = ch_stats['total']
        print(f"📈 總記錄數: {format_number(int(total_records))} 條")
        print(f"⏰ 數據時間跨度: {first_record} ~ {last_record}")
        
        # 計算數據採集時長和速率
        first_dt = datetime.fromisoformat(first_record.replace(' ', 'T'))
        last_dt = datetime.fromisoformat(last_record.replace(' ', 'T'))
        duration = (last_dt - first_dt).total_seconds()
        rate = int(total_records) / duration if duration > 0 else 0
        
        print(f"📊 採集時長: {duration/60:.1f} 分鐘")
        print(f"🚀 平均速率: {rate:.1f} 記錄/秒")
        
        print("\n💰 交易對數據分布:")
        for symbol_line in ch_stats['symbols']:
            if symbol_line:
                parts = symbol_line.split('\t')
                if len(parts) >= 6:
                    symbol, records, first, last, avg_price, avg_volume = parts
                    print(f"  • {symbol}: {format_number(int(records))} 條, 均價: ${float(avg_price):.2f}")
        
        print("\n📊 買賣分布:")
        for side_line in ch_stats['sides']:
            if side_line:
                parts = side_line.split('\t')
                if len(parts) >= 3:
                    side, records, percentage = parts
                    icon = "🟢" if side == "buy" else "🔴"
                    print(f"  {icon} {side.upper()}: {format_number(int(records))} 條 ({float(percentage):.1f}%)")
        
        print("\n🕒 最新數據示例:")
        for i, recent_line in enumerate(ch_stats['recent'][:3]):
            if recent_line:
                parts = recent_line.split('\t')
                if len(parts) >= 5:
                    timestamp, symbol, price, volume, side = parts
                    icon = "🟢" if side == "buy" else "🔴"
                    print(f"  {i+1}. {icon} {timestamp} | {symbol} | ${float(price):.2f} | Vol: {float(volume):.3f}")
    
    print()
    
    # Redis 狀態
    print("🗄️  Redis 緩存狀態")
    print("-" * 30)
    
    redis_status = check_redis_status()
    if redis_status['status'] == 'connected':
        print(f"✅ Redis 連接: 正常")
        print(f"👥 活躍連接: {redis_status['clients']} 個")
        print(f"💾 內存使用: {redis_status['memory']}")
    else:
        print(f"❌ Redis 連接: {redis_status.get('error', '異常')}")
    
    print()
    
    # 系統性能指標
    print("⚡ 系統性能指標")
    print("-" * 30)
    if 'error' not in ch_stats and int(ch_stats['total'][0]) > 0:
        total_records = int(ch_stats['total'][0])
        if total_records > 6000:
            print("🟢 數據採集: 優秀 (>6K 記錄)")
        elif total_records > 3000:
            print("🟡 數據採集: 良好 (>3K 記錄)")
        else:
            print("🔴 數據採集: 需要改進 (<3K 記錄)")
        
        # 數據完整性評估
        symbols_count = len([line for line in ch_stats['symbols'] if line])
        if symbols_count >= 3:
            print("🟢 數據完整性: 優秀 (多交易對)")
        else:
            print("🟡 數據完整性: 可以改進")
    else:
        print("❌ 無法評估性能 (數據不足)")
    
    print()
    print("📝 備註: 系統正在實時收集 Bitget 交易所真實市場數據")
    print("🔄 數據實時同步到 ClickHouse 時序數據庫和 Redis 緩存")

if __name__ == "__main__":
    display_data_status()