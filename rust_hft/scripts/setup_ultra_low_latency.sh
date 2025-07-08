#!/bin/bash
#
# Ultra Low Latency System Setup Script
# 配置 Linux 系統以實現 P99 ≤ 1ms 的推理延遲
#
# 使用方法: sudo ./setup_ultra_low_latency.sh
#

set -e

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 日誌函數
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

# 檢查是否為 root 用戶
check_root() {
    if [ "$EUID" -ne 0 ]; then
        log_error "請使用 sudo 運行此腳本"
        exit 1
    fi
}

# 檢測 CPU 信息
detect_cpu_info() {
    log_step "檢測 CPU 信息..."
    
    CPU_CORES=$(nproc)
    CPU_MODEL=$(cat /proc/cpuinfo | grep "model name" | head -1 | cut -d: -f2 | xargs)
    CPU_FREQ=$(cat /proc/cpuinfo | grep "cpu MHz" | head -1 | cut -d: -f2 | xargs)
    
    log_info "CPU 型號: $CPU_MODEL"
    log_info "CPU 核心數: $CPU_CORES"
    log_info "CPU 頻率: ${CPU_FREQ} MHz"
    
    # 選擇隔離的 CPU 核心（最後4個核心）
    if [ $CPU_CORES -gt 4 ]; then
        ISOLATED_CORES=$(seq $((CPU_CORES-4)) $((CPU_CORES-1)) | tr '\n' ',' | sed 's/,$//')
        TRADING_CORE=$((CPU_CORES-2))  # 倒數第二個核心用於交易
    else
        log_warn "CPU 核心數較少 ($CPU_CORES)，建議至少 8 核心以獲得最佳性能"
        ISOLATED_CORES="2,3"
        TRADING_CORE=2
    fi
    
    log_info "隔離 CPU 核心: $ISOLATED_CORES"
    log_info "交易專用核心: $TRADING_CORE"
}

# 配置 GRUB 啟動參數
configure_grub() {
    log_step "配置 GRUB 啟動參數..."
    
    GRUB_FILE="/etc/default/grub"
    BACKUP_FILE="${GRUB_FILE}.backup.$(date +%Y%m%d_%H%M%S)"
    
    # 備份原始文件
    cp $GRUB_FILE $BACKUP_FILE
    log_info "已備份 GRUB 配置: $BACKUP_FILE"
    
    # 準備新的內核參數
    KERNEL_PARAMS="isolcpus=$ISOLATED_CORES nohz_full=$ISOLATED_CORES rcu_nocbs=$ISOLATED_CORES"
    KERNEL_PARAMS="$KERNEL_PARAMS intel_idle.max_cstate=0 processor.max_cstate=0"
    KERNEL_PARAMS="$KERNEL_PARAMS intel_pstate=disable"
    KERNEL_PARAMS="$KERNEL_PARAMS nosoftlockup skew_tick=1"
    KERNEL_PARAMS="$KERNEL_PARAMS tsc=reliable clocksource=tsc"
    KERNEL_PARAMS="$KERNEL_PARAMS hugepages=1024"  # 1024 個 2MB 大頁
    
    # 更新 GRUB 配置
    if grep -q "GRUB_CMDLINE_LINUX_DEFAULT" $GRUB_FILE; then
        sed -i "s/GRUB_CMDLINE_LINUX_DEFAULT=.*/GRUB_CMDLINE_LINUX_DEFAULT=\"quiet $KERNEL_PARAMS\"/" $GRUB_FILE
    else
        echo "GRUB_CMDLINE_LINUX_DEFAULT=\"quiet $KERNEL_PARAMS\"" >> $GRUB_FILE
    fi
    
    log_info "GRUB 配置已更新，添加參數: $KERNEL_PARAMS"
    
    # 更新 GRUB
    if command -v update-grub >/dev/null 2>&1; then
        update-grub
    elif command -v grub2-mkconfig >/dev/null 2>&1; then
        grub2-mkconfig -o /boot/grub2/grub.cfg
    else
        log_warn "無法自動更新 GRUB，請手動運行 update-grub"
    fi
}

