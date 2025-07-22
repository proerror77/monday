#!/usr/bin/env python3
"""
HFT 系統測試運行器
=================

統一的測試運行工具，支持不同類型的測試執行
"""

import os
import sys
import subprocess
import argparse
from pathlib import Path
import time

def run_command(cmd, description):
    """運行命令並顯示結果"""
    print(f"\n🔍 {description}")
    print("=" * 50)
    
    start_time = time.time()
    result = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    end_time = time.time()
    
    print(f"命令: {cmd}")
    print(f"執行時間: {end_time - start_time:.2f}秒")
    
    if result.returncode == 0:
        print("✅ 成功")
        if result.stdout:
            print(result.stdout)
    else:
        print("❌ 失敗")
        if result.stderr:
            print("錯誤輸出:")
            print(result.stderr)
        if result.stdout:
            print("標準輸出:")
            print(result.stdout)
    
    return result.returncode == 0


def main():
    parser = argparse.ArgumentParser(description="HFT 系統測試運行器")
    
    # 測試類型選項
    parser.add_argument('--unit', action='store_true', help='運行單元測試')
    parser.add_argument('--integration', action='store_true', help='運行集成測試')
    parser.add_argument('--performance', action='store_true', help='運行性能測試')
    parser.add_argument('--e2e', action='store_true', help='運行端到端測試')
    parser.add_argument('--all', action='store_true', help='運行所有測試')
    parser.add_argument('--fast', action='store_true', help='只運行快速測試（排除 slow 標記）')
    
    # 測試選項
    parser.add_argument('--coverage', action='store_true', help='生成覆蓋率報告')
    parser.add_argument('--parallel', type=int, help='並行測試數量')
    parser.add_argument('--verbose', '-v', action='store_true', help='詳細輸出')
    parser.add_argument('--fail-fast', action='store_true', help='遇到失敗立即停止')
    
    # 特定測試
    parser.add_argument('--test-file', help='運行特定測試文件')
    parser.add_argument('--test-func', help='運行特定測試函數')
    
    args = parser.parse_args()
    
    # 確保在項目根目錄
    project_root = Path(__file__).parent
    os.chdir(project_root)
    
    # 檢查依賴
    print("🔧 檢查測試環境...")
    
    # 檢查 pytest 是否安裝
    try:
        import pytest
        print(f"✅ pytest {pytest.__version__} 已安裝")
    except ImportError:
        print("❌ pytest 未安裝，請運行: pip install pytest pytest-asyncio")
        return 1
    
    # 構建基本 pytest 命令 - 使用 uv run
    cmd_parts = ["uv", "run", "pytest"]
    
    if args.verbose:
        cmd_parts.append("-v")
    
    if args.fail_fast:
        cmd_parts.append("-x")
    
    if args.parallel:
        cmd_parts.extend(["-n", str(args.parallel)])
    
    if args.coverage:
        cmd_parts.extend([
            "--cov=workflows",
            "--cov=ml", 
            "--cov=system_integration",
            "--cov-report=html",
            "--cov-report=term-missing"
        ])
    
    # 添加測試標記
    markers = []
    
    if args.unit:
        markers.append("unit")
    if args.integration:
        markers.append("integration")
    if args.performance:
        markers.append("performance")
    if args.e2e:
        markers.append("e2e")
    
    if args.fast:
        cmd_parts.extend(["-m", "'not slow'"])
    elif markers:
        marker_expr = " or ".join(markers)
        cmd_parts.extend(["-m", marker_expr])
    elif not args.all:
        # 默認運行單元測試和集成測試
        cmd_parts.extend(["-m", "unit or integration"])
    
    # 添加特定測試
    if args.test_file:
        cmd_parts.append(args.test_file)
    elif args.test_func:
        cmd_parts.append(f"-k {args.test_func}")
    
    # 執行測試
    cmd = " ".join(cmd_parts)
    success = run_command(cmd, "運行測試套件")
    
    if success:
        print("\n🎉 所有測試通過！")
        
        # 如果生成了覆蓋率報告，顯示路徑
        if args.coverage:
            coverage_html = project_root / "htmlcov" / "index.html"
            if coverage_html.exists():
                print(f"📊 覆蓋率報告: {coverage_html}")
    else:
        print("\n❌ 測試失敗")
        return 1
    
    return 0


def run_test_suite():
    """預定義的測試套件"""
    print("🚀 運行 HFT 系統完整測試套件")
    print("=" * 60)
    
    test_suites = [
        {
            "name": "快速單元測試",
            "cmd": "uv run pytest tests/unit -m 'not slow' -v",
            "required": True
        },
        {
            "name": "集成測試", 
            "cmd": "uv run pytest tests/integration -v",
            "required": True
        },
        {
            "name": "性能基準測試",
            "cmd": "uv run pytest tests/performance -m 'not slow' -v",
            "required": False
        },
        {
            "name": "完整端到端測試",
            "cmd": "uv run pytest tests/integration/test_end_to_end_workflow.py -v",
            "required": False
        }
    ]
    
    results = []
    
    for suite in test_suites:
        success = run_command(suite["cmd"], suite["name"])
        results.append((suite["name"], success, suite["required"]))
        
        if not success and suite["required"]:
            print(f"\n❌ 必需測試套件 '{suite['name']}' 失敗，停止執行")
            break
    
    # 總結結果
    print("\n📋 測試結果總結:")
    print("=" * 50)
    
    all_passed = True
    for name, success, required in results:
        status = "✅ 通過" if success else "❌ 失敗"
        req_text = "（必需）" if required else "（可選）"
        print(f"{name} {req_text}: {status}")
        
        if not success and required:
            all_passed = False
    
    if all_passed:
        print("\n🎉 測試套件執行完成，關鍵測試全部通過！")
        return 0
    else:
        print("\n❌ 測試套件執行失敗，存在關鍵錯誤")
        return 1


if __name__ == "__main__":
    # 如果沒有參數，運行預定義測試套件
    if len(sys.argv) == 1:
        exit_code = run_test_suite()
    else:
        exit_code = main()
    
    sys.exit(exit_code)