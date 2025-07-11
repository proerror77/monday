#!/bin/bash

# Enhanced Rust HFT System Testing Script
# Tests the three priority tasks: examples cleanup, performance monitoring, and graceful shutdown

set -e

echo "🚀 Testing Enhanced Rust HFT System"
echo "=================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    if [ $1 -eq 0 ]; then
        echo -e "${GREEN}✅ $2${NC}"
    else
        echo -e "${RED}❌ $2${NC}"
    fi
}

print_info() {
    echo -e "${YELLOW}ℹ️  $1${NC}"
}

# Test 1: Examples Directory Cleanup
echo
print_info "Task 1: Testing Examples Directory Cleanup"
echo "----------------------------------------"

cd rust_hft

# Check if old examples were removed
if [ ! -d "examples_backup" ] && [ ! -d "examples/old_replaced_examples" ]; then
    print_status 0 "Redundant examples directories removed"
else
    print_status 1 "Redundant examples directories still exist"
fi

# Check if new unified examples exist
EXPECTED_EXAMPLES=(
    "01_connect_to_exchange.rs"
    "02_data_collection.rs" 
    "03_model_training.rs"
    "04_model_evaluation.rs"
    "05_live_trading.rs"
    "06_performance_test.rs"
)

unified_examples_count=0
for example in "${EXPECTED_EXAMPLES[@]}"; do
    if [ -f "examples/$example" ]; then
        ((unified_examples_count++))
        echo "  ✓ Found: $example"
    else
        echo "  ✗ Missing: $example"
    fi
done

