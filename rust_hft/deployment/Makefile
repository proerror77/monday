# HFT 系統部署 Makefile
# 簡化常用部署操作

.PHONY: help build push deploy dev-up dev-down status logs clean

# 默認配置
REGISTRY ?= localhost:5000
IMAGE_TAG ?= latest
NAMESPACE := hft-trading

# 顏色
GREEN := \033[0;32m
NC := \033[0m

help:
	@echo "HFT 系統部署 Makefile"
	@echo ""
	@echo "使用方法:"
	@echo "  make build        - 構建所有 Docker 鏡像"
	@echo "  make push         - 推送鏡像到 Registry"
	@echo "  make deploy       - 部署到 Kubernetes"
	@echo "  make dev-up       - 啟動本地開發環境"
	@echo "  make dev-down     - 停止本地開發環境"
	@echo "  make status       - 顯示部署狀態"
	@echo "  make logs SVC=xxx - 顯示服務日誌"
	@echo "  make clean        - 清理本地資源"
	@echo "  make all          - 構建 + 推送 + 部署"
	@echo ""
	@echo "環境變數:"
	@echo "  REGISTRY=$(REGISTRY)"
	@echo "  IMAGE_TAG=$(IMAGE_TAG)"

# ============================================
# Docker 操作
# ============================================

build:
	@echo "$(GREEN)[BUILD]$(NC) 構建所有鏡像..."
	@./scripts/deploy.sh build

push:
	@echo "$(GREEN)[PUSH]$(NC) 推送鏡像..."
	@./scripts/deploy.sh push

# ============================================
# 本地開發環境
# ============================================

dev-up:
	@echo "$(GREEN)[DEV]$(NC) 啟動本地開發環境..."
	@cd docker && docker compose up -d redis clickhouse prometheus grafana
	@echo "$(GREEN)[DEV]$(NC) 環境已啟動:"
	@echo "  Redis: localhost:6379"
	@echo "  ClickHouse: localhost:8123"
	@echo "  Prometheus: localhost:9090"
	@echo "  Grafana: localhost:3000"

dev-down:
	@echo "$(GREEN)[DEV]$(NC) 停止本地開發環境..."
	@cd docker && docker compose down

dev-trading:
	@echo "$(GREEN)[DEV]$(NC) 啟動本地交易服務..."
	@cd docker && docker compose up -d

dev-logs:
	@cd docker && docker compose logs -f

# ============================================
# Kubernetes 部署
# ============================================

deploy:
	@echo "$(GREEN)[DEPLOY]$(NC) 部署到 Kubernetes..."
	@./scripts/deploy.sh deploy

deploy-infra:
	@echo "$(GREEN)[DEPLOY]$(NC) 部署基礎設施..."
	@kubectl apply -f k8s/namespace.yaml
	@kubectl apply -f k8s/configmaps.yaml
	@kubectl apply -f k8s/secrets.yaml
	@kubectl apply -f k8s/infrastructure.yaml

deploy-services:
	@echo "$(GREEN)[DEPLOY]$(NC) 部署服務..."
	@envsubst < k8s/sentinel.yaml | kubectl apply -f -
	@envsubst < k8s/market-data.yaml | kubectl apply -f -
	@envsubst < k8s/trading-engine.yaml | kubectl apply -f -

undeploy:
	@echo "$(GREEN)[UNDEPLOY]$(NC) 刪除所有資源..."
	@kubectl delete namespace $(NAMESPACE) --ignore-not-found

# ============================================
# 狀態和日誌
# ============================================

status:
	@echo "$(GREEN)[STATUS]$(NC) 檢查狀態..."
	@kubectl -n $(NAMESPACE) get pods,svc,deployments 2>/dev/null || \
		(cd docker && docker compose ps)

logs:
	@kubectl -n $(NAMESPACE) logs -f -l app=$(SVC) --tail=100 2>/dev/null || \
		docker logs -f hft-$(SVC)

# ============================================
# 清理
# ============================================

clean:
	@echo "$(GREEN)[CLEAN]$(NC) 清理資源..."
	@cd docker && docker compose down -v
	@docker image prune -f

clean-all: clean
	@echo "$(GREEN)[CLEAN]$(NC) 清理所有鏡像..."
	@docker rmi $(shell docker images -q "*/hft-*") 2>/dev/null || true

# ============================================
# 完整流程
# ============================================

all: build push deploy
	@echo "$(GREEN)[DONE]$(NC) 完整部署流程完成!"

# ============================================
# 開發輔助
# ============================================

test-connection:
	@echo "$(GREEN)[TEST]$(NC) 測試交易所連接..."
	@cargo run -p ws-connection-test

check:
	@echo "$(GREEN)[CHECK]$(NC) 檢查代碼..."
	@cargo check --workspace

test:
	@echo "$(GREEN)[TEST]$(NC) 運行測試..."
	@cargo test --workspace
