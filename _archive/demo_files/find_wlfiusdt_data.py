#!/usr/bin/env python3
"""
Comprehensive search for WLFIUSDT data across all tables
"""

import sys
import requests
import pandas as pd
import json
from datetime import datetime, timedelta

import os
import argparse

# ClickHouse connection details (read from env or args)
CH_HOST = os.getenv("CH_HOST", "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443")
CH_USER = os.getenv("CH_USER", "default")
CH_PASSWORD = os.getenv("CH_PASSWORD", "")
SYMBOL = os.getenv("CH_SYMBOL", "WLFIUSDT")

def execute_query(query: str):
    """Execute ClickHouse query"""
    try:
        response = requests.post(
            CH_HOST,
            auth=(CH_USER, CH_PASSWORD),
            data=query,
            headers={'Content-Type': 'text/plain'},
            timeout=30
        )
        
        if response.status_code == 200:
            return response.text.strip()
        else:
            print(f"❌ Query failed ({response.status_code}): {query[:50]}...")
            print(f"Response: {response.text}")
            return None
    except Exception as e:
        print(f"❌ Query error: {str(e)}")
        return None

def get_all_tables():
    """Get all tables from all databases"""
    print("📋 Getting ALL tables from ALL databases...")
    
    # Get all databases first
    databases_result = execute_query("SHOW DATABASES")
    if not databases_result:
        return []
    
    databases = [db.strip() for db in databases_result.split('\n') if db.strip()]
    print(f"Found {len(databases)} databases: {databases}")
    
    all_tables = []
    
    for db in databases:
        if db in ['system', 'information_schema', 'INFORMATION_SCHEMA']:
            continue  # Skip system databases
            
        print(f"\n🔍 Checking database: {db}")
        
        # Get tables in this database
        query = f"SHOW TABLES FROM {db}"
        tables_result = execute_query(query)
        
        if tables_result:
            tables = [t.strip() for t in tables_result.split('\n') if t.strip()]
            print(f"  Found {len(tables)} tables in {db}")
            
            for table in tables:
                full_table_name = f"{db}.{table}"
                all_tables.append(full_table_name)
                print(f"    • {table}")
        else:
            print(f"  No tables found in {db}")
    
    return all_tables

def describe_table(table_name):
    """Get detailed table structure"""
    print(f"\n🔍 Table structure: {table_name}")
    
    result = execute_query(f"DESCRIBE {table_name}")
    if result:
        print("  Columns:")
        for line in result.split('\n'):
            if line.strip():
                parts = line.split('\t')
                if len(parts) >= 2:
                    col_name, col_type = parts[0], parts[1]
                    print(f"    {col_name:20} {col_type}")
        return True
    return False

def search_wlfiusdt_comprehensive(table_name):
    """Comprehensive search for WLFIUSDT data"""
    print(f"\n🎯 Searching WLFIUSDT in {table_name}...")
    
    # Get table columns first
    desc_result = execute_query(f"DESCRIBE {table_name}")
    if not desc_result:
        return False, {}
    
    columns = []
    for line in desc_result.split('\n'):
        if line.strip():
            parts = line.split('\t')
            if parts:
                columns.append(parts[0])
    
    print(f"  Available columns: {', '.join(columns[:10])}{'...' if len(columns) > 10 else ''}")
    
    # Search patterns for target symbol (case-insensitive) and base token aliases
    s = SYMBOL or "WLFIUSDT"
    base = "WLFI"
    if s.lower().startswith("wlfi"):
        base = "WLFI"
    search_patterns = [s.upper(), s.lower(), base.upper(), base.lower(), base[:3].upper(), base[:3].lower()]
    
    # Common column names that might contain symbol data
    symbol_columns = [
        'symbol', 'Symbol', 'SYMBOL',
        'instId', 'instid', 'inst_id',
        'instrument', 'Instrument',
        'pair', 'Pair', 'trading_pair',
        'market', 'Market',
        'coin', 'Coin',
        'base', 'Base', 'quote', 'Quote'
    ]
    
    found_data = {}
    
    # Search in potential symbol columns
    for col in columns:
        if any(sym_col.lower() in col.lower() for sym_col in symbol_columns):
            print(f"  🔍 Checking column: {col}")
            
            for pattern in search_patterns:
                # Count matching records
                query = f"SELECT COUNT(*) FROM {table_name} WHERE {col} LIKE '%{pattern}%'"
                result = execute_query(query)
                
                if result and result.strip() != '0':
                    count = int(result.strip())
                    print(f"    ✅ Found {count} records with '{pattern}' in {col}")
                    found_data[f"{col}_{pattern}"] = count
                    
                    # Get sample values
                    sample_query = f"SELECT DISTINCT {col} FROM {table_name} WHERE {col} LIKE '%{pattern}%' LIMIT 10"
                    sample_result = execute_query(sample_query)
                    if sample_result:
                        values = [v.strip() for v in sample_result.split('\n') if v.strip()]
                        print(f"    📋 Sample values: {', '.join(values[:5])}")
    
    return len(found_data) > 0, found_data