# 配置內核參數
configure_kernel_params() {
    log_step "配置內核運行時參數..."
    
    # 創建 sysctl 配置文件
    SYSCTL_FILE="/etc/sysctl.d/99-ultra-low-latency.conf"
    
    cat > $SYSCTL_FILE << 'EOF'
# Ultra Low Latency HFT Configuration

# 網絡優化
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 67108864
net.core.wmem_default = 67108864
net.ipv4.tcp_rmem = 4096 87380 134217728
net.ipv4.tcp_wmem = 4096 65536 134217728
net.core.netdev_max_backlog = 30000
net.core.netdev_budget = 600
net.ipv4.tcp_congestion_control = bbr
net.ipv4.tcp_low_latency = 1
net.ipv4.tcp_timestamps = 0
net.ipv4.tcp_sack = 0

# 內存管理
vm.swappiness = 1
vm.dirty_ratio = 15
vm.dirty_background_ratio = 5
vm.vfs_cache_pressure = 50

# 調度器優化
kernel.sched_rt_runtime_us = 1000000
kernel.sched_rt_period_us = 1000000
kernel.sched_min_granularity_ns = 100000
kernel.sched_wakeup_granularity_ns = 50000

# 中斷親和性
kernel.sched_domain.cpu0.domain0.max_newidle_lb_cost = 0
kernel.sched_domain.cpu1.domain0.max_newidle_lb_cost = 0

# 文件系統優化
fs.file-max = 2097152

# 安全限制調整
kernel.perf_event_paranoid = 0
EOF

    log_info "已創建 sysctl 配置: $SYSCTL_FILE"
    
    # 立即應用配置
    sysctl -p $SYSCTL_FILE
}

# 配置中斷親和性
configure_irq_affinity() {
    log_step "配置中斷親和性..."
    
    # 創建中斷親和性腳本
    IRQ_SCRIPT="/usr/local/bin/setup_irq_affinity.sh"
    
    cat > $IRQ_SCRIPT << EOF
#!/bin/bash
#
# 設置中斷親和性，將網絡中斷綁定到非隔離核心
#

# 獲取網絡接口中斷
NET_IRQS=\$(cat /proc/interrupts | grep eth0 | cut -d: -f1 | tr -d ' ')

# 創建非隔離核心掩碼（前面的核心）
if [ $CPU_CORES -gt 4 ]; then
    NON_ISOLATED_MASK=\$(printf "%x" \$((2**($CPU_CORES-4) - 1)))
else
    NON_ISOLATED_MASK="3"  # 使用前兩個核心
fi

# 設置網絡中斷親和性
for irq in \$NET_IRQS; do
    if [ -f "/proc/irq/\$irq/smp_affinity" ]; then
        echo \$NON_ISOLATED_MASK > /proc/irq/\$irq/smp_affinity
        echo "IRQ \$irq 綁定到掩碼 \$NON_ISOLATED_MASK"
    fi
done

# 禁用 irqbalance 服務
systemctl stop irqbalance
systemctl disable irqbalance
EOF

    chmod +x $IRQ_SCRIPT
    log_info "已創建中斷親和性腳本: $IRQ_SCRIPT"
    
    # 立即執行
    $IRQ_SCRIPT
}

