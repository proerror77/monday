#!/usr/bin/env python3
"""
使用真實數據的 ML 模型訓練
=========================

從 ClickHouse 讀取真實市場數據並訓練價格預測模型
"""

import asyncio
import numpy as np
import pandas as pd
import torch
import torch.nn as nn
import torch.optim as optim
from sklearn.preprocessing import StandardScaler
from sklearn.model_selection import train_test_split
from sklearn.metrics import mean_squared_error, r2_score
import clickhouse_connect
import logging
from datetime import datetime
import json

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class RealMLTrainer:
    def __init__(self):
        self.clickhouse_client = None
        self.scaler = StandardScaler()
        self.model = None
        self.data = None
        
    def setup_clickhouse(self):
        """設置 ClickHouse 連接"""
        try:
            self.clickhouse_client = clickhouse_connect.get_client(
                host='localhost',
                port=8123,
                database='default'
            )
            logger.info("✅ ClickHouse 連接成功")
        except Exception as e:
            logger.error(f"❌ ClickHouse 連接失敗: {e}")
            raise
    
    def load_real_data(self):
        """從 ClickHouse 加載真實數據"""
        try:
            logger.info("📊 從 ClickHouse 加載真實市場數據...")
            
            # 查詢真實數據
            query = """
            SELECT 
                timestamp,
                symbol,
                price,
                volume_24h,
                market_cap,
                price_change_24h
            FROM real_market_data 
            WHERE symbol IN ('BTC', 'ETH', 'SOL') 
            ORDER BY symbol, timestamp
            """
            
            result = self.clickhouse_client.query(query)
            
            if not result.result_rows:
                raise Exception("沒有找到真實數據")
            
            # 轉換為 DataFrame
            columns = ['timestamp', 'symbol', 'price', 'volume_24h', 'market_cap', 'price_change_24h']
            self.data = pd.DataFrame(result.result_rows, columns=columns)
            
            logger.info(f"✅ 成功加載 {len(self.data)} 條真實市場數據")
            logger.info(f"📈 數據範圍: {self.data['timestamp'].min()} 到 {self.data['timestamp'].max()}")
            logger.info(f"💰 包含幣種: {self.data['symbol'].unique()}")
            
            # 顯示數據統計
            for symbol in self.data['symbol'].unique():
                symbol_data = self.data[self.data['symbol'] == symbol]
                avg_price = symbol_data['price'].mean()
                price_std = symbol_data['price'].std()
                logger.info(f"  {symbol}: 平均價格 ${avg_price:,.2f} (標準差: ${price_std:,.2f})")
            
            return True
            
        except Exception as e:
            logger.error(f"❌ 加載數據失敗: {e}")
            return False
    
    def prepare_features(self):
        """準備特徵數據"""
        try:
            logger.info("🔧 準備機器學習特徵...")
            
            features_list = []
            targets_list = []
            
            for symbol in self.data['symbol'].unique():
                symbol_data = self.data[self.data['symbol'] == symbol].copy()
                symbol_data = symbol_data.sort_values('timestamp').reset_index(drop=True)
                
                if len(symbol_data) < 5:  # 需要至少5個數據點
                    continue
                
                # 創建特徵
                symbol_data['price_ma_3'] = symbol_data['price'].rolling(window=3, min_periods=1).mean()
                symbol_data['price_std_3'] = symbol_data['price'].rolling(window=3, min_periods=1).std().fillna(0)
                symbol_data['volume_ma_3'] = symbol_data['volume_24h'].rolling(window=3, min_periods=1).mean()
                
                # 價格變化率
                symbol_data['price_return'] = symbol_data['price'].pct_change().fillna(0)
                symbol_data['price_return_lag1'] = symbol_data['price_return'].shift(1).fillna(0)
                
                # 為每個符號編碼
                symbol_code = {'BTC': 1, 'ETH': 2, 'SOL': 3}.get(symbol, 0)
                
                # 創建時間序列特徵和目標
                for i in range(2, len(symbol_data)-1):
                    # 特徵: 前2個時間點的數據
                    features = [
                        symbol_code,
                        symbol_data.iloc[i]['price'],
                        symbol_data.iloc[i-1]['price'],
                        symbol_data.iloc[i]['price_ma_3'],
                        symbol_data.iloc[i]['price_std_3'],
                        symbol_data.iloc[i]['volume_24h'],
                        symbol_data.iloc[i]['volume_ma_3'],
                        symbol_data.iloc[i]['price_change_24h'],
                        symbol_data.iloc[i]['price_return'],
                        symbol_data.iloc[i]['price_return_lag1']
                    ]
                    
                    # 目標: 下一個時間點的價格
                    target = symbol_data.iloc[i+1]['price']
                    
                    features_list.append(features)
                    targets_list.append(target)
            
            if not features_list:
                raise Exception("無法創建特徵數據")
            
            self.features = np.array(features_list)
            self.targets = np.array(targets_list)
            
            logger.info(f"✅ 成功創建 {len(self.features)} 個訓練樣本")
            logger.info(f"📊 特徵維度: {self.features.shape[1]}")
            
            return True
            
        except Exception as e:
            logger.error(f"❌ 特徵準備失敗: {e}")
            return False
    
    def create_model(self, input_dim):
        """創建神經網絡模型"""
        class PricePredictionNet(nn.Module):
            def __init__(self, input_dim):
                super(PricePredictionNet, self).__init__()
                self.layers = nn.Sequential(
                    nn.Linear(input_dim, 64),
                    nn.ReLU(),
                    nn.Dropout(0.2),
                    nn.Linear(64, 32),
                    nn.ReLU(),
                    nn.Dropout(0.2),
                    nn.Linear(32, 16),
                    nn.ReLU(),
                    nn.Linear(16, 1)
                )
            
            def forward(self, x):
                return self.layers(x)
        
        return PricePredictionNet(input_dim)
    
    def train_model(self):
        """訓練模型"""
        try:
            logger.info("🤖 開始訓練神經網絡模型...")
            
            # 標準化特徵
            X_scaled = self.scaler.fit_transform(self.features)
            
            # 分割數據
            X_train, X_test, y_train, y_test = train_test_split(
                X_scaled, self.targets, test_size=0.2, random_state=42
            )
            
            # 轉換為 PyTorch 張量
            X_train_tensor = torch.FloatTensor(X_train)
            y_train_tensor = torch.FloatTensor(y_train).reshape(-1, 1)
            X_test_tensor = torch.FloatTensor(X_test)
            y_test_tensor = torch.FloatTensor(y_test).reshape(-1, 1)
            
            # 創建模型
            self.model = self.create_model(X_train.shape[1])
            criterion = nn.MSELoss()
            optimizer = optim.Adam(self.model.parameters(), lr=0.001)
            
            # 訓練
            logger.info(f"📈 訓練數據: {len(X_train)} 樣本, 測試數據: {len(X_test)} 樣本")
            
            best_loss = float('inf')
            patience = 0
            
            for epoch in range(200):
                # 前向傳播
                outputs = self.model(X_train_tensor)
                loss = criterion(outputs, y_train_tensor)
                
                # 反向傳播
                optimizer.zero_grad()
                loss.backward()
                optimizer.step()
                
                # 評估
                if epoch % 20 == 0:
                    self.model.eval()
                    with torch.no_grad():
                        test_outputs = self.model(X_test_tensor)
                        test_loss = criterion(test_outputs, y_test_tensor)
                        
                        # 計算 R²
                        test_predictions = test_outputs.numpy()
                        r2 = r2_score(y_test, test_predictions)
                        
                        logger.info(f"Epoch {epoch}: 訓練損失 {loss.item():.4f}, 測試損失 {test_loss.item():.4f}, R² {r2:.4f}")
                        
                        if test_loss.item() < best_loss:
                            best_loss = test_loss.item()
                            patience = 0
                        else:
                            patience += 1
                    
                    self.model.train()
                
                # 早停
                if patience > 5:
                    logger.info("📊 早停觸發，訓練結束")
                    break
            
            # 最終評估
            self.model.eval()
            with torch.no_grad():
                final_predictions = self.model(X_test_tensor).numpy()
                final_r2 = r2_score(y_test, final_predictions)
                final_mse = mean_squared_error(y_test, final_predictions)
                
                logger.info("🎯 最終模型性能:")
                logger.info(f"  R² Score: {final_r2:.4f}")
                logger.info(f"  MSE: {final_mse:.4f}")
                logger.info(f"  RMSE: ${np.sqrt(final_mse):,.2f}")
                
                # 驗證預測合理性
                for i, symbol in enumerate(['BTC', 'ETH', 'SOL']):
                    symbol_mask = X_test[:, 0] == (i + 1)
                    if np.any(symbol_mask):
                        symbol_predictions = final_predictions[symbol_mask].flatten()
                        symbol_actual = y_test[symbol_mask]
                        
                        if len(symbol_predictions) > 0:
                            avg_prediction = np.mean(symbol_predictions)
                            avg_actual = np.mean(symbol_actual)
                            logger.info(f"  {symbol}: 預測均價 ${avg_prediction:,.2f}, 實際均價 ${avg_actual:,.2f}")
            
            return final_r2 > 0.1  # 如果 R² > 0.1 認為模型有效
            
        except Exception as e:
            logger.error(f"❌ 模型訓練失敗: {e}")
            return False
    
    def save_model(self):
        """保存模型"""
        try:
            # 保存 PyTorch 模型
            torch.save(self.model.state_dict(), '/Users/proerror/Documents/monday/real_price_model.pth')
            
            # 保存模型信息
            model_info = {
                'timestamp': datetime.now().isoformat(),
                'model_type': 'price_prediction_nn',
                'features_count': self.features.shape[1],
                'samples_count': len(self.features),
                'data_source': 'real_market_data'
            }
            
            with open('/Users/proerror/Documents/monday/real_model_info.json', 'w') as f:
                json.dump(model_info, f, indent=2)
            
            logger.info("💾 模型已保存到文件")
            return True
            
        except Exception as e:
            logger.error(f"❌ 模型保存失敗: {e}")
            return False
    
    async def run(self):
        """運行完整的 ML 流程"""
        try:
            logger.info("🚀 開始真實數據 ML 訓練流程")
            
            # 1. 設置連接
            self.setup_clickhouse()
            
            # 2. 加載真實數據
            if not self.load_real_data():
                return False
            
            # 3. 準備特徵
            if not self.prepare_features():
                return False
            
            # 4. 訓練模型
            if not self.train_model():
                return False
            
            # 5. 保存模型
            if not self.save_model():
                return False
            
            logger.info("🎉 真實數據 ML 訓練成功完成！")
            return True
            
        except Exception as e:
            logger.error(f"❌ ML 訓練流程失敗: {e}")
            return False

async def main():
    trainer = RealMLTrainer()
    success = await trainer.run()
    return success

if __name__ == "__main__":
    result = asyncio.run(main())
    if result:
        print("🎉 真實數據 ML 訓練成功！")
    else:
        print("❌ ML 訓練失敗")