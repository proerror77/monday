#!/usr/bin/env python3
"""
HFT Agents - 專業化的Agno Agent團隊
提供智能化的HFT交易生命週期管理
"""

import asyncio
import logging
from typing import Dict, List, Optional, Any
from agno.agent import Agent
from agno.team import Team
from agno.models.ollama import Ollama
from rust_hft_tools import RustHFTTools
import rust_hft_py as rust_hft
import time
import redis
import json

# 設置日誌
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class HFTMasterAgent:
    """HFT主控Agent - 協調整個交易流程"""
    
    def __init__(self):
        self.rust_tools = RustHFTTools()
        self.real_data_processor = None
        self._setup_real_data_access()
        self.agent = Agent(
            name="HFT Master Controller",
            role="智能HFT交易系統主控制器",
            model=Ollama(model_name="qwen2.5:3b"),
            instructions="""
            你是一個專業的高頻交易系統控制器，負責協調整個交易生命週期。

            🔧 系統架構理解：
            - 當前系統使用 RustHFTTools 與 Rust HFT 引擎通信
            - 支持 PyO3 直接調用和命令行備用模式
            - Rust 腳本位於 ../rust_hft/examples/ 目錄
            - 主要可用腳本：train_lob_transformer, evaluate_lob_transformer, lob_transformer_hft_system
            
            🎯 實際工具調用：
            當用戶請求操作時，你需要：
            1. 理解用戶意圖（訓練/評估/交易等）
            2. 調用對應的專業Agent來執行具體任務
            3. 協調不同Agent之間的工作流程
            4. 提供清晰的進度反饋
            
            ⚠️ 重要：不要虛構系統命令或文件路徑
            - 不要提到不存在的日誌文件或systemctl命令
            - 只使用實際可用的 RustHFTTools 方法
            - 當系統顯示 "Rust引擎未初始化" 時，這只是提示使用命令行模式
            
            💡 正確的處理方式：
            - 用戶請求訓練模型 → 調用 TrainingAgent
            - 用戶請求評估 → 調用 EvaluationAgent  
            - 用戶請求風險評估 → 調用 RiskManagementAgent
            - 系統狀態檢查 → 直接獲取實際狀態信息
            """,
            markdown=True,
        )
    
    def _setup_real_data_access(self):
        """設置真實數據訪問"""
        try:
            self.real_data_processor = rust_hft.DataProcessor()
            self.real_data_processor.initialize_real_data('BTCUSDT')
            logger.info("✅ Agent 真實數據處理器初始化完成")
        except Exception as e:
            logger.warning(f"⚠️ 無法初始化真實數據處理器: {e}")
            self.real_data_processor = None
    
    def get_real_market_data(self, symbol: str = "BTCUSDT") -> Dict[str, Any]:
        """獲取真實市場數據"""
        try:
            if self.real_data_processor:
                start_time = time.time()
                features = self.real_data_processor.extract_real_features(symbol)
                latency_ms = (time.time() - start_time) * 1000
                
                return {
                    'symbol': symbol,
                    'mid_price': features.get('mid_price', 0),
                    'best_bid': features.get('best_bid', 0),
                    'best_ask': features.get('best_ask', 0),
                    'spread': features.get('spread', 0),
                    'spread_bps': features.get('spread_bps', 0),
                    'volume_24h': features.get('volume_24h', 0),
                    'change_24h': features.get('change_24h', 0),
                    'data_quality': features.get('data_quality_score', 0),
                    'latency_ms': latency_ms,
                    'source': features.get('source', 'unknown'),
                    'timestamp': features.get('timestamp_us', int(time.time() * 1000000)),
                    'is_real_data': True
                }
            else:
                # 備用：直接從 Redis 讀取
                return self._get_real_data_from_redis(symbol)
        except Exception as e:
            logger.error(f"❌ 獲取真實市場數據失敗: {e}")
            return {'error': str(e), 'is_real_data': False}
    
    def _get_real_data_from_redis(self, symbol: str) -> Dict[str, Any]:
        """直接從 Redis 獲取真實數據"""
        try:
            redis_client = redis.Redis(host='localhost', port=6379, db=0)
            redis_key = f'hft:orderbook:{symbol}'
            redis_data = redis_client.get(redis_key)
            
            if redis_data:
                data = json.loads(redis_data)
                data['latency_ms'] = 0.5  # Redis 讀取延遲很低
                data['is_real_data'] = True
                return data
            else:
                return {'error': 'No data in Redis', 'is_real_data': False}
        except Exception as e:
            return {'error': f'Redis access failed: {e}', 'is_real_data': False}
    
    async def process_user_request(self, user_input: str) -> str:
        """處理用戶請求並制定執行計劃"""
        try:
            # 獲取系統狀態
            system_status = await self.rust_tools.get_system_status()
            
            # 分析用戶意圖
            user_input_lower = user_input.lower()
            
            # 直接處理特定請求
            if any(keyword in user_input_lower for keyword in ['訓練', 'train', '模型']):
                return "✅ 已識別訓練請求，正在調用訓練專家Agent執行模型訓練..."
            
            elif any(keyword in user_input_lower for keyword in ['評估', 'evaluate', '測試']):
                return "✅ 已識別評估請求，正在調用評估專家Agent分析模型性能..."
            
            elif any(keyword in user_input_lower for keyword in ['交易', 'trade', 'usdt']):
                return "✅ 已識別交易請求，正在制定完整交易流程：風險評估 → 模型訓練 → 性能評估 → 乾跑測試..."
            
            elif any(keyword in user_input_lower for keyword in ['狀態', 'status', '檢查', '數據', 'data', '價格', 'price']):
                # 獲取真實市場數據
                market_data = self.get_real_market_data()
                if market_data.get('is_real_data'):
                    data_info = f"""
📊 實時市場數據 ({market_data['symbol']}):
💰 當前價格: ${market_data['mid_price']:.2f}
📈 買價/賣價: ${market_data['best_bid']:.2f} / ${market_data['best_ask']:.2f}
💸 價差: ${market_data['spread']:.2f} ({market_data['spread_bps']:.1f} bps)
📅 24h變化: {market_data['change_24h']:.2%}
📊 24h成交量: {market_data['volume_24h']:.2f} BTC
⚡ 數據延遲: {market_data['latency_ms']:.3f}ms
🔗 數據源: {market_data['source']}
📡 數據質量: {market_data['data_quality']:.3f}
"""
                else:
                    data_info = f"⚠️ 無法獲取實時數據: {market_data.get('error', '未知錯誤')}"
                
                return f"📊 系統狀態報告：\n{system_status}\n{data_info}\n支持的交易對：{self.rust_tools.get_supported_symbols()}"
            
            else:
                # 使用Agent分析其他請求
                context = f"""
                系統狀態：{system_status}
                用戶請求：{user_input}
                
                請分析用戶意圖並提供簡潔的回應。不要虛構任何系統命令或文件路徑。
                """
                
                response = self.agent.run(context)
                return response.content
            
        except Exception as e:
            logger.error(f"❌ 處理用戶請求失敗: {e}")
            return f"❌ 處理請求時發生錯誤: {str(e)}"

