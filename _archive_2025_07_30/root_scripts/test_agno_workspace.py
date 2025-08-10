#!/usr/bin/env python3
"""
測試 Agno Workspace 配置
驗證 ops_workspace 和 ml_workspace 是否正確配置
"""

import subprocess
import sys
import os
from pathlib import Path

def test_agno_installation():
    """測試 Agno 是否正確安裝"""
    print("🔍 檢查 Agno 安裝...")
    
    try:
        import agno
        print(f"✅ Agno 已安裝，版本: {agno.__version__ if hasattr(agno, '__version__') else '未知'}")
        return True
    except ImportError:
        print("❌ Agno 未安裝")
        print("請執行: pip3 install agno")
        return False

def test_ag_command():
    """測試 ag 命令是否可用"""
    print("\n🔍 檢查 ag 命令...")
    
    try:
        result = subprocess.run(['ag', '--version'], capture_output=True, text=True, timeout=10)
        if result.returncode == 0:
            print(f"✅ ag 命令可用: {result.stdout.strip()}")
            return True
        else:
            print("❌ ag 命令不可用")
            return False
    except (subprocess.TimeoutExpired, FileNotFoundError):
        print("❌ ag 命令未找到")
        print("請確保 Agno CLI 已正確安裝")
        return False

def test_workspace_structure(workspace_path, workspace_name):
    """測試 workspace 結構"""
    print(f"\n🔍 檢查 {workspace_name} 結構...")
    
    workspace_dir = Path(workspace_path)
    if not workspace_dir.exists():
        print(f"❌ {workspace_name} 目錄不存在: {workspace_path}")
        return False
    
    required_files = [
        "agno.toml",
        "workspace/settings.py",
        "workspace/dev_resources.py"
    ]
    
    missing_files = []
    for file in required_files:
        file_path = workspace_dir / file
        if file_path.exists():
            print(f"  ✅ {file}")
        else:
            print(f"  ❌ {file} (缺失)")
            missing_files.append(file)
    
    if missing_files:
        print(f"❌ {workspace_name} 缺少必要文件: {missing_files}")
        return False
    else:
        print(f"✅ {workspace_name} 結構完整")
        return True

def test_workspace_config(workspace_path, workspace_name):
    """測試 workspace 配置"""
    print(f"\n🔍 測試 {workspace_name} 配置...")
    
    original_cwd = os.getcwd()
    
    try:
        os.chdir(workspace_path)
        
        # 測試 ag ws status
        result = subprocess.run(['ag', 'ws', 'status'], capture_output=True, text=True, timeout=30)
        
        if result.returncode == 0:
            print(f"✅ {workspace_name} 配置有效")
            print(f"狀態: {result.stdout.strip()}")
            return True
        else:
            print(f"❌ {workspace_name} 配置錯誤")
            print(f"錯誤: {result.stderr.strip()}")
            return False
            
    except subprocess.TimeoutExpired:
        print(f"⚠️ {workspace_name} 狀態檢查超時")
        return False
    except Exception as e:
        print(f"❌ {workspace_name} 配置測試失敗: {e}")
        return False
    finally:
        os.chdir(original_cwd)

def test_dev_resources(workspace_path, workspace_name):
    """測試開發資源配置"""
    print(f"\n🔍 測試 {workspace_name} 開發資源...")
    
    try:
        # 添加路徑
        sys.path.insert(0, str(Path(workspace_path) / "workspace"))
        
        # 導入開發資源
        import dev_resources
        
        if hasattr(dev_resources, 'dev_resources'):
            resources = dev_resources.dev_resources
            print(f"✅ 找到 {len(resources)} 個開發資源:")
            
            for i, resource in enumerate(resources):
                if hasattr(resource, 'name'):
                    print(f"  {i+1}. {resource.name}")
                else:
                    print(f"  {i+1}. {type(resource).__name__}")
            
            return True
        else:
            print("❌ dev_resources.py 中未找到 dev_resources 列表")
            return False
            
    except ImportError as e:
        print(f"❌ 無法導入 dev_resources: {e}")
        return False
    except Exception as e:
        print(f"❌ 開發資源測試失敗: {e}")
        return False
    finally:
        # 清理路徑
        if str(Path(workspace_path) / "workspace") in sys.path:
            sys.path.remove(str(Path(workspace_path) / "workspace"))

def main():
    """主測試函數"""
    print("🧪 Agno Workspace 配置測試")
    print("=" * 50)
    
    # 基礎測試
    agno_installed = test_agno_installation()
    ag_available = test_ag_command()
    
    if not agno_installed or not ag_available:
        print("\n❌ 基礎環境不滿足，請先安裝 Agno")
        return False
    
    # Workspace 路徑
    base_path = Path(__file__).parent
    workspaces = [
        (base_path / "ops_workspace", "Ops Workspace"),
        (base_path / "ml_workspace", "ML Workspace")
    ]
    
    results = []
    
    for workspace_path, workspace_name in workspaces:
        # 結構測試
        structure_ok = test_workspace_structure(workspace_path, workspace_name)
        
        # 配置測試
        config_ok = test_workspace_config(workspace_path, workspace_name) if structure_ok else False
        
        # 開發資源測試
        resources_ok = test_dev_resources(workspace_path, workspace_name) if structure_ok else False
        
        results.append({
            "name": workspace_name,
            "structure": structure_ok,
            "config": config_ok,
            "resources": resources_ok,
            "overall": structure_ok and config_ok and resources_ok
        })
    
    # 測試結果總結
    print("\n" + "=" * 50)
    print("📊 測試結果總結")
    print("=" * 50)
    
    for result in results:
        status = "✅ 通過" if result["overall"] else "❌ 失敗"
        print(f"\n{result['name']}: {status}")
        print(f"  - 結構檢查: {'✅' if result['structure'] else '❌'}")
        print(f"  - 配置驗證: {'✅' if result['config'] else '❌'}")
        print(f"  - 開發資源: {'✅' if result['resources'] else '❌'}")
    
    all_passed = all(r["overall"] for r in results)
    
    print(f"\n🎯 整體結果: {'✅ 全部通過' if all_passed else '❌ 部分失敗'}")
    
    if all_passed:
        print("\n🚀 Agno Workspace 配置正確，可以使用以下命令啟動:")
        print("  cd ops_workspace && ag ws up --env dev")
        print("  cd ml_workspace && ag ws up --env dev")
    else:
        print("\n🔧 請修復上述問題後重新測試")
    
    return all_passed

if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)