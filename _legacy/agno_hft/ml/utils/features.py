"""
yµå!J
===========

º LOB xÚÐÖ	©„yµì
- ®ÀPËyµùîñ¦saI	
- €SûÕsGâÕ‡I	
- ®Ayµ¤7¦·ãÓ›I	
"""

import pandas as pd
import numpy as np
from typing import Dict, List, Tuple, Optional
import talib
from scipy import stats
import logging

logger = logging.getLogger(__name__)


class FeatureExtractor:
    """
    LOB yµÐÖh
    
    ÐÖ	^yµ
    1. úyµù<xÏùîI
    2. ®ÀPËyµ®sañ¦I
    3. Myµ€SqyµI
    """
    
    def __init__(self, lookback_windows: List[int] = [10, 30, 60, 120]):
        """
        Args:
            lookback_windows: (¼—þÕyµ„—ã'
        """
        self.lookback_windows = lookback_windows
        
    def extract_all_features(self, df: pd.DataFrame) -> pd.DataFrame:
        """
        ÐÖ@	yµ
        
        Args:
            df: + LOB xÚ„ DataFrame
            
        Returns:
            +@	yµ„ DataFrame
        """
        # ýxÚMî9ŸËxÚ
        feature_df = df.copy()
        
        # 1. úyµ
        feature_df = self._extract_basic_features(feature_df)
        
        # 2. ®ÀPËyµ
        feature_df = self._extract_microstructure_features(feature_df)
        
        # 3. €S
        feature_df = self._extract_technical_features(feature_df)
        
        # 4. ®Ayµ
        feature_df = self._extract_orderflow_features(feature_df)
        
        # 5. qyµ
        feature_df = self._extract_statistical_features(feature_df)
        
        #  NaN <
        feature_df = self._handle_missing_values(feature_df)
        
        return feature_df
        
    def _extract_basic_features(self, df: pd.DataFrame) -> pd.DataFrame:
        """ÐÖúyµ"""
        # -“ù
        if 'bid_price_1' in df.columns and 'ask_price_1' in df.columns:
            df['mid_price'] = (df['bid_price_1'] + df['ask_price_1']) / 2
            
        # ùî
        if 'ask_price_1' in df.columns and 'bid_price_1' in df.columns:
            df['spread'] = df['ask_price_1'] - df['bid_price_1']
            df['spread_pct'] = df['spread'] / df['mid_price'] * 100
            
        #  
sGù<M5”	
        bid_prices = [f'bid_price_{i}' for i in range(1, 6)]
        bid_sizes = [f'bid_size_{i}' for i in range(1, 6)]
        ask_prices = [f'ask_price_{i}' for i in range(1, 6)]
        ask_sizes = [f'ask_size_{i}' for i in range(1, 6)]
        
        # ¢å/&X(
        if all(col in df.columns for col in bid_prices + bid_sizes):
            # — 
sG·ù
            weighted_bid = 0
            total_bid_size = 0
            for price_col, size_col in zip(bid_prices, bid_sizes):
                weighted_bid += df[price_col] * df[size_col]
                total_bid_size += df[size_col]
            df['weighted_bid_price'] = weighted_bid / (total_bid_size + 1e-8)
            
        if all(col in df.columns for col in ask_prices + ask_sizes):
            # — 
