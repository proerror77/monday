#!/bin/bash

# WLFI Demo Runner Script
# Make sure to install requirements first: pip install -r requirements.txt

echo "🚀 WLFI Trading Demo Runner"
echo "=========================="
echo ""

# Check if password is provided
if [ -z "$1" ]; then
    echo "Usage: $0 <clickhouse_password> [symbol] [hours]"
    echo "Example: $0 mypassword WLFI 168"
    echo ""
    echo "Parameters:"
    echo "  clickhouse_password: Your ClickHouse cloud password (required)"
    echo "  symbol: Trading symbol to analyze (default: WLFI)"
    echo "  hours: Hours of historical data (default: 168 = 1 week)"
    exit 1
fi

PASSWORD=$1
SYMBOL=${2:-WLFI}
HOURS=${3:-168}

echo "📊 Configuration:"
echo "  Symbol: $SYMBOL"
echo "  Hours back: $HOURS"
echo "  Timestamp: $(date)"
echo ""

# Check if Python environment is set up
if ! python3 -c "import pandas, numpy, xgboost, torch, sklearn" 2>/dev/null; then
    echo "❌ Missing dependencies. Please install requirements:"
    echo "   pip install -r requirements.txt"
    echo ""
    exit 1
fi

echo "✅ Dependencies check passed"
echo ""

# Create output directory
OUTPUT_DIR="wlfi_results_$(date +%Y%m%d_%H%M%S)"
mkdir -p "$OUTPUT_DIR"
cd "$OUTPUT_DIR"

echo "📁 Output directory: $OUTPUT_DIR"
echo ""

# Run the demo
echo "🚀 Starting WLFI Complete Demo..."
echo ""

python3 ../wlfi_complete_demo.py \
    --password "$PASSWORD" \
    --symbol "$SYMBOL" \
    --hours "$HOURS" \
    --host "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443"

DEMO_EXIT_CODE=$?

echo ""
echo "=========================="

if [ $DEMO_EXIT_CODE -eq 0 ]; then
    echo "✅ Demo completed successfully!"
    echo ""
    echo "📁 Generated files:"
    ls -la *.csv *.txt *.png *.joblib *.pth 2>/dev/null || echo "  (No output files found)"
    echo ""
    echo "🔍 Key files to check:"
    echo "  • executive_summary_*.txt - Overall results"
    echo "  • model_performance_report_*.txt - Model metrics"
    echo "  • backtest_*.txt - Strategy performance"
    echo "  • *.csv - Raw and processed data"
    echo "  • *.png - Visualization plots"
    echo "  • *.joblib, *.pth - Trained models"
else
    echo "❌ Demo failed with exit code $DEMO_EXIT_CODE"
    echo "📋 Check the log file for details"
    echo ""
fi

echo ""
echo "📋 Log files:"
ls -la *.log 2>/dev/null || echo "  (No log files found)"

cd ..
echo ""
echo "🏁 Demo runner finished"