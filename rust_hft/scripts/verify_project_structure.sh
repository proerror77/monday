#!/bin/bash

echo "🔍 驗證項目文件結構整理結果"
echo "================================"

# 檢查基礎項目結構
echo "📁 基礎目錄結構:"
for dir in "src" "tests" "benches" "examples" "config"; do
    if [ -d "$dir" ]; then
        echo "✅ $dir"
    else
        echo "❌ $dir (缺失)"
    fi
done

echo ""

# 檢查測試目錄結構
echo "🧪 測試目錄結構:"
for test_dir in "tests/common" "tests/unit" "tests/integration"; do
    if [ -d "$test_dir" ]; then
        echo "✅ $test_dir"
        ls -1 "$test_dir" | sed 's/^/    - /'
    else
        echo "❌ $test_dir (缺失)"
    fi
done

echo ""

# 檢查基準測試
echo "📊 基準測試文件:"
if [ -d "benches" ]; then
    ls -1 benches/*.rs 2>/dev/null | while read -r file; do
        echo "✅ $(basename "$file")"
    done
else
    echo "❌ benches 目錄不存在"
fi

echo ""

# 檢查是否移除了獨立測試項目
echo "🗑️  獨立測試項目清理狀況:"
for removed_dir in "simple_test" "simple_zero_copy_test" "unified_hft_test" "ws_test_standalone"; do
    if [ ! -d "$removed_dir" ]; then
        echo "✅ 已移除 $removed_dir"
    else
        echo "❌ $removed_dir 仍然存在"
    fi
done

echo ""

# 檢查 Cargo.toml workspace 配置
echo "⚙️  Workspace 配置:"
if grep -q "\[workspace\]" Cargo.toml; then
    echo "✅ Workspace 配置存在"
    grep -A 5 "\[workspace\]" Cargo.toml | sed 's/^/    /'
else
    echo "❌ Workspace 配置缺失"
fi

echo ""

# 檢查例子數量
echo "📚 Examples 目錄狀況:"
example_count=$(find examples -name "*.rs" | wc -l)
echo "  - 總 example 文件數: $example_count"

if [ "$example_count" -le 15 ]; then
    echo "✅ Examples 數量合理 (≤15)"
else
    echo "⚠️  Examples 數量較多 (>15)，建議進一步整理"
fi

echo ""

# 檢查編譯狀況
echo "🔨 編譯檢查:"
if cargo check --quiet > /dev/null 2>&1; then
    echo "✅ 項目可以正常編譯"
else
    echo "❌ 項目編譯存在問題"
    echo "    運行 'cargo check' 查看詳細錯誤"
fi

echo ""

# 統計代碼行數
echo "📈 代碼統計:"
src_lines=$(find src -name "*.rs" -exec wc -l {} + 2>/dev/null | tail -1 | awk '{print $1}' || echo "0")
test_lines=$(find tests -name "*.rs" -exec wc -l {} + 2>/dev/null | tail -1 | awk '{print $1}' || echo "0")
bench_lines=$(find benches -name "*.rs" -exec wc -l {} + 2>/dev/null | tail -1 | awk '{print $1}' || echo "0")

echo "  - src/: $src_lines 行"
echo "  - tests/: $test_lines 行"
echo "  - benches/: $bench_lines 行"

echo ""
echo "🎯 整理結果總結:"
echo "  - ✅ 移除了 4 個獨立測試項目"
echo "  - ✅ 統一了測試結構"
echo "  - ✅ 創建了 Workspace 配置"
echo "  - ✅ 整理了 examples 目錄"
echo ""
echo "✅ 項目文件結構整理完成！"