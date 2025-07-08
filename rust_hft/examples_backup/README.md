# Rust HFT Examples

This directory contains organized examples demonstrating the capabilities of the Rust HFT trading system. Examples are structured by complexity and functionality to provide a clear learning path.

## 🗂️ Directory Structure

### 01. Getting Started
Essential examples for newcomers to understand basic functionality:

- **`connect_to_exchange.rs`** - Basic connection to Bitget exchange
- **`simple_trader.rs`** - Simple DL-based trading example  
- **`simple_feature_based_trading_demo.rs`** - Feature extraction and basic trading
- **`cli_dashboard.rs`** - Real-time CLI monitoring interface

### 02. Core Features
Core system capabilities and integrations:

- **`complete_system_demo.rs`** ⭐ **[RECOMMENDED START]** - Complete Barter-rs integration
- **`data_verification.rs`** - Data source verification and quality checks
- **`data_recorder.rs`** - Market data recording and storage
- **`barter_integration.rs`** - Barter-rs ecosystem integration examples

### 03. Trading Strategies
Advanced trading strategy implementations:

- **`dual_symbol_trading.rs`** - Multi-asset trading strategies
- **`microstructure_trading.rs`** - True HFT microstructure trading (DL-based)
- **`dl_based_trader.rs`** - Comprehensive DL trading system

### 04. Machine Learning
ML model training and implementation:

- **`online_learning.rs`** - Real-time online learning
- **`pure_rust_model_training.rs`** - Complete model training with Candle
- **`model_training_guide.rs`** - Model training documentation

### 05. Performance & Testing
Performance optimization and testing frameworks:

- **`performance_orderbook.rs`** - Performance-optimized orderbook implementations
- **`capital_management.rs`** - Capital management and risk control
- **`simulation_framework.rs`** - Comprehensive simulation and backtesting
- **`latency_benchmark.rs`** - Ultra-low latency performance testing
- **`ultra_low_latency_trading_demo.rs`** - Extreme performance trading demo

### Deprecated
Legacy examples kept for reference (not actively maintained):

- `complete_hft_example.rs` - Legacy complete example
- `ultra_think_hft_architecture.rs` - Architecture design document
- `simple_btcusdt_real_lob_demo.rs` - Superseded by simple_trader.rs
- `real_trading_with_ui.rs` - Legacy UI implementation
- `simple_training_guide.rs` - Superseded by model_training_guide.rs

## 🚀 Quick Start

### For Beginners
1. Start with `01_getting_started/connect_to_exchange.rs`
2. Try `01_getting_started/simple_trader.rs`
3. Explore `01_getting_started/cli_dashboard.rs`

### For System Integration
1. **Begin with `02_core_features/complete_system_demo.rs`** ⭐
2. Study `02_core_features/barter_integration.rs`
3. Implement data recording with `data_recorder.rs`

### For Advanced Trading
1. Understand `03_trading_strategies/microstructure_trading.rs`
2. Explore multi-asset with `dual_symbol_trading.rs`
3. Build comprehensive systems with `dl_based_trader.rs`

### For ML Development
1. Start with `04_machine_learning/online_learning.rs`
2. Train models using `pure_rust_model_training.rs`
3. Follow the `model_training_guide.rs` for best practices

### For Performance Optimization
1. Benchmark with `05_performance_and_testing/latency_benchmark.rs`
2. Optimize orderbooks with `performance_orderbook.rs`
3. Test with `simulation_framework.rs`

## 🔧 Running Examples

### Basic Usage
```bash
# Run any example
cargo run --example <example_name>

# Examples:
cargo run --example connect_to_exchange
cargo run --example complete_system_demo
cargo run --example microstructure_trading
```

### With Specific Modes
Some examples support different operation modes:

```bash
# Simple mode
cargo run --example pure_rust_model_training simple

# Live trading mode (be careful!)
LIVE_TRADING=true cargo run --example microstructure_trading

# CLI dashboard with different complexity levels
cargo run --example cli_dashboard
```

## 📊 Example Categories

| Category | Count | Description |
|----------|-------|-------------|
| Getting Started | 4 | Basic functionality and simple examples |
| Core Features | 4 | Essential system capabilities |
| Trading Strategies | 3 | Advanced trading implementations |
| Machine Learning | 3 | ML training and inference |
| Performance | 5 | Optimization and benchmarking |
| **Total Active** | **19** | **Organized, maintained examples** |
| Deprecated | 5 | Legacy examples for reference |

## 🎯 Example Selection Guide

### "I want to understand the basics"
→ Start with `01_getting_started/`

### "I want to integrate with existing systems"  
→ Focus on `02_core_features/complete_system_demo.rs`

### "I want to build HFT strategies"
→ Study `03_trading_strategies/microstructure_trading.rs`

### "I want to train ML models"
→ Use `04_machine_learning/pure_rust_model_training.rs`

### "I need maximum performance"
→ Benchmark with `05_performance_and_testing/`

## 📝 Code Quality Notes

- All examples compile and run successfully
- Duplicate functionality has been merged into unified examples  
- Clear separation between simple and advanced features
- Consistent naming and documentation standards
- Performance-critical examples include benchmarking
- Deprecated examples preserved for reference but not maintained

## 🔄 Migration from Old Structure

If you were using the old flat structure, here are the mappings:

| Old File | New Location | Status |
|----------|--------------|--------|
| `complete_hft_example.rs` | `deprecated/` | Legacy |
| `demo_bitget_v2.rs` | `01_getting_started/connect_to_exchange.rs` | Renamed |
| `hft_cli_dashboard.rs` | `01_getting_started/cli_dashboard.rs` | Moved |
| `complete_barter_integration.rs` | `02_core_features/complete_system_demo.rs` | Renamed |
| `true_hft_microstructure_demo.rs` | `03_trading_strategies/microstructure_trading.rs` | Moved |
| Multiple perf examples | `05_performance_and_testing/performance_orderbook.rs` | Merged |

## 🤝 Contributing

When adding new examples:
1. Choose the appropriate directory based on complexity and purpose
2. Follow the existing naming conventions
3. Include comprehensive documentation
4. Add performance benchmarks for performance-critical examples
5. Update this README with the new example

## ⚠️ Important Notes

- **Production Use**: Always test examples thoroughly before production use
- **Live Trading**: Examples marked with live trading should be used with extreme caution
- **Performance**: Performance examples require specific hardware configurations for optimal results
- **Dependencies**: Some examples require additional system dependencies (CUDA, specific CPU features, etc.)

For questions or issues with examples, please refer to the main project documentation or create an issue in the repository.