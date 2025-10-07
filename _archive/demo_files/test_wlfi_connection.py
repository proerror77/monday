#!/usr/bin/env python3
"""
Simple WLFI connection test and data exploration
"""

import sys
import requests
import pandas as pd
import json
from datetime import datetime, timedelta

# ClickHouse connection details
CH_HOST = "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443"
CH_USER = "default"
CH_PASSWORD = "sIiFK.4ygf.9R"

def test_connection():
    """Test ClickHouse connection"""
    print("🔗 Testing ClickHouse connection...")
    
    try:
        response = requests.post(
            CH_HOST,
            auth=(CH_USER, CH_PASSWORD),
            data="SELECT 1 as test",
            headers={'Content-Type': 'text/plain'},
            timeout=10
        )
        
        if response.status_code == 200:
            print("✅ Connection successful!")
            return True
        else:
            print(f"❌ Connection failed: {response.status_code}")
            print(f"Response: {response.text}")
            return False
    except Exception as e:
        print(f"❌ Connection error: {str(e)}")
        return False

def get_tables():
    """Get list of available tables"""
    print("\n📋 Getting available tables...")
    
    try:
        response = requests.post(
            CH_HOST,
            auth=(CH_USER, CH_PASSWORD),
            data="SHOW TABLES",
            headers={'Content-Type': 'text/plain'},
            timeout=10
        )
        
        if response.status_code == 200:
            tables = [line.strip() for line in response.text.split('\n') if line.strip()]
            print(f"✅ Found {len(tables)} tables:")
            for i, table in enumerate(tables, 1):
                print(f"  {i:2d}. {table}")
            return tables
        else:
            print(f"❌ Failed to get tables: {response.status_code}")
            return []
    except Exception as e:
        print(f"❌ Error getting tables: {str(e)}")
        return []

def describe_table(table_name):
    """Get table structure"""
    print(f"\n🔍 Describing table: {table_name}")
    
    try:
        response = requests.post(
            CH_HOST,
            auth=(CH_USER, CH_PASSWORD),
            data=f"DESCRIBE TABLE {table_name}",
            headers={'Content-Type': 'text/plain'},
            timeout=10
        )
        
        if response.status_code == 200:
            lines = response.text.strip().split('\n')
            print("  Columns:")
            for line in lines:
                if line.strip():
                    parts = line.split('\t')
                    if len(parts) >= 2:
                        print(f"    {parts[0]} ({parts[1]})")
            return True
        else:
            print(f"❌ Failed to describe table: {response.status_code}")
            return False
    except Exception as e:
        print(f"❌ Error describing table: {str(e)}")
        return False

def search_wlfi_data(table_name):
    """Search for WLFI data in a table"""
    print(f"\n🔍 Searching for WLFI data in {table_name}...")
    
    search_queries = [
        f"SELECT COUNT(*) FROM {table_name} WHERE symbol LIKE '%WLFI%'",
        f"SELECT COUNT(*) FROM {table_name} WHERE instId LIKE '%WLFI%'",
    ]
    
    for query in search_queries:
        try:
            response = requests.post(
                CH_HOST,
                auth=(CH_USER, CH_PASSWORD),
                data=query,
                headers={'Content-Type': 'text/plain'},
                timeout=10
            )
            
            if response.status_code == 200:
                count = response.text.strip()
                if count and int(count) > 0:
                    print(f"  ✅ Found {count} WLFI records")
                    return True
            
        except Exception as e:
            continue
    
    print(f"  ❌ No WLFI data found")
    return False

def get_sample_data(table_name, limit=10):
    """Get sample data from table"""
    print(f"\n📊 Getting sample data from {table_name}...")
    
    try:
        query = f"SELECT * FROM {table_name} LIMIT {limit}"
        response = requests.post(
            CH_HOST,
            auth=(CH_USER, CH_PASSWORD),
            data=query,
            headers={'Content-Type': 'text/plain'},
            timeout=10
        )
        
        if response.status_code == 200:
            lines = response.text.strip().split('\n')
            print(f"  Sample data ({len(lines)} rows):")
            for i, line in enumerate(lines[:5], 1):
                if line.strip():
                    print(f"    {i}. {line}")
            if len(lines) > 5:
                print(f"    ... and {len(lines) - 5} more rows")
            return True
        else:
            print(f"❌ Failed to get sample data: {response.status_code}")
            return False
    except Exception as e:
        print(f"❌ Error getting sample data: {str(e)}")
        return False

def main():
    """Main function"""
    print("🚀 WLFI Data Connection Test")
    print("=" * 40)
    
    # Test connection
    if not test_connection():
        print("\n❌ Cannot proceed without database connection")
        return
    
    # Get tables
    tables = get_tables()
    if not tables:
        print("\n❌ No tables found")
        return
    
    # Check first few tables for structure and WLFI data
    wlfi_tables = []
    for table in tables[:5]:  # Check first 5 tables
        describe_table(table)
        if search_wlfi_data(table):
            wlfi_tables.append(table)
        get_sample_data(table, 3)
        print()
    
    # Summary
    print("📋 SUMMARY")
    print("-" * 20)
    print(f"Total tables found: {len(tables)}")
    print(f"Tables checked: {min(5, len(tables))}")
    print(f"Tables with WLFI data: {len(wlfi_tables)}")
    
    if wlfi_tables:
        print(f"\n✅ WLFI data found in:")
        for table in wlfi_tables:
            print(f"  • {table}")
        print(f"\n🎯 You can now use these tables for analysis!")
    else:
        print(f"\n❌ No WLFI data found in the checked tables")
        print(f"💡 You may need to check more tables or verify the symbol name")
    
    print(f"\n🏁 Test completed!")

if __name__ == "__main__":
    main()