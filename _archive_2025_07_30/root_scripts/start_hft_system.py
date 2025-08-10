#!/usr/bin/env python3
"""
HFT 系統統一啟動腳本
===================

統一的主控制器方式啟動整個 HFT 系統，包括：
- 基礎設施檢查 (Redis, ClickHouse)
- Rust HFT 核心引擎啟動
- ML 工作區管理
- Ops 監控工作區
- 實時健康監控
"""

import asyncio
import logging
from master_hft_agent import MasterHFTAgent

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

async def main():
    """主函數 - 統一啟動 HFT 系統"""
    
    print("""
🚀 HFT 系統統一啟動器 v2.0
========================================
基於 Master Agent 架構的統一系統管理

架構優勢：
✅ 統一控制入口
✅ 自動依賴管理  
✅ 智能健康監控
✅ 故障自動恢復
✅ AI 驅動分析

開始啟動...
    """)
    
    master = MasterHFTAgent()
    
    try:
        await master.start_system()
    except KeyboardInterrupt:
        logger.info("\n🛑 收到停止信號，正在優雅關閉系統...")
    except Exception as e:
        logger.error(f"❌ 系統異常: {e}")
    finally:
        await master.stop_system()
        logger.info("✅ HFT 系統已安全關閉")

if __name__ == "__main__":
    asyncio.run(main())