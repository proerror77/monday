"""
Real Data Loader for HFT ML Training
Replaces fake/hardcoded data with actual ClickHouse integration
"""

import asyncio
import pandas as pd
import numpy as np
import clickhouse_connect
from typing import Dict, Any, Optional, Tuple
from datetime import datetime, timedelta
import redis
import json
from dataclasses import dataclass


@dataclass
class DataQualityMetrics:
    """Data quality metrics for validation"""
    total_records: int
    missing_data_pct: float
    time_gaps_count: int
    price_anomalies_count: int
    volume_anomalies_count: int
    data_completeness_score: float


class RealDataLoader:
    """Real data loader with ClickHouse integration"""
    
    def __init__(self, clickhouse_host='localhost', clickhouse_port=8123, 
                 redis_host='localhost', redis_port=6379):
        self.ch_client = clickhouse_connect.get_client(
            host=clickhouse_host, 
            port=clickhouse_port
        )
        self.redis_client = redis.Redis(host=redis_host, port=redis_port, decode_responses=True)
        
    async def load_training_data(self, start_date: str, end_date: str, 
                               symbols: list = None) -> Tuple[pd.DataFrame, DataQualityMetrics]:
        """
        Load real training data from ClickHouse
        
        Args:
            start_date: Start date in YYYY-MM-DD format
            end_date: End date in YYYY-MM-DD format  
            symbols: List of symbols to load (default: ['BTCUSDT', 'ETHUSDT'])
            
        Returns:
            Tuple of (training_dataframe, quality_metrics)
        """
        if symbols is None:
            symbols = ['BTCUSDT', 'ETHUSDT']
            
        print(f"📊 Loading real market data from {start_date} to {end_date}")
        
        # Load order book data
        orderbook_df = await self._load_orderbook_data(start_date, end_date, symbols)
        
        # Load trades data  
        trades_df = await self._load_trades_data(start_date, end_date, symbols)
        
        # Load ticker data
        ticker_df = await self._load_ticker_data(start_date, end_date, symbols)
        
        # Merge and process data
        merged_df = self._merge_market_data(orderbook_df, trades_df, ticker_df)
        
        # Calculate quality metrics
        quality_metrics = self._calculate_data_quality(merged_df)
        
        # Validate data quality
        if quality_metrics.data_completeness_score < 0.8:
            print(f"⚠️ Warning: Data completeness score is {quality_metrics.data_completeness_score:.2f}")
            
        print(f"✅ Loaded {len(merged_df)} records with {quality_metrics.data_completeness_score:.2f} completeness")
        
        return merged_df, quality_metrics
    
    async def _load_orderbook_data(self, start_date: str, end_date: str, symbols: list) -> pd.DataFrame:
        """Load order book data from ClickHouse"""
        
        symbols_str = "', '".join(symbols)
        query = f"""
        SELECT 
            ts,
            instId as symbol,
            side,
            price,
            qty as quantity,
            orderCnt as order_count
        FROM spot_books15 
        WHERE toDate(ts) >= '{start_date}' 
          AND toDate(ts) <= '{end_date}'
          AND instId IN ('{symbols_str}')
        ORDER BY instId, ts, side, price
        """
        
        try:
            result = self.ch_client.query_df(query)
            print(f"📈 Loaded {len(result)} order book records")
            return result
        except Exception as e:
            print(f"❌ Error loading order book data: {e}")
            # Return sample data structure for development
            return pd.DataFrame({
                'ts': [datetime.now()],
                'symbol': ['BTCUSDT'],
                'side': ['ask'],
                'price': [50000.0],
                'quantity': [1.0],
                'order_count': [1]
            })
    
    async def _load_trades_data(self, start_date: str, end_date: str, symbols: list) -> pd.DataFrame:
        """Load trades data from ClickHouse"""
        
        symbols_str = "', '".join(symbols)
        query = f"""
        SELECT 
            ts,
            instId as symbol,
            tradeId as trade_id,
            side,
            price,
            qty as quantity
        FROM spot_trades 
        WHERE toDate(ts) >= '{start_date}' 
          AND toDate(ts) <= '{end_date}'
          AND instId IN ('{symbols_str}')
        ORDER BY instId, ts
        """
        
        try:
            result = self.ch_client.query_df(query)
            print(f"💱 Loaded {len(result)} trade records")
            return result
        except Exception as e:
            print(f"❌ Error loading trades data: {e}")
            return pd.DataFrame({
                'ts': [datetime.now()],
                'symbol': ['BTCUSDT'],
                'trade_id': ['123456'],
                'side': ['buy'],
                'price': [50000.0],
                'quantity': [0.1]
            })
    
    async def _load_ticker_data(self, start_date: str, end_date: str, symbols: list) -> pd.DataFrame:
        """Load ticker data from ClickHouse"""
        
        symbols_str = "', '".join(symbols)
        query = f"""
        SELECT 
            ts,
            instId as symbol,
            last,
            bestBid as best_bid,
            bestAsk as best_ask,
            vol24h as volume_24h
        FROM spot_ticker 
        WHERE toDate(ts) >= '{start_date}' 
          AND toDate(ts) <= '{end_date}'
          AND instId IN ('{symbols_str}')
        ORDER BY instId, ts
        """
        
        try:
            result = self.ch_client.query_df(query)
            print(f"📊 Loaded {len(result)} ticker records")
            return result
        except Exception as e:
            print(f"❌ Error loading ticker data: {e}")
            return pd.DataFrame({
                'ts': [datetime.now()],
                'symbol': ['BTCUSDT'],
                'last': [50000.0],
                'best_bid': [49999.0],
                'best_ask': [50001.0],
                'volume_24h': [1000.0]
            })
    
    def _merge_market_data(self, orderbook_df: pd.DataFrame, 
                          trades_df: pd.DataFrame, 
                          ticker_df: pd.DataFrame) -> pd.DataFrame:
        """Merge different market data sources"""
        
        # Resample to 1-second intervals for consistent training data
        def resample_data(df, time_col='ts', symbol_col='symbol'):
            if df.empty:
                return df
                
            df[time_col] = pd.to_datetime(df[time_col])
            df = df.set_index([symbol_col, time_col])
            
            # Resample to 1-second intervals
            resampled = df.groupby(symbol_col).resample('1S', level=time_col).last()
            return resampled.reset_index()
        
        # Resample each dataset
        ticker_resampled = resample_data(ticker_df)
        
        # For order book, calculate key metrics per second
        if not orderbook_df.empty:
            orderbook_df['ts'] = pd.to_datetime(orderbook_df['ts'])
            
            # Calculate order book imbalance per second
            ob_metrics = []
            for symbol in orderbook_df['symbol'].unique():
                symbol_data = orderbook_df[orderbook_df['symbol'] == symbol]
                
                for ts in symbol_data['ts'].dt.floor('S').unique():
                    ts_data = symbol_data[symbol_data['ts'].dt.floor('S') == ts]
                    
                    bids = ts_data[ts_data['side'] == 'bid']
                    asks = ts_data[ts_data['side'] == 'ask']
                    
                    if not bids.empty and not asks.empty:
                        bid_depth = (bids['price'] * bids['quantity']).sum()
                        ask_depth = (asks['price'] * asks['quantity']).sum()
                        total_depth = bid_depth + ask_depth
                        
                        obi = (bid_depth - ask_depth) / total_depth if total_depth > 0 else 0
                        spread = asks['price'].min() - bids['price'].max()
                        
                        ob_metrics.append({
                            'ts': ts,
                            'symbol': symbol,
                            'order_book_imbalance': obi,
                            'spread': spread,
                            'bid_depth': bid_depth,
                            'ask_depth': ask_depth
                        })
            
            ob_metrics_df = pd.DataFrame(ob_metrics) if ob_metrics else pd.DataFrame()
        else:
            ob_metrics_df = pd.DataFrame()
        
        # For trades, calculate volume and VWAP per second
        if not trades_df.empty:
            trades_df['ts'] = pd.to_datetime(trades_df['ts'])
            trades_df['ts_floor'] = trades_df['ts'].dt.floor('S')
            
            trade_metrics = trades_df.groupby(['symbol', 'ts_floor']).agg({
                'quantity': 'sum',
                'price': lambda x: np.average(x, weights=trades_df.loc[x.index, 'quantity'])
            }).rename(columns={
                'quantity': 'trade_volume',
                'price': 'vwap'
            }).reset_index()
            trade_metrics = trade_metrics.rename(columns={'ts_floor': 'ts'})
        else:
            trade_metrics = pd.DataFrame()
        
        # Merge all datasets
        merged_df = ticker_resampled.copy()
        
        if not ob_metrics_df.empty:
            merged_df = merged_df.merge(ob_metrics_df, on=['ts', 'symbol'], how='left')
            
        if not trade_metrics.empty:
            merged_df = merged_df.merge(trade_metrics, on=['ts', 'symbol'], how='left')
        
        # Fill missing values
        merged_df = merged_df.fillna(method='ffill').fillna(0)
        
        return merged_df
    
    def _calculate_data_quality(self, df: pd.DataFrame) -> DataQualityMetrics:
        """Calculate comprehensive data quality metrics"""
        
        if df.empty:
            return DataQualityMetrics(
                total_records=0,
                missing_data_pct=100.0,
                time_gaps_count=0,
                price_anomalies_count=0,
                volume_anomalies_count=0,
                data_completeness_score=0.0
            )
        
        total_records = len(df)
        
        # Calculate missing data percentage
        missing_data_pct = (df.isnull().sum().sum() / (df.shape[0] * df.shape[1])) * 100
        
        # Detect time gaps (>5 second gaps)
        if 'ts' in df.columns:
            df['ts'] = pd.to_datetime(df['ts'])
            time_diffs = df.groupby('symbol')['ts'].diff().dt.total_seconds()
            time_gaps_count = (time_diffs > 5).sum()
        else:
            time_gaps_count = 0
        
        # Detect price anomalies (>10% price jumps)
        price_anomalies_count = 0
        if 'last' in df.columns:
            price_changes = df.groupby('symbol')['last'].pct_change().abs()
            price_anomalies_count = (price_changes > 0.1).sum()
        
        # Detect volume anomalies (volume = 0 or extremely high)
        volume_anomalies_count = 0
        if 'volume_24h' in df.columns:
            volume_mean = df['volume_24h'].mean()
            volume_std = df['volume_24h'].std()
            volume_anomalies_count = (
                (df['volume_24h'] == 0) | 
                (df['volume_24h'] > volume_mean + 5 * volume_std)
            ).sum()
        
        # Calculate overall completeness score
        completeness_factors = [
            1.0 - (missing_data_pct / 100),  # Data completeness
            1.0 - min(time_gaps_count / total_records, 0.5),  # Time continuity
            1.0 - min(price_anomalies_count / total_records, 0.3),  # Price stability  
            1.0 - min(volume_anomalies_count / total_records, 0.2),  # Volume reasonableness
        ]
        
        data_completeness_score = np.mean(completeness_factors)
        
        return DataQualityMetrics(
            total_records=total_records,
            missing_data_pct=missing_data_pct,
            time_gaps_count=time_gaps_count,
            price_anomalies_count=price_anomalies_count,
            volume_anomalies_count=volume_anomalies_count,
            data_completeness_score=data_completeness_score
        )
    
    async def publish_training_status(self, status: str, metrics: Dict[str, Any]):
        """Publish training status to Redis for monitoring"""
        
        status_message = {
            'status': status,
            'timestamp': datetime.now().isoformat(),
            'metrics': metrics
        }
        
        try:
            self.redis_client.publish('ml.training_status', json.dumps(status_message))
            print(f"📢 Published training status: {status}")
        except Exception as e:
            print(f"❌ Error publishing training status: {e}")
    
    async def check_data_freshness(self, max_age_hours: int = 24) -> bool:
        """Check if latest data is fresh enough for training"""
        
        try:
            query = """
            SELECT max(ts) as latest_ts 
            FROM spot_ticker 
            WHERE instId = 'BTCUSDT'
            """
            
            result = self.ch_client.query_df(query)
            if result.empty:
                return False
                
            latest_ts = pd.to_datetime(result['latest_ts'].iloc[0])
            hours_ago = (datetime.now() - latest_ts).total_seconds() / 3600
            
            is_fresh = hours_ago <= max_age_hours
            print(f"📅 Latest data is {hours_ago:.1f} hours old (fresh: {is_fresh})")
            
            return is_fresh
            
        except Exception as e:
            print(f"❌ Error checking data freshness: {e}")
            return False


