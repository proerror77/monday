"""
多儀器深度學習交易策略代理
===============================

整合ml_workspace中的多儀器訓練管道，實現實時信號生成和交易決策。
使用TCN/GRU模型預測edge/fill/collapse信號，結合風險控制生成交易指令。
"""

import asyncio
import logging
import json
import torch
from pathlib import Path
from typing import Dict, Any, List, Optional
from datetime import datetime

from agno import Agent
from control_ws.tools.hft_control import HFTControlTool

# 導入ML組件
from ml_workspace.feature_engineering import FeatureEngineer
from ml_workspace.multi_instrument_trainer import MultiInstrumentTrainer

logger = logging.getLogger(__name__)

class MultiInstrumentStrategyAgent(Agent):
    """
    多儀器深度學習交易策略代理
    
    核心職責：
    1. 實時特徵提取與模型推理
    2. 多任務信號解釋 (edge/fill/collapse)
    3. 風險調整與倉位管理
    4. 交易指令生成與執行
    5. 性能監控與模型更新
    """
    
    def __init__(self, 
                 name: str = "MultiInstrumentStrategyAgent",
                 model_dir: str = "models/multi_instrument_v1",
                 ch_host: str = "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443",
                 ch_username: str = "default",
                 ch_password: str = "sIiFK.4ygf.9R",
                 tools: List[Any] = None,
                 risk_limits: Dict[str, float] = None):
        super().__init__(name=name, tools=tools or [])
        
        # 配置參數
        self.model_dir = Path(model_dir)
        self.ch_host = ch_host
        self.ch_username = ch_username
        self.ch_password = ch_password
        
        # 風險限制
        self.risk_limits = risk_limits or {
            "max_position_size": 0.02,      # 2% 資金倉位限制
            "max_daily_loss": 0.05,         # 5% 日損失限制
            "confidence_threshold": 0.7,    # 信號置信度閾值
            "max_concurrent_trades": 5,     # 最大並發交易數
            "stop_loss_pct": 0.03,          # 3% 止損
            "take_profit_pct": 0.06         # 6% 止盈
        }
        
        # 初始化組件
        self.feature_engineer = None
        self.model = None
        self.model_config = None
        self.current_positions = {}  # symbol -> position_info
        self.daily_pnl = 0.0
        self.trade_history = []
        
        # 統計指標
        self.performance_metrics = {
            "total_trades": 0,
            "winning_trades": 0,
            "total_pnl": 0.0,
            "win_rate": 0.0,
            "sharpe_ratio": 0.0,
            "max_drawdown": 0.0
        }
        
        # 載入配置和模型
        self._load_deployment_config()
        self._initialize_ml_components()
        
        logger.info(f"✅ {name} 初始化完成 - 模型: {self.model_config['model_type']}")
    
    def _load_deployment_config(self):
        """載入部署配置"""
        config_path = self.model_dir / "deployment_config.json"
        if config_path.exists():
            with open(config_path, "r") as f:
                self.model_config = json.load(f)
            logger.info(f"✅ 部署配置載入: {config_path}")
        else:
            # 創建默認配置
            self.model_config = {
                "model": {"type": "tcn", "input_dim": 26, "sequence_length": 60},
                "features": {"total_features": 26},
                "trading_config": {"symbols": ["BTCUSDT", "ETHUSDT", "SOLUSDT", "WLFIUSDT"]},
                "risk_limits": self.risk_limits
            }
            logger.warning(f"⚠️ 配置檔案不存在，使用默認配置")
    
    def _initialize_ml_components(self):
        """初始化ML組件"""
        try:
            # 初始化特徵工程器
            self.feature_engineer = FeatureEngineer(
                ch_host=self.ch_host,
                ch_username=self.ch_username,
                ch_password=self.ch_password
            )
            
            # 載入模型
            self._load_trained_model()
            
            logger.info("✅ ML組件初始化完成")
        except Exception as e:
            logger.error(f"❌ ML組件初始化失敗: {e}")
            raise
    
    def _load_trained_model(self):
        """載入訓練好的模型"""
        model_path = self.model_dir / "multi_instrument_tcn_final.pt"
        if not model_path.exists():
            raise FileNotFoundError(f"模型檔案不存在: {model_path}")
        
        # 根據配置初始化模型
        in_dim = self.model_config["model"]["input_dim"]
        seq_len = self.model_config["model"]["sequence_length"]
        
        if self.model_config["model"]["type"] == "tcn":
            from ml_workspace.train import TCNModel
            self.model = TCNModel(in_dim=in_dim, widths=[32, 64, 64], drop=0.1)
        elif self.model_config["model"]["type"] == "gru":
            from ml_workspace.train import GRUModel
            self.model = GRUModel(in_dim=in_dim, hid=64, layers=2, drop=0.1)
        else:
            raise ValueError(f"不支持的模型類型: {self.model_config['model']['type']}")
        
        # 載入權重
        device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
        self.model.load_state_dict(torch.load(str(model_path), map_location=device))
        self.model.to(device)
        self.model.eval()
        
        self.device = device
        logger.info(f"✅ 模型載入完成: {model_path}, 設備: {device}")
    
    async def rollout(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        """代理執行邏輯 - 處理交易決策請求"""
        request_type = payload.get("request_type", "generate_signals")
        
        try:
            if request_type == "generate_signals":
                return await self._generate_trading_signals(payload)
            elif request_type == "execute_trade":
                return await self._execute_trade(payload)
            elif request_type == "get_positions":
                return self._get_current_positions()
            elif request_type == "get_performance":
                return self._get_performance_metrics()
            elif request_type == "retrain_model":
                return await self._retrain_model(payload)
            else:
                return {
                    "success": False,
                    "error": f"未知的請求類型: {request_type}",
                    "timestamp": datetime.now().isoformat()
                }
        except Exception as e:
            logger.error(f"❌ 代理執行失敗: {e}")
            return {
                "success": False,
                "error": str(e),
                "timestamp": datetime.now().isoformat()
            }
    
    async def _generate_trading_signals(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        """生成交易信號"""
        symbols = payload.get("symbols", self.model_config["trading_config"]["symbols"])
        confidence_threshold = payload.get("confidence_threshold", self.risk_limits["confidence_threshold"])
        
        signals = {}
        valid_signals = []
        
        for symbol in symbols:
            try:
                # 1. 提取實時特徵
                features = await self.feature_engineer.extract_real_time_features(symbol)
                
                if features is None:
                    logger.warning(f"⚠️ 無法提取 {symbol} 的實時特徵")
                    continue
                
                # 2. 模型推理
                with torch.no_grad():
                    # 確保特徵維度正確
                    expected_dim = self.model_config["model"]["input_dim"]
                    seq_len = self.model_config["model"]["sequence_length"]
                    
                    if features.shape[1] != expected_dim:
                        # 填充或截斷特徵
                        if features.shape[1] < expected_dim:
                            padding = torch.zeros(1, expected_dim - features.shape[1])
                            features = torch.cat([features, padding], dim=1)
                        else:
                            features = features[:, :expected_dim]
                    
                    # 創建序列 (重複最後時間步)
                    sequence = features.repeat(1, 1, seq_len).to(self.device)
                    sequence = sequence.transpose(1, 2)  # [B, L, D] -> [B, D, L] for TCN
                    
                    # 模型預測
                    if self.model_config["model"]["type"] == "tcn":
                        edge_pred, fill_pred, collapse_pred = self.model(sequence)
                    else:
                        edge_pred, fill_pred, collapse_pred = self.model(sequence.transpose(1, 2))
                    
                    # 轉換為概率
                    edge_prob = torch.sigmoid(edge_pred).cpu().numpy()[0]
                    fill_prob = torch.sigmoid(fill_pred).cpu().numpy()[0]
                    collapse_prob = torch.sigmoid(collapse_pred).cpu().numpy()[0]
                
                # 3. 信號解釋與決策
                signal = self._interpret_signals(
                    symbol, edge_prob, fill_prob, collapse_prob, confidence_threshold
                )
                
                if signal["action"] != "HOLD":
                    valid_signals.append(signal)
                
                signals[symbol] = {
                    "timestamp": datetime.now().isoformat(),
                    "edge_probability": float(edge_prob),
                    "fill_probability": float(fill_prob),
                    "collapse_probability": float(collapse_prob),
                    "action": signal["action"],
                    "confidence": signal["confidence"],
                    "suggested_size": signal["suggested_size"],
                    "risk_score": signal["risk_score"],
                    "features_shape": list(features.shape)
                }
                
                logger.info(f"📊 {symbol}: {signal['action']} (置信度: {signal['confidence']:.3f})")
                
            except Exception as e:
                logger.error(f"❌ {symbol} 信號生成失敗: {e}")
                signals[symbol] = {"error": str(e)}
                continue
        
        # 4. 全局風險檢查
        valid_signals = await self._apply_global_risk_controls(valid_signals)
        
        # 5. 執行交易
        execution_results = await self._execute_signals(valid_signals)
        
        result = {
            "success": True,
            "timestamp": datetime.now().isoformat(),
            "symbols_processed": len(signals),
            "valid_signals": len(valid_signals),
            "executed_trades": len(execution_results),
            "signals": signals,
            "execution_results": execution_results,
            "portfolio_exposure": self._calculate_portfolio_exposure(),
            "daily_pnl": self.daily_pnl
        }
        
        # 更新性能指標
        self._update_performance_metrics(result)
        
        logger.info(f"✅ 交易信號生成完成: {len(valid_signals)} 個有效信號, {len(execution_results)} 個執行")
        return result
    
    def _interpret_signals(self, symbol: str, edge_prob: float, fill_prob: float, collapse_prob: float, 
                          confidence_threshold: float) -> Dict[str, Any]:
        """解釋模型輸出並生成交易決策"""
        # 計算綜合置信度
        signal_strength = max(edge_prob, fill_prob, 1 - collapse_prob)
        confidence = min(signal_strength, 0.95)  # 上限95%
        
        # 基本決策邏輯
        if edge_prob > 0.6 and confidence > confidence_threshold:
            action = "BUY"
            risk_score = min((edge_prob - 0.5) * 2, 1.0)  # 0-1 風險分數
        elif collapse_prob > 0.6 and confidence > confidence_threshold:
            action = "SELL"
            risk_score = min((collapse_prob - 0.5) * 2, 1.0)
        elif fill_prob > 0.7:
            # 填補信號 - 根據當前倉位調整
            current_pos = self.current_positions.get(symbol, {"size": 0})
            if current_pos["size"] > 0:
                action = "SELL"  # 獲利了結
            elif current_pos["size"] < 0:
                action = "BUY"   # 止損回補
            else:
                action = "HOLD"
            risk_score = fill_prob * 0.8
        else:
            action = "HOLD"
            risk_score = 0.0
        
        # 計算建議倉位大小
        base_size = self.risk_limits["max_position_size"] * risk_score * confidence
        suggested_size = max(0.001, base_size)  # 最小0.1%
        
        return {
            "action": action,
            "confidence": confidence,
            "risk_score": risk_score,
            "suggested_size": suggested_size,
            "edge_prob": edge_prob,
            "fill_prob": fill_prob,
            "collapse_prob": collapse_prob,
            "timestamp": datetime.now().isoformat()
        }
    
    async def _apply_global_risk_controls(self, signals: List[Dict]) -> List[Dict]:
        """應用全局風險控制"""
        filtered_signals = []
        
        # 1. 檢查總曝光限制
        total_exposure = self._calculate_portfolio_exposure()
        if total_exposure > self.risk_limits["max_position_size"] * 0.8:
            logger.warning("⚠️ 總曝光接近限制，過濾高風險信號")
            signals = [s for s in signals if s["risk_score"] < 0.7]
        
        # 2. 檢查日損失限制
        if abs(self.daily_pnl) > self.risk_limits["max_daily_loss"] * 0.5:
            logger.warning("⚠️ 日損失接近限制，只允許低風險交易")
            signals = [s for s in signals if s["risk_score"] < 0.5]
        
        # 3. 檢查並發交易限制
        current_trades = len([p for p in self.current_positions.values() if p["status"] == "active"])
        if current_trades >= self.risk_limits["max_concurrent_trades"]:
            logger.warning("⚠️ 達到最大並發交易限制")
            return []
        
        # 4. 相關性檢查 (簡化版)
        crypto_symbols = ["BTCUSDT", "ETHUSDT"]
        if sum(1 for s in signals if s["symbol"] in crypto_symbols) > 2:
            logger.warning("⚠️ 加密貨幣相關性過高，過濾部分信號")
            signals = signals[:2]  # 只保留前2個
        
        # 5. 最終過濾
        for signal in signals:
            # 單個交易風險檢查
            if signal["suggested_size"] * signal["risk_score"] > 0.01:  # 單筆超過1%
                signal["suggested_size"] *= 0.8  # 減少倉位
            
            # 止損止盈檢查
            current_pos = self.current_positions.get(signal["symbol"], {})
            if current_pos and current_pos.get("unrealized_pnl", 0) < -self.risk_limits["stop_loss_pct"]:
                signal["action"] = "SELL" if current_pos["size"] > 0 else "BUY"
                signal["suggested_size"] = abs(current_pos["size"])  # 清倉
            
            filtered_signals.append(signal)
        
        return filtered_signals[:self.risk_limits["max_concurrent_trades"] - current_trades]
    
    async def _execute_signals(self, signals: List[Dict]) -> List[Dict]:
        """執行交易信號"""
        execution_results = []
        
        for signal in signals:
            try:
                symbol = signal["symbol"]
                action = signal["action"]
                size = signal["suggested_size"]
                confidence = signal["confidence"]
                
                # 使用HFT控制工具執行交易
                if hasattr(self, 'tools') and self.tools:
                    hft_tool = next((t for t in self.tools if isinstance(t, HFTControlTool)), None)
                    if hft_tool:
                        # 調用HFT工具執行交易
                        trade_request = {
                            "symbol": symbol,
                            "side": action,
                            "quantity": size,
                            "order_type": "market",
                            "confidence": confidence,
                            "strategy": "multi_instrument_dl",
                            "timestamp": datetime.now().isoformat()
                        }
                        
                        tool_result = await hft_tool.execute_trade(trade_request)
                        
                        # 更新本地狀態
                        self._update_position(symbol, action, size, tool_result)
                        self.trade_history.append({
                            "symbol": symbol,
                            "action": action,
                            "size": size,
                            "confidence": confidence,
                            "execution_result": tool_result,
                            "timestamp": datetime.now().isoformat()
                        })
                        
                        self.performance_metrics["total_trades"] += 1
                        
                        execution_results.append({
                            "symbol": symbol,
                            "action": action,
                            "status": tool_result.get("status", "unknown"),
                            "order_id": tool_result.get("order_id"),
                            "filled_quantity": tool_result.get("filled_quantity", 0),
                            "average_price": tool_result.get("average_price"),
                            "confidence": confidence
                        })
                        
                        logger.info(f"✅ {symbol} {action} 執行成功: {size} @ {confidence:.3f}")
                    else:
                        logger.warning(f"⚠️ 未找到HFT控制工具，跳過 {symbol} 執行")
                        execution_results.append({
                            "symbol": symbol,
                            "action": action,
                            "status": "tool_not_available",
                            "error": "HFT control tool not found"
                        })
                else:
                    # 模擬執行
                    logger.info(f"🔄 模擬執行 {symbol} {action} {size}")
                    execution_results.append({
                        "symbol": symbol,
                        "action": action,
                        "status": "simulated",
                        "filled_quantity": size,
                        "average_price": 50000.0,  # 模擬價格
                        "confidence": confidence
                    })
                    self._update_position(symbol, action, size, {
                        "status": "filled",
                        "filled_quantity": size,
                        "average_price": 50000.0
                    })
            
            except Exception as e:
                logger.error(f"❌ {symbol} 執行失敗: {e}")
                execution_results.append({
                    "symbol": signal["symbol"],
                    "action": signal["action"],
                    "status": "failed",
                    "error": str(e)
                })
        
        return execution_results
    
    def _update_position(self, symbol: str, action: str, size: float, execution_result: Dict):
        """更新倉位信息"""
        if symbol not in self.current_positions:
            self.current_positions[symbol] = {
                "size": 0.0,
                "entry_price": 0.0,
                "unrealized_pnl": 0.0,
                "status": "closed",
                "trades": []
            }
        
        position = self.current_positions[symbol]
        
        if action == "BUY":
            position["size"] += execution_result.get("filled_quantity", size)
            if position["size"] > 0 and position["entry_price"] == 0:
                position["entry_price"] = execution_result.get("average_price", 0)
        elif action == "SELL":
            position["size"] -= execution_result.get("filled_quantity", size)
        
        # 更新未實現盈虧
        if position["size"] != 0 and execution_result.get("average_price"):
            current_price = execution_result.get("average_price", position["entry_price"])
            position["unrealized_pnl"] = position["size"] * (current_price - position["entry_price"])
        
        position["status"] = "active" if position["size"] != 0 else "closed"
        position["last_update"] = datetime.now().isoformat()
        position["trades"].append({
            "action": action,
            "size": execution_result.get("filled_quantity", size),
            "price": execution_result.get("average_price"),
            "timestamp": datetime.now().isoformat()
        })
        
        # 更新日盈虧
        realized_pnl = execution_result.get("realized_pnl", 0)
        self.daily_pnl += realized_pnl
    
    def _calculate_portfolio_exposure(self) -> float:
        """計算投資組合總曝光"""
        total_exposure = 0.0
        for position in self.current_positions.values():
            if position["status"] == "active":
                total_exposure += abs(position["size"])
        return total_exposure
    
    def _get_current_positions(self) -> Dict[str, Any]:
        """獲取當前倉位"""
        return {
            "success": True,
            "timestamp": datetime.now().isoformat(),
            "positions": self.current_positions,
            "total_exposure": self._calculate_portfolio_exposure(),
            "daily_pnl": self.daily_pnl,
            "num_active_positions": len([p for p in self.current_positions.values() if p["status"] == "active"])
        }
    
    def _get_performance_metrics(self) -> Dict[str, Any]:
        """獲取性能指標"""
        if self.performance_metrics["total_trades"] > 0:
            self.performance_metrics["win_rate"] = self.performance_metrics["winning_trades"] / self.performance_metrics["total_trades"]
        
        return {
            "success": True,
            "timestamp": datetime.now().isoformat(),
            "metrics": self.performance_metrics.copy(),
            "positions": len(self.current_positions),
            "active_trades": len([p for p in self.current_positions.values() if p["status"] == "active"]),
            "daily_pnl": self.daily_pnl,
            "total_exposure": self._calculate_portfolio_exposure()
        }
    
    async def _retrain_model(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        """重新訓練模型"""
        try:
            hours = payload.get("hours", 168)  # 默認1週數據
            symbols = payload.get("symbols", None)
            
            logger.info(f"🔄 開始模型重新訓練，使用 {hours} 小時數據")
            
            # 使用訓練器重新訓練
            trainer = MultiInstrumentTrainer(
                ch_host=self.ch_host,
                ch_username=self.ch_username,
                ch_password=self.ch_password,
                model_dir=str(self.model_dir),
                hours=hours,
                model_type=self.model_config["model"]["type"]
            )
            
            results = await trainer.run_complete_pipeline()
            
            # 更新模型
            self._load_trained_model()
            
            retrain_result = {
                "success": True,
                "timestamp": datetime.now().isoformat(),
                "retrained_at": datetime.now().isoformat(),
                "instruments_processed": len(results["features_data"]),
                "best_val_loss": results["training_results"]["best_val_loss"],
                "model_path": results["training_results"]["model_path"],
                "hours_used": hours,
                "symbols": list(results["features_data"].keys()) if symbols is None else symbols
            }
            
            logger.info(f"✅ 模型重新訓練完成: 損失 {results['training_results']['best_val_loss']:.6f}")
            return retrain_result
            
        except Exception as e:
            logger.error(f"❌ 模型重新訓練失敗: {e}")
            return {
                "success": False,
                "error": str(e),
                "timestamp": datetime.now().isoformat()
            }
    
    def _update_performance_metrics(self, result: Dict):
        """更新性能指標"""
        executed_trades = result.get("execution_results", [])
        for trade in executed_trades:
            if trade["status"] == "filled":
                self.performance_metrics["total_trades"] += 1
                # 簡化勝率計算 - 實際應根據後續盈虧更新
                if trade.get("realized_pnl", 0) > 0:
                    self.performance_metrics["winning_trades"] += 1
                self.performance_metrics["total_pnl"] += trade.get("realized_pnl", 0)
    
    async def monitor_and_rebalance(self) -> Dict[str, Any]:
        """監控倉位並重新平衡"""
        logger.info("🔍 執行倉位監控和重新平衡")
        
        rebalance_actions = []
        
        for symbol, position in self.current_positions.items():
            if position["status"] != "active":
                continue
            
            try:
                # 獲取最新價格和信號
                current_features = await self.feature_engineer.extract_real_time_features(symbol)
                if current_features is None:
                    continue
                
                # 重新評估倉位
                with torch.no_grad():
                    seq_len = self.model_config["model"]["sequence_length"]
                    expected_dim = self.model_config["model"]["input_dim"]
                    
                    # 準備輸入
                    if current_features.shape[1] != expected_dim:
                        if current_features.shape[1] < expected_dim:
                            padding = torch.zeros(1, expected_dim - current_features.shape[1])
                            current_features = torch.cat([current_features, padding], dim=1)
                        else:
                            current_features = current_features[:, :expected_dim]
                    
                    sequence = current_features.repeat(1, 1, seq_len).to(self.device)
                    
                    if self.model_config["model"]["type"] == "tcn":
                        edge_pred, fill_pred, collapse_pred = self.model(sequence)
                    else:
                        edge_pred, fill_pred, collapse_pred = self.model(sequence.transpose(1, 2))
                    
                    edge_prob = torch.sigmoid(edge_pred).cpu().numpy()[0]
                    fill_prob = torch.sigmoid(fill_pred).cpu().numpy()[0]
                    collapse_prob = torch.sigmoid(collapse_pred).cpu().numpy()[0]
                
                # 檢查止損止盈
                current_price = 50000.0  # 應從實時數據獲取
                unrealized_pnl_pct = position["unrealized_pnl"] / (abs(position["size"]) * position["entry_price"])
                
                rebalance_action = None
                
                if unrealized_pnl_pct <= -self.risk_limits["stop_loss_pct"]:
                    # 止損
                    rebalance_action = {
                        "symbol": symbol,
                        "action": "SELL" if position["size"] > 0 else "BUY",
                        "reason": "stop_loss",
                        "current_pnl_pct": unrealized_pnl_pct,
                        "confidence": 1.0
                    }
                    logger.info(f"🛑 {symbol} 觸發止損: PnL {unrealized_pnl_pct:.3f}")
                
                elif unrealized_pnl_pct >= self.risk_limits["take_profit_pct"]:
                    # 止盈
                    rebalance_action = {
                        "symbol": symbol,
                        "action": "SELL" if position["size"] > 0 else "BUY",
                        "reason": "take_profit",
                        "current_pnl_pct": unrealized_pnl_pct,
                        "confidence": 1.0
                    }
                    logger.info(f"🎉 {symbol} 觸發止盈: PnL {unrealized_pnl_pct:.3f}")
                
                elif collapse_prob > 0.8 and abs(position["size"]) > 0:
                    # 崩盤信號 - 緊急清倉
                    rebalance_action = {
                        "symbol": symbol,
                        "action": "SELL" if position["size"] > 0 else "BUY",
                        "reason": "collapse_signal",
                        "collapse_prob": float(collapse_prob),
                        "confidence": 0.95
                    }
                    logger.warning(f"🚨 {symbol} 檢測到崩盤信號 (概率: {collapse_prob:.3f})")
                
                # 執行重新平衡
                if rebalance_action:
                    execution_result = await self._execute_rebalance(symbol, rebalance_action)
                    rebalance_actions.append({
                        "action": rebalance_action,
                        "execution": execution_result
                    })
            
            except Exception as e:
                logger.error(f"❌ {symbol} 監控失敗: {e}")
                continue
        
        return {
            "success": True,
            "timestamp": datetime.now().isoformat(),
            "rebalance_actions": rebalance_actions,
            "active_positions": len([p for p in self.current_positions.values() if p["status"] == "active"]),
            "total_exposure": self._calculate_portfolio_exposure()
        }
    
    async def _execute_rebalance(self, symbol: str, action: Dict) -> Dict:
        """執行重新平衡操作"""
        size = abs(self.current_positions[symbol]["size"])
        side = action["action"]
        
        execution_request = {
            "symbol": symbol,
            "side": side,
            "quantity": size,
            "order_type": "market",
            "reason": action["reason"],
            "confidence": action.get("confidence", 0.8),
            "strategy": "rebalance",
            "timestamp": datetime.now().isoformat()
        }
        
        # 執行交易 (使用HFT工具或模擬)
        if hasattr(self, 'tools') and self.tools:
            hft_tool = next((t for t in self.tools if isinstance(t, HFTControlTool)), None)
            if hft_tool:
                result = await hft_tool.execute_trade(execution_request)
            else:
                # 模擬執行
                result = {
                    "status": "filled",
                    "order_id": f"sim_{datetime.now().timestamp()}",
                    "filled_quantity": size,
                    "average_price": 50000.0,
                    "realized_pnl": self.current_positions[symbol]["unrealized_pnl"]
                }
        else:
            # 模擬執行
            result = {
                "status": "filled",
                "order_id": f"sim_{datetime.now().timestamp()}",
                "filled_quantity": size,
                "average_price": 50000.0,
                "realized_pnl": self.current_positions[symbol]["unrealized_pnl"]
            }
        
        # 更新倉位 (清倉)
        self.current_positions[symbol]["size"] = 0.0
        self.current_positions[symbol]["status"] = "closed"
        self.current_positions[symbol]["unrealized_pnl"] = 0.0
        self.daily_pnl += result.get("realized_pnl", 0)
        
        if result["status"] == "filled":
            self.performance_metrics["total_trades"] += 1
            if result.get("realized_pnl", 0) > 0:
                self.performance_metrics["winning_trades"] += 1
        
        logger.info(f"✅ {symbol} 重新平衡執行: {action['reason']} PnL: {result.get('realized_pnl', 0):.2f}")
        return result


# ==========================================
# 工具整合
# ==========================================

def create_strategy_agent(tools: List[Any] = None) -> MultiInstrumentStrategyAgent:
    """創建策略代理實例"""
    hft_tools = [t for t in (tools or []) if isinstance(t, HFTControlTool)]
    
    agent = MultiInstrumentStrategyAgent(
        tools=tools,
        model_dir="models/multi_instrument_v1",
        risk_limits={
            "max_position_size": 0.02,
            "max_daily_loss": 0.05,
            "confidence_threshold": 0.7,
            "max_concurrent_trades": 5,
            "stop_loss_pct": 0.03,
            "take_profit_pct": 0.06
        }
    )
    
    logger.info(f"🎯 策略代理創建完成，HFT工具數量: {len(hft_tools)}")
    return agent


# ==========================================
# 測試和驗證
# ==========================================

async def test_strategy_agent():
    """測試策略代理"""
    from control_ws.tools.hft_control import HFTControlTool
    
    # 模擬工具
    mock_hft_tool = HFTControlTool()
    
    # 創建代理
    agent = create_strategy_agent(tools=[mock_hft_tool])
    
    # 測試信號生成
    payload = {
        "request_type": "generate_signals",
        "symbols": ["BTCUSDT", "ETHUSDT"],
        "confidence_threshold": 0.6
    }
    
    result = await agent.rollout(payload)
    print("信號生成測試結果:", json.dumps(result, indent=2, default=str))
    
    # 測試倉位查詢
    position_result = await agent.rollout({"request_type": "get_positions"})
    print("倉位查詢結果:", json.dumps(position_result, indent=2, default=str))
    
    # 測試性能指標
    perf_result = await agent.rollout({"request_type": "get_performance"})
    print("性能指標:", json.dumps(perf_result, indent=2, default=str))
    
    return agent


if __name__ == "__main__":
    # 運行測試
    asyncio.run(test_strategy_agent())
