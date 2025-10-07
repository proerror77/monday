.PHONY: help dev test build clean down logs status check deps

# 默認目標
help: ## 顯示幫助信息
	@echo "HFT 系統運維命令"
	@echo ""
	@awk 'BEGIN {FS = ":.*##"; printf "\033[36m%-15s\033[0m %s\n", "命令", "描述"} /^[a-zA-Z_-]+:.*?##/ { printf "\033[36m%-15s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

# 開發環境
dev: ## 啟動完整開發環境
	@echo "🚀 啟動 HFT 開發環境..."
	cd ops && docker-compose up -d
	@echo "✅ 開發環境已啟動"
	@echo "📊 Grafana: http://localhost:3000 (admin/admin)"
	@echo "📈 Prometheus: http://localhost:9090"
	@echo "🗄️ ClickHouse: http://localhost:8123"

test: ## 啟動測試環境
	@echo "🧪 啟動測試環境..."
	cd ops && docker-compose -f docker-compose.test.yml up -d
	@echo "✅ 測試環境已啟動"

# 構建和部署
build: ## 構建所有應用
	@echo "🔨 構建 HFT 應用..."
	cargo build --release
	@echo "✅ 構建完成"

build-apps: ## 構建所有 apps
	@echo "🔨 構建所有應用..."
	cargo build --release -p hft-live
	cargo build --release -p hft-paper  
	cargo build --release -p hft-replay
	@echo "✅ 應用構建完成"

# 運行應用
run-live: ## 運行真盤應用
	@echo "💰 啟動真盤交易..."
	cargo run -p hft-live --features="bitget,trend-strategy,metrics"

run-quotes-only: ## 運行只行情（Binance）
	@echo "📈 啟動只行情（Binance）..."
	cargo run -p hft-live --features="binance" -- \
		--config config/dev/binance_quotes_only.yaml --exit-after-ms 15000

run-multi-quotes: ## 運行多交易所只行情（Binance/Bitget/Aster）
	@echo "📈 啟動多交易所只行情（Binance/Bitget/Aster）..."
	cargo run -p hft-live --features="binance,bitget,asterdex,metrics" -- \
		--config config/prod/system_accounts_multi_quotes_only.yaml --exit-after-ms 30000

run-paper: ## 運行模擬盤應用  
	@echo "📊 啟動模擬盤交易..."
	cargo run -p hft-paper

run-replay: ## 運行歷史回放
	@echo "⏮️ 啟動歷史回放..."
	cargo run -p hft-replay

# 測試
test-unit: ## 運行單元測試
	@echo "🧪 運行單元測試..."
	cargo test --lib

test-integration: ## 運行集成測試
	@echo "🔗 運行集成測試..."
	cargo test --test '*'

test-all: ## 運行所有測試
	@echo "🧪 運行所有測試..."
	cargo test

# 代碼質量
check: ## 檢查代碼質量
	@echo "🔍 檢查代碼質量..."
	cargo check --all-targets --all-features
	cargo clippy --all-targets --all-features -- -D warnings
	cargo fmt -- --check

fmt: ## 格式化代碼
	@echo "🎨 格式化代碼..."
	cargo fmt

fix: ## 自動修復代碼問題
	@echo "🔧 自動修復代碼..."
	cargo clippy --all-targets --all-features --fix --allow-dirty

# 服務管理
down: ## 停止所有服務
	@echo "⏹️ 停止所有服務..."
	cd ops && docker-compose down
	cd ops && docker-compose -f docker-compose.test.yml down 2>/dev/null || true
	@echo "✅ 所有服務已停止"

restart: ## 重啟服務
	@echo "🔄 重啟服務..."
	$(MAKE) down
	$(MAKE) dev

logs: ## 查看服務日誌
	@echo "📋 查看服務日誌..."
	cd ops && docker-compose logs -f

status: ## 檢查服務狀態
	@echo "📊 檢查服務狀態..."
	cd ops && docker-compose ps

# 數據庫管理
db-init: ## 初始化數據庫
	@echo "🗄️ 初始化 ClickHouse 數據庫..."
	@echo "等待 ClickHouse 啟動..."
	@sleep 10
	cat ops/clickhouse/init.sql | docker exec -i $$(cd ops && docker-compose ps -q clickhouse) clickhouse-client
	@echo "✅ 數據庫初始化完成"

db-connect: ## 連接到 ClickHouse
	@echo "🔗 連接到 ClickHouse..."
	docker exec -it $$(cd ops && docker-compose ps -q clickhouse) clickhouse-client

db-backup: ## 備份數據庫
	@echo "💾 備份數據庫..."
	mkdir -p backups
	docker exec $$(cd ops && docker-compose ps -q clickhouse) clickhouse-client --query "BACKUP DATABASE default TO Disk('backups', 'backup_$$(date +%Y%m%d_%H%M%S).zip')"

# 監控
metrics: ## 查看核心指標
	@echo "📊 查看核心指標..."
	@echo "執行延遲 p99:"
	@curl -s "http://localhost:9090/api/v1/query?query=hft_exec_latency_ms_p99" | jq -r '.data.result[0].value[1] // "N/A"'
	@echo "數據攝取延遲:"
	@curl -s "http://localhost:9090/api/v1/query?query=ingest_lag_ms" | jq -r '.data.result[0].value[1] // "N/A"'

health: ## 檢查系統健康狀態
	@echo "💚 檢查系統健康狀態..."
	@echo "ClickHouse:"
	@curl -s http://localhost:8123/ping && echo " ✅" || echo " ❌"
	@echo "Prometheus:"
	@curl -s http://localhost:9090/-/healthy && echo " ✅" || echo " ❌"
	@echo "Redis:"
	@docker exec $$(cd ops && docker-compose ps -q redis) redis-cli ping 2>/dev/null && echo " ✅" || echo " ❌"

# 清理
clean: ## 清理構建產物
	@echo "🧹 清理構建產物..."
	cargo clean
	@echo "✅ 清理完成"

clean-all: ## 清理所有數據（包括 Docker 卷）
	@echo "⚠️ 警告: 這將刪除所有數據!"
	@read -p "確定要繼續嗎? (y/N): " confirm && [ "$$confirm" = "y" ]
	$(MAKE) down
	cd ops && docker-compose down -v
	docker system prune -f
	@echo "✅ 所有數據已清理"

# 依賴管理
deps: ## 安裝/更新依賴
	@echo "📦 檢查依賴..."
	@command -v docker >/dev/null 2>&1 || { echo "❌ Docker 未安裝"; exit 1; }
	@command -v docker-compose >/dev/null 2>&1 || { echo "❌ Docker Compose 未安裝"; exit 1; }
	@command -v cargo >/dev/null 2>&1 || { echo "❌ Rust/Cargo 未安裝"; exit 1; }
	@echo "✅ 所有依賴已就緒"

# 版本信息
version: ## 顯示版本信息
	@echo "HFT 系統版本信息:"
	@echo "Rust: $$(rustc --version)"
	@echo "Cargo: $$(cargo --version)"
	@echo "Docker: $$(docker --version)"
	@echo "Docker Compose: $$(docker-compose --version 2>/dev/null || echo 'N/A')"