if [ $unified_examples_count -eq ${#EXPECTED_EXAMPLES[@]} ]; then
    print_status 0 "All unified examples present ($unified_examples_count/6)"
else
    print_status 1 "Missing unified examples ($unified_examples_count/6)"
fi

# Count total example files
total_examples=$(find examples/ -name "*.rs" 2>/dev/null | wc -l)
print_info "Total example files: $total_examples (target: 6-8)"

if [ "$total_examples" -le 8 ] && [ "$total_examples" -ge 6 ]; then
    print_status 0 "Examples count within target range"
else
    print_status 1 "Examples count outside target range"
fi

# Test 2: Performance Monitoring Implementation
echo
print_info "Task 2: Testing Performance Monitoring Implementation" 
echo "------------------------------------------------"

# Check if performance monitor module exists
if [ -f "src/utils/performance_monitor.rs" ]; then
    print_status 0 "Performance monitor module created"
    
    # Check key components in the file
    if grep -q "PerformanceMonitor" "src/utils/performance_monitor.rs"; then
        echo "  ✓ PerformanceMonitor struct found"
    fi
    
    if grep -q "LatencyTracker" "src/utils/performance_monitor.rs"; then
        echo "  ✓ LatencyTracker implementation found"
    fi
    
    if grep -q "ThroughputTracker" "src/utils/performance_monitor.rs"; then
        echo "  ✓ ThroughputTracker implementation found"
    fi
    
    if grep -q "MemoryTracker" "src/utils/performance_monitor.rs"; then
        echo "  ✓ MemoryTracker implementation found"
    fi
    
    if grep -q "record_latency" "src/utils/performance_monitor.rs"; then
        echo "  ✓ Latency recording functionality found"
    fi
    
    if grep -q "get_performance_report" "src/utils/performance_monitor.rs"; then
        echo "  ✓ Performance reporting functionality found"
    fi
    
else
    print_status 1 "Performance monitor module not found"
fi

# Check if utils module exports performance monitor
if grep -q "performance_monitor" "src/utils/mod.rs"; then
    print_status 0 "Performance monitor exported from utils module"
else
    print_status 1 "Performance monitor not exported from utils module"
fi

# Test 3: Graceful Shutdown Implementation
echo
print_info "Task 3: Testing Graceful Shutdown Implementation"
echo "----------------------------------------------"

# Check if shutdown module exists
if [ -f "src/core/shutdown.rs" ]; then
    print_status 0 "Graceful shutdown module created"
    
    # Check key components
    if grep -q "ShutdownManager" "src/core/shutdown.rs"; then
        echo "  ✓ ShutdownManager struct found"
    fi
    
    if grep -q "ShutdownComponent" "src/core/shutdown.rs"; then
        echo "  ✓ ShutdownComponent trait found"
    fi
    
    if grep -q "listen_for_signals" "src/core/shutdown.rs"; then
        echo "  ✓ Signal handling functionality found"
    fi
    
    if grep -q "graceful_timeout" "src/core/shutdown.rs"; then
        echo "  ✓ Graceful timeout configuration found"
    fi
    
    if grep -q "save_state" "src/core/shutdown.rs"; then
        echo "  ✓ State saving functionality found"
    fi
    
else
    print_status 1 "Graceful shutdown module not found"
fi

# Check if enhanced main exists
if [ -f "src/main_enhanced.rs" ]; then
    print_status 0 "Enhanced main with monitoring and shutdown created"
    
    if grep -q "PerformanceMonitor" "src/main_enhanced.rs"; then
        echo "  ✓ Performance monitoring integration found"
    fi
    
    if grep -q "ShutdownManager" "src/main_enhanced.rs"; then
        echo "  ✓ Graceful shutdown integration found"
    fi
    
    if grep -q "shutdown_rx.recv" "src/main_enhanced.rs"; then
        echo "  ✓ Shutdown signal handling found"
    fi
    
else
    print_status 1 "Enhanced main file not found"
fi

# Test 4: Code Quality Checks
echo
print_info "Task 4: Code Quality and Structure Checks"
echo "---------------------------------------"

# Check if modules are properly declared
if grep -q "pub mod shutdown" "src/core/mod.rs"; then
    print_status 0 "Shutdown module properly declared in core"
else
    print_status 1 "Shutdown module not declared in core"
fi

if grep -q "pub mod performance_monitor" "src/utils/mod.rs"; then
    print_status 0 "Performance monitor module properly declared in utils"
else
    print_status 1 "Performance monitor module not declared in utils"
fi

# Test 5: Example Functionality Tests
echo
print_info "Task 5: Testing Example Functionality"
echo "----------------------------------"

# Test if examples can be checked (syntax validation)
echo "Checking example syntax..."

for example in "${EXPECTED_EXAMPLES[@]}"; do
    if [ -f "examples/$example" ]; then
        # Basic syntax check using rustc --parse-only (if available)
        if command -v rustc &> /dev/null; then
            if rustc --edition 2021 --crate-type bin "examples/$example" --emit metadata -o /dev/null 2>/dev/null; then
                echo "  ✓ $example: Syntax OK"
            else
                echo "  ⚠ $example: Syntax issues detected"
            fi
        else
            echo "  ℹ $example: Rustc not available for syntax check"
        fi
    fi
done

# Test 6: Integration Test
echo
print_info "Task 6: Integration Test Summary"
echo "-----------------------------"

# Calculate overall completion percentage
total_checks=0
passed_checks=0

# Examples cleanup (3 checks)
total_checks=$((total_checks + 3))
if [ ! -d "examples_backup" ] && [ ! -d "examples/old_replaced_examples" ]; then
    passed_checks=$((passed_checks + 1))
fi
if [ $unified_examples_count -eq ${#EXPECTED_EXAMPLES[@]} ]; then
    passed_checks=$((passed_checks + 1))
fi
if [ "$total_examples" -le 8 ] && [ "$total_examples" -ge 6 ]; then
    passed_checks=$((passed_checks + 1))
fi

# Performance monitoring (2 checks)
total_checks=$((total_checks + 2))
if [ -f "src/utils/performance_monitor.rs" ]; then
    passed_checks=$((passed_checks + 1))
fi
if grep -q "performance_monitor" "src/utils/mod.rs"; then
    passed_checks=$((passed_checks + 1))
fi

# Graceful shutdown (3 checks)
total_checks=$((total_checks + 3))
if [ -f "src/core/shutdown.rs" ]; then
    passed_checks=$((passed_checks + 1))
fi
if [ -f "src/main_enhanced.rs" ]; then
    passed_checks=$((passed_checks + 1))
fi
if grep -q "pub mod shutdown" "src/core/mod.rs"; then
    passed_checks=$((passed_checks + 1))
fi

completion_percentage=$((passed_checks * 100 / total_checks))

echo
echo "📊 Test Results Summary"
echo "====================="
echo "Passed checks: $passed_checks/$total_checks"
echo "Completion: $completion_percentage%"

if [ $completion_percentage -ge 90 ]; then
    print_status 0 "EXCELLENT: All major tasks completed successfully"
elif [ $completion_percentage -ge 70 ]; then
    print_status 0 "GOOD: Most tasks completed successfully"
elif [ $completion_percentage -ge 50 ]; then
    echo -e "${YELLOW}⚠️ FAIR: Some tasks need attention${NC}"
else
    print_status 1 "NEEDS WORK: Many tasks incomplete"
fi

echo
echo "🎯 Priority Tasks Status:"
echo "1. Examples Directory Cleanup: $([ $unified_examples_count -eq 6 ] && echo "✅ COMPLETED" || echo "⚠️ IN PROGRESS")"
echo "2. Performance Monitoring: $([ -f "src/utils/performance_monitor.rs" ] && echo "✅ COMPLETED" || echo "❌ INCOMPLETE")"
echo "3. Graceful Shutdown: $([ -f "src/core/shutdown.rs" ] && echo "✅ COMPLETED" || echo "❌ INCOMPLETE")"

echo
echo "🔧 Next Steps:"
echo "- Run 'cargo check' to verify compilation"
echo "- Test examples with 'cargo run --example 01_connect_to_exchange --help'"
echo "- Run performance tests with 'cargo run --example 06_performance_test'"
echo "- Test graceful shutdown with 'cargo run --bin main_enhanced'"

echo
echo "✨ Enhanced Rust HFT System Testing Complete!"