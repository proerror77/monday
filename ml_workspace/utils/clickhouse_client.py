"""
ClickHouse Client for WLFI Data Analysis
Connects to ClickHouse Cloud and provides data query capabilities
"""

import requests
import pandas as pd
import numpy as np
from datetime import datetime, timedelta
from typing import Dict, List, Optional, Any
import logging
from urllib.parse import urljoin
import json

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class ClickHouseClient:
    """ClickHouse cloud client for data operations"""

    def __init__(
        self,
        host: Optional[str] = None,
        username: Optional[str] = None,
        password: Optional[str] = None,
        database: Optional[str] = None,
    ):
        """Initialize ClickHouse client.

        Prefers environment-based configuration via utils.config if parameters are not provided.
        """
        try:
            from utils.config import get_clickhouse_settings

            ch = get_clickhouse_settings()
            env_host = ch.host
            env_username = ch.username
            env_password = ch.password
            env_database = ch.database
        except Exception:
            # Fallback to neutral defaults if config is unavailable
            env_host = "https://localhost:8443"
            env_username = "default"
            env_password = ""
            env_database = "default"

        self.host = host or env_host
        self.username = username or env_username
        self.password = password or env_password
        self.database = database or env_database
        self.auth = (self.username, self.password) if self.password else (self.username, "")
        
    def execute_query(self, query: str, format: str = "TabSeparated", timeout: int = 30) -> str:
        """
        Execute raw query and return response text
        
        Args:
            query: SQL query string
            format: Output format (TabSeparated, JSON, CSV, etc.)
            timeout: Request timeout in seconds (default 30)
            
        Returns:
            Query result as string
        """
        try:
            # Only add FORMAT for SELECT queries, not for DDL/DML operations
            query_upper = query.strip().upper()
            if query_upper.startswith('SELECT') and "FORMAT" not in query_upper:
                query = query + f" FORMAT {format}"
                    
            response = requests.post(
                self.host,
                auth=self.auth,
                params={'database': self.database},
                data=query.encode('utf-8'),
                headers={'Content-Type': 'text/plain'},
                timeout=(30, 600),  # 连接超时30秒，读取超时600秒(10分钟)
                stream=True  # 启用流式传输
            )
            
            if response.status_code == 200:
                logger.info(f"Query executed successfully: {len(response.text)} chars returned")
                return response.text
            else:
                logger.error(f"Query failed: {response.status_code} - {response.text}")
                raise Exception(f"Query failed with status {response.status_code}: {response.text}")
                
        except Exception as e:
            logger.error(f"Query execution error: {str(e)}")
            raise

    def execute(self, query: str) -> str:
        """
        Simple execute method for DDL/DML operations

        Args:
            query: SQL statement to execute

        Returns:
            Query result as string
        """
        return self.execute_query(query)

    def query_to_dataframe(self, query: str) -> pd.DataFrame:
        """
        Execute query and return pandas DataFrame with robust parsing.

        Strategy:
        - If query is DESCRIBE TABLE, rewrite to SELECT from system.columns and parse as JSONEachRow
        - Otherwise try FORMAT JSONEachRow; if parsing yields 0 rows, fallback to FORMAT TabSeparatedWithNamesAndTypes and parse header+types
        """
        try:
            q = query.strip()
            q_upper = q.upper()

            # Special-case: DESCRIBE TABLE -> system.columns
            if q_upper.startswith('DESCRIBE') or q_upper.startswith('DESC'):
                try:
                    # naive parse table name after 'TABLE'
                    parts = q.split()
                    if 'TABLE' in [p.upper() for p in parts]:
                        idx = [p.upper() for p in parts].index('TABLE')
                        table_name = parts[idx + 1]
                    else:
                        table_name = parts[-1]
                except Exception:
                    table_name = q.split()[-1]
                syscol_sql = (
                    f"SELECT name, type FROM system.columns "
                    f"WHERE database = '{self.database}' AND table = '{table_name}' ORDER BY position"
                )
                result = self.execute_query(syscol_sql, format="JSONEachRow")
            else:
                result = self.execute_query(q, format="JSONEachRow")

            if not result or result.strip() == "":
                return pd.DataFrame()

            # Try JSONEachRow parse
            data = []
            ok = True
            for line in result.strip().split('\n'):
                if not line.strip():
                    continue
                try:
                    data.append(json.loads(line))
                except json.JSONDecodeError:
                    ok = False
                    break

            if ok and data:
                df = pd.DataFrame(data)
                logger.info(f"✅ 成功解析 {len(df)} 行数据")
                return df

            # Fallback: re-run with TabSeparatedWithNamesAndTypes
            if q_upper.startswith('DESCRIBE') or q_upper.startswith('DESC'):
                # Already rewritten; if JSON failed unexpectedly, try TSV fallback
                re_q = syscol_sql
            else:
                re_q = q

            tsv = self.execute_query(re_q, format="TabSeparatedWithNamesAndTypes")
            if not tsv or tsv.strip() == "":
                logger.warning("⚠️ TSV回退解析失败：空响应")
                return pd.DataFrame()

            lines = tsv.splitlines()
            if len(lines) < 3:
                logger.warning("⚠️ TSV行数不足，无法解析header/types")
                return pd.DataFrame()

            names = lines[0].split('\t')
            types = lines[1].split('\t')
            rows = []
            for line in lines[2:]:
                if not line.strip():
                    continue
                parts = line.split('\t')
                if len(parts) != len(names):
                    continue
                rows.append(parts)

            if not rows:
                logger.warning("⚠️ TSV没有有效数据行")
                return pd.DataFrame()

            df = pd.DataFrame(rows, columns=names)
            # Basic type coercion for numeric types
            for col, t in zip(names, types):
                if any(k in t for k in ['Int', 'UInt', 'Float', 'Decimal']):
                    df[col] = pd.to_numeric(df[col], errors='coerce')
            df = df.dropna(axis=0, how='any')
            logger.info(f"✅ TSV解析成功: {len(df)} 行")
            return df

        except Exception as e:
            logger.error(f"❌ DataFrame转换错误: {str(e)}")
            return pd.DataFrame()
    
    def test_connection(self) -> bool:
        """Test database connection"""
        try:
            result = self.execute_query("SELECT 1 as test")
            return "1" in result
        except Exception as e:
            logger.error(f"Connection test failed: {str(e)}")
            return False
    
    def show_tables(self) -> List[str]:
        """Show all tables in database"""
        try:
            result = self.execute_query("SHOW TABLES")
            return [line.strip() for line in result.split('\n') if line.strip()]
        except Exception as e:
            logger.error(f"Show tables error: {str(e)}")
            return []
    
    def describe_table(self, table_name: str) -> pd.DataFrame:
        """Describe table structure"""
        query = f"DESCRIBE TABLE {table_name}"
        return self.query_to_dataframe(query)
    
    def find_wlfi_tables(self) -> List[str]:
        """Find tables containing WLFI data"""
        try:
            tables = self.show_tables()
            wlfi_tables = []
            
            for table in tables:
                # Check if table name contains wlfi or similar patterns
                if any(pattern in table.lower() for pattern in ['wlfi', 'world', 'liberty']):
                    wlfi_tables.append(table)
                else:
                    # Check if table has WLFI symbol data
                    sample_query = f"""
                    SELECT COUNT(*) as cnt 
                    FROM {table} 
                    WHERE symbol LIKE '%WLFI%' OR instId LIKE '%WLFI%'
                    LIMIT 1
                    """
                    try:
                        result = self.execute_query(sample_query)
                        if result and int(result.strip()) > 0:
                            wlfi_tables.append(table)
                    except:
                        pass
                        
            return wlfi_tables
            
        except Exception as e:
            logger.error(f"Find WLFI tables error: {str(e)}")
            return []
    
    def get_wlfi_data(self, 
                      table_name: str, 
                      hours_back: int = 24,
                      limit: int = 10000) -> pd.DataFrame:
        """
        Get WLFI data from specified table
        
        Args:
            table_name: Table name to query
            hours_back: Hours to look back from now
            limit: Maximum rows to return
            
        Returns:
            DataFrame with WLFI data
        """
        end_time = datetime.utcnow()
        start_time = end_time - timedelta(hours=hours_back)
        
        query = f"""
        SELECT *
        FROM {table_name}
        WHERE (symbol LIKE '%WLFI%' OR instId LIKE '%WLFI%')
          AND ts >= '{start_time.strftime('%Y-%m-%d %H:%M:%S')}'
          AND ts <= '{end_time.strftime('%Y-%m-%d %H:%M:%S')}'
        ORDER BY ts DESC
        LIMIT {limit}
        """
        
        return self.query_to_dataframe(query)
    
    def get_l1_bbo_data(self, 
                        symbol: str = "WLFI", 
                        hours_back: int = 24) -> pd.DataFrame:
        """Get L1 BBO (Best Bid/Offer) data"""
        end_time = datetime.utcnow()
        start_time = end_time - timedelta(hours=hours_back)
        
        query = f"""
        SELECT 
            ts,
            instId as symbol,
            last as price,
            bestBid as bid_price,
            bestAsk as ask_price,
            vol24h as volume
        FROM spot_ticker
        WHERE instId LIKE '%{symbol}%'
          AND ts >= '{start_time.strftime('%Y-%m-%d %H:%M:%S')}'
          AND ts <= '{end_time.strftime('%Y-%m-%d %H:%M:%S')}'
        ORDER BY ts ASC
        """
        
        return self.query_to_dataframe(query)
    
    def get_l2_orderbook_data(self, 
                              symbol: str = "WLFI", 
                              hours_back: int = 24) -> pd.DataFrame:
        """Get L2 order book data"""
        end_time = datetime.utcnow()
        start_time = end_time - timedelta(hours=hours_back)
        
        query = f"""
        SELECT 
            ts,
            instId as symbol,
            side,
            price,
            qty,
            orderCnt
        FROM spot_books15
        WHERE instId LIKE '%{symbol}%'
          AND ts >= '{start_time.strftime('%Y-%m-%d %H:%M:%S')}'
          AND ts <= '{end_time.strftime('%Y-%m-%d %H:%M:%S')}'
        ORDER BY ts ASC, side ASC, price ASC
        """
        
        return self.query_to_dataframe(query)
    
    def get_trades_data(self, 
                        symbol: str = "WLFI", 
                        hours_back: int = 24) -> pd.DataFrame:
        """Get trades data"""
        end_time = datetime.utcnow()
        start_time = end_time - timedelta(hours=hours_back)
        
        query = f"""
        SELECT 
            ts,
            instId as symbol,
            tradeId,
            side,
            price,
            qty
        FROM spot_trades
        WHERE instId LIKE '%{symbol}%'
          AND ts >= '{start_time.strftime('%Y-%m-%d %H:%M:%S')}'
          AND ts <= '{end_time.strftime('%Y-%m-%d %H:%M:%S')}'
        ORDER BY ts ASC
        """
        
        return self.query_to_dataframe(query)
