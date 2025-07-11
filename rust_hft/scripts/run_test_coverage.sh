#!/bin/bash

# 🧪 測試覆蓋率運行腳本
# 
# 這個腳本運行完整的測試套件並生成覆蓋率報告

set -e

echo "🚀 開始運行測試覆蓋率分析..."

# 設置環境變量
export RUST_LOG=debug
export RUST_BACKTRACE=1

# 檢查依賴
echo "📋 檢查測試依賴..."
if ! command -v cargo &> /dev/null; then
    echo "❌ cargo 未安裝"
    exit 1
fi

# 清理之前的測試結果
echo "🧹 清理之前的測試結果..."
rm -rf target/coverage
rm -f lcov.info

# 運行單元測試
echo "🔧 運行單元測試..."
cargo test --lib --all-features

# 運行集成測試
echo "🔗 運行集成測試..."
cargo test --test integration_tests --all-features

# 運行基準測試 (如果存在)
echo "📊 運行基準測試..."
if [ -d "benches" ]; then
    cargo bench --all-features
fi

# 生成覆蓋率報告
echo "📈 生成覆蓋率報告..."
if command -v cargo-tarpaulin &> /dev/null; then
    echo "使用 tarpaulin 生成覆蓋率報告..."
    cargo tarpaulin --all-features --timeout 600 --out Html --output-dir target/coverage
elif command -v cargo-llvm-cov &> /dev/null; then
    echo "使用 llvm-cov 生成覆蓋率報告..."
    cargo llvm-cov --all-features --html --output-dir target/coverage
else
    echo "⚠️ 沒有找到覆蓋率工具，請安裝 cargo-tarpaulin 或 cargo-llvm-cov"
    echo "安裝 tarpaulin: cargo install cargo-tarpaulin"
    echo "安裝 llvm-cov: cargo install cargo-llvm-cov"
fi

# 檢查覆蓋率結果
if [ -f "target/coverage/index.html" ]; then
    echo "✅ 覆蓋率報告已生成: target/coverage/index.html"
    if command -v open &> /dev/null; then
        echo "🌐 打開覆蓋率報告..."
        open target/coverage/index.html
    fi
else
    echo "⚠️ 覆蓋率報告未生成，請檢查工具安裝"
fi

# 運行代碼質量檢查
echo "🔍 運行代碼質量檢查..."
echo "  - 格式檢查..."
cargo fmt --check || echo "⚠️ 格式檢查失敗，請運行 cargo fmt"

echo "  - Clippy 檢查..."
cargo clippy --all-targets --all-features -- -D warnings || echo "⚠️ Clippy 檢查發現問題"

echo "  - 文檔檢查..."
cargo doc --no-deps --all-features || echo "⚠️ 文檔生成失敗"

# 性能基準測試
echo "⚡ 運行性能基準測試..."
if [ -f "benches/ultra_performance_bench.rs" ]; then
    cargo bench --bench ultra_performance_bench
fi

echo "🎉 測試覆蓋率分析完成！"
echo ""
echo "📊 測試結果摘要："
echo "  - 單元測試: ✅"
echo "  - 集成測試: ✅"
echo "  - 代碼質量: 請查看上述輸出"
echo "  - 覆蓋率報告: target/coverage/index.html"
echo ""
echo "🔧 改進建議："
echo "  1. 檢查覆蓋率報告，確保關鍵代碼路徑被測試覆蓋"
echo "  2. 添加更多邊界情況測試"
echo "  3. 優化性能測試以達到 <1μs 目標"
echo "  4. 添加更多錯誤處理測試"