#!/usr/bin/env python3
"""
Agno 工作區簡化啟動腳本
======================

基於實際安裝的 Agno 1.7.6，使用 Docker Compose 啟動所有工作區
"""

import os
import subprocess
import time
from pathlib import Path

def start_workspace_with_compose(workspace_name: str, port_ui: int, port_api: int):
    """使用 Docker Compose 啟動工作區"""
    
    workspace_path = Path(f"/Users/proerror/Documents/monday/{workspace_name}")
    if not workspace_path.exists():
        print(f"❌ 工作區目錄不存在: {workspace_path}")
        return False
    
    # 建立 docker-compose.yml
    compose_content = f"""version: '3.8'

services:
  {workspace_name}-ui:
    build:
      context: .
      dockerfile: Dockerfile
    container_name: {workspace_name}-ui
    ports:
      - "{port_ui}:{port_ui}"
    environment:
      - RUNTIME_ENV=dev
      - REDIS_URL=redis://host.docker.internal:6379
      - CLICKHOUSE_URL=http://host.docker.internal:8123
      - RUST_GRPC_URL=http://host.docker.internal:50051
    command: streamlit run ui/main.py --server.port={port_ui} --server.address=0.0.0.0 --server.headless=true
    restart: unless-stopped
    networks:
      - hft-network

  {workspace_name}-api:
    build:
      context: .
      dockerfile: Dockerfile
    container_name: {workspace_name}-api  
    ports:
      - "{port_api}:{port_api}"
    environment:
      - RUNTIME_ENV=dev
      - REDIS_URL=redis://host.docker.internal:6379
      - CLICKHOUSE_URL=http://host.docker.internal:8123
      - RUST_GRPC_URL=http://host.docker.internal:50051
    command: uvicorn api.main:app --host 0.0.0.0 --port {port_api} --reload
    restart: unless-stopped
    networks:
      - hft-network

networks:
  hft-network:
    external: true
"""
    
    compose_file = workspace_path / "docker-compose.yml"
    with open(compose_file, 'w') as f:
        f.write(compose_content)
    
    print(f"🚀 啟動 {workspace_name}...")
    
    try:
        # 切換到工作區目錄並啟動
        result = subprocess.run([
            "docker-compose", "up", "-d", "--build"
        ], cwd=workspace_path, capture_output=True, text=True)
        
        if result.returncode == 0:
            print(f"✅ {workspace_name} 啟動成功")
            print(f"   UI: http://localhost:{port_ui}")
            print(f"   API: http://localhost:{port_api}")
            return True
        else:
            print(f"❌ {workspace_name} 啟動失敗:")
            print(result.stderr)
            return False
            
    except Exception as e:
        print(f"❌ 啟動 {workspace_name} 時發生錯誤: {e}")
        return False

def main():
    """主函數"""
    print("🔧 Agno 工作區簡化啟動")
    print("=" * 40)
    
    # 檢查 Docker
    try:
        subprocess.run(["docker", "--version"], capture_output=True, check=True)
        subprocess.run(["docker-compose", "--version"], capture_output=True, check=True)
    except subprocess.CalledProcessError:
        print("❌ Docker 或 Docker Compose 未安裝")
        return
    
    # 建立網路
    try:
        subprocess.run([
            "docker", "network", "create", "hft-network"
        ], capture_output=True)
        print("✅ HFT 網路已建立")
    except:
        print("ℹ️  HFT 網路已存在")
    
    # 啟動各個工作區
    workspaces = [
        ("master_workspace", 8503, 8002),
        ("ops_workspace", 8501, 8001), 
        ("ml_workspace", 8502, 8003),
    ]
    
    success_count = 0
    for workspace_name, port_ui, port_api in workspaces:
        if start_workspace_with_compose(workspace_name, port_ui, port_api):
            success_count += 1
        time.sleep(2)  # 避免同時啟動造成衝突
    
    print("\n" + "=" * 40)
    print(f"✅ 成功啟動 {success_count}/{len(workspaces)} 個工作區")
    
    if success_count == len(workspaces):
        print("\n🎉 所有 HFT 工作區已就緒!")
        print("📊 Dashboard URLs:")
        print("   • Master: http://localhost:8503")
        print("   • Ops: http://localhost:8501") 
        print("   • ML: http://localhost:8502")

if __name__ == "__main__":
    main()