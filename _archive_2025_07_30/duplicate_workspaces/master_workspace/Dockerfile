# Master Workspace Dockerfile
# ===========================
# 
# 構建 Master Workspace 容器
# 包含基礎設施管理和控制功能

FROM python:3.11-slim

# 設置工作目錄
WORKDIR /app

# 安裝系統依賴
RUN apt-get update && apt-get install -y \
    curl \
    wget \
    git \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# 安裝 Docker CLI (用於管理其他容器)
RUN curl -fsSL https://get.docker.com | sh

# 複製依賴文件
COPY requirements.txt .

# 安裝 Python 依賴
RUN pip install --no-cache-dir -r requirements.txt

# 複製應用代碼
COPY . .

# 創建必要的目錄
RUN mkdir -p /logs /config /models

# 設置環境變量
ENV PYTHONPATH="/app"
ENV PYTHONUNBUFFERED=1

# 暴露端口 (Agno 標準: Streamlit 8503, FastAPI 8002)
EXPOSE 8503 8002

# 健康檢查
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -f http://localhost:8503/ || exit 1

# 默認啟動 Streamlit (可通過 CMD 覆蓋)
CMD ["streamlit", "run", "ui/master_dashboard.py", "--server.port=8503", "--server.address=0.0.0.0"]