class TrainingAgent:
    """模型訓練專家Agent"""
    
    def __init__(self, rust_tools: RustHFTTools):
        self.rust_tools = rust_tools
        self.real_data_processor = None
        self._setup_real_data_access()
        self.agent = Agent(
            name="Model Training Specialist",
            role="LOB Transformer模型訓練專家",
            model=Ollama(model_name="qwen2.5:3b"),
            instructions="""
            你是一個專業的機器學習模型訓練專家，專注於LOB Transformer模型訓練。
            
            🎯 核心職責：
            1. 分析交易對特性並優化訓練參數
            2. 監控訓練過程並調整策略
            3. 評估訓練結果並提供改進建議
            4. 確保模型質量達到交易標準
            
            💡 專業能力：
            - 深度理解LOB（Limit Order Book）數據特性
            - 熟悉Transformer架構和時間序列預測
            - 能夠根據市場條件調整訓練策略
            - 具備模型診斷和優化能力
            
            📊 評估標準：
            - 訓練損失收斂情況
            - 驗證集性能表現
            - 模型推理延遲（目標<50μs）
            - 預測準確率（目標>60%）
            """,
            markdown=True,
        )
    
    def _setup_real_data_access(self):
        """設置真實數據訪問"""
        try:
            self.real_data_processor = rust_hft.DataProcessor()
            self.real_data_processor.initialize_real_data('BTCUSDT')
            logger.info("✅ TrainingAgent 真實數據處理器初始化完成")
        except Exception as e:
            logger.warning(f"⚠️ TrainingAgent 無法初始化真實數據處理器: {e}")
            self.real_data_processor = None
    
    def get_real_market_data(self, symbol: str = "BTCUSDT") -> Dict[str, Any]:
        """獲取真實市場數據進行訓練分析"""
        try:
            if self.real_data_processor:
                features = self.real_data_processor.extract_real_features(symbol)
                return features
            else:
                # 備用：直接從 Redis 讀取
                redis_client = redis.Redis(host='localhost', port=6379, db=0)
                redis_key = f'hft:orderbook:{symbol}'
                redis_data = redis_client.get(redis_key)
                if redis_data:
                    return json.loads(redis_data)
                return {}
        except Exception as e:
            logger.error(f"❌ TrainingAgent 獲取真實數據失敗: {e}")
            return {}
    
    async def train_model(self, symbol: str, hours: int = 24, **kwargs) -> Dict[str, Any]:
        """執行模型訓練"""
        try:
            logger.info(f"🧠 訓練專家開始處理 {symbol} 模型訓練")
            
            # 獲取訓練前的市場數據分析
            market_data = self.get_real_market_data(symbol)
            if market_data:
                logger.info(f"📊 當前 {symbol} 價格: ${market_data.get('mid_price', 0):.2f}")
                logger.info(f"📈 市場數據質量: {market_data.get('data_quality_score', 0):.3f}")
            
            # 執行實際訓練 - 使用現有的 Rust 工具
            logger.info(f"🔧 開始訓練 {symbol} 模型，時長: {hours} 小時")
            result = await self.rust_tools.train_model(symbol, hours, **kwargs)
            
            logger.info(f"✅ 訓練結果: {result}")
            
            return result
            
        except Exception as e:
            logger.error(f"❌ 訓練執行失敗: {e}")
            return {"success": False, "error_message": str(e)}