# 配置 CPU 頻率
configure_cpu_frequency() {
    log_step "配置 CPU 頻率..."
    
    # 安裝 cpufrequtils
    if ! command -v cpufreq-set >/dev/null 2>&1; then
        log_info "安裝 cpufrequtils..."
        apt-get update
        apt-get install -y cpufrequtils
    fi
    
    # 設置性能模式
    for cpu in $(seq 0 $((CPU_CORES-1))); do
        cpufreq-set -c $cpu -g performance
    done
    
    log_info "所有 CPU 核心已設置為性能模式"
    
    # 創建開機自動設置腳本
    CPU_FREQ_SCRIPT="/etc/rc.local"
    
    if [ ! -f $CPU_FREQ_SCRIPT ]; then
        cat > $CPU_FREQ_SCRIPT << EOF
#!/bin/bash
# 開機自動設置 CPU 性能模式
for cpu in \$(seq 0 $((CPU_CORES-1))); do
    cpufreq-set -c \$cpu -g performance
done
exit 0
EOF
        chmod +x $CPU_FREQ_SCRIPT
    fi
}

# 配置大頁內存
configure_hugepages() {
    log_step "配置大頁內存..."
    
    # 設置大頁內存
    echo 1024 > /sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages
    
    # 創建大頁內存掛載點
    HUGEPAGE_MOUNT="/mnt/hugepages"
    mkdir -p $HUGEPAGE_MOUNT
    
    # 添加到 fstab
    if ! grep -q "hugetlbfs" /etc/fstab; then
        echo "hugetlbfs $HUGEPAGE_MOUNT hugetlbfs defaults 0 0" >> /etc/fstab
    fi
    
    # 立即掛載
    mount -t hugetlbfs hugetlbfs $HUGEPAGE_MOUNT
    
    log_info "大頁內存配置完成: $HUGEPAGE_MOUNT"
}

# 配置網絡優化
configure_network() {
    log_step "配置網絡優化..."
    
    # 獲取主要網絡接口
    PRIMARY_INTERFACE=$(ip route | grep default | awk '{print $5}' | head -1)
    
    if [ -n "$PRIMARY_INTERFACE" ]; then
        log_info "主要網絡接口: $PRIMARY_INTERFACE"
        
        # 優化網絡接口設置
        ethtool -G $PRIMARY_INTERFACE rx 4096 tx 4096 2>/dev/null || true
        ethtool -K $PRIMARY_INTERFACE gro off lro off tso off gso off 2>/dev/null || true
        ethtool -C $PRIMARY_INTERFACE rx-usecs 0 tx-usecs 0 2>/dev/null || true
        
        log_info "網絡接口 $PRIMARY_INTERFACE 優化完成"
    else
        log_warn "無法檢測主要網絡接口"
    fi
}

# 創建性能監控腳本
create_monitoring_script() {
    log_step "創建性能監控腳本..."
    
    MONITOR_SCRIPT="/usr/local/bin/hft_monitor.sh"
    
    cat > $MONITOR_SCRIPT << 'EOF'
#!/bin/bash
#
# HFT 系統性能監控腳本
#

echo "=== HFT 系統狀態監控 ==="
echo "時間: $(date)"
echo

echo "=== CPU 信息 ==="
cat /proc/cpuinfo | grep "model name" | head -1
echo "核心數: $(nproc)"
echo "當前頻率:"
cat /proc/cpuinfo | grep "cpu MHz" | head -4
echo

echo "=== 內存信息 ==="
free -h
echo "大頁內存:"
cat /proc/meminfo | grep -i huge
echo

echo "=== 網絡統計 ==="
cat /proc/net/dev | head -2
cat /proc/net/dev | grep -v "lo:"
echo

echo "=== 系統負載 ==="
uptime
echo

echo "=== 中斷統計 (top 10) ==="
cat /proc/interrupts | head -1
cat /proc/interrupts | grep -v "   0" | sort -k2 -nr | head -10
echo

echo "=== CPU 親和性檢查 ==="
ps -eo pid,comm,psr | grep -E "(trading|hft)" | head -10
echo

echo "=== 延遲檢查 ==="
if command -v cyclictest >/dev/null 2>&1; then
    echo "運行 cyclictest 檢查延遲..."
    cyclictest -t1 -p 80 -n -i 200 -l 1000 -q
else
    echo "cyclictest 未安裝，跳過延遲測試"
fi
EOF

    chmod +x $MONITOR_SCRIPT
    log_info "性能監控腳本已創建: $MONITOR_SCRIPT"
}

