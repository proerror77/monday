#!/bin/bash
# Monday Research - 快速 Sandbox 测试
# 在现有运行的 Pod 中快速安装工具并测试

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}🚀 Monday Research Quick Sandbox${NC}"
echo ""

# 1. 查找运行中的 Pod
echo "📋 Finding running pods..."
PODS=$(kubectl get pods -n monday-research --field-selector=status.phase=Running -o jsonpath='{.items[*].metadata.name}')

if [ -z "$PODS" ]; then
    echo -e "${RED}❌ No running pods found in monday-research namespace${NC}"
    exit 1
fi

# 显示可用的 Pods
echo -e "${YELLOW}Available pods:${NC}"
i=1
for pod in $PODS; do
    echo "  $i) $pod"
    i=$((i+1))
done
echo ""

# 选择第一个 Pod 或让用户选择
POD_ARRAY=($PODS)
if [ ${#POD_ARRAY[@]} -eq 1 ]; then
    SELECTED_POD="${POD_ARRAY[0]}"
    echo -e "${GREEN}✓ Using pod: $SELECTED_POD${NC}"
else
    read -p "Select pod number (default 1): " choice
    choice=${choice:-1}
    SELECTED_POD="${POD_ARRAY[$((choice-1))]}"
    echo -e "${GREEN}✓ Selected: $SELECTED_POD${NC}"
fi
echo ""

# 2. 检查 Pod 状态
echo "🔍 Checking pod status..."
POD_STATUS=$(kubectl get pod $SELECTED_POD -n monday-research -o jsonpath='{.status.phase}')
if [ "$POD_STATUS" != "Running" ]; then
    echo -e "${RED}❌ Pod is not running (status: $POD_STATUS)${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Pod is running${NC}"
echo ""

# 3. 安装 DuckDB
echo "📦 Installing DuckDB..."
kubectl exec -n monday-research $SELECTED_POD -- bash -c "
    if command -v duckdb &> /dev/null; then
        echo 'DuckDB already installed'
        duckdb --version
    else
        echo 'Downloading DuckDB...'
        cd /tmp
        wget -q https://github.com/duckdb/duckdb/releases/download/v1.1.3/duckdb_cli-linux-amd64.zip
        unzip -q duckdb_cli-linux-amd64.zip
        chmod +x duckdb
        mv duckdb /usr/local/bin/ || cp duckdb /tmp/duckdb
        rm duckdb_cli-linux-amd64.zip
        echo 'DuckDB installed'
        /usr/local/bin/duckdb --version || /tmp/duckdb --version
    fi
" 2>&1 | grep -v "unable to upgrade connection"
echo -e "${GREEN}✓ DuckDB ready${NC}"
echo ""

# 4. 检查挂载点
echo "🗂️  Checking mounts..."
kubectl exec -n monday-research $SELECTED_POD -- bash -c "
    echo 'Checking /lake/raw...'
    if [ -d /lake/raw ]; then
        echo '✓ /lake/raw mounted'
        ls -la /lake/raw 2>/dev/null | head -3 || echo 'Cannot list (permission issue)'
    else
        echo '✗ /lake/raw NOT mounted'
    fi

    echo ''
    echo 'Checking /lake/output...'
    if [ -d /lake/output ]; then
        echo '✓ /lake/output mounted'
        ls -la /lake/output 2>/dev/null | head -3 || echo 'Cannot list (permission issue)'
    else
        echo '✗ /lake/output NOT mounted'
    fi
" 2>&1 | grep -v "unable to upgrade connection"
echo ""

# 5. 测试 DuckDB 基本查询
echo "🧪 Testing DuckDB..."
kubectl exec -n monday-research $SELECTED_POD -- bash -c "
    (/usr/local/bin/duckdb || /tmp/duckdb) -c \"SELECT 'Hello from DuckDB!' as message, version() as version;\"
" 2>&1 | grep -v "unable to upgrade connection"
echo -e "${GREEN}✓ DuckDB working${NC}"
echo ""

# 6. 检查是否有 Parquet 索引
echo "📊 Checking for segment index..."
INDEX_EXISTS=$(kubectl exec -n monday-research $SELECTED_POD -- bash -c "
    if [ -f /lake/output/metadata/segments.parquet ]; then
        echo 'true'
    else
        echo 'false'
    fi
" 2>&1 | grep -v "unable to upgrade connection" | tail -1)

if [ "$INDEX_EXISTS" = "true" ]; then
    echo -e "${GREEN}✓ Segment index found!${NC}"
    echo ""
    echo "📈 Querying index statistics..."
    kubectl exec -n monday-research $SELECTED_POD -- bash -c "
        (/usr/local/bin/duckdb || /tmp/duckdb) -c \"
        SELECT
            COUNT(*) as total_segments,
            COUNT(DISTINCT symbol) as unique_symbols,
            SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_segments,
            MIN(date) as first_date,
            MAX(date) as last_date
        FROM read_parquet('/lake/output/metadata/segments.parquet')
        \"
    " 2>&1 | grep -v "unable to upgrade connection"
else
    echo -e "${YELLOW}⚠️  Segment index not found at /lake/output/metadata/segments.parquet${NC}"
    echo "   This is expected if backfill hasn't run yet."
fi
echo ""

# 7. 提供交互命令
echo -e "${GREEN}✅ Sandbox is ready!${NC}"
echo ""
echo -e "${YELLOW}Available commands:${NC}"
echo ""
echo "# Connect to pod:"
echo "  kubectl exec -it -n monday-research $SELECTED_POD -- bash"
echo ""
echo "# Run DuckDB:"
echo "  kubectl exec -n monday-research $SELECTED_POD -- /usr/local/bin/duckdb"
echo ""
echo "# Query segment index (if exists):"
cat << 'EOF'
  kubectl exec -n monday-research $SELECTED_POD -- /usr/local/bin/duckdb -c "
    SELECT date, hour, symbol, replay_safe
    FROM read_parquet('/lake/output/metadata/segments.parquet')
    LIMIT 10
  "
EOF
echo ""
echo "# Test emergency fix script (copy first):"
echo "  kubectl cp scripts/emergency_fix_2026_09_02.sh monday-research/$SELECTED_POD:/tmp/"
echo "  kubectl exec -n monday-research $SELECTED_POD -- bash /tmp/emergency_fix_2026_09_02.sh"
echo ""
echo -e "${GREEN}Happy testing! 🎉${NC}"