sGãù
            weighted_ask = 0
            total_ask_size = 0
            for price_col, size_col in zip(ask_prices, ask_sizes):
                weighted_ask += df[price_col] * df[size_col]
                total_ask_size += df[size_col]
            df['weighted_ask_price'] = weighted_ask / (total_ask_size + 1e-8)
            
        # ®?ñ¦
        if all(col in df.columns for col in bid_sizes):
            df['total_bid_volume'] = df[bid_sizes].sum(axis=1)
            
        if all(col in df.columns for col in ask_sizes):
            df['total_ask_volume'] = df[ask_sizes].sum(axis=1)
            
        return df
        
    def _extract_microstructure_features(self, df: pd.DataFrame) -> pd.DataFrame:
        """ÐÖ4®ÀPËyµ"""
        # ®sa
        if 'total_bid_volume' in df.columns and 'total_ask_volume' in df.columns:
            df['order_imbalance'] = (df['total_bid_volume'] - df['total_ask_volume']) / \
                                   (df['total_bid_volume'] + df['total_ask_volume'] + 1e-8)
                                   
        # ”M„sa
        for i in range(1, 6):
            bid_col = f'bid_size_{i}'
            ask_col = f'ask_size_{i}'
            if bid_col in df.columns and ask_col in df.columns:
                df[f'imbalance_level_{i}'] = (df[bid_col] - df[ask_col]) / \
                                             (df[bid_col] + df[ask_col] + 1e-8)
                                             
        # ù<œ‡®?bÀ	
        if all(f'bid_price_{i}' in df.columns for i in range(1, 6)):
            # ·¹œ‡
            bid_prices = df[[f'bid_price_{i}' for i in range(1, 6)]].values
            bid_slopes = []
            for row in bid_prices:
                if len(row) > 1:
                    slope, _ = np.polyfit(range(len(row)), row, 1)
                    bid_slopes.append(slope)
                else:
                    bid_slopes.append(0)
            df['bid_slope'] = bid_slopes
            
        if all(f'ask_price_{i}' in df.columns for i in range(1, 6)):
            # ã¹œ‡
            ask_prices = df[[f'ask_price_{i}' for i in range(1, 6)]].values
            ask_slopes = []
            for row in ask_prices:
                if len(row) > 1:
                    slope, _ = np.polyfit(range(len(row)), row, 1)
                    ask_slopes.append(slope)
                else:
                    ask_slopes.append(0)
            df['ask_slope'] = ask_slopes
            
        # ñ¦ 
