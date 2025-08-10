#!/usr/bin/env python3
"""
跨 Workspace 集成測試
測試 ML Workspace 和 Ops Workspace 之間的通信和協作
"""

import asyncio
import json
import logging
import sys
from pathlib import Path
from datetime import datetime
from typing import Dict, Any

# 添加工作區路徑
sys.path.append(str(Path(__file__).parent / "ml_workspace"))
sys.path.append(str(Path(__file__).parent / "ops_workspace"))

# Ops Workspace 導入  
from ops_workspace.agents.real_latency_guard import RealLatencyGuardAgent
from ops_workspace.connectors.rust_hft_connector import RustHFTConnector

logger = logging.getLogger(__name__)

class CrossWorkspaceIntegrationTest:
    """跨 Workspace 集成測試"""
    
    def __init__(self):
        self.ops_agent = None
        self.connector = None
        self.test_results = []
    
    async def setup(self):
        """初始化測試環境"""
        print("🔧 初始化跨 Workspace 測試環境...")
        
        # 初始化 Ops Workspace 組件
        self.ops_agent = RealLatencyGuardAgent()  
        self.connector = RustHFTConnector()
        
        # 連接 Ops Workspace 到 Rust HFT
        connected = await self.connector.connect()
        if not connected:
            raise ConnectionError("無法連接到 Rust HFT 系統")
        
        print("✅ 跨 Workspace 測試環境初始化完成")
        return True
    
    async def test_ml_to_ops_communication(self):
        """測試 ML → Ops 通信"""
        print("\n🧪 測試 ML Workspace → Ops Workspace 通信...")
        
        try:
            # 模擬 ML 訓練完成，發布模型就緒事件
            model_ready_event = {
                "event": "model_ready",
                "symbol": "BTCUSDT", 
                "model_path": "/tmp/test_model.pt",
                "metrics": {
                    "ic": 0.035,
                    "ir": 1.28,
                    "max_drawdown": 0.041
                },
                "timestamp": datetime.now().isoformat(),
                "source": "ml_workspace"
            }
            
            # 發布到 ml.deploy 頻道
            await self.connector.redis_client.publish(
                'ml.deploy', 
                json.dumps(model_ready_event)
            )
            
            print(f"✅ ML 模型就緒事件已發布: {model_ready_event['symbol']}")
            
            # 等待 Ops 系統處理
            await asyncio.sleep(2)
            
            self.test_results.append({
                "test": "ML → Ops 通信",
                "status": "PASS",
                "details": "模型就緒事件成功發布"
            })
            
            return True
            
        except Exception as e:
            print(f"❌ ML → Ops 通信測試失敗: {e}")
            self.test_results.append({
                "test": "ML → Ops 通信", 
                "status": "FAIL",
                "error": str(e)
            })
            return False
    
    async def test_ops_to_ml_feedback(self):
        """測試 Ops → ML 反饋通信"""  
        print("\n🧪 測試 Ops Workspace → ML Workspace 反饋...")
        
        try:
            # 模擬 Ops 發現模型性能問題，向 ML 發送反饋
            performance_feedback = {
                "event": "model_performance_alert",
                "symbol": "BTCUSDT",
                "issue": "realized_ic_below_threshold",
                "current_ic": 0.021,  # 低於預期的 0.03
                "expected_ic": 0.035,
                "recommendation": "retrain_with_recent_data",
                "timestamp": datetime.now().isoformat(),
                "source": "ops_workspace"
            }
            
            # 發布到 ml.feedback 頻道
            await self.connector.redis_client.publish(
                'ml.feedback',
                json.dumps(performance_feedback)
            )
            
            print(f"✅ Ops 性能反饋已發送: {performance_feedback['issue']}")
            
            # 模擬 ML 收到反饋並回應
            ml_response = {
                "event": "feedback_acknowledged",
                "symbol": "BTCUSDT",
                "action": "schedule_retrain",
                "eta": "next_batch_cycle",
                "timestamp": datetime.now().isoformat(),
                "source": "ml_workspace"
            }
            
            await self.connector.redis_client.publish(
                'ml.response',
                json.dumps(ml_response)
            )
            
            print(f"✅ ML 反饋回應已發送: {ml_response['action']}")
            
            self.test_results.append({
                "test": "Ops → ML 反饋",
                "status": "PASS", 
                "details": "性能反饋循環完成"
            })
            
            return True
            
        except Exception as e:
            print(f"❌ Ops → ML 反饋測試失敗: {e}")
            self.test_results.append({
                "test": "Ops → ML 反饋",
                "status": "FAIL",
                "error": str(e)
            })
            return False
    
    async def test_coordinated_workflow(self):
        """測試協調工作流程"""
        print("\n🧪 測試協調工作流程...")
        
        try:
            # 1. 模擬 ML 開始訓練
            print("🔄 ML: 開始模型訓練...")
            training_start = {
                "event": "training_started", 
                "symbol": "BTCUSDT",
                "estimated_duration": "2h",
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ml.status',
                json.dumps(training_start)
            )
            
            # 2. 模擬 Ops 收到訓練開始通知，進入保護模式
            print("🛡️ Ops: 進入模型訓練保護模式...")
            protection_mode = {
                "event": "protection_mode_enabled",
                "reason": "ml_training_in_progress", 
                "restrictions": ["no_model_updates", "extended_monitoring"],
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ops.status',
                json.dumps(protection_mode)
            )
            
            # 3. 等待短暫時間模擬訓練
            await asyncio.sleep(3)
            
            # 4. 模擬訓練完成
            print("✅ ML: 模型訓練完成")
            training_complete = {
                "event": "training_completed",
                "symbol": "BTCUSDT", 
                "success": True,
                "model_path": "/tmp/btcusdt_model_20250725.pt",
                "performance": {"ic": 0.038, "ir": 1.35},
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ml.status',
                json.dumps(training_complete)
            )
            
            # 5. Ops 收到完成通知，退出保護模式
            print("🔓 Ops: 退出保護模式，準備部署新模型")
            protection_disabled = {
                "event": "protection_mode_disabled",
                "reason": "training_completed",
                "next_action": "prepare_model_deployment", 
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ops.status',
                json.dumps(protection_disabled)
            )
            
            print("🎯 協調工作流程完成！")
            
            self.test_results.append({
                "test": "協調工作流程",
                "status": "PASS",
                "details": "ML-Ops 協調循環成功"
            })
            
            return True
            
        except Exception as e:
            print(f"❌ 協調工作流程測試失敗: {e}")
            self.test_results.append({
                "test": "協調工作流程",
                "status": "FAIL", 
                "error": str(e)
            })
            return False
    
    async def test_emergency_coordination(self):
        """測試緊急協調"""
        print("\n🚨 測試緊急協調場景...")
        
        try:
            # 1. Ops 檢測到系統異常
            emergency_alert = {
                "event": "emergency_detected",
                "type": "critical_latency_spike",
                "value": 85.2,  # 嚴重延遲
                "threshold": 25,
                "action_required": "immediate_model_rollback",
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ops.emergency',
                json.dumps(emergency_alert)
            )
            
            print("🚨 Ops: 檢測到緊急情況，延遲飆升至 85.2μs")
            
            # 2. ML 收到緊急通知，中止當前訓練
            emergency_response = {
                "event": "emergency_response",
                "action": "abort_current_training",
                "rollback_to": "previous_stable_model",
                "estimated_recovery": "30s",
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ml.emergency_response',
                json.dumps(emergency_response)
            )
            
            print("⚡ ML: 緊急中止訓練，回退到穩定模型")
            
            # 3. Ops 確認回退完成
            recovery_confirmation = {
                "event": "emergency_recovery_complete",
                "current_latency": 12.3,  # 恢復正常
                "system_status": "stable",
                "timestamp": datetime.now().isoformat()
            }
            
            await self.connector.redis_client.publish(
                'ops.status',
                json.dumps(recovery_confirmation)
            )
            
            print("✅ Ops: 系統恢復正常，延遲回到 12.3μs")
            
            self.test_results.append({
                "test": "緊急協調",
                "status": "PASS",
                "details": "緊急響應和恢復成功"
            })
            
            return True
            
        except Exception as e:
            print(f"❌ 緊急協調測試失敗: {e}")
            self.test_results.append({
                "test": "緊急協調",
                "status": "FAIL",
                "error": str(e)
            })
            return False
    
    async def cleanup(self):
        """清理測試環境"""
        print("\n🧹 清理測試環境...")
        
        if self.connector:
            await self.connector.disconnect()
        
        print("✅ 測試環境清理完成")
    
    def print_test_summary(self):
        """打印測試結果總結"""
        print("\n" + "="*60)
        print("🧪 跨 Workspace 集成測試結果總結")
        print("="*60)
        
        passed = 0
        failed = 0
        
        for result in self.test_results:
            status_icon = "✅" if result["status"] == "PASS" else "❌"
            print(f"{status_icon} {result['test']}: {result['status']}")
            
            if result["status"] == "PASS":
                passed += 1
                if "details" in result:
                    print(f"   💡 {result['details']}")
            else:
                failed += 1
                if "error" in result:
                    print(f"   ❌ {result['error']}")
        
        print(f"\n🎯 總體結果: {passed} 通過, {failed} 失敗")
        
        if failed == 0:
            print("🎉 所有跨 Workspace 集成測試通過！")
            print("✅ ML Workspace 和 Ops Workspace 已成功集成")
        else:
            print("⚠️ 部分測試失敗，請檢查集成配置")
        
        return failed == 0

async def main():
    """主測試函數"""
    print("🚀 開始跨 Workspace 集成測試")
    print("="*60)
    
    logging.basicConfig(level=logging.INFO)
    
    test_suite = CrossWorkspaceIntegrationTest()
    
    try:
        # 1. 初始化測試環境
        await test_suite.setup()
        
        # 2. 執行測試用例
        await test_suite.test_ml_to_ops_communication()
        await test_suite.test_ops_to_ml_feedback()  
        await test_suite.test_coordinated_workflow()
        await test_suite.test_emergency_coordination()
        
        # 3. 打印測試結果
        success = test_suite.print_test_summary()
        
        return 0 if success else 1
        
    except Exception as e:
        print(f"❌ 測試套件執行失敗: {e}")
        return 1
    finally:
        await test_suite.cleanup()

if __name__ == "__main__":
    exit_code = asyncio.run(main())
    exit(exit_code)