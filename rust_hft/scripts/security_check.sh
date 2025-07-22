#!/bin/bash
#
# 全面安全檢查腳本
# 檢查依賴漏洞、許可證合規性、代碼質量
#

set -euo pipefail

# 顏色輸出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

echo_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

echo_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

echo_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 檢查必要工具
check_tools() {
    echo_info "檢查必要工具..."
    
    local missing_tools=()
    
    if ! command -v cargo >/dev/null; then
        missing_tools+=("cargo")
    fi
    
    if ! command -v cargo-audit >/dev/null; then
        echo_warning "cargo-audit 未安裝，正在安裝..."
        cargo install cargo-audit
    fi
    
    if [ ${#missing_tools[@]} -ne 0 ]; then
        echo_error "缺少必要工具: ${missing_tools[*]}"
        exit 1
    fi
    
    echo_success "所有必要工具已安裝"
}

# 運行安全審計
run_security_audit() {
    echo_info "運行依賴安全審計..."
    
    # 更新漏洞數據庫
    echo_info "更新漏洞數據庫..."
    cargo audit --update || echo_warning "無法更新漏洞數據庫"
    
    # 運行安全審計
    if cargo audit --json > audit_results.json 2>/dev/null; then
        local vuln_count=$(jq -r '.vulnerabilities.count // 0' audit_results.json 2>/dev/null || echo "0")
        
        if [ "$vuln_count" -eq 0 ]; then
            echo_success "未發現安全漏洞"
        else
            echo_error "發現 $vuln_count 個安全漏洞"
            echo_info "詳細信息:"
            cargo audit --color=always
            return 1
        fi
    else
        echo_error "安全審計失敗"
        return 1
    fi
}

# 檢查許可證合規性
check_license_compliance() {
    echo_info "檢查許可證合規性..."
    
    # 安裝 cargo-license 如果不存在
    if ! command -v cargo-license >/dev/null; then
        echo_info "安裝 cargo-license..."
        cargo install cargo-license
    fi
    
    # 檢查許可證
    echo_info "生成許可證報告..."
    cargo license --json > licenses.json
    
    # 檢查不兼容的許可證
    local incompatible_licenses=("GPL-3.0" "AGPL-3.0" "LGPL-3.0")
    local found_incompatible=false
    
    for license in "${incompatible_licenses[@]}"; do
        local count=$(jq -r ".[] | select(.license == \"$license\") | .name" licenses.json 2>/dev/null | wc -l)
        if [ "$count" -gt 0 ]; then
            echo_error "發現不兼容許可證: $license"
            jq -r ".[] | select(.license == \"$license\") | \"  - \" + .name + \" (\" + .version + \")\""  licenses.json
            found_incompatible=true
        fi
    done
    
    if [ "$found_incompatible" = true ]; then
        return 1
    else
        echo_success "所有依賴許可證均兼容"
    fi
    
    # 統計許可證分佈
    echo_info "許可證分佈:"
    jq -r 'group_by(.license) | .[] | "\(.length) packages: \(.[0].license)"' licenses.json | sort -nr
}

# 檢查代碼質量
check_code_quality() {
    echo_info "檢查代碼質量..."
    
    # Clippy 檢查
    echo_info "運行 Clippy 檢查..."
    if cargo clippy --all-targets --all-features -- -D warnings; then
        echo_success "Clippy 檢查通過"
    else
        echo_error "Clippy 檢查失敗"
        return 1
    fi
    
    # 格式檢查
    echo_info "檢查代碼格式..."
    if cargo fmt --all -- --check; then
        echo_success "代碼格式正確"
    else
        echo_error "代碼格式不正確，運行 'cargo fmt' 修復"
        return 1
    fi
    
    # 檢查未使用依賴
    if command -v cargo-machete >/dev/null; then
        echo_info "檢查未使用依賴..."
        if cargo machete; then
            echo_success "未發現未使用的依賴"
        else
            echo_warning "發現未使用的依賴"
        fi
    else
        echo_warning "cargo-machete 未安裝，跳過未使用依賴檢查"
    fi
}

# 生成安全報告
generate_security_report() {
    echo_info "生成安全報告..."
    
    local report_file="security_report_$(date +%Y%m%d_%H%M%S).md"
    
    cat > "$report_file" << EOF
# Rust HFT 安全檢查報告

**生成時間**: $(date -u)
**項目**: Rust HFT Trading System
**提交**: $(git rev-parse HEAD 2>/dev/null || echo "N/A")

## 📊 依賴統計

- **總依賴數**: $(cargo tree --depth 1 | grep -c '^├\|^└' || echo "N/A")
- **直接依賴數**: $(grep -c "^[a-zA-Z]" Cargo.toml | grep -v "\[" || echo "N/A")

## 🔒 安全審計結果

EOF

    if [ -f "audit_results.json" ]; then
        local vuln_count=$(jq -r '.vulnerabilities.count // 0' audit_results.json)
        echo "- **發現漏洞數**: $vuln_count" >> "$report_file"
        
        if [ "$vuln_count" -gt 0 ]; then
            echo "" >> "$report_file"
            echo "### 漏洞詳情" >> "$report_file"
            jq -r '.vulnerabilities.list[] | "- **\(.package)** (\(.patched_versions)): \(.title)"' audit_results.json >> "$report_file"
        fi
    else
        echo "- **審計狀態**: 未完成" >> "$report_file"
    fi
    
    cat >> "$report_file" << EOF

## ⚖️ 許可證合規性

EOF

    if [ -f "licenses.json" ]; then
        echo "- **許可證檢查**: 通過" >> "$report_file"
        echo "" >> "$report_file"
        echo "### 許可證分佈" >> "$report_file"
        jq -r 'group_by(.license) | .[] | "- **\(.[0].license)**: \(.length) packages"' licenses.json >> "$report_file"
    else
        echo "- **許可證檢查**: 未完成" >> "$report_file"
    fi
    
    cat >> "$report_file" << EOF

## 📈 代碼質量指標

- **Rust 文件**: $(find src -name "*.rs" | wc -l) 個
- **代碼行數**: $(find src -name "*.rs" -exec wc -l {} + | tail -1 | awk '{print $1}') 行
- **函數數量**: $(grep -r "fn " src --include="*.rs" | wc -l) 個
- **結構體數量**: $(grep -r "struct " src --include="*.rs" | wc -l) 個
- **待辦事項**: $(grep -r "TODO\|FIXME\|XXX" src --include="*.rs" | wc -l) 個

---
**自動生成**: $(date -u)
EOF
    
    echo_success "安全報告已生成: $report_file"
}

# 主函數
main() {
    echo_info "開始 Rust HFT 項目安全檢查..."
    echo ""
    
    local exit_code=0
    
    check_tools || exit_code=$?
    echo ""
    
    run_security_audit || exit_code=$?
    echo ""
    
    check_license_compliance || exit_code=$?
    echo ""
    
    check_code_quality || exit_code=$?
    echo ""
    
    generate_security_report
    echo ""
    
    # 清理臨時文件
    rm -f audit_results.json licenses.json
    
    if [ $exit_code -eq 0 ]; then
        echo_success "🎉 安全檢查全部通過！"
    else
        echo_error "❌ 安全檢查發現問題，請查看上述輸出"
    fi
    
    exit $exit_code
}

# 腳本入口
main "$@"