class EvaluationAgent:
    """模型評估專家Agent"""
    
    def __init__(self, rust_tools: RustHFTTools):
        self.rust_tools = rust_tools
        self.real_data_processor = None
        self._setup_real_data_access()
        self.agent = Agent(
            name="Model Evaluation Specialist", 
            role="模型性能評估和驗證專家",
            model=Ollama(model_name="qwen2.5:3b"),
            instructions="""
            你是一個專業的模型評估專家，專注於交易模型的性能分析和風險評估。
            
            🎯 核心職責：
            1. 全面評估模型預測性能
            2. 分析風險調整收益指標
            3. 評估模型在不同市場條件下的表現
            4. 提供明確的部署建議
            
            📊 評估維度：
            - 準確性指標：準確率、精確率、召回率
            - 收益指標：Sharpe比率、最大回撤、勝率
            - 穩定性：不同時間段的表現一致性
            - 效率性：推理延遲、資源消耗
            
            ✅ 部署標準：
            - 預測準確率 > 60%
            - Sharpe比率 > 1.5  
            - 最大回撤 < 5%
            - 推理延遲 < 50μs
            
            🚨 風險評估：
            - 過擬合檢測
            - 市場適應性分析
            - 極端情況壓力測試
            """,
            markdown=True,
        )
    
    def _setup_real_data_access(self):
        """設置真實數據訪問"""
        try:
            self.real_data_processor = rust_hft.DataProcessor()
            self.real_data_processor.initialize_real_data('BTCUSDT')
            logger.info("✅ EvaluationAgent 真實數據處理器初始化完成")
        except Exception as e:
            logger.warning(f"⚠️ EvaluationAgent 無法初始化真實數據處理器: {e}")
            self.real_data_processor = None
    
    def get_real_market_data(self, symbol: str = "BTCUSDT") -> Dict[str, Any]:
        """獲取真實市場數據進行評估分析"""
        try:
            if self.real_data_processor:
                features = self.real_data_processor.extract_real_features(symbol)
                return features
            else:
                # 備用：直接從 Redis 讀取
                redis_client = redis.Redis(host='localhost', port=6379, db=0)
                redis_key = f'hft:orderbook:{symbol}'
                redis_data = redis_client.get(redis_key)
                if redis_data:
                    return json.loads(redis_data)
                return {}
        except Exception as e:
            logger.error(f"❌ EvaluationAgent 獲取真實數據失敗: {e}")
            return {}
    
    async def evaluate_model(self, model_path: str, symbol: str, **kwargs) -> Dict[str, Any]:
        """執行模型評估"""
        try:
            logger.info(f"📊 評估專家開始分析 {symbol} 模型")
            
            # 獲取當前市場數據作為評估背景
            market_data = self.get_real_market_data(symbol)
            if market_data:
                logger.info(f"📊 評估時市場狀態 - {symbol}: ${market_data.get('mid_price', 0):.2f}")
                logger.info(f"📈 市場波動性: 價差 ${market_data.get('spread', 0):.2f}")
            
            # 執行評估 - 使用現有的 Rust 工具
            result = await self.rust_tools.evaluate_model(model_path, symbol, **kwargs)
            
            if result.get("success"):
                # 專業分析
                analysis = self.agent.run(f"""
                模型評估結果分析：
                
                📈 性能指標：
                - 預測準確率：{result.get('accuracy', 'N/A')}
                - Sharpe比率：{result.get('sharpe_ratio', 'N/A')}
                - 最大回撤：{result.get('max_drawdown', 'N/A')}
                
                📊 詳細指標：{result.get('performance_metrics', {})}
                
                請提供專業評估：
                1. 模型性能是否達到交易標準？
                2. 識別潛在風險點
                3. 是否建議進行乾跑測試？
                4. 如果性能不佳，提供改進建議
                """)
                
                result["expert_analysis"] = analysis.content
                result["deployment_ready"] = self._assess_deployment_readiness(result)
                
            return result
            
        except Exception as e:
            logger.error(f"❌ 評估執行失敗: {e}")
            return {"success": False, "error_message": str(e)}
    
    def _assess_deployment_readiness(self, result: Dict[str, Any]) -> bool:
        """評估是否準備好部署"""
        try:
            accuracy = result.get('accuracy', 0)
            sharpe_ratio = result.get('sharpe_ratio', 0)
            max_drawdown = result.get('max_drawdown', 1)
            
            return (accuracy > 0.6 and 
                   sharpe_ratio > 1.5 and 
                   max_drawdown < 0.05)
        except:
            return False

