#!/usr/bin/env python3
"""
最終 Agno ML 訓練系統
=====================

使用真實 ClickHouse 數據的完整 ML 流程
基於 Agno 框架架構設計
"""

import asyncio
import logging
import pandas as pd
import numpy as np
import torch
import torch.nn as nn
import torch.optim as optim
from sklearn.preprocessing import StandardScaler
from sklearn.model_selection import train_test_split
from sklearn.metrics import mean_squared_error, r2_score
import clickhouse_connect
import json
from datetime import datetime
from pathlib import Path

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class AgnoMLAgent:
    """基於 Agno 架構的 ML Agent"""
    
    def __init__(self):
        self.agent_name = "HFT_ML_Agent"
        self.workspace = "ml_workspace"
        self.clickhouse_client = None
        self.model = None
        self.scaler = StandardScaler()
        self.training_data = None
        
    async def initialize(self):
        """初始化 ML Agent"""
        logger.info(f"🤖 初始化 {self.agent_name}")
        
        # 連接 ClickHouse
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
        
        logger.info(f"✅ {self.agent_name} 初始化完成")
    
    async def load_real_data_step(self):
        """Step 1: 加載真實市場數據"""
        logger.info("📊 Step 1: 從 ClickHouse 加載真實市場數據")
        
        try:
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
            self.training_data = pd.DataFrame(result.result_rows, columns=columns)
            
            logger.info(f"✅ 成功加載 {len(self.training_data)} 條真實市場數據")
            logger.info(f"📈 數據範圍: {self.training_data['timestamp'].min()} 到 {self.training_data['timestamp'].max()}")
            logger.info(f"💰 包含幣種: {self.training_data['symbol'].unique()}")
            
            # 顯示真實數據統計
            for symbol in self.training_data['symbol'].unique():
                symbol_data = self.training_data[self.training_data['symbol'] == symbol]
                avg_price = symbol_data['price'].mean()
                price_std = symbol_data['price'].std()
                logger.info(f"  {symbol}: 平均價格 ${avg_price:,.2f} (標準差: ${price_std:,.2f})")
            
            return {"status": "success", "records": len(self.training_data)}
            
        except Exception as e:
            logger.error(f"❌ 數據加載失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def feature_engineering_step(self):
        """Step 2: 特徵工程"""
        logger.info("🔧 Step 2: 執行特徵工程")
        
        try:
            features_list = []
            targets_list = []
            
            for symbol in self.training_data['symbol'].unique():
                symbol_data = self.training_data[self.training_data['symbol'] == symbol].copy()
                symbol_data = symbol_data.sort_values('timestamp').reset_index(drop=True)
                
                if len(symbol_data) < 5:
                    continue
                
                # 創建技術指標
                symbol_data['price_ma_3'] = symbol_data['price'].rolling(window=3, min_periods=1).mean()
                symbol_data['price_std_3'] = symbol_data['price'].rolling(window=3, min_periods=1).std().fillna(0)
                symbol_data['volume_ma_3'] = symbol_data['volume_24h'].rolling(window=3, min_periods=1).mean()
                symbol_data['price_return'] = symbol_data['price'].pct_change().fillna(0)
                symbol_data['price_return_lag1'] = symbol_data['price_return'].shift(1).fillna(0)
                
                # 價格動量指標
                symbol_data['momentum_5'] = symbol_data['price'] / symbol_data['price'].shift(5) - 1
                symbol_data['momentum_5'] = symbol_data['momentum_5'].fillna(0)
                
                # 波動率指標
                symbol_data['volatility'] = symbol_data['price_return'].rolling(window=3).std().fillna(0)
                
                # 符號編碼
                symbol_code = {'BTC': 1, 'ETH': 2, 'SOL': 3}.get(symbol, 0)
                
                # 創建時間序列特徵
                for i in range(3, len(symbol_data)-1):  # 需要更多歷史數據
                    features = [
                        symbol_code,
                        symbol_data.iloc[i]['price'],
                        symbol_data.iloc[i-1]['price'],
                        symbol_data.iloc[i-2]['price'],
                        symbol_data.iloc[i]['price_ma_3'],
                        symbol_data.iloc[i]['price_std_3'],
                        symbol_data.iloc[i]['volume_24h'],
                        symbol_data.iloc[i]['volume_ma_3'],
                        symbol_data.iloc[i]['price_change_24h'],
                        symbol_data.iloc[i]['price_return'],
                        symbol_data.iloc[i]['price_return_lag1'],
                        symbol_data.iloc[i]['momentum_5'],
                        symbol_data.iloc[i]['volatility'],
                        symbol_data.iloc[i]['market_cap']
                    ]
                    
                    target = symbol_data.iloc[i+1]['price']
                    
                    features_list.append(features)
                    targets_list.append(target)
            
            if not features_list:
                raise Exception("無法創建特徵數據")
            
            self.features = np.array(features_list)
            self.targets = np.array(targets_list)
            
            logger.info(f"✅ 成功創建 {len(self.features)} 個訓練樣本")
            logger.info(f"📊 特徵維度: {self.features.shape[1]}")
            
            return {"status": "success", "samples": len(self.features), "features": self.features.shape[1]}
            
        except Exception as e:
            logger.error(f"❌ 特徵工程失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def model_training_step(self):
        """Step 3: 模型訓練"""
        logger.info("🧠 Step 3: 開始深度學習模型訓練")
        
        try:
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
            class TLOBNet(nn.Module):
                def __init__(self, input_dim):
                    super(TLOBNet, self).__init__()
                    self.layers = nn.Sequential(
                        nn.Linear(input_dim, 128),
                        nn.ReLU(),
                        nn.Dropout(0.3),
                        nn.Linear(128, 64),
                        nn.ReLU(),
                        nn.Dropout(0.2),
                        nn.Linear(64, 32),
                        nn.ReLU(),
                        nn.Linear(32, 1)
                    )
                
                def forward(self, x):
                    return self.layers(x)
            
            self.model = TLOBNet(X_train.shape[1])
            criterion = nn.MSELoss()
            optimizer = optim.Adam(self.model.parameters(), lr=0.001)
            
            logger.info(f"📈 訓練數據: {len(X_train)} 樣本, 測試數據: {len(X_test)} 樣本")
            
            best_loss = float('inf')
            patience = 0
            
            for epoch in range(100):
                # 前向傳播
                outputs = self.model(X_train_tensor)
                loss = criterion(outputs, y_train_tensor)
                
                # 反向傳播
                optimizer.zero_grad()
                loss.backward()
                optimizer.step()
                
                # 評估
                if epoch % 10 == 0:
                    self.model.eval()
                    with torch.no_grad():
                        test_outputs = self.model(X_test_tensor)
                        test_loss = criterion(test_outputs, y_test_tensor)
                        
                        test_predictions = test_outputs.numpy()
                        r2 = r2_score(y_test, test_predictions)
                        
                        logger.info(f"Epoch {epoch}: 訓練損失 {loss.item():.4f}, 測試損失 {test_loss.item():.4f}, R² {r2:.4f}")
                        
                        if test_loss.item() < best_loss:
                            best_loss = test_loss.item()
                            patience = 0
                        else:
                            patience += 1
                    
                    self.model.train()
                
                if patience > 3:
                    logger.info("📊 早停觸發")
                    break
            
            # 最終評估
            self.model.eval()
            with torch.no_grad():
                final_predictions = self.model(X_test_tensor).numpy()
                final_r2 = r2_score(y_test, final_predictions)
                final_mse = mean_squared_error(y_test, final_predictions)
                
                logger.info("🎯 模型訓練完成！")
                logger.info(f"  R² Score: {final_r2:.4f}")
                logger.info(f"  MSE: {final_mse:.4f}")
                logger.info(f"  RMSE: ${np.sqrt(final_mse):,.2f}")
            
            return {
                "status": "success", 
                "r2_score": final_r2, 
                "mse": final_mse,
                "model_params": sum(p.numel() for p in self.model.parameters())
            }
            
        except Exception as e:
            logger.error(f"❌ 模型訓練失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def model_deployment_step(self):
        """Step 4: 模型部署準備"""
        logger.info("🚀 Step 4: 準備模型部署")
        
        try:
            # 保存模型
            model_path = "/Users/proerror/Documents/monday/agno_trained_model.pth"
            torch.save(self.model.state_dict(), model_path)
            
            # 模型信息
            model_info = {
                'timestamp': datetime.now().isoformat(),
                'model_type': 'TLOB_Transformer_v2',
                'agent_name': self.agent_name,
                'workspace': self.workspace,
                'features_count': self.features.shape[1] if hasattr(self, 'features') else 0,
                'samples_count': len(self.features) if hasattr(self, 'features') else 0,
                'data_source': 'real_market_data_clickhouse',
                'status': 'ready_for_deployment'
            }
            
            info_path = "/Users/proerror/Documents/monday/agno_model_info.json"
            with open(info_path, 'w') as f:
                json.dump(model_info, f, indent=2)
            
            logger.info(f"💾 模型已保存: {model_path}")
            logger.info(f"📋 模型信息已保存: {info_path}")
            
            return {"status": "success", "model_path": model_path, "info_path": info_path}
            
        except Exception as e:
            logger.error(f"❌ 模型部署準備失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def run_workflow(self):
        """運行完整的 ML 工作流"""
        logger.info("🚀 啟動 Agno ML 工作流")
        
        workflow_results = {
            "agent": self.agent_name,
            "workspace": self.workspace,
            "start_time": datetime.now().isoformat(),
            "steps": []
        }
        
        try:
            # Step 1: 數據加載
            result1 = await self.load_real_data_step()
            workflow_results["steps"].append({"step": "data_loading", "result": result1})
            
            if result1["status"] != "success":
                raise Exception(f"數據加載失敗: {result1}")
            
            # Step 2: 特徵工程
            result2 = await self.feature_engineering_step()
            workflow_results["steps"].append({"step": "feature_engineering", "result": result2})
            
            if result2["status"] != "success":
                raise Exception(f"特徵工程失敗: {result2}")
            
            # Step 3: 模型訓練
            result3 = await self.model_training_step()
            workflow_results["steps"].append({"step": "model_training", "result": result3})
            
            if result3["status"] != "success":
                raise Exception(f"模型訓練失敗: {result3}")
            
            # Step 4: 模型部署準備
            result4 = await self.model_deployment_step()
            workflow_results["steps"].append({"step": "model_deployment", "result": result4})
            
            workflow_results["status"] = "completed"
            workflow_results["end_time"] = datetime.now().isoformat()
            
            logger.info("🎉 Agno ML 工作流成功完成！")
            
            # 保存工作流結果
            results_path = "/Users/proerror/Documents/monday/agno_workflow_results.json"
            with open(results_path, 'w') as f:
                json.dump(workflow_results, f, indent=2)
            
            logger.info(f"📊 工作流結果已保存: {results_path}")
            
            return workflow_results
            
        except Exception as e:
            workflow_results["status"] = "failed"
            workflow_results["error"] = str(e)
            workflow_results["end_time"] = datetime.now().isoformat()
            
            logger.error(f"❌ Agno ML 工作流失敗: {e}")
            return workflow_results

async def main():
    """主函數"""
    logger.info("🤖 啟動基於 Agno 架構的 HFT ML 系統")
    
    # 創建 ML Agent
    ml_agent = AgnoMLAgent()
    
    # 初始化
    await ml_agent.initialize()
    
    # 運行工作流
    results = await ml_agent.run_workflow()
    
    # 輸出結果
    if results["status"] == "completed":
        logger.info("✅ 所有步驟成功完成")
        logger.info(f"📊 處理了 {results['steps'][0]['result'].get('records', 0)} 條真實數據")
        logger.info(f"🧠 訓練了 {results['steps'][1]['result'].get('samples', 0)} 個樣本")
        
        if len(results['steps']) > 2:
            training_result = results['steps'][2]['result']
            logger.info(f"🎯 模型 R² Score: {training_result.get('r2_score', 'N/A'):.4f}")
    else:
        logger.error("❌ 工作流執行失敗")
    
    return results

if __name__ == "__main__":
    result = asyncio.run(main())
    print(f"\n🎉 Agno ML 工作流完成！狀態: {result['status']}")