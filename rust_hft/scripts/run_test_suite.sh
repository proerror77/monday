#!/bin/bash

# HFT System Test Suite Runner
# ============================
# 
# 全面的测试套件执行脚本，支持不同类型的测试运行

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test configuration
RUN_UNIT_TESTS=true
RUN_INTEGRATION_TESTS=true
RUN_PERFORMANCE_TESTS=true
RUN_STRESS_TESTS=false
VERBOSE=false
RELEASE_MODE=false

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --unit-only)
            RUN_UNIT_TESTS=true
            RUN_INTEGRATION_TESTS=false
            RUN_PERFORMANCE_TESTS=false
            RUN_STRESS_TESTS=false
            shift
            ;;
        --integration-only)
            RUN_UNIT_TESTS=false
            RUN_INTEGRATION_TESTS=true
            RUN_PERFORMANCE_TESTS=false
            RUN_STRESS_TESTS=false
            shift
            ;;
        --performance-only)
            RUN_UNIT_TESTS=false
            RUN_INTEGRATION_TESTS=false
            RUN_PERFORMANCE_TESTS=true
            RUN_STRESS_TESTS=false
            shift
            ;;
        --stress)
            RUN_STRESS_TESTS=true
            shift
            ;;
        --verbose)
            VERBOSE=true
            shift
            ;;
        --release)
            RELEASE_MODE=true
            shift
            ;;
        --help)
            echo "HFT System Test Suite Runner"
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  --unit-only        Run only unit tests"
            echo "  --integration-only Run only integration tests"
            echo "  --performance-only Run only performance tests"
            echo "  --stress           Include stress tests (resource intensive)"
            echo "  --verbose          Verbose output"
            echo "  --release          Run tests in release mode (optimized)"
            echo "  --help             Show this help message"
            echo ""
            echo "Examples:"
            echo "  $0                 # Run all standard tests"
            echo "  $0 --unit-only    # Run only unit tests"
            echo "  $0 --performance-only --release  # Run performance tests in release mode"
            echo "  $0 --stress       # Run all tests including stress tests"
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

echo -e "${BLUE}🚀 HFT System Test Suite${NC}"
echo "========================="
echo ""

# Set up test environment variables
export RUST_LOG=${RUST_LOG:-"info"}
export RUST_BACKTRACE=${RUST_BACKTRACE:-"1"}

# Detect if Redis is available for integration tests
if command -v redis-cli &> /dev/null; then
    redis-cli ping &> /dev/null && REDIS_AVAILABLE=true || REDIS_AVAILABLE=false
else
    REDIS_AVAILABLE=false
fi

# Prepare test command
TEST_CMD="cargo test"
if [ "$RELEASE_MODE" = true ]; then
    TEST_CMD="$TEST_CMD --release"
    echo -e "${YELLOW}📦 Running in release mode (optimized)${NC}"
fi

if [ "$VERBOSE" = true ]; then
    TEST_CMD="$TEST_CMD --verbose"
fi

# Function to run test category
run_test_category() {
    local category=$1
    local filter=$2
    local description=$3
    
    echo -e "${BLUE}🧪 Running $description...${NC}"
    echo "----------------------------------------"
    
    if [ -n "$filter" ]; then
        TEST_CMD_WITH_FILTER="$TEST_CMD $filter"
    else
        TEST_CMD_WITH_FILTER="$TEST_CMD"
    fi
    
    if $TEST_CMD_WITH_FILTER; then
        echo -e "${GREEN}✅ $description: PASSED${NC}"
        echo ""
        return 0
    else
        echo -e "${RED}❌ $description: FAILED${NC}"
        echo ""
        return 1
    fi
}

# Track test results
UNIT_PASSED=true
INTEGRATION_PASSED=true
PERFORMANCE_PASSED=true
STRESS_PASSED=true

# Run unit tests
if [ "$RUN_UNIT_TESTS" = true ]; then
    if ! run_test_category "unit" "multi_exchange_manager_test orderbook_comprehensive_test trading_strategies_test unified_engine_test" "Unit Tests"; then
        UNIT_PASSED=false
    fi