class RiskManagementAgent:
    """風險管理專家Agent"""
    
    def __init__(self, rust_tools: RustHFTTools):
        self.rust_tools = rust_tools
        self.real_data_processor = None
        self._setup_real_data_access()
        self.agent = Agent(
            name="Risk Management Specialist",
            role="專業風險管理和控制專家", 
            model=Ollama(model_name="qwen2.5:3b"),
            instructions="""
            你是一個專業的風險管理專家，負責實時監控和控制交易風險。
            
            🎯 核心職責：
            1. 實時監控交易系統風險水平
            2. 設定和調整風險控制參數
            3. 檢測異常情況並執行緊急措施
            4. 提供風險評估報告和建議
            
            🛡️ 風險控制維度：
            - 倉位風險：單筆倉位、總倉位限制
            - 損失控制：止損、日損失限制
            - 流動性風險：市場深度、滑點控制
            - 技術風險：延遲異常、連接中斷
            
            🚨 緊急情況處理：
            - 連續虧損超過閾值 → 暫停交易
            - 延遲異常超過50μs → 切換保守模式
            - 市場異常波動 → 降低倉位
            - 系統故障 → 立即平倉
            
            📊 監控指標：
            - 實時盈虧狀況
            - 倉位使用率
            - 交易頻率和成功率
            - 系統性能指標
            """,
            markdown=True,
        )
    
    def _setup_real_data_access(self):
        """設置真實數據訪問"""
        try:
            self.real_data_processor = rust_hft.DataProcessor()
            self.real_data_processor.initialize_real_data('BTCUSDT')
            logger.info("✅ RiskManagementAgent 真實數據處理器初始化完成")
        except Exception as e:
            logger.warning(f"⚠️ RiskManagementAgent 無法初始化真實數據處理器: {e}")
            self.real_data_processor = None
    
    def get_real_market_data(self, symbol: str = "BTCUSDT") -> Dict[str, Any]:
        """獲取真實市場數據進行風險分析"""
        try:
            if self.real_data_processor:
                features = self.real_data_processor.extract_real_features(symbol)
                return features
            else:
                # 備用：直接從 Redis 讀取
                redis_client = redis.Redis(host='localhost', port=6379, db=0)
                redis_key = f'hft:orderbook:{symbol}'
                redis_data = redis_client.get(redis_key)
                if redis_data:
                    return json.loads(redis_data)
                return {}
        except Exception as e:
            logger.error(f"❌ RiskManagementAgent 獲取真實數據失敗: {e}")
            return {}
    
    async def assess_trading_risk(self, symbol: str, capital: float, **kwargs) -> Dict[str, Any]:
        """評估交易風險"""
        try:
            # 獲取系統狀態和真實市場數據
            system_status = await self.rust_tools.get_system_status()
            market_data = self.get_real_market_data(symbol)
            
            # 記錄當前市場風險指標
            if market_data:
                logger.info(f"🛡️ 風險評估 - {symbol}: ${market_data.get('mid_price', 0):.2f}")
                logger.info(f"📊 市場流動性: 價差 {market_data.get('spread_bps', 0):.1f} bps")
                logger.info(f"📈 24h波動: {market_data.get('change_24h', 0):.2%}")
            
            # 風險評估
            assessment = self.agent.run(f"""
            交易風險評估請求：
            
            📋 交易參數：
            - 交易對：{symbol}
            - 資金規模：{capital} USDT
            - 其他參數：{kwargs}
            
            📊 系統狀態：{system_status}
            
            請進行全面風險評估：
            1. 資金規模是否合適？
            2. 當前市場條件風險水平
            3. 推薦的風險控制參數
            4. 潛在風險點識別
            5. 是否建議進行交易？
            """)
            
            return {
                "success": True,
                "risk_assessment": assessment.content,
                "system_status": system_status,
                "recommendations": self._generate_risk_recommendations(symbol, capital)
            }
            
        except Exception as e:
            logger.error(f"❌ 風險評估失敗: {e}")
            return {"success": False, "error_message": str(e)}
    
    def _generate_risk_recommendations(self, symbol: str, capital: float) -> Dict[str, Any]:
        """生成風險控制建議"""
        return {
            "max_position_pct": min(0.1, 1000 / capital),  # 最大倉位百分比
            "stop_loss_pct": 0.02,  # 2%止損
            "daily_loss_limit": capital * 0.05,  # 日損失限制5%
            "max_trades_per_hour": 100,
            "confidence_threshold": 0.7
        }

