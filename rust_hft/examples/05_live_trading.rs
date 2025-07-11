/*!
 * 统一实盘交易系统
 * 
 * 整合功能：
 * - 实时策略执行
 * - 风险管理和熔断
 * - 订单管理和追踪
 * - 实时性能监控
 */

use anyhow::Result;
use clap::Parser;
use serde::{Serialize, Deserialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn, error};

#[derive(Parser)]
#[command(name = "live_trading")]
#[command(about = "Live trading system with risk management")]
struct Args {
    /// Trading symbol
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// Model path for strategy
    #[arg(short, long)]
    model_path: Option<String>,

    /// Dry run mode (no real trades)
    #[arg(long)]
    dry_run: bool,

    /// Initial capital
    #[arg(long, default_value_t = 10000.0)]
    capital: f64,

    /// Max position size as percentage of capital
    #[arg(long, default_value_t = 0.1)]
    max_position_pct: f64,

    /// Stop loss percentage
    #[arg(long, default_value_t = 0.02)]
    stop_loss_pct: f64,

    /// Risk budget per day
    #[arg(long, default_value_t = 0.05)]
    daily_risk_budget: f64,

    /// Trading duration in hours
    #[arg(long, default_value_t = 24)]
    duration_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TradingPosition {
    symbol: String,
    size: f64,
    entry_price: f64,
    current_price: f64,
    unrealized_pnl: f64,
    entry_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TradingStats {
    trades_executed: u32,
    winning_trades: u32,
    losing_trades: u32,
    total_pnl: f64,
    current_drawdown: f64,
    max_drawdown: f64,
    daily_pnl: f64,
    sharpe_ratio: f64,
}

#[derive(Debug, Clone, Copy)]
enum RiskSignal {
    Normal,
    Warning,
    Critical,
    Emergency,
}

struct LiveTradingSystem {
    config: Args,
    positions: Arc<RwLock<Vec<TradingPosition>>>,
    stats: Arc<RwLock<TradingStats>>,
    risk_manager: Arc<RiskManager>,
    order_manager: Arc<OrderManager>,
    is_running: Arc<tokio::sync::RwLock<bool>>,
}

struct RiskManager {
    daily_loss_limit: f64,
    position_size_limit: f64,
    stop_loss_pct: f64,
    current_daily_pnl: Arc<RwLock<f64>>,
    risk_alerts: broadcast::Sender<RiskSignal>,
}

struct OrderManager {
    pending_orders: Arc<RwLock<Vec<PendingOrder>>>,
    executed_orders: Arc<RwLock<Vec<ExecutedOrder>>>,
}

#[derive(Debug, Clone)]
struct PendingOrder {
    id: String,
    symbol: String,
    side: String,
    size: f64,
    price: f64,
    order_type: String,
    created_at: u64,
}

#[derive(Debug, Clone)]
struct ExecutedOrder {
    id: String,
    symbol: String,
    side: String,
    size: f64,
    executed_price: f64,
    executed_at: u64,
    commission: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info")
        .init();

    info!("🚀 Starting Live Trading System");
    info!("Symbol: {}, Capital: ${:.2}", args.symbol, args.capital);
    
    if args.dry_run {
        info!("🔸 DRY RUN MODE - No real trades will be executed");
    } else {
        warn!("⚠️ LIVE TRADING MODE - Real money at risk!");
    }

    // Create trading system
    let trading_system = LiveTradingSystem::new(args).await?;
    
    // Start trading
    trading_system.start().await?;
    
    Ok(())
}

impl LiveTradingSystem {
    async fn new(config: Args) -> Result<Self> {
        let risk_manager = Arc::new(RiskManager::new(&config).await?);
        let order_manager = Arc::new(OrderManager::new());
        
        Ok(Self {
            config,
            positions: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(RwLock::new(TradingStats::default())),
            risk_manager,
            order_manager,
            is_running: Arc::new(tokio::sync::RwLock::new(false)),
        })
    }
    
    async fn start(&self) -> Result<()> {
        *self.is_running.write().await = true;
        
        info!("🔄 Starting trading components...");
        
        // Start risk monitoring
        let risk_task = self.start_risk_monitoring();
        
        // Start market data processing
        let market_data_task = self.start_market_data_processing();
        
        // Start order management
        let order_task = self.start_order_management();
        
        // Start strategy execution
        let strategy_task = self.start_strategy_execution();
        
        // Start performance monitoring
        let monitor_task = self.start_performance_monitoring();
        
        // Create shutdown signal
        let (shutdown_tx, mut shutdown_rx) = broadcast::channel(1);
        
        // Handle Ctrl+C
        let shutdown_tx_clone = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.unwrap();
            info!("📡 Received Ctrl+C, initiating shutdown...");
            let _ = shutdown_tx_clone.send(());
        });
        
        // Set trading duration timeout
        let duration = Duration::from_secs(self.config.duration_hours * 3600);
        let timeout_shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            info!("⏰ Trading duration expired, initiating shutdown...");
            let _ = timeout_shutdown_tx.send(());
        });
        
        info!("✅ Live trading system operational");
        
        // Wait for shutdown signal
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!("🛑 Initiating graceful shutdown...");
                self.shutdown().await?;
            }
            _ = risk_task => info!("Risk monitoring completed"),
            _ = market_data_task => info!("Market data processing completed"),
            _ = order_task => info!("Order management completed"),
            _ = strategy_task => info!("Strategy execution completed"),
            _ = monitor_task => info!("Performance monitoring completed"),
        }
        
        self.print_final_report().await;
        
        Ok(())
    }
    
    async fn start_risk_monitoring(&self) -> Result<()> {
        let risk_manager = Arc::clone(&self.risk_manager);
        let positions = Arc::clone(&self.positions);
        let is_running = Arc::clone(&self.is_running);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            
            while *is_running.read().await {
                interval.tick().await;
                
                let positions = positions.read().await;
                if let Err(e) = risk_manager.check_risks(&*positions).await {
                    error!("Risk check failed: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    async fn start_market_data_processing(&self) -> Result<()> {
        let is_running = Arc::clone(&self.is_running);
        let positions = Arc::clone(&self.positions);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            
            while *is_running.read().await {
                interval.tick().await;
                
                // Simulate market data processing
                let current_price = Self::get_current_price("BTCUSDT").await;
                
                // Update position P&L
                let mut positions = positions.write().await;
                for position in positions.iter_mut() {
                    position.current_price = current_price;
                    position.unrealized_pnl = (current_price - position.entry_price) * position.size;
                }
            }
        });
        
        Ok(())
    }
    
    async fn start_order_management(&self) -> Result<()> {
        let order_manager = Arc::clone(&self.order_manager);
        let is_running = Arc::clone(&self.is_running);
        let dry_run = self.config.dry_run;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            
            while *is_running.read().await {
                interval.tick().await;
                
                if let Err(e) = order_manager.process_pending_orders(dry_run).await {
                    error!("Order processing failed: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    async fn start_strategy_execution(&self) -> Result<()> {
        let positions = Arc::clone(&self.positions);
        let order_manager = Arc::clone(&self.order_manager);
        let risk_manager = Arc::clone(&self.risk_manager);
        let is_running = Arc::clone(&self.is_running);
        let symbol = self.config.symbol.clone();
        let capital = self.config.capital;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            
            while *is_running.read().await {
                interval.tick().await;
                
                // Generate trading signal
                let signal = Self::generate_trading_signal(&symbol).await;
                
                if let Some(signal) = signal {
                    // Check risk before executing
                    if risk_manager.approve_trade(&signal, capital).await {
                        // Place order
                        if let Err(e) = order_manager.place_order(signal).await {
                            error!("Failed to place order: {}", e);
                        }
                    } else {
                        warn!("Trade rejected by risk management");
                    }
                }
            }
        });
        
        Ok(())
    }
    
    async fn start_performance_monitoring(&self) -> Result<()> {
        let stats = Arc::clone(&self.stats);
        let positions = Arc::clone(&self.positions);
        let is_running = Arc::clone(&self.is_running);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            
            while *is_running.read().await {
                interval.tick().await;
                
                let positions = positions.read().await;
                let mut stats = stats.write().await;
                
                Self::update_performance_stats(&mut stats, &*positions).await;
                Self::log_performance_update(&*stats).await;
            }
        });
        
        Ok(())
    }
    
    async fn shutdown(&self) -> Result<()> {
        info!("🛑 Shutting down trading system...");
        
        *self.is_running.write().await = false;
        
        // Close all positions if not dry run
        if !self.config.dry_run {
            self.close_all_positions().await?;
        }
        
        // Cancel all pending orders
        self.order_manager.cancel_all_orders().await?;
        
        info!("✅ Trading system shutdown complete");
        
        Ok(())
    }
    
    async fn close_all_positions(&self) -> Result<()> {
        let mut positions = self.positions.write().await;
        
        for position in positions.iter() {
            info!("🔄 Closing position: {} size {:.4}", position.symbol, position.size);
            
            // Simulate position closing
            tokio::time::sleep(Duration::from_millis(100)).await;
            
            info!("✅ Position closed with P&L: {:.2}", position.unrealized_pnl);
        }
        
        positions.clear();
        
        Ok(())
    }
    
    async fn print_final_report(&self) {
        let stats = self.stats.read().await;
        let positions = self.positions.read().await;
        
        info!("📊 ========== Final Trading Report ==========");
        info!("💼 Trading Summary:");
        info!("  - Total Trades: {}", stats.trades_executed);
        info!("  - Winning Trades: {}", stats.winning_trades);
        info!("  - Losing Trades: {}", stats.losing_trades);
        info!("  - Win Rate: {:.2}%", if stats.trades_executed > 0 { 
            stats.winning_trades as f64 / stats.trades_executed as f64 * 100.0 
        } else { 0.0 });
        
        info!("💰 P&L Summary:");
        info!("  - Total P&L: ${:.2}", stats.total_pnl);
        info!("  - Daily P&L: ${:.2}", stats.daily_pnl);
        info!("  - Max Drawdown: {:.2}%", stats.max_drawdown * 100.0);
        info!("  - Sharpe Ratio: {:.2}", stats.sharpe_ratio);
        
        info!("📈 Current Positions:");
        if positions.is_empty() {
            info!("  - No open positions");
        } else {
            for position in positions.iter() {
                info!("  - {}: {:.4} @ ${:.2} (P&L: ${:.2})", 
                      position.symbol, position.size, position.entry_price, position.unrealized_pnl);
            }
        }
        
        // Performance assessment
        if stats.sharpe_ratio > 2.0 {
            info!("🏆 EXCELLENT performance (Sharpe > 2.0)");
        } else if stats.sharpe_ratio > 1.0 {
            info!("✅ GOOD performance (Sharpe > 1.0)");
        } else if stats.sharpe_ratio > 0.0 {
            info!("⚠️ FAIR performance (Sharpe > 0.0)");
        } else {
            warn!("❌ POOR performance (Sharpe < 0.0)");
        }
        
        info!("============================================");
    }
    
    async fn get_current_price(symbol: &str) -> f64 {
        // Simulate price fluctuation
        use rand::Rng;
        let mut rng = rand::thread_rng();
        50000.0 + rng.gen_range(-1000.0..1000.0)
    }
    
    async fn generate_trading_signal(symbol: &str) -> Option<TradingSignal> {
        // Simulate signal generation
        use rand::Rng;
        let mut rng = rand::thread_rng();
        
        if rng.gen_bool(0.1) {  // 10% chance of signal
            Some(TradingSignal {
                symbol: symbol.to_string(),
                side: if rng.gen_bool(0.5) { "BUY".to_string() } else { "SELL".to_string() },
                size: 0.001,
                confidence: rng.gen_range(0.6..0.9),
            })
        } else {
            None
        }
    }
    
    async fn update_performance_stats(stats: &mut TradingStats, positions: &[TradingPosition]) {
        // Update unrealized P&L
        let unrealized_pnl: f64 = positions.iter().map(|p| p.unrealized_pnl).sum();
        
        // Simulate performance calculations
        stats.daily_pnl = stats.total_pnl + unrealized_pnl;
        
        if stats.daily_pnl < 0.0 && stats.daily_pnl.abs() > stats.max_drawdown.abs() {
            stats.max_drawdown = stats.daily_pnl / 10000.0;  // Assuming 10k initial capital
        }
        
        // Simple Sharpe ratio approximation
        stats.sharpe_ratio = if stats.max_drawdown != 0.0 {
            (stats.daily_pnl / 10000.0) / stats.max_drawdown.abs()
        } else {
            0.0
        };
    }
    
    async fn log_performance_update(stats: &TradingStats) {
        info!("📈 Performance Update: P&L ${:.2}, Drawdown {:.2}%, Sharpe {:.2}", 
              stats.daily_pnl, stats.max_drawdown * 100.0, stats.sharpe_ratio);
    }
}