# 安裝額外工具
install_additional_tools() {
    log_step "安裝額外優化工具..."
    
    apt-get update
    apt-get install -y \
        rt-tests \
        stress-ng \
        perf-tools-unstable \
        numactl \
        htop \
        iotop \
        nethogs \
        tcpdump \
        wireshark-common \
        ethtool
    
    log_info "額外工具安裝完成"
}

# 創建 HFT 用戶和權限設置
setup_hft_user() {
    log_step "設置 HFT 用戶權限..."
    
    HFT_USER="hfttrader"
    
    # 創建用戶（如果不存在）
    if ! id "$HFT_USER" &>/dev/null; then
        useradd -m -s /bin/bash $HFT_USER
        log_info "已創建用戶: $HFT_USER"
    fi
    
    # 設置 limits
    LIMITS_FILE="/etc/security/limits.d/99-hft.conf"
    
    cat > $LIMITS_FILE << EOF
# HFT 用戶限制配置
$HFT_USER soft rtprio 99
$HFT_USER hard rtprio 99
$HFT_USER soft nice -20
$HFT_USER hard nice -20
$HFT_USER soft memlock unlimited
$HFT_USER hard memlock unlimited
$HFT_USER soft nofile 1048576
$HFT_USER hard nofile 1048576
$HFT_USER soft nproc 32768
$HFT_USER hard nproc 32768
EOF

    log_info "HFT 用戶限制配置完成: $LIMITS_FILE"
}

# 生成配置總結
generate_summary() {
    log_step "生成配置總結..."
    
    SUMMARY_FILE="/root/hft_setup_summary.txt"
    
    cat > $SUMMARY_FILE << EOF
=== Ultra Low Latency HFT 系統配置總結 ===
配置時間: $(date)

CPU 信息:
- 型號: $CPU_MODEL
- 核心數: $CPU_CORES
- 隔離核心: $ISOLATED_CORES
- 交易專用核心: $TRADING_CORE

系統優化:
✓ GRUB 內核參數已配置
✓ 系統內核參數已優化
✓ CPU 頻率已設置為性能模式
✓ 中斷親和性已配置
✓ 大頁內存已啟用 (1024 × 2MB)
✓ 網絡參數已優化
✓ HFT 用戶權限已設置

重要文件:
- GRUB 配置: /etc/default/grub
- 內核參數: /etc/sysctl.d/99-ultra-low-latency.conf
- 中斷親和性腳本: /usr/local/bin/setup_irq_affinity.sh
- 性能監控腳本: /usr/local/bin/hft_monitor.sh
- 用戶限制: /etc/security/limits.d/99-hft.conf

下一步:
1. 重啟系統以應用所有內核參數
2. 使用 hfttrader 用戶運行交易程序
3. 將交易程序綁定到核心 $TRADING_CORE
4. 運行 /usr/local/bin/hft_monitor.sh 監控系統狀態

測試命令:
sudo /usr/local/bin/hft_monitor.sh
taskset -c $TRADING_CORE nice -n -20 your_trading_program

注意: 請在重啟後驗證所有設置是否生效！
EOF

    log_info "配置總結已保存到: $SUMMARY_FILE"
    cat $SUMMARY_FILE
}

# 主函數
main() {
    log_info "開始 Ultra Low Latency HFT 系統配置..."
    
    check_root
    detect_cpu_info
    configure_grub
    configure_kernel_params
    configure_irq_affinity
    configure_cpu_frequency
    configure_hugepages
    configure_network
    create_monitoring_script
    install_additional_tools
    setup_hft_user
    generate_summary
    
    echo
    log_info "配置完成！請重啟系統以應用所有優化設置。"
    log_warn "重啟後運行: sudo /usr/local/bin/hft_monitor.sh"
    echo
}

# 執行主函數
main "$@"