ù<
        if 'weighted_bid_price' in df.columns and 'weighted_ask_price' in df.columns:
            df['depth_weighted_mid'] = (df['weighted_bid_price'] + df['weighted_ask_price']) / 2
            
        return df
        
    def _extract_technical_features(self, df: pd.DataFrame) -> pd.DataFrame:
        """ÐÖ€Syµ"""
        if 'mid_price' not in df.columns:
            return df
            
        price = df['mid_price'].values
        
        # ûÕsGÚ
        for window in self.lookback_windows:
            if len(price) >= window:
                df[f'sma_{window}'] = talib.SMA(price, timeperiod=window)
                df[f'ema_{window}'] = talib.EMA(price, timeperiod=window)
                
        # —6
        if len(price) >= 20:
            upper, middle, lower = talib.BBANDS(price, timeperiod=20, nbdevup=2, nbdevdn=2)
            df['bb_upper'] = upper
            df['bb_middle'] = middle
            df['bb_lower'] = lower
            df['bb_width'] = upper - lower
            df['bb_position'] = (price - lower) / (upper - lower + 1e-8)
            
        # RSI
        for window in [14, 30]:
            if len(price) >= window:
                df[f'rsi_{window}'] = talib.RSI(price, timeperiod=window)
                
        # MACD
        if len(price) >= 26:
            macd, macd_signal, macd_hist = talib.MACD(price, fastperiod=12, slowperiod=26, signalperiod=9)
            df['macd'] = macd
            df['macd_signal'] = macd_signal
            df['macd_hist'] = macd_hist
            
        # ATRsGæÄ	
        if all(col in df.columns for col in ['bid_price_1', 'ask_price_1']):
            high = df['ask_price_1'].values
            low = df['bid_price_1'].values
            close = df['mid_price'].values
            
            for window in [14, 30]:
                if len(price) >= window:
                    df[f'atr_{window}'] = talib.ATR(high, low, close, timeperiod=window)
                    
        return df
        
    def _extract_orderflow_features(self, df: pd.DataFrame) -> pd.DataFrame:
        """ÐÖ®Ayµ"""
        # ¤ÏøÜyµ
        if 'volume' in df.columns:
            # ¤ÏûÕsG
            for window in self.lookback_windows:
                df[f'volume_ma_{window}'] = df['volume'].rolling(window).mean()
                
            # ¤ÏÔ‡
            df['volume_ratio'] = df['volume'] / df['volume'].rolling(20).mean()
            
        # ·ãÓ›
        if 'total_bid_volume' in df.columns and 'total_ask_volume' in df.columns:
            # /M·ãÓ›
            for window in self.lookback_windows:
                df[f'bid_pressure_{window}'] = df['total_bid_volume'].rolling(window).sum()
                df[f'ask_pressure_{window}'] = df['total_ask_volume'].rolling(window).sum()
                df[f'pressure_ratio_{window}'] = df[f'bid_pressure_{window}'] / \
                                                 (df[f'ask_pressure_{window}'] + 1e-8)
                                                 
        # ®0T‡!H,ú¼volumeŠ	
        if 'total_bid_volume' in df.columns:
            df['bid_arrival_rate'] = df['total_bid_volume'].diff().fillna(0)
            
        if 'total_ask_volume' in df.columns:
            df['ask_arrival_rate'] = df['total_ask_volume'].diff().fillna(0)
            
        return df
        
    def _extract_statistical_features(self, df: pd.DataFrame) -> pd.DataFrame:
        """ÐÖqyµ"""
        if 'mid_price' not in df.columns:
            return df
            
        # 6Ê‡
        df['return'] = df['mid_price'].pct_change()
        
        # x6Ê‡
        df['log_return'] = np.log(df['mid_price'] / df['mid_price'].shift(1))
        
        # þÕqyµ
        for window in self.lookback_windows:
            # 6Ê‡q
            df[f'return_mean_{window}'] = df['return'].rolling(window).mean()
            df[f'return_std_{window}'] = df['return'].rolling(window).std()
            df[f'return_skew_{window}'] = df['return'].rolling(window).skew()
            df[f'return_kurt_{window}'] = df['return'].rolling(window).kurt()
            
            # ù<q
            df[f'price_std_{window}'] = df['mid_price'].rolling(window).std()
            df[f'price_range_{window}'] = df['mid_price'].rolling(window).max() - \
                                          df['mid_price'].rolling(window).min()
                                          
            # æþâÕ‡
            df[f'realized_vol_{window}'] = df['log_return'].rolling(window).std() * np.sqrt(252 * 24 * 60 * 60)
            
        # êøÜ
        for lag in [1, 5, 10]:
            df[f'return_autocorr_{lag}'] = df['return'].rolling(30).apply(
                lambda x: x.autocorr(lag=lag) if len(x) > lag else np.nan
            )
            
        return df
        
    def _handle_missing_values(self, df: pd.DataFrame) -> pd.DataFrame:
        """U:1<"""
        # MkE
        df = df.fillna(method='ffill')
        
        # ¼Í6X(„ NaN‚‹Ëè	(ŒkE
        df = df.fillna(method='bfill')
        
        # ¼Í6X(„ NaNkEº 0
        df = df.fillna(0)
        
        # ÿÛ!®'<
        df = df.replace([np.inf, -np.inf], 0)
        
        return df
        
    def get_feature_names(self, df: pd.DataFrame) -> List[str]:
        """rÖ@	yµ1"""
        # ’dŸËxÚ
        original_columns = [
            'timestamp', 'symbol', 'exchange',
            'bid_price_1', 'bid_price_2', 'bid_price_3', 'bid_price_4', 'bid_price_5',
            'bid_size_1', 'bid_size_2', 'bid_size_3', 'bid_size_4', 'bid_size_5',
            'ask_price_1', 'ask_price_2', 'ask_price_3', 'ask_price_4', 'ask_price_5',
            'ask_size_1', 'ask_size_2', 'ask_size_3', 'ask_size_4', 'ask_size_5'
        ]
        
        feature_columns = [col for col in df.columns if col not in original_columns]
        return feature_columns
        
    def normalize_features(self, df: pd.DataFrame, feature_columns: List[str]) -> pd.DataFrame:
        """–yµ"""
        from sklearn.preprocessing import StandardScaler
        
        scaler = StandardScaler()
        df[feature_columns] = scaler.fit_transform(df[feature_columns])
        
        return df, scaler