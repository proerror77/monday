#!/usr/bin/env python3
"""
HFT系統啟動腳本
簡化的啟動接口
"""

import asyncio
import sys
import os

# 添加當前目錄到Python路徑
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from main import main

if __name__ == "__main__":
    print("🚀 啟動HFT Agno Agent系統...")
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n👋 系統已停止")
    except Exception as e:
        print(f"❌ 啟動失敗: {e}")
        sys.exit(1)