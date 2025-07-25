#!/usr/bin/env python3
"""
HFT Agno主控制台 - 用戶交互入口
提供自然語言驅動的HFT交易系統
"""

import asyncio
import logging
import sys
from typing import Optional
import signal
from hft_agents import HFTTeam
from pipeline_manager import pipeline_manager, PipelineType

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class HFTConversationInterface:
    """HFT對話界面 - 提供自然語言交互"""
    
    def __init__(self):
        self.hft_team = None
        self.running = False
        
    async def initialize(self):
        """初始化HFT Agent團隊"""
        try:
            logger.info("🚀 正在初始化HFT Agent團隊...")
            self.hft_team = HFTTeam()
            
            # 檢查系統狀態
            status = await self.hft_team.get_status()
            logger.info(f"📊 系統狀態: {status}")
            
            self.running = True
            logger.info("✅ HFT系統初始化完成，準備接收指令")
            
        except Exception as e:
            logger.error(f"❌ 初始化失敗: {e}")
            raise
    
    async def start_conversation(self):
        """開始對話循環"""
        print("\n" + "="*60)
        print("🤖 HFT Agno Agent - 智能高頻交易助手")
        print("="*60)
        print("💡 使用自然語言與我對話，例如:")
        print("   - '訓練SOLUSDT模型'")
        print("   - '同時訓練多個商品'")
        print("   - '用1000 USDT交易BTCUSDT'")
        print("   - '檢查系統狀態'")
        print("   - '停止所有任務'")
        print("   - '退出'")
        print("="*60)
        
        while self.running:
            try:
                # 獲取用戶輸入
                user_input = input("\n💬 您: ").strip()
                
                if not user_input:
                    continue
                
                # 處理退出命令
                if user_input.lower() in ['退出', 'exit', 'quit', 'bye']:
                    print("👋 再見！正在安全關閉系統...")
                    await self.shutdown()
                    break
                
                # 處理用戶請求
                await self.process_user_request(user_input)
                
            except KeyboardInterrupt:
                print("\n\n🛑 檢測到Ctrl+C，正在安全關閉...")
                await self.shutdown()
                break
            except EOFError:
                print("\n👋 會話結束")
                await self.shutdown()
                break
            except Exception as e:
                logger.error(f"❌ 處理用戶輸入失敗: {e}")
                print(f"❌ 發生錯誤: {e}")
    
    async def process_user_request(self, user_input: str):
        """處理用戶請求"""
        try:
            print("🤔 正在分析您的請求...")
            
            # 使用主控Agent分析請求
            response = await self.hft_team.master_agent.process_user_request(user_input)
            print(f"\n🤖 Agent: {response}")
            
            # 檢查是否是具體的交易請求
            if await self._is_trading_request(user_input):
                await self._handle_trading_request(user_input)
            elif await self._is_status_request(user_input):
                await self._handle_status_request()
            elif await self._is_stop_request(user_input):
                await self._handle_stop_request()
            elif await self._is_multi_training_request(user_input):
                await self._handle_multi_training_request(user_input)
            
        except Exception as e:
            logger.error(f"❌ 處理請求失敗: {e}")
            print(f"❌ 處理請求時發生錯誤: {e}")
    
    async def _is_trading_request(self, user_input: str) -> bool:
        """使用LLM判斷是否是交易相關請求"""
        response = self.hft_team.master_agent.agent.run(f"""
        請分析以下用戶輸入，判斷是否屬於交易相關請求。
        交易相關請求包括：模型訓練、交易執行、性能評估、乾跑測試等。
        
        用戶輸入："{user_input}"
        
        請只回答：是 或 否
        """)
        
        return "是" in response.content
    
    async def _is_status_request(self, user_input: str) -> bool:
        """判斷是否是狀態查詢"""
        status_keywords = ['狀態', '狀況', 'status', '檢查', '查看']
        return any(keyword in user_input.lower() for keyword in status_keywords)
    
    async def _is_stop_request(self, user_input: str) -> bool:
        """判斷是否是停止請求"""
        stop_keywords = ['停止', '暫停', 'stop', '緊急', 'emergency']
        return any(keyword in user_input.lower() for keyword in stop_keywords)
    
    async def _is_multi_training_request(self, user_input: str) -> bool:
        """判斷是否是多商品訓練請求"""
        multi_keywords = ['多個', '同時', '批量', '全部', '所有']
        training_keywords = ['訓練', 'train']
        return any(multi_kw in user_input.lower() for multi_kw in multi_keywords) and any(train_kw in user_input.lower() for train_kw in training_keywords)
    
    async def _handle_trading_request(self, user_input: str):
        """處理交易請求"""
        try:
            user_input_lower = user_input.lower()
            
            # 處理單純的訓練請求
            if any(keyword in user_input_lower for keyword in ['訓練', 'train']) and not any(keyword in user_input_lower for keyword in ['usdt', '交易']):
                print("🧠 開始執行模型訓練...")
                
                # 解析交易對
                symbol = self._extract_symbol(user_input)
                if not symbol:
                    symbol = "SOLUSDT"  # 默認使用SOLUSDT
                
                print(f"📊 訓練參數: 交易對={symbol}, 訓練時長=24小時")
                
                # 使用Pipeline管理器提交訓練任務
                task_id = await pipeline_manager.submit_task(
                    PipelineType.TRAINING,
                    symbol,
                    hours=24
                )
                
                print(f"✅ 訓練任務已提交，任務ID: {task_id}")
                print("💡 使用 '檢查任務狀態' 或 '查看所有任務' 來監控進度")
                
                return
            
            # 處理完整交易請求
            symbol, capital = self._parse_trading_params(user_input)
            
            if symbol and capital:
                print(f"\n🚀 開始執行交易流程:")
                print(f"   - 交易對: {symbol}")
                print(f"   - 資金: {capital} USDT")
                print("   - 流程: 訓練 → 評估 → 測試 → 部署")
                
                confirmation = input("\n❓ 確認執行完整工作流程? (y/N): ").strip().lower()
                
                if confirmation in ['y', 'yes', '是', '確認']:
                    # 執行完整工作流程
                    result = await self.hft_team.execute_full_workflow(symbol, capital)
                    
                    print(f"\n📊 工作流程執行完成:")
                    for step in result.get('workflow_steps', []):
                        step_name = step['step']
                        success = step['result'].get('success', False)
                        status = "✅" if success else "❌"
                        print(f"   {status} {step_name}")
                    
                    # 檢查是否準備好實盤交易
                    if result.get('ready_for_live'):
                        print("\n🎯 系統已準備好實盤交易！")
                        live_confirmation = input("❓ 是否立即開始實盤交易? (y/N): ").strip().lower()
                        
                        if live_confirmation in ['y', 'yes', '是', '確認']:
                            live_result = await self.hft_team.start_live_trading(
                                result['live_trading_config']
                            )
                            print(f"💰 實盤交易啟動結果: {live_result}")
                        else:
                            print("💡 實盤交易配置已保存，您可以稍後手動啟動")
                else:
                    print("❌ 取消執行")
            else:
                print("❌ 無法解析交易參數，請提供明確的交易對和資金金額")
                
        except Exception as e:
            logger.error(f"❌ 處理交易請求失敗: {e}")
            print(f"❌ 交易處理失敗: {e}")
    
    async def _handle_status_request(self):
        """處理狀態查詢"""
        try:
            # 獲取系統狀態
            status = await self.hft_team.get_status()
            print(f"\n📊 系統狀態報告:")
            print(f"   - 整體狀態: {status.get('overall_status', 'unknown')}")
            print(f"   - 運行時間: {status.get('uptime_seconds', 0)} 秒")
            
            # 獲取Pipeline狀態
            pipeline_status = await pipeline_manager.get_status()
            print(f"\n🔄 Pipeline狀態:")
            print(f"   - 活躍任務數: {pipeline_status.get('active_tasks', 0)}")
            print(f"   - 等待任務數: {pipeline_status.get('pending_tasks', 0)}")
            print(f"   - 完成任務數: {pipeline_status.get('completed_tasks', 0)}")
            
            # 顯示活躍任務詳情
            active_tasks = pipeline_status.get('task_details', {})
            if active_tasks:
                print(f"\n📋 活躍任務詳情:")
                for task_id, task_info in active_tasks.items():
                    status_icon = "🔄" if task_info['status'] == 'running' else "⏳"
                    print(f"   {status_icon} {task_id}: {task_info['symbol']} - {task_info['type']} ({task_info['status']})")
            
        except Exception as e:
            logger.error(f"❌ 獲取狀態失敗: {e}")
            print(f"❌ 無法獲取系統狀態: {e}")
    
    async def _handle_stop_request(self):
        """處理停止請求"""
        try:
            print("🛑 正在停止所有任務...")
            
            # 停止所有pipeline任務
            pipeline_result = await pipeline_manager.stop_all_tasks()
            print(f"📋 Pipeline停止結果: {pipeline_result}")
            
            # 停止HFT系統
            hft_result = await self.hft_team.emergency_stop()
            print(f"🤖 HFT系統停止結果: {hft_result}")
            
        except Exception as e:
            logger.error(f"❌ 停止操作失敗: {e}")
            print(f"❌ 停止失敗: {e}")
    
    def _extract_symbol(self, user_input: str) -> Optional[str]:
        """從用戶輸入中提取交易對"""
        try:
            user_input = user_input.upper()
            symbols = ['BTCUSDT', 'ETHUSDT', 'SOLUSDT', 'ADAUSDT', 'DOTUSDT']
            
            for symbol in symbols:
                if symbol in user_input or symbol.replace('USDT', '') in user_input:
                    return symbol
            return None
            
        except Exception as e:
            logger.error(f"❌ 提取交易對失敗: {e}")
            return None

    def _parse_trading_params(self, user_input: str) -> tuple[Optional[str], Optional[float]]:
        """解析交易參數"""
        try:
            user_input = user_input.upper()
            
            # 支持的交易對
            symbols = ['BTCUSDT', 'ETHUSDT', 'SOLUSDT', 'ADAUSDT', 'DOTUSDT']
            symbol = None
            capital = None
            
            # 查找交易對
            for s in symbols:
                if s in user_input or s.replace('USDT', '') in user_input:
                    symbol = s
                    break
            
            # 查找資金金額
            import re
            numbers = re.findall(r'\d+(?:\.\d+)?', user_input)
            if numbers:
                capital = float(numbers[0])
            
            return symbol, capital
            
        except Exception as e:
            logger.error(f"❌ 解析交易參數失敗: {e}")
            return None, None
    
    async def _handle_multi_training_request(self, user_input: str):
        """處理多商品訓練請求"""
        try:
            print("🔄 正在啟動多商品訓練...")
            
            # 預設的主要交易對
            default_symbols = ['BTCUSDT', 'ETHUSDT', 'SOLUSDT', 'ADAUSDT', 'DOTUSDT']
            
            # 檢查用戶是否指定了特定商品
            specified_symbols = []
            for symbol in default_symbols:
                if symbol in user_input.upper() or symbol.replace('USDT', '') in user_input.upper():
                    specified_symbols.append(symbol)
            
            # 如果沒有指定，詢問用戶
            if not specified_symbols:
                print("🤔 您想要訓練哪些商品？")
                print("可選商品：", ", ".join(default_symbols))
                
                selection = input("💬 請輸入商品代號（用逗號分隔），或輸入'all'選擇全部: ").strip()
                
                if selection.lower() == 'all':
                    specified_symbols = default_symbols
                else:
                    specified_symbols = [s.strip().upper() for s in selection.split(',') if s.strip()]
                    # 驗證輸入的商品代號
                    valid_symbols = []
                    for symbol in specified_symbols:
                        if symbol in default_symbols:
                            valid_symbols.append(symbol)
                        elif symbol + 'USDT' in default_symbols:
                            valid_symbols.append(symbol + 'USDT')
                    specified_symbols = valid_symbols
            
            if not specified_symbols:
                print("❌ 沒有選擇有效的商品")
                return
            
            print(f"🎯 即將開始訓練：{', '.join(specified_symbols)}")
            
            # 確認執行
            confirmation = input(f"❓ 確認同時訓練 {len(specified_symbols)} 個商品？ (y/N): ").strip().lower()
            
            if confirmation not in ['y', 'yes', '是', '確認']:
                print("❌ 取消執行")
                return
            
            # 提交所有訓練任務
            task_ids = []
            for symbol in specified_symbols:
                task_id = await pipeline_manager.submit_task(
                    PipelineType.TRAINING,
                    symbol,
                    hours=24
                )
                task_ids.append(task_id)
                print(f"✅ {symbol} 訓練任務已提交，任務ID: {task_id}")
            
            print(f"\n🚀 總共提交了 {len(task_ids)} 個訓練任務")
            print("💡 使用以下命令監控進度：")
            print("   - '檢查狀態' 或 '查看所有任務'")
            print("   - '停止所有任務'")
            
        except Exception as e:
            logger.error(f"❌ 處理多商品訓練請求失敗: {e}")
            print(f"❌ 多商品訓練處理失敗: {e}")
    
    async def shutdown(self):
        """安全關閉系統"""
        try:
            self.running = False
            
            # 停止pipeline管理器
            if pipeline_manager:
                await pipeline_manager.stop_all_tasks()
                logger.info("✅ Pipeline管理器已停止")
            
            # 停止HFT團隊
            if self.hft_team:
                await self.hft_team.emergency_stop()
                logger.info("✅ HFT團隊已停止")
            
            logger.info("✅ 系統已安全關閉")
            
        except Exception as e:
            logger.error(f"❌ 關閉系統失敗: {e}")

async def main():
    """主函數"""
    interface = HFTConversationInterface()
    
    # 設置信號處理
    def signal_handler(signum, frame):
        print("\n🛑 收到停止信號...")
        asyncio.create_task(interface.shutdown())
    
    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)
    
    try:
        # 初始化系統
        await interface.initialize()
        
        # 開始對話
        await interface.start_conversation()
        
    except KeyboardInterrupt:
        print("\n👋 程序已中斷")
    except Exception as e:
        logger.error(f"❌ 主程序異常: {e}")
        print(f"❌ 系統錯誤: {e}")
    finally:
        await interface.shutdown()

if __name__ == "__main__":
    asyncio.run(main())