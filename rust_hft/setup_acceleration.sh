#!/bin/bash
# HFT 本地加速設定腳本

echo "🚀 設定 HFT 本地加速環境..."

# 1. 安裝 sccache (編譯快取)
echo "📦 安裝 sccache..."
if ! command -v sccache &> /dev/null; then
    cargo install sccache
    echo "✅ sccache 安裝完成"
else
    echo "✅ sccache 已安裝"
fi

# 2. 安裝快速 linker
echo "🔗 檢查 linker..."
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    # Linux: 安裝 mold
    if ! command -v mold &> /dev/null; then
        echo "📦 安裝 mold (Linux)..."
        # Ubuntu/Debian
        if command -v apt &> /dev/null; then
            sudo apt update && sudo apt install -y mold
        # Arch Linux  
        elif command -v pacman &> /dev/null; then
            sudo pacman -S mold
        # 其他發行版用 cargo 安裝
        else
            cargo install mold
        fi
    else
        echo "✅ mold 已安裝"
    fi
elif [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS: 安裝 lld
    if ! command -v lld &> /dev/null; then
        echo "📦 安裝 lld (macOS)..."
        brew install llvm
    else
        echo "✅ lld 已安裝"
    fi
fi

# 3. 設定環境變數
echo "⚙️  設定環境變數..."
export RUSTC_WRAPPER=sccache
export CARGO_INCREMENTAL=1

# 4. 顯示加速配置
echo ""
echo "🎯 加速配置已完成！"
echo "📈 編譯加速策略："
echo "   - sccache: 編譯結果快取"
echo "   - mold/lld: 快速 linker"  
echo "   - resolver=2: 防止 feature 合併"
echo "   - 增量編譯: 只編譯變更部分"
echo ""
echo "🧪 測試加速效果："
echo "   冷編譯: cargo clean && time cargo check"
echo "   熱編譯: 修改一個檔案後再次 cargo check"
echo ""
echo "📊 檢查 sccache 統計："
echo "   sccache -s"