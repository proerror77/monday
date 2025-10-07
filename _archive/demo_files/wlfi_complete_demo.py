#!/usr/bin/env python3
"""
WLFI Complete Demo Script
End-to-end demonstration of data querying, cleaning, backtesting, and model training
Integrates all components: ClickHouse, preprocessing, XGBoost, GRU/Transformer models
"""

import sys
import os
from pathlib import Path
import argparse
import logging
from datetime import datetime, timedelta
import pandas as pd
import numpy as np
import matplotlib.pyplot as plt
import seaborn as sns
import warnings
warnings.filterwarnings('ignore')

# Setup paths
PROJECT_ROOT = Path(__file__).parent
sys.path.append(str(PROJECT_ROOT))
sys.path.append(str(PROJECT_ROOT / "ml_workspace"))

# Import custom modules
from ml_workspace.db.clickhouse_client import ClickHouseClient
from ml_workspace.utils.data_preprocessing import DataPreprocessor
from ml_workspace.utils.backtesting import BacktestEngine, simple_momentum_strategy, mean_reversion_strategy
from ml_workspace.models.xgboost_trainer import XGBoostTrainer
from ml_workspace.models.deep_learning_models import DeepLearningTrainer

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s',
    handlers=[
        logging.StreamHandler(sys.stdout),
        logging.FileHandler(f'wlfi_demo_{datetime.now().strftime("%Y%m%d_%H%M%S")}.log')
    ]
)
logger = logging.getLogger(__name__)

# Set plot style
plt.style.use('default')
sns.set_palette("husl")