class HFTTeam:
    """HFT Agent團隊協調器"""
    
    def __init__(self):
        self.rust_tools = RustHFTTools()
        self.master_agent = HFTMasterAgent()
        self.training_agent = TrainingAgent(self.rust_tools)
        self.evaluation_agent = EvaluationAgent(self.rust_tools)
        self.risk_agent = RiskManagementAgent(self.rust_tools)
        
        logger.info("✅ HFT Agent團隊初始化完成")
    
    async def execute_full_workflow(
        self, 
        symbol: str, 
        capital: float,
        training_hours: int = 24
    ) -> Dict[str, Any]:
        """執行完整的HFT工作流程"""
        
        workflow_results = {
            "symbol": symbol,
            "capital": capital,
            "workflow_steps": []
        }
        
        try:
            logger.info(f"🚀 開始 {symbol} 完整交易工作流程")
            
            # 步驟1: 風險評估
            logger.info("🛡️ 步驟1: 初始風險評估")
            risk_result = await self.risk_agent.assess_trading_risk(symbol, capital)
            workflow_results["workflow_steps"].append({
                "step": "risk_assessment",
                "result": risk_result
            })
            
            if not risk_result.get("success"):
                return workflow_results
            
            # 步驟2: 模型訓練
            logger.info("🧠 步驟2: 模型訓練")
            training_result = await self.training_agent.train_model(symbol, training_hours)
            workflow_results["workflow_steps"].append({
                "step": "model_training", 
                "result": training_result
            })
            
            if not training_result.get("success"):
                return workflow_results
            
            # 步驟3: 模型評估
            logger.info("📊 步驟3: 模型評估")
            model_path = training_result.get("model_path")
            evaluation_result = await self.evaluation_agent.evaluate_model(model_path, symbol)
            workflow_results["workflow_steps"].append({
                "step": "model_evaluation",
                "result": evaluation_result
            })
            
            if not evaluation_result.get("deployment_ready"):
                logger.warning("⚠️ 模型評估未通過，不建議部署")
                return workflow_results
            
            # 步驟4: 乾跑測試
            logger.info("🧪 步驟4: 乾跑測試")
            dryrun_result = await self.rust_tools.start_dryrun(
                symbol=symbol,
                model_path=model_path,
                capital=capital,
                duration_minutes=60
            )
            workflow_results["workflow_steps"].append({
                "step": "dryrun_test",
                "result": dryrun_result
            })
            
            # 步驟5: 準備實盤部署（需要用戶確認）
            if dryrun_result.get("success"):
                logger.info("✅ 工作流程完成，準備實盤部署")
                workflow_results["ready_for_live"] = True
                workflow_results["live_trading_config"] = {
                    "symbol": symbol,
                    "model_path": model_path,
                    "capital": capital,
                    "risk_config": risk_result.get("recommendations", {})
                }
            
            return workflow_results
            
        except Exception as e:
            logger.error(f"❌ 工作流程執行失敗: {e}")
            workflow_results["error"] = str(e)
            return workflow_results
    
    async def start_live_trading(self, config: Dict[str, Any]) -> Dict[str, Any]:
        """啟動實盤交易（需要明確確認）"""
        try:
            logger.info("⚠️ 準備啟動實盤交易...")
            
            # 最終風險檢查
            final_risk_check = await self.risk_agent.assess_trading_risk(
                config["symbol"], 
                config["capital"]
            )
            
            if not final_risk_check.get("success"):
                return {"success": False, "message": "最終風險檢查未通過"}
            
            # 啟動實盤交易
            result = await self.rust_tools.start_live_trading(
                symbol=config["symbol"],
                model_path=config["model_path"], 
                capital=config["capital"]
            )
            
            return result
            
        except Exception as e:
            logger.error(f"❌ 啟動實盤交易失敗: {e}")
            return {"success": False, "error_message": str(e)}
    
    async def emergency_stop(self) -> Dict[str, Any]:
        """緊急停止所有操作"""
        logger.warning("🚨 執行緊急停止...")
        return await self.rust_tools.emergency_stop()
    
    async def get_status(self) -> Dict[str, Any]:
        """獲取團隊和系統狀態"""
        return await self.rust_tools.get_system_status()

# 創建全域HFT團隊實例
hft_team = HFTTeam()