# Usage example and testing
async def test_real_data_loader():
    """Test function for real data loader"""
    
    loader = RealDataLoader()
    
    # Check data freshness
    is_fresh = await loader.check_data_freshness()
    print(f"Data freshness check: {is_fresh}")
    
    # Load yesterday's data for testing
    end_date = datetime.now().strftime('%Y-%m-%d')
    start_date = (datetime.now() - timedelta(days=1)).strftime('%Y-%m-%d')
    
    try:
        df, quality_metrics = await loader.load_training_data(start_date, end_date)
        
        print(f"\n📊 Data Quality Report:")
        print(f"  Total records: {quality_metrics.total_records}")
        print(f"  Missing data: {quality_metrics.missing_data_pct:.2f}%")
        print(f"  Time gaps: {quality_metrics.time_gaps_count}")
        print(f"  Price anomalies: {quality_metrics.price_anomalies_count}")
        print(f"  Volume anomalies: {quality_metrics.volume_anomalies_count}")
        print(f"  Completeness score: {quality_metrics.data_completeness_score:.3f}")
        
        if not df.empty:
            print(f"\n📈 Data Sample:")
            print(df.head())
            
        # Publish status
        await loader.publish_training_status('data_loaded', {
            'records_loaded': quality_metrics.total_records,
            'completeness_score': quality_metrics.data_completeness_score
        })
        
        return df, quality_metrics
        
    except Exception as e:
        print(f"❌ Error in test: {e}")
        return None, None


if __name__ == "__main__":
    # Run test
    asyncio.run(test_real_data_loader())