class WLFIDemo:
    """
    Complete WLFI trading demo with data pipeline, models, and backtesting
    """
    
    def __init__(self, clickhouse_host: str, clickhouse_password: str):
        """
        Initialize demo with ClickHouse connection
        
        Args:
            clickhouse_host: ClickHouse server URL
            clickhouse_password: Database password
        """
        self.ch_client = ClickHouseClient(
            host=clickhouse_host,
            username="default",
            password=clickhouse_password
        )
        
        self.preprocessor = DataPreprocessor(
            sequence_length=60,
            prediction_horizon=5,
            min_tick_size=0.000001
        )
        
        self.backtest_engine = BacktestEngine(
            initial_capital=100000.0,
            commission_rate=0.001,
            slippage_bps=1.0
        )
        
        # Data storage
        self.raw_data = {}
        self.processed_data = {}
        self.models = {}
        self.backtest_results = {}
        
        logger.info("WLFI Demo initialized")
    
    def run_complete_demo(self, symbol: str = "WLFI", hours_back: int = 24 * 7):
        """
        Run complete end-to-end demo
        
        Args:
            symbol: Trading symbol to analyze
            hours_back: Hours of historical data to fetch
        """
        print("🚀 WLFI Complete Trading Demo")
        print("=" * 60)
        
        try:
            # Step 1: Data Collection
            print("\n📊 STEP 1: Data Collection and Exploration")
            print("-" * 40)
            self._collect_data(symbol, hours_back)
            
            # Step 2: Data Preprocessing
            print("\n🔧 STEP 2: Data Preprocessing and Feature Engineering")
            print("-" * 40)
            self._preprocess_data()
            
            # Step 3: Model Training
            print("\n🤖 STEP 3: Model Training (XGBoost + Deep Learning)")
            print("-" * 40)
            self._train_models()
            
            # Step 4: Backtesting
            print("\n📈 STEP 4: Strategy Backtesting")
            print("-" * 40)
            self._run_backtests()
            
            # Step 5: Generate Reports
            print("\n📋 STEP 5: Generate Comprehensive Reports")
            print("-" * 40)
            self._generate_reports()
            
            print("\n🎉 Demo completed successfully!")
            print("📁 Check generated files for detailed results.")
            
        except Exception as e:
            logger.error(f"Demo failed: {str(e)}")
            print(f"\n❌ Demo failed: {str(e)}")
            raise
    
    def _collect_data(self, symbol: str, hours_back: int):
        """Collect data from ClickHouse"""
        print(f"🔗 Connecting to ClickHouse...")
        
        # Test connection
        if not self.ch_client.test_connection():
            raise ConnectionError("Failed to connect to ClickHouse")
        print("✅ Connected successfully")
        
        # Get available tables
        tables = self.ch_client.show_tables()
        print(f"📋 Found {len(tables)} tables: {', '.join(tables[:5])}...")
        
        # Try to get different types of market data
        data_types = ['l1_bbo', 'l2_orderbook', 'trades']
        
        for data_type in data_types:
            try:
                print(f"\n📊 Fetching {data_type} data...")
                
                if data_type == 'l1_bbo':
                    data = self.ch_client.get_l1_bbo_data(symbol, hours_back)
                elif data_type == 'l2_orderbook':
                    data = self.ch_client.get_l2_orderbook_data(symbol, hours_back)
                else:  # trades
                    data = self.ch_client.get_trades_data(symbol, hours_back)
                
                if not data.empty:
                    self.raw_data[data_type] = data
                    print(f"✅ Found {len(data)} {data_type} records")
                    print(f"📅 Date range: {data['ts'].min()} to {data['ts'].max()}")
                    
                    # Save raw data
                    output_file = f"raw_{data_type}_data.csv"
                    data.to_csv(output_file, index=False)
                    print(f"💾 Saved to {output_file}")
                else:
                    print(f"❌ No {data_type} data found")
                    
            except Exception as e:
                print(f"❌ Error fetching {data_type}: {str(e)}")
        
        if not self.raw_data:
            # Fallback: check all tables for any WLFI data
            print("\n🔍 Fallback: Searching all tables for WLFI data...")
            wlfi_tables = self.ch_client.find_wlfi_tables()
            
            if wlfi_tables:
                for table in wlfi_tables[:2]:  # Check first 2 tables
                    try:
                        data = self.ch_client.get_wlfi_data(table, hours_back, limit=5000)
                        if not data.empty:
                            self.raw_data[f'table_{table}'] = data
                            print(f"✅ Found {len(data)} records in {table}")
                    except Exception as e:
                        print(f"❌ Error with table {table}: {str(e)}")
        
        if not self.raw_data:
            raise ValueError("No WLFI data found in any table")
        
        print(f"\n✅ Data collection completed: {len(self.raw_data)} datasets")
    
    def _preprocess_data(self):
        """Preprocess and engineer features"""
        print("🔧 Processing raw data...")
        
        for data_type, raw_data in self.raw_data.items():
            try:
                print(f"\n📊 Processing {data_type} data ({len(raw_data)} records)")
                
                # Determine data type for preprocessing
                if 'bbo' in data_type or 'l1' in data_type:
                    clean_type = 'l1'
                elif 'orderbook' in data_type or 'l2' in data_type:
                    clean_type = 'l2'
                elif 'trade' in data_type:
                    clean_type = 'trades'
                else:
                    clean_type = 'l1'  # Default
                
                # Clean data
                cleaned = self.preprocessor.clean_market_data(raw_data, clean_type)
                print(f"🧹 Cleaned: {len(cleaned)} records ({len(raw_data) - len(cleaned)} removed)")
                
                if cleaned.empty:
                    print(f"⚠️ No data remaining after cleaning for {data_type}")
                    continue
                
                # Create features
                featured = self.preprocessor.create_features(cleaned, clean_type)
                print(f"🔧 Features created: {len(featured.columns)} columns")
                
                # Create labels for prediction
                labeled = self.preprocessor.create_labels(featured, 
                                                        target_col='mid_price' if 'mid_price' in featured.columns else 'price')
                
                self.processed_data[data_type] = labeled
                print(f"🏷️ Labels created: {len(labeled)} samples")
                
                # Save processed data
                output_file = f"processed_{data_type}_data.csv"
                labeled.to_csv(output_file, index=False)
                print(f"💾 Saved processed data to {output_file}")
                
            except Exception as e:
                print(f"❌ Error processing {data_type}: {str(e)}")
                logger.error(f"Preprocessing error for {data_type}: {str(e)}")
        
        if not self.processed_data:
            raise ValueError("No data survived preprocessing")
        
        print(f"\n✅ Preprocessing completed: {len(self.processed_data)} datasets ready")
    
    def _train_models(self):
        """Train XGBoost and deep learning models"""
        print("🤖 Training machine learning models...")
        
        # Use the largest dataset for training
        largest_dataset = max(self.processed_data.items(), key=lambda x: len(x[1]))
        data_name, data = largest_dataset
        
        print(f"📊 Training on {data_name} dataset ({len(data)} samples)")
        
        # XGBoost Training
        print("\n🌳 Training XGBoost model...")
        try:
            xgb_trainer = XGBoostTrainer(
                sequence_length=30,  # Shorter for XGBoost
                prediction_horizon=5,
                n_estimators=200,
                max_depth=5,
                learning_rate=0.1
            )
            
            # Prepare features
            X, y = xgb_trainer.prepare_features(data)
            
            if len(X) > 0:
                # Adjust step size for stability
                optimal_lr = xgb_trainer.adjust_step_size_for_stability(X, y)
                print(f"🎯 Optimal learning rate: {optimal_lr}")
                
                # Train model
                results = xgb_trainer.train_with_stability_enhancements(
                    X, y, validation_split=0.2, optimize_params=False
                )
                
                self.models['xgboost'] = {
                    'trainer': xgb_trainer,
                    'results': results
                }
                
                print(f"✅ XGBoost training completed")
                print(f"📊 Validation accuracy: {results['val_metrics']['accuracy']:.4f}")
                
                # Save model
                xgb_trainer.save_model('xgboost_wlfi_model.joblib')
                
                # Feature importance
                importance = xgb_trainer.get_feature_importance(top_n=10)
                print(f"\n🔝 Top 10 Important Features:")
                for _, row in importance.head(10).iterrows():
                    print(f"  {row['feature']}: {row['importance']:.4f}")
            else:
                print("❌ Insufficient data for XGBoost training")
                
        except Exception as e:
            print(f"❌ XGBoost training failed: {str(e)}")
            logger.error(f"XGBoost training error: {str(e)}")
        
        # Deep Learning Training
        for model_type in ['gru', 'transformer']:
            print(f"\n🧠 Training {model_type.upper()} model...")
            try:
                dl_trainer = DeepLearningTrainer(
                    model_type=model_type,
                    sequence_length=60,
                    prediction_horizon=5,
                    batch_size=32,
                    learning_rate=0.001,
                    max_epochs=50,
                    patience=10
                )
                
                # Prepare sequences
                X, y = dl_trainer.prepare_sequences(data)
                
                if len(X) > 100:  # Need more data for deep learning
                    # Adjust step size for stability
                    optimal_lr = dl_trainer.adjust_step_size_for_stability(X, y)
                    print(f"🎯 Optimal learning rate: {optimal_lr}")
                    
                    # Train model
                    results = dl_trainer.train_with_stability_enhancements(
                        X, y, validation_split=0.2
                    )
                    
                    self.models[model_type] = {
                        'trainer': dl_trainer,
                        'results': results
                    }
                    
                    print(f"✅ {model_type.upper()} training completed")
                    print(f"📊 Final accuracy: {results['final_metrics']['accuracy']:.4f}")
                    print(f"📈 Epochs trained: {results['epochs_trained']}")
                    
                    # Save model
                    dl_trainer.save_model(f'{model_type}_wlfi_model.pth')
                else:
                    print(f"❌ Insufficient data for {model_type.upper()} training (need >100 samples)")
                    
            except Exception as e:
                print(f"❌ {model_type.upper()} training failed: {str(e)}")
                logger.error(f"{model_type} training error: {str(e)}")
        
        print(f"\n✅ Model training completed: {len(self.models)} models trained")
    
    def _run_backtests(self):
        """Run strategy backtesting"""
        print("📈 Running strategy backtests...")
        
        # Use the processed data for backtesting
        for data_name, data in self.processed_data.items():
            if len(data) < 100:  # Need sufficient data
                continue
                
            print(f"\n📊 Backtesting on {data_name} ({len(data)} samples)")
            
            # Strategy 1: Simple Momentum
            print("\n🚀 Testing Momentum Strategy...")
            try:
                momentum_results = self.backtest_engine.run_strategy(
                    data, 
                    simple_momentum_strategy,
                    lookback=10,
                    threshold=0.01
                )
                
                self.backtest_results[f'{data_name}_momentum'] = momentum_results
                
                print(f"✅ Momentum Strategy Results:")
                print(f"  Total Return: {momentum_results.get('total_return', 0):.2%}")
                print(f"  Sharpe Ratio: {momentum_results.get('sharpe_ratio', 0):.3f}")
                print(f"  Max Drawdown: {momentum_results.get('max_drawdown', 0):.2%}")
                print(f"  Win Rate: {momentum_results.get('win_rate', 0):.2%}")
                
            except Exception as e:
                print(f"❌ Momentum strategy failed: {str(e)}")
            
            # Strategy 2: Mean Reversion
            print("\n↩️ Testing Mean Reversion Strategy...")
            try:
                mean_rev_results = self.backtest_engine.run_strategy(
                    data,
                    mean_reversion_strategy,
                    bb_threshold=0.8
                )
                
                self.backtest_results[f'{data_name}_mean_reversion'] = mean_rev_results
                
                print(f"✅ Mean Reversion Strategy Results:")
                print(f"  Total Return: {mean_rev_results.get('total_return', 0):.2%}")
                print(f"  Sharpe Ratio: {mean_rev_results.get('sharpe_ratio', 0):.3f}")
                print(f"  Max Drawdown: {mean_rev_results.get('max_drawdown', 0):.2%}")
                print(f"  Win Rate: {mean_rev_results.get('win_rate', 0):.2%}")
                
            except Exception as e:
                print(f"❌ Mean reversion strategy failed: {str(e)}")
        
        print(f"\n✅ Backtesting completed: {len(self.backtest_results)} strategy results")
    
    def _generate_reports(self):
        """Generate comprehensive reports and visualizations"""
        print("📋 Generating comprehensive reports...")
        
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        
        # 1. Model Performance Report
        self._generate_model_report(timestamp)
        
        # 2. Backtest Performance Report
        self._generate_backtest_report(timestamp)
        
        # 3. Data Quality Report
        self._generate_data_report(timestamp)
        
        # 4. Visualizations
        self._generate_visualizations(timestamp)
        
        # 5. Executive Summary
        self._generate_executive_summary(timestamp)
    
    def _generate_model_report(self, timestamp: str):
        """Generate model performance report"""
        if not self.models:
            return
            
        report = []
        report.append("MODEL PERFORMANCE REPORT")
        report.append("=" * 50)
        report.append(f"Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        report.append("")
        
        for model_name, model_data in self.models.items():
            results = model_data['results']
            report.append(f"{model_name.upper()} MODEL")
            report.append("-" * 30)
            
            if model_name == 'xgboost':
                val_metrics = results['val_metrics']
                report.append(f"Validation Accuracy:  {val_metrics['accuracy']:.4f}")
                report.append(f"Validation Precision: {val_metrics['precision']:.4f}")
                report.append(f"Validation Recall:    {val_metrics['recall']:.4f}")
                report.append(f"Validation F1 Score:  {val_metrics['f1_score']:.4f}")
                report.append(f"Best Iteration:       {results.get('best_iteration', 'N/A')}")
            else:  # Deep learning
                final_metrics = results['final_metrics']
                report.append(f"Final Accuracy:       {final_metrics['accuracy']:.4f}")
                report.append(f"Final Precision:      {final_metrics['precision']:.4f}")
                report.append(f"Final Recall:         {final_metrics['recall']:.4f}")
                report.append(f"Final F1 Score:       {final_metrics['f1_score']:.4f}")
                report.append(f"Epochs Trained:       {results['epochs_trained']}")
            
            report.append("")
        
        report_text = "\n".join(report)
        report_file = f"model_performance_report_{timestamp}.txt"
        
        with open(report_file, 'w') as f:
            f.write(report_text)
        
        print(f"📄 Model report saved: {report_file}")
    
    def _generate_backtest_report(self, timestamp: str):
        """Generate backtest performance report"""
        if not self.backtest_results:
            return
            
        for strategy_name, results in self.backtest_results.items():
            report_text = self.backtest_engine.generate_report(results)
            report_file = f"backtest_{strategy_name}_{timestamp}.txt"
            
            with open(report_file, 'w') as f:
                f.write(f"STRATEGY: {strategy_name.upper()}\n")
                f.write(f"Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n\n")
                f.write(report_text)
            
            print(f"📄 Backtest report saved: {report_file}")
    
    def _generate_data_report(self, timestamp: str):
        """Generate data quality report"""
        report = []
        report.append("DATA QUALITY REPORT")
        report.append("=" * 40)
        report.append(f"Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        report.append("")
        
        # Raw data summary
        report.append("RAW DATA SUMMARY")
        report.append("-" * 20)
        for data_type, data in self.raw_data.items():
            report.append(f"{data_type}:")
            report.append(f"  Records: {len(data)}")
            report.append(f"  Columns: {len(data.columns)}")
            if 'ts' in data.columns:
                report.append(f"  Date range: {data['ts'].min()} to {data['ts'].max()}")
            report.append("")
        
        # Processed data summary
        report.append("PROCESSED DATA SUMMARY")
        report.append("-" * 25)
        for data_type, data in self.processed_data.items():
            report.append(f"{data_type}:")
            report.append(f"  Records: {len(data)}")
            report.append(f"  Features: {len(data.columns)}")
            if 'label' in data.columns:
                label_counts = data['label'].value_counts().sort_index()
                report.append(f"  Label distribution: {dict(label_counts)}")
            report.append("")
        
        report_text = "\n".join(report)
        report_file = f"data_quality_report_{timestamp}.txt"
        
        with open(report_file, 'w') as f:
            f.write(report_text)
        
        print(f"📄 Data report saved: {report_file}")
    
    def _generate_visualizations(self, timestamp: str):
        """Generate visualization plots"""
        try:
            # Plot model training history if available
            for model_name, model_data in self.models.items():
                if model_name in ['gru', 'transformer'] and 'results' in model_data:
                    history = model_data['results'].get('training_history', [])
                    if history:
                        self._plot_training_history(model_name, history, timestamp)
            
            # Plot backtest performance
            for strategy_name, results in self.backtest_results.items():
                if self.backtest_engine.equity_curve:
                    fig = self.backtest_engine.plot_performance(
                        save_path=f"backtest_{strategy_name}_{timestamp}.png"
                    )
                    plt.close(fig)
            
            print("📊 Visualizations generated")
            
        except Exception as e:
            print(f"❌ Visualization error: {str(e)}")
    
    def _plot_training_history(self, model_name: str, history: List, timestamp: str):
        """Plot model training history"""
        df_history = pd.DataFrame(history)
        
        fig, axes = plt.subplots(2, 2, figsize=(12, 8))
        fig.suptitle(f'{model_name.upper()} Training History', fontsize=16)
        
        # Loss curves
        axes[0, 0].plot(df_history['epoch'], df_history['train_loss'], label='Train Loss', color='blue')
        axes[0, 0].plot(df_history['epoch'], df_history['val_loss'], label='Val Loss', color='red')
        axes[0, 0].set_title('Training & Validation Loss')
        axes[0, 0].set_xlabel('Epoch')
        axes[0, 0].set_ylabel('Loss')
        axes[0, 0].legend()
        axes[0, 0].grid(True)
        
        # Accuracy curves
        axes[0, 1].plot(df_history['epoch'], df_history['train_acc'], label='Train Acc', color='green')
        axes[0, 1].plot(df_history['epoch'], df_history['val_acc'], label='Val Acc', color='orange')
        axes[0, 1].set_title('Training & Validation Accuracy')
        axes[0, 1].set_xlabel('Epoch')
        axes[0, 1].set_ylabel('Accuracy')
        axes[0, 1].legend()
        axes[0, 1].grid(True)
        
        # Learning rate
        axes[1, 0].plot(df_history['epoch'], df_history['lr'], color='purple')
        axes[1, 0].set_title('Learning Rate')
        axes[1, 0].set_xlabel('Epoch')
        axes[1, 0].set_ylabel('Learning Rate')
        axes[1, 0].grid(True)
        
        # Final metrics text
        if history:
            final_metrics = history[-1]
            metrics_text = f"Final Metrics:\n"
            metrics_text += f"Train Acc: {final_metrics['train_acc']:.4f}\n"
            metrics_text += f"Val Acc: {final_metrics['val_acc']:.4f}\n"
            metrics_text += f"Epochs: {len(history)}"
            
            axes[1, 1].text(0.1, 0.5, metrics_text, transform=axes[1, 1].transAxes,
                           fontsize=12, verticalalignment='center',
                           bbox=dict(boxstyle='round', facecolor='lightgray', alpha=0.8))
            axes[1, 1].set_title('Final Performance')
            axes[1, 1].axis('off')
        
        plt.tight_layout()
        plot_file = f"{model_name}_training_history_{timestamp}.png"
        plt.savefig(plot_file, dpi=300, bbox_inches='tight')
        plt.close(fig)
        
        print(f"📈 Training plot saved: {plot_file}")
    
    def _generate_executive_summary(self, timestamp: str):
        """Generate executive summary"""
        summary = []
        summary.append("WLFI TRADING ANALYSIS - EXECUTIVE SUMMARY")
        summary.append("=" * 60)
        summary.append(f"Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        summary.append("")
        
        # Data Summary
        summary.append("DATA OVERVIEW")
        summary.append("-" * 20)
        total_records = sum(len(data) for data in self.raw_data.values())
        summary.append(f"Total Raw Records: {total_records:,}")
        summary.append(f"Data Sources: {len(self.raw_data)}")
        summary.append(f"Processed Datasets: {len(self.processed_data)}")
        summary.append("")
        
        # Model Summary
        summary.append("MODEL PERFORMANCE")
        summary.append("-" * 20)
        if self.models:
            for model_name, model_data in self.models.items():
                results = model_data['results']
                if model_name == 'xgboost':
                    acc = results['val_metrics']['accuracy']
                else:
                    acc = results['final_metrics']['accuracy']
                summary.append(f"{model_name.capitalize()}: {acc:.4f} accuracy")
        else:
            summary.append("No models trained")
        summary.append("")
        
        # Backtest Summary
        summary.append("STRATEGY PERFORMANCE")
        summary.append("-" * 25)
        if self.backtest_results:
            best_strategy = None
            best_return = float('-inf')
            
            for strategy_name, results in self.backtest_results.items():
                total_return = results.get('total_return', 0)
                sharpe = results.get('sharpe_ratio', 0)
                max_dd = results.get('max_drawdown', 0)
                
                summary.append(f"{strategy_name}:")
                summary.append(f"  Return: {total_return:.2%}")
                summary.append(f"  Sharpe: {sharpe:.3f}")
                summary.append(f"  Max DD: {max_dd:.2%}")
                summary.append("")
                
                if total_return > best_return:
                    best_return = total_return
                    best_strategy = strategy_name
            
            if best_strategy:
                summary.append(f"BEST STRATEGY: {best_strategy} ({best_return:.2%} return)")
        else:
            summary.append("No backtest results available")
        
        summary.append("")
        summary.append("RECOMMENDATIONS")
        summary.append("-" * 20)
        
        if self.models:
            summary.append("• Models trained successfully - consider ensemble approach")
        else:
            summary.append("• Insufficient data for model training - collect more data")
        
        if self.backtest_results:
            summary.append("• Strategy backtests completed - optimize parameters")
        else:
            summary.append("• No strategy testing performed - implement trading strategies")
        
        summary.append("• Monitor data quality and update models regularly")
        summary.append("• Implement risk management controls")
        summary.append("• Consider paper trading before live deployment")
        
        summary_text = "\n".join(summary)
        summary_file = f"executive_summary_{timestamp}.txt"
        
        with open(summary_file, 'w') as f:
            f.write(summary_text)
        
        print(f"📋 Executive summary saved: {summary_file}")
        
        # Also print to console
        print("\n" + "=" * 60)
        print("EXECUTIVE SUMMARY")
        print("=" * 60)
        print(summary_text.split("EXECUTIVE SUMMARY")[1])


def main():
    """Main demo function"""
    parser = argparse.ArgumentParser(description='WLFI Complete Trading Demo')
    parser.add_argument('--symbol', default='WLFI', help='Trading symbol to analyze')
    parser.add_argument('--hours', type=int, default=24*7, help='Hours of historical data')
    parser.add_argument('--password', required=True, help='ClickHouse password')
    parser.add_argument('--host', default='https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443',
                       help='ClickHouse host URL')
    
    args = parser.parse_args()
    
    # Initialize and run demo
    demo = WLFIDemo(args.host, args.password)
    demo.run_complete_demo(args.symbol, args.hours)


if __name__ == "__main__":
    main()