fi

# Run integration tests
if [ "$RUN_INTEGRATION_TESTS" = true ]; then
    if [ "$REDIS_AVAILABLE" = false ]; then
        echo -e "${YELLOW}⚠️  Redis not available - some integration tests may be skipped${NC}"
    fi
    
    if ! run_test_category "integration" "end_to_end_system_test" "Integration Tests"; then
        INTEGRATION_PASSED=false
    fi
fi

# Run performance tests
if [ "$RUN_PERFORMANCE_TESTS" = true ]; then
    echo -e "${BLUE}⚡ Running Performance Benchmarks...${NC}"
    echo "----------------------------------------"
    
    # Performance tests need specific environment
    export CARGO_TARGET_DIR="target/performance"
    
    if $TEST_CMD performance_benchmarks; then
        echo -e "${GREEN}✅ Performance Tests: PASSED${NC}"
        echo ""
    else
        echo -e "${RED}❌ Performance Tests: FAILED${NC}"
        echo ""
        PERFORMANCE_PASSED=false
    fi
fi

# Run stress tests (if enabled)
if [ "$RUN_STRESS_TESTS" = true ]; then
    echo -e "${YELLOW}🔥 Running Stress Tests (this may take several minutes)...${NC}"
    echo "----------------------------------------"
    
    # Stress tests need more resources
    export RUST_TEST_THREADS=1  # Limit parallelism for stress tests
    
    if $TEST_CMD --ignored stress_test; then
        echo -e "${GREEN}✅ Stress Tests: PASSED${NC}"
        echo ""
    else
        echo -e "${RED}❌ Stress Tests: FAILED${NC}"
        echo ""
        STRESS_PASSED=false
    fi
fi

# Generate test report
echo -e "${BLUE}📊 TEST SUITE SUMMARY${NC}"
echo "====================="
echo ""

if [ "$RUN_UNIT_TESTS" = true ]; then
    if [ "$UNIT_PASSED" = true ]; then
        echo -e "Unit Tests:        ${GREEN}✅ PASSED${NC}"
    else
        echo -e "Unit Tests:        ${RED}❌ FAILED${NC}"
    fi
fi

if [ "$RUN_INTEGRATION_TESTS" = true ]; then
    if [ "$INTEGRATION_PASSED" = true ]; then
        echo -e "Integration Tests: ${GREEN}✅ PASSED${NC}"
    else
        echo -e "Integration Tests: ${RED}❌ FAILED${NC}"
    fi
fi

if [ "$RUN_PERFORMANCE_TESTS" = true ]; then
    if [ "$PERFORMANCE_PASSED" = true ]; then
        echo -e "Performance Tests: ${GREEN}✅ PASSED${NC}"
    else
        echo -e "Performance Tests: ${RED}❌ FAILED${NC}"
    fi
fi

if [ "$RUN_STRESS_TESTS" = true ]; then
    if [ "$STRESS_PASSED" = true ]; then
        echo -e "Stress Tests:      ${GREEN}✅ PASSED${NC}"
    else
        echo -e "Stress Tests:      ${RED}❌ FAILED${NC}"
    fi
fi

echo ""

# Overall result
if [ "$UNIT_PASSED" = true ] && [ "$INTEGRATION_PASSED" = true ] && [ "$PERFORMANCE_PASSED" = true ] && [ "$STRESS_PASSED" = true ]; then
    echo -e "${GREEN}🎉 ALL TESTS PASSED - System ready for production!${NC}"
    exit 0
else
    echo -e "${RED}❌ SOME TESTS FAILED - Review failures before deployment${NC}"
    echo ""
    echo "💡 Debugging tips:"
    echo "   - Run with --verbose for detailed output"
    echo "   - Run specific test categories with --unit-only, --integration-only, etc."
    echo "   - Check logs for specific failure details"
    echo "   - Ensure all dependencies (Redis, ClickHouse) are running for integration tests"
    exit 1
fi