def get_sample_wlfiusdt_data(table_name, symbol_column, limit=10):
    """Get sample WLFIUSDT data"""
    print(f"\n📊 Getting sample WLFIUSDT data from {table_name}...")
    
    query = f"SELECT * FROM {table_name} WHERE {symbol_column} LIKE '%WLFI%' ORDER BY 1 DESC LIMIT {limit}"
    result = execute_query(query)
    
    if result:
        lines = result.split('\n')
        print(f"  Sample data ({len([l for l in lines if l.strip()])} rows):")
        for i, line in enumerate(lines[:5], 1):
            if line.strip():
                # Truncate long lines
                display_line = line if len(line) <= 100 else line[:97] + "..."
                print(f"    {i}. {display_line}")
        
        if len(lines) > 5:
            print(f"    ... and {len([l for l in lines[5:] if l.strip()])} more rows")
        return True
    
    return False

def main():
    """Main comprehensive search function"""
    parser = argparse.ArgumentParser(description="Comprehensive WLFIUSDT Data Search")
    parser.add_argument("--host", default=CH_HOST)
    parser.add_argument("--user", default=CH_USER)
    parser.add_argument("--password", default=CH_PASSWORD)
    parser.add_argument("--symbol", default=SYMBOL)
    args = parser.parse_args()

    # override globals from args
    global CH_HOST, CH_USER, CH_PASSWORD
    CH_HOST, CH_USER, CH_PASSWORD, SYMBOL = args.host, args.user, args.password, args.symbol

    print("🚀 Comprehensive WLFIUSDT Data Search")
    print("=" * 50)
    
    # Test connection first
    print("🔗 Testing connection...")
    test_result = execute_query("SELECT 1")
    if not test_result:
        print("❌ Cannot connect to database")
        return
    print("✅ Connection successful!")
    
    # Get all tables
    all_tables = get_all_tables()
    if not all_tables:
        print("❌ No tables found")
        return
    
    print(f"\n📊 Total tables to search: {len(all_tables)}")
    
    # Search each table
    wlfiusdt_tables = []
    
    for i, table in enumerate(all_tables, 1):
        print(f"\n{'='*60}")
        print(f"[{i}/{len(all_tables)}] Analyzing: {table}")
        print(f"{'='*60}")
        
        # Skip system tables
        if any(sys_word in table.lower() for sys_word in ['system', 'information_schema', 'query_log']):
            print("  ⏭️ Skipping system table")
            continue
        
        try:
            # Get table structure
            if describe_table(table):
                # Search for WLFIUSDT
                found, data_summary = search_wlfiusdt_comprehensive(table)
                
                if found:
                    wlfiusdt_tables.append({
                        'table': table,
                        'data': data_summary
                    })
                    
                    # Get sample data from the first matching column
                    for key in data_summary.keys():
                        col_name = key.split('_')[0]
                        get_sample_wlfiusdt_data(table, col_name, 5)
                        break
                else:
                    print("  ❌ No WLFIUSDT data found")
            else:
                print("  ❌ Cannot access table structure")
                
        except Exception as e:
            print(f"  ❌ Error processing table: {str(e)}")
    
    # Final summary
    print(f"\n{'='*60}")
    print("📋 COMPREHENSIVE SEARCH RESULTS")
    print(f"{'='*60}")
    
    print(f"Total tables searched: {len(all_tables)}")
    print(f"Tables with WLFIUSDT data: {len(wlfiusdt_tables)}")
    
    if wlfiusdt_tables:
        print("\n✅ WLFIUSDT DATA FOUND IN:")
        for table_info in wlfiusdt_tables:
            table_name = table_info['table']
            data_summary = table_info['data']
            print(f"\n🎯 {table_name}")
            for key, count in data_summary.items():
                col_name, pattern = key.rsplit('_', 1)
                print(f"  • {col_name}: {count:,} records matching '{pattern}'")
        
        print(f"\n🚀 SUCCESS! You can now use these tables for WLFI analysis:")
        for table_info in wlfiusdt_tables:
            print(f"  • {table_info['table']}")
            
    else:
        print("\n❌ NO WLFIUSDT DATA FOUND")
        print("Possible reasons:")
        print("  • Symbol might be named differently")
        print("  • Data might be in a different format")
        print("  • Tables might need different access permissions")
    
    print(f"\n🏁 Search completed!")

if __name__ == "__main__":
    main()
