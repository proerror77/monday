#!/usr/bin/env python3
"""
WLFI Data Explorer and Analysis Script
Connects to your ClickHouse cloud instance and explores WLFI trading data
"""

import sys
import os
import pandas as pd
import numpy as np
from datetime import datetime, timedelta
import matplotlib.pyplot as plt
import seaborn as sns
from pathlib import Path

# Add ml_workspace to path
sys.path.append(str(Path(__file__).parent / "ml_workspace"))

from db.clickhouse_client import ClickHouseClient
import logging

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)


def main():
    """Main data exploration function"""
    print("🚀 WLFI Data Explorer Starting...")
    print("=" * 60)
    
    # Initialize ClickHouse client
    print("\n📊 Connecting to ClickHouse Cloud...")
    
    # You'll need to provide the password
    password = input("Please enter your ClickHouse password: ").strip()
    
    client = ClickHouseClient(
        host="https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443",
        username="default",
        password=password
    )
    
    # Test connection
    print("🔗 Testing connection...")
    if not client.test_connection():
        print("❌ Connection failed! Please check your credentials.")
        return
    
    print("✅ Connection successful!")
    
    # Show all tables
    print("\n📋 Available tables:")
    tables = client.show_tables()
    for i, table in enumerate(tables, 1):
        print(f"  {i:2d}. {table}")
    
    if not tables:
        print("⚠️ No tables found in database")
        return
    
    # Look for WLFI-related tables
    print("\n🔍 Searching for WLFI-related data...")
    wlfi_tables = client.find_wlfi_tables()
    
    if wlfi_tables:
        print(f"✅ Found WLFI data in {len(wlfi_tables)} table(s):")
        for table in wlfi_tables:
            print(f"  • {table}")
    else:
        print("❌ No WLFI-specific tables found")
        print("📊 Let's check all tables for WLFI symbol data...")
        
        # Check each table for WLFI data
        for table in tables[:5]:  # Check first 5 tables to avoid timeout
            print(f"\n🔎 Checking table: {table}")
            try:
                # Get table structure
                desc = client.describe_table(table)
                print(f"  Columns: {', '.join(desc['name'].tolist())}")
                
                # Try to find WLFI data
                sample_queries = [
                    f"SELECT COUNT(*) as cnt FROM {table} WHERE symbol LIKE '%WLFI%' LIMIT 1",
                    f"SELECT COUNT(*) as cnt FROM {table} WHERE instId LIKE '%WLFI%' LIMIT 1",
                    f"SELECT DISTINCT symbol FROM {table} WHERE symbol LIKE '%WLF%' LIMIT 10",
                    f"SELECT DISTINCT instId FROM {table} WHERE instId LIKE '%WLF%' LIMIT 10"
                ]
                
                for query in sample_queries:
                    try:
                        result = client.execute_query(query)
                        if result and result.strip() and result.strip() != "0":
                            print(f"  ✅ Found potential WLFI data: {result.strip()}")
                            wlfi_tables.append(table)
                            break
                    except:
                        continue
                        
            except Exception as e:
                print(f"  ❌ Error checking table {table}: {str(e)}")
    
    # If we found WLFI data, analyze it
    if wlfi_tables:
        print(f"\n📈 Analyzing WLFI data from {len(wlfi_tables)} table(s)...")
        
        for table in wlfi_tables[:3]:  # Analyze first 3 tables
            print(f"\n📊 Table: {table}")
            print("-" * 40)
            
            try:
                # Get recent WLFI data
                data = client.get_wlfi_data(table, hours_back=24, limit=1000)
                
                if data.empty:
                    print("  📭 No recent data found")
                    continue
                
                print(f"  📏 Data shape: {data.shape}")
                print(f"  📅 Date range: {data['ts'].min()} to {data['ts'].max()}")
                print(f"  🔢 Columns: {', '.join(data.columns)}")
                
                # Show sample data
                print("\n  📋 Sample data:")
                print(data.head().to_string(index=False))
                
                # Basic statistics
                if 'price' in data.columns:
                    price_col = 'price'
                elif 'last' in data.columns:
                    price_col = 'last'
                else:
                    price_col = None
                
                if price_col and not data[price_col].empty:
                    print(f"\n  📊 Price statistics ({price_col}):")
                    print(f"    Min:  {data[price_col].min():.6f}")
                    print(f"    Max:  {data[price_col].max():.6f}")
                    print(f"    Mean: {data[price_col].mean():.6f}")
                    print(f"    Std:  {data[price_col].std():.6f}")
                
                # Save data for further analysis
                output_file = f"wlfi_data_{table}_{datetime.now().strftime('%Y%m%d_%H%M%S')}.csv"
                data.to_csv(output_file, index=False)
                print(f"  💾 Data saved to: {output_file}")
                
            except Exception as e:
                print(f"  ❌ Error analyzing table {table}: {str(e)}")
    
    # Try to get specific market data types
    print("\n🎯 Attempting to get specific market data types...")
    
    # L1 BBO data
    try:
        print("\n📊 Getting L1 BBO data...")
        l1_data = client.get_l1_bbo_data("WLFI", hours_back=24)
        if not l1_data.empty:
            print(f"✅ Found {len(l1_data)} L1 BBO records")
            l1_data.to_csv("wlfi_l1_bbo_data.csv", index=False)
            print("💾 L1 data saved to: wlfi_l1_bbo_data.csv")
        else:
            print("❌ No L1 BBO data found")
    except Exception as e:
        print(f"❌ L1 BBO error: {str(e)}")
    
    # L2 Order Book data
    try:
        print("\n📈 Getting L2 Order Book data...")
        l2_data = client.get_l2_orderbook_data("WLFI", hours_back=24)
        if not l2_data.empty:
            print(f"✅ Found {len(l2_data)} L2 order book records")
            l2_data.to_csv("wlfi_l2_orderbook_data.csv", index=False)
            print("💾 L2 data saved to: wlfi_l2_orderbook_data.csv")
        else:
            print("❌ No L2 order book data found")
    except Exception as e:
        print(f"❌ L2 order book error: {str(e)}")
    
    # Trades data
    try:
        print("\n💹 Getting Trades data...")
        trades_data = client.get_trades_data("WLFI", hours_back=24)
        if not trades_data.empty:
            print(f"✅ Found {len(trades_data)} trade records")
            trades_data.to_csv("wlfi_trades_data.csv", index=False)
            print("💾 Trades data saved to: wlfi_trades_data.csv")
        else:
            print("❌ No trades data found")
    except Exception as e:
        print(f"❌ Trades data error: {str(e)}")
    
    print("\n🎉 Data exploration completed!")
    print("📁 Check the generated CSV files for detailed analysis.")
    

if __name__ == "__main__":
    main()