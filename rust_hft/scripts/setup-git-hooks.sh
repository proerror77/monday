#!/bin/bash
# Git hooks 安裝腳本

set -e

HOOKS_DIR=".git/hooks"
SCRIPTS_DIR="scripts/git-hooks"

echo "🔧 安裝 Git Hooks"

# 創建 hooks 目錄
mkdir -p "$HOOKS_DIR"
mkdir -p "$SCRIPTS_DIR"

# 創建 pre-commit hook
cat > "$HOOKS_DIR/pre-commit" << 'EOF'
#!/bin/bash
# Pre-commit hook: 在提交前檢查代碼質量

set -e

echo "🔍 Pre-commit 檢查..."
echo ""

# 格式檢查
echo "📝 檢查代碼格式..."
if ! cargo fmt -- --check 2>&1 | grep -q "Diff"; then
    echo "✅ 代碼格式符合規範"
else
    echo "❌ 代碼格式不符，運行以下命令修復:"
    echo "   cargo fmt"
    exit 1
fi

# 核心 crates 快速編譯檢查
echo ""
echo "🔨 檢查核心 crates 編譯..."
CORE_CRATES=(
    "hft-core"
    "hft-snapshot"
    "hft-engine"
)

for crate in "${CORE_CRATES[@]}"; do
    echo -n "  ├─ 檢查 $crate... "
    if cargo check -p "$crate" --quiet 2>&1; then
        echo "✅"
    else
        echo "❌"
        echo ""
        echo "❌ $crate 編譯失敗，請修復後重試"
        exit 1
    fi
done

# Clippy 快速檢查（只檢查核心 crates）
echo ""
echo "📎 Clippy 檢查..."
if cargo clippy -p hft-core -p hft-snapshot -p hft-engine -- -D warnings 2>&1 | grep -q "error"; then
    echo "❌ Clippy 發現問題，請修復後重試"
    exit 1
else
    echo "✅ Clippy 檢查通過"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Pre-commit 檢查通過！"
echo ""
echo "💡 提示: 提交後 CI 會運行完整的 feature matrix 測試"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
EOF

chmod +x "$HOOKS_DIR/pre-commit"

echo ""
echo "✅ Pre-commit hook 安裝成功！"
echo ""
echo "📋 Hook 功能:"
echo "  1. 代碼格式檢查（cargo fmt）"
echo "  2. 核心 crates 編譯檢查"
echo "  3. Clippy 靜態分析"
echo ""
echo "⏱️  預計檢查時間: 20-30 秒"
echo ""
echo "💡 提示: 如需跳過 hook，使用 'git commit --no-verify'"