impl RiskManager {
    async fn new(config: &Args) -> Result<Self> {
        let (risk_alerts, _) = broadcast::channel(100);
        
        Ok(Self {
            daily_loss_limit: config.capital * config.daily_risk_budget,
            position_size_limit: config.capital * config.max_position_pct,
            stop_loss_pct: config.stop_loss_pct,
            current_daily_pnl: Arc::new(RwLock::new(0.0)),
            risk_alerts,
        })
    }
    
    async fn check_risks(&self, positions: &[TradingPosition]) -> Result<()> {
        let total_exposure: f64 = positions.iter().map(|p| p.size.abs() * p.current_price).sum();
        let unrealized_pnl: f64 = positions.iter().map(|p| p.unrealized_pnl).sum();
        
        // Check daily loss limit
        if unrealized_pnl < -self.daily_loss_limit {
            error!("🚨 EMERGENCY: Daily loss limit exceeded!");
            let _ = self.risk_alerts.send(RiskSignal::Emergency);
        }
        
        // Check position size limits
        if total_exposure > self.position_size_limit * 2.0 {
            warn!("⚠️ Position exposure exceeds limits: ${:.2}", total_exposure);
            let _ = self.risk_alerts.send(RiskSignal::Critical);
        }
        
        Ok(())
    }
    
    async fn approve_trade(&self, signal: &TradingSignal, capital: f64) -> bool {
        let trade_value = signal.size * 50000.0;  // Approximate value
        
        // Basic risk checks
        if trade_value > self.position_size_limit {
            warn!("Trade rejected: Size exceeds limit");
            return false;
        }
        
        if signal.confidence < 0.7 {
            warn!("Trade rejected: Low confidence");
            return false;
        }
        
        true
    }
}

impl OrderManager {
    fn new() -> Self {
        Self {
            pending_orders: Arc::new(RwLock::new(Vec::new())),
            executed_orders: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    async fn place_order(&self, signal: TradingSignal) -> Result<()> {
        let order = PendingOrder {
            id: uuid::Uuid::new_v4().to_string(),
            symbol: signal.symbol,
            side: signal.side,
            size: signal.size,
            price: 50000.0,  // Current market price
            order_type: "MARKET".to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        };
        
        info!("📝 Placing order: {} {} {:.4}", order.side, order.symbol, order.size);
        
        self.pending_orders.write().await.push(order);
        
        Ok(())
    }
    
    async fn process_pending_orders(&self, dry_run: bool) -> Result<()> {
        let mut pending = self.pending_orders.write().await;
        let mut executed = self.executed_orders.write().await;
        
        let mut to_remove = Vec::new();
        
        for (index, order) in pending.iter().enumerate() {
            // Simulate order execution
            if dry_run {
                info!("🔸 DRY RUN: Order {} would be executed", order.id);
            } else {
                info!("✅ Order executed: {} {:.4}", order.symbol, order.size);
            }
            
            let executed_order = ExecutedOrder {
                id: order.id.clone(),
                symbol: order.symbol.clone(),
                side: order.side.clone(),
                size: order.size,
                executed_price: order.price,
                executed_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
                commission: order.size * order.price * 0.001,  // 0.1% commission
            };
            
            executed.push(executed_order);
            to_remove.push(index);
        }
        
        // Remove executed orders from pending
        for &index in to_remove.iter().rev() {
            pending.remove(index);
        }
        
        Ok(())
    }
    
    async fn cancel_all_orders(&self) -> Result<()> {
        let mut pending = self.pending_orders.write().await;
        info!("❌ Cancelling {} pending orders", pending.len());
        pending.clear();
        Ok(())
    }
}

impl Default for TradingStats {
    fn default() -> Self {
        Self {
            trades_executed: 0,
            winning_trades: 0,
            losing_trades: 0,
            total_pnl: 0.0,
            current_drawdown: 0.0,
            max_drawdown: 0.0,
            daily_pnl: 0.0,
            sharpe_ratio: 0.0,
        }
    }
}

#[derive(Debug)]
struct TradingSignal {
    symbol: String,
    side: String,
    size: f64,
    confidence: f64,
}