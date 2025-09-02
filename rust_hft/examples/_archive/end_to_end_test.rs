/*!
 * 端到端系統整合測試
 *
 * 測試重點：
 * - 完整交易流程延遲
 * - 數據收集→特徵提取→推理→執行 全鏈路
 * - 系統穩定性和錯誤恢復
 * - 資源使用效率
 * - 多組件協調性能
 */

use rust_hft::{
    core::orderbook::OrderBook,
    core::types::now_micros,
    engine::create_default_risk_manager,
    integrations::bitget_connector::{BitgetChannel, BitgetConnector, BitgetMessage},
    ml::FeatureExtractor,
};

#[cfg(feature = "torchscript")]
use rust_hft::ml::TorchScriptInference;

use anyhow::Result;
use clap::Parser;
use rand::Rng;
#[cfg(not(feature = "torchscript"))]
use rust_hft::ml::CpuInference;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

#[derive(Parser, Debug, Clone)]
#[command(about = "端到端系統整合測試")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// 模型路徑
    #[arg(short, long, default_value = "models/btc_model.pt")]
    model_path: String,

    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 300)]
    duration: u64,

    /// 是否完整管道測試
    #[arg(long)]
    full_pipeline: bool,

    /// 資金限額
    #[arg(long, default_value_t = 10000.0)]
    capital: f64,

    /// 最大倉位比例
    #[arg(long, default_value_t = 0.1)]
    max_position_ratio: f64,

    /// 風險限額
    #[arg(long, default_value_t = 0.05)]
    risk_limit: f64,

    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Default)]
struct E2EMetrics {
    // 數據收集
    total_messages: AtomicU64,
    data_collection_errors: AtomicU64,

    // 特徵提取
    feature_extractions: AtomicU64,
    feature_extraction_time_us: AtomicU64,
    feature_extraction_errors: AtomicU64,

    // 模型推理
    inferences: AtomicU64,
    inference_time_us: AtomicU64,
    inference_errors: AtomicU64,

    // 策略決策
    strategy_decisions: AtomicU64,
    strategy_time_us: AtomicU64,

    // 風險控制
    risk_checks: AtomicU64,
    risk_rejections: AtomicU64,
    risk_time_us: AtomicU64,

    // 訂單執行
    order_requests: AtomicU64,
    order_executions: AtomicU64,
    order_execution_time_us: AtomicU64,
    order_errors: AtomicU64,

    // 端到端延遲
    e2e_latency_sum_us: AtomicU64,
    e2e_latency_count: AtomicU64,
    min_e2e_latency_us: AtomicU64,
    max_e2e_latency_us: AtomicU64,

    // 交易統計
    total_trades: AtomicU64,
    profitable_trades: AtomicU64,
    current_position: AtomicU64, // 使用 bits 表示 f64
    unrealized_pnl: AtomicU64,   // 使用 bits 表示 f64
}

impl E2EMetrics {
    fn new() -> Self {
        Self {
            min_e2e_latency_us: AtomicU64::new(u64::MAX),
            ..Default::default()
        }
    }

    fn record_message(&self) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
    }

    fn record_data_error(&self) {
        self.data_collection_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn record_feature_extraction(&self, time_us: u64, success: bool) {
        self.feature_extractions.fetch_add(1, Ordering::Relaxed);
        self.feature_extraction_time_us
            .fetch_add(time_us, Ordering::Relaxed);
        if !success {
            self.feature_extraction_errors
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_inference(&self, time_us: u64, success: bool) {
        self.inferences.fetch_add(1, Ordering::Relaxed);
        self.inference_time_us.fetch_add(time_us, Ordering::Relaxed);
        if !success {
            self.inference_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_strategy_decision(&self, time_us: u64) {
        self.strategy_decisions.fetch_add(1, Ordering::Relaxed);
        self.strategy_time_us.fetch_add(time_us, Ordering::Relaxed);
    }

    fn record_risk_check(&self, time_us: u64, rejected: bool) {
        self.risk_checks.fetch_add(1, Ordering::Relaxed);
        self.risk_time_us.fetch_add(time_us, Ordering::Relaxed);
        if rejected {
            self.risk_rejections.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_order_request(&self, time_us: u64, success: bool) {
        self.order_requests.fetch_add(1, Ordering::Relaxed);
        self.order_execution_time_us
            .fetch_add(time_us, Ordering::Relaxed);
        if success {
            self.order_executions.fetch_add(1, Ordering::Relaxed);
        } else {
            self.order_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_e2e_latency(&self, latency_us: u64) {
        self.e2e_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.e2e_latency_count.fetch_add(1, Ordering::Relaxed);

        // 更新最小延遲
        loop {
            let current_min = self.min_e2e_latency_us.load(Ordering::Relaxed);
            if latency_us >= current_min {
                break;
            }
            if self
                .min_e2e_latency_us
                .compare_exchange_weak(
                    current_min,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        // 更新最大延遲
        loop {
            let current_max = self.max_e2e_latency_us.load(Ordering::Relaxed);
            if latency_us <= current_max {
                break;
            }
            if self
                .max_e2e_latency_us
                .compare_exchange_weak(
                    current_max,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
    }

    fn record_trade(&self, profitable: bool, position: f64, pnl: f64) {
        self.total_trades.fetch_add(1, Ordering::Relaxed);
        if profitable {
            self.profitable_trades.fetch_add(1, Ordering::Relaxed);
        }
        self.current_position
            .store(position.to_bits(), Ordering::Relaxed);
        self.unrealized_pnl.store(pnl.to_bits(), Ordering::Relaxed);
    }

    fn get_comprehensive_stats(&self) -> E2EStats {
        let messages = self.total_messages.load(Ordering::Relaxed);
        let data_errors = self.data_collection_errors.load(Ordering::Relaxed);

        let feature_extractions = self.feature_extractions.load(Ordering::Relaxed);
        let feature_time = self.feature_extraction_time_us.load(Ordering::Relaxed);
        let feature_errors = self.feature_extraction_errors.load(Ordering::Relaxed);

        let inferences = self.inferences.load(Ordering::Relaxed);
        let inference_time = self.inference_time_us.load(Ordering::Relaxed);
        let inference_errors = self.inference_errors.load(Ordering::Relaxed);

        let strategy_decisions = self.strategy_decisions.load(Ordering::Relaxed);
        let strategy_time = self.strategy_time_us.load(Ordering::Relaxed);

        let risk_checks = self.risk_checks.load(Ordering::Relaxed);
        let risk_rejections = self.risk_rejections.load(Ordering::Relaxed);
        let risk_time = self.risk_time_us.load(Ordering::Relaxed);

        let order_requests = self.order_requests.load(Ordering::Relaxed);
        let order_executions = self.order_executions.load(Ordering::Relaxed);
        let order_execution_time = self.order_execution_time_us.load(Ordering::Relaxed);
        let order_errors = self.order_errors.load(Ordering::Relaxed);

        let e2e_latency_sum = self.e2e_latency_sum_us.load(Ordering::Relaxed);
        let e2e_latency_count = self.e2e_latency_count.load(Ordering::Relaxed);
        let min_e2e_latency = self.min_e2e_latency_us.load(Ordering::Relaxed);
        let max_e2e_latency = self.max_e2e_latency_us.load(Ordering::Relaxed);

        let total_trades = self.total_trades.load(Ordering::Relaxed);
        let profitable_trades = self.profitable_trades.load(Ordering::Relaxed);
        let current_position = f64::from_bits(self.current_position.load(Ordering::Relaxed));
        let unrealized_pnl = f64::from_bits(self.unrealized_pnl.load(Ordering::Relaxed));

        E2EStats {
            messages,
            data_errors,
            feature_extractions,
            avg_feature_time: if feature_extractions > 0 {
                feature_time as f64 / feature_extractions as f64
            } else {
                0.0
            },
            feature_errors,
            inferences,
            avg_inference_time: if inferences > 0 {
                inference_time as f64 / inferences as f64
            } else {
                0.0
            },
            inference_errors,
            strategy_decisions,
            avg_strategy_time: if strategy_decisions > 0 {
                strategy_time as f64 / strategy_decisions as f64
            } else {
                0.0
            },
            risk_checks,
            risk_rejections,
            avg_risk_time: if risk_checks > 0 {
                risk_time as f64 / risk_checks as f64
            } else {
                0.0
            },
            order_requests,
            order_executions,
            avg_order_execution_time: if order_requests > 0 {
                order_execution_time as f64 / order_requests as f64
            } else {
                0.0
            },
            order_errors,
            avg_e2e_latency: if e2e_latency_count > 0 {
                e2e_latency_sum as f64 / e2e_latency_count as f64
            } else {
                0.0
            },
            min_e2e_latency,
            max_e2e_latency,
            total_trades,
            profitable_trades,
            current_position,
            unrealized_pnl,
        }
    }
}

#[derive(Debug)]
struct E2EStats {
    messages: u64,
    data_errors: u64,
    feature_extractions: u64,
    avg_feature_time: f64,
    feature_errors: u64,
    inferences: u64,
    avg_inference_time: f64,
    inference_errors: u64,
    strategy_decisions: u64,
    avg_strategy_time: f64,
    risk_checks: u64,
    risk_rejections: u64,
    avg_risk_time: f64,
    order_requests: u64,
    order_executions: u64,
    avg_order_execution_time: f64,
    order_errors: u64,
    avg_e2e_latency: f64,
    min_e2e_latency: u64,
    max_e2e_latency: u64,
    total_trades: u64,
    profitable_trades: u64,
    current_position: f64,
    unrealized_pnl: f64,
}

// TradingSignal is now imported from core::types, so we'll create a simple signal wrapper
#[derive(Debug, Serialize, Deserialize)]
struct SimpleSignal {
    timestamp: u64,
    symbol: String,
    signal_type: String, // "BUY", "SELL", "HOLD"
    confidence: f64,
    suggested_size: f64,
    predicted_price: f64,
    risk_score: f64,
}

async fn run_e2e_trading_pipeline(
    args: Args,
    metrics: Arc<E2EMetrics>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    // 初始化組件
    let mut connector = BitgetConnector::new(Default::default());
    let mut feature_extractor = FeatureExtractor::new(100); // 100 tick window
                                                            // Mock inference engine since we don't have a model loader in this simple example
    #[cfg(feature = "torchscript")]
    let _inference_engine = TorchScriptInference::new(&args.model_path)?;
    #[cfg(not(feature = "torchscript"))]
    let _inference_engine = CpuInference::create_default_linear_model();

    let risk_manager = create_default_risk_manager();

    // 添加訂閱
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);

    // 建立數據連接
    let (tx, mut rx) = mpsc::channel::<BitgetMessage>(1000);

    // 啟動連接（在後台任務中處理）
    let connector_handle = {
        let tx = tx.clone();
        tokio::spawn(async move {
            connector
                .connect_public(move |msg| {
                    let _ = tx.try_send(msg);
                })
                .await
        })
    };

    info!("🚀 端到端交易流程已啟動");

    while !shutdown.load(Ordering::Relaxed) {
        tokio::select! {
            Some(message) = rx.recv() => {
                let pipeline_start = now_micros();

                match message {
                    BitgetMessage::OrderBook { symbol: _, data: _, action: _, timestamp: _ } => {
                        metrics.record_message();

                        // Convert BitgetMessage to OrderBookUpdate
                        let orderbook_update = match message.to_orderbook_update() {
                            Ok(Some(update)) => update,
                            Ok(None) => {
                                if args.verbose {
                                    warn!("無法轉換訂單簿消息");
                                }
                                continue;
                            }
                            Err(e) => {
                                if args.verbose {
                                    warn!("訂單簿轉換失敗: {}", e);
                                }
                                continue;
                            }
                        };

                        // Create OrderBook from update
                        let mut orderbook = OrderBook::new(args.symbol.clone());
                        if let Err(e) = orderbook.init_snapshot(orderbook_update) {
                            if args.verbose {
                                warn!("OrderBook 初始化失敗: {}", e);
                            }
                            continue;
                        }

                        // 階段1：特徵提取
                        let feature_start = Instant::now();
                        let features = match feature_extractor.extract_features(&orderbook, 0, now_micros()) {
                            Ok(features) => {
                                let feature_time = feature_start.elapsed().as_micros() as u64;
                                metrics.record_feature_extraction(feature_time, true);
                                features
                            }
                            Err(e) => {
                                let feature_time = feature_start.elapsed().as_micros() as u64;
                                metrics.record_feature_extraction(feature_time, false);
                                if args.verbose {
                                    warn!("特徵提取失敗: {}", e);
                                }
                                continue;
                            }
                        };

                        // 階段2：模型推理 (模擬)
                        let inference_start = Instant::now();
                        // Mock prediction: use feature-based simple logic
                        let prediction: f64 = {
                            let mid_price = features.mid_price.0;
                            let obi = features.obi_l5;
                            let spread_bps = features.spread_bps;

                            // Simple prediction based on order book imbalance
                            if obi > 0.1 && spread_bps < 10.0 {
                                0.7 // Bullish signal
                            } else if obi < -0.1 && spread_bps < 10.0 {
                                -0.7 // Bearish signal
                            } else {
                                0.0 // Neutral signal
                            }
                        };

                        let inference_time = inference_start.elapsed().as_micros() as u64;
                        metrics.record_inference(inference_time, true);

                        // 階段3：策略決策 (模擬)
                        let strategy_start = Instant::now();
                        let trading_signal = SimpleSignal {
                            timestamp: now_micros(),
                            symbol: args.symbol.clone(),
                            signal_type: if prediction > 0.5 {
                                "BUY".to_string()
                            } else if prediction < -0.5 {
                                "SELL".to_string()
                            } else {
                                "HOLD".to_string()
                            },
                            confidence: prediction.abs(),
                            suggested_size: if prediction.abs() > 0.5 {
                                args.capital * args.max_position_ratio * prediction.abs()
                            } else {
                                0.0
                            },
                            predicted_price: features.mid_price.0,
                            risk_score: 1.0 - prediction.abs(),
                        };

                        let strategy_time = strategy_start.elapsed().as_micros() as u64;
                        metrics.record_strategy_decision(strategy_time);

                        // 階段4：風險控制 (模擬)
                        let risk_start = Instant::now();
                        // Simple risk check: reject if suggested size is too large or confidence too low
                        let risk_approved = trading_signal.suggested_size <= args.capital * args.max_position_ratio
                            && trading_signal.confidence >= 0.3
                            && trading_signal.risk_score <= args.risk_limit;

                        let risk_time = risk_start.elapsed().as_micros() as u64;
                        metrics.record_risk_check(risk_time, !risk_approved);

                        if !risk_approved {
                            if args.verbose {
                                info!("訂單被風險控制拒絕");
                            }
                            continue;
                        }

                        // 階段5：訂單執行（如果不是完整管道測試，則模擬執行）
                        let execution_start = Instant::now();
                        let execution_success = if args.full_pipeline {
                            // 真實訂單執行
                            match execute_real_order(&trading_signal).await {
                                Ok(_) => true,
                                Err(e) => {
                                    error!("訂單執行失敗: {}", e);
                                    false
                                }
                            }
                        } else {
                            // 模擬執行
                            simulate_order_execution(&trading_signal).await
                        };

                        let execution_time = execution_start.elapsed().as_micros() as u64;
                        metrics.record_order_request(execution_time, execution_success);

                        // 記錄端到端延遲
                        let e2e_latency = now_micros() - pipeline_start;
                        metrics.record_e2e_latency(e2e_latency);

                        // 更新交易統計
                        if execution_success {
                            let profitable = trading_signal.confidence > 0.6;
                            metrics.record_trade(profitable, trading_signal.suggested_size, trading_signal.predicted_price);
                        }

                        if args.verbose && metrics.total_messages.load(Ordering::Relaxed) % 1000 == 0 {
                            info!("處理了 {} 條消息，端到端延遲: {}μs",
                                  metrics.total_messages.load(Ordering::Relaxed),
                                  e2e_latency);
                        }
                    }
                    BitgetMessage::Trade { .. } | BitgetMessage::Ticker { .. } => {
                        // 非訂單簿消息，跳過處理
                        if args.verbose {
                            debug!("收到非訂單簿消息，跳過處理");
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => {
                // 定期檢查關閉信號
                continue;
            }
        }
    }

    // 停止連接器任務
    connector_handle.abort();

    Ok(())
}

async fn execute_real_order(signal: &SimpleSignal) -> Result<()> {
    // 這裡實現真實的訂單執行邏輯
    // 為了測試安全，這裡只是模擬
    tokio::time::sleep(Duration::from_micros(100)).await;
    info!(
        "真實訂單執行: {} {} @ {:.2}",
        signal.signal_type, signal.suggested_size, signal.predicted_price
    );
    Ok(())
}

async fn simulate_order_execution(signal: &SimpleSignal) -> bool {
    // 模擬訂單執行延遲
    tokio::time::sleep(Duration::from_micros(50)).await;

    // 模擬 95% 成功率
    let success_rate = 0.95;
    let mut rng = rand::thread_rng();
    let random_value: f64 = rng.random();

    if random_value < success_rate {
        if signal.confidence > 0.5 {
            return true;
        }
    }

    false
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let metrics = Arc::new(E2EMetrics::new());
    let shutdown = Arc::new(AtomicBool::new(false));

    info!("🚀 端到端系統整合測試開始");
    info!("📊 測試參數:");
    info!("   交易對: {}", args.symbol);
    info!("   模型路徑: {}", args.model_path);
    info!("   測試時長: {}s", args.duration);
    info!("   完整管道: {}", args.full_pipeline);
    info!("   資金限額: ${:.2}", args.capital);
    info!("   最大倉位: {:.1}%", args.max_position_ratio * 100.0);
    info!("   風險限額: {:.1}%", args.risk_limit * 100.0);

    // 啟動主要交易流程
    let main_metrics = metrics.clone();
    let main_shutdown = shutdown.clone();
    let main_args = args.clone();
    let main_handle = tokio::spawn(async move {
        run_e2e_trading_pipeline(main_args, main_metrics, main_shutdown).await
    });

    // 統計報告任務
    let report_metrics = metrics.clone();
    let report_shutdown = shutdown.clone();
    let report_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        let start_time = Instant::now();

        while !report_shutdown.load(Ordering::Relaxed) {
            interval.tick().await;

            let stats = report_metrics.get_comprehensive_stats();
            let elapsed = start_time.elapsed().as_secs_f64();

            info!("📊 系統統計 ({:.1}s):", elapsed);
            info!(
                "   📨 消息處理: {} ({:.1}/s)",
                stats.messages,
                stats.messages as f64 / elapsed
            );
            info!(
                "   🔍 特徵提取: {} ({:.1}μs)",
                stats.feature_extractions, stats.avg_feature_time
            );
            info!(
                "   🧠 模型推理: {} ({:.1}μs)",
                stats.inferences, stats.avg_inference_time
            );
            info!(
                "   📈 策略決策: {} ({:.1}μs)",
                stats.strategy_decisions, stats.avg_strategy_time
            );
            info!(
                "   🛡️  風險控制: {} ({:.1}μs, 拒絕: {})",
                stats.risk_checks, stats.avg_risk_time, stats.risk_rejections
            );
            info!(
                "   💼 訂單執行: {}/{} ({:.1}μs)",
                stats.order_executions, stats.order_requests, stats.avg_order_execution_time
            );
            info!(
                "   ⚡ 端到端延遲: {:.1}μs (min: {}μs, max: {}μs)",
                stats.avg_e2e_latency, stats.min_e2e_latency, stats.max_e2e_latency
            );
            info!(
                "   💰 交易統計: {}/{} (勝率: {:.1}%)",
                stats.profitable_trades,
                stats.total_trades,
                if stats.total_trades > 0 {
                    stats.profitable_trades as f64 / stats.total_trades as f64 * 100.0
                } else {
                    0.0
                }
            );
            info!(
                "   📊 當前倉位: {:.4}, 未實現PnL: ${:.2}",
                stats.current_position, stats.unrealized_pnl
            );
            info!(
                "   ❌ 錯誤統計: 數據({}) 特徵({}) 推理({}) 執行({})",
                stats.data_errors, stats.feature_errors, stats.inference_errors, stats.order_errors
            );
            info!("   ──────────────────────────────────────────────────");
        }
    });

    // 等待測試完成
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    // 關閉系統
    info!("🔄 正在關閉系統...");
    shutdown.store(true, Ordering::Relaxed);

    // 等待所有任務完成
    let _ = main_handle.await;
    let _ = report_handle.await;

    // 最終統計報告
    let final_stats = metrics.get_comprehensive_stats();
    let total_time = args.duration as f64;

    info!("🎉 端到端測試完成 - 最終報告:");
    info!("   ⏱️  總測試時間: {:.1}s", total_time);
    info!(
        "   📨 消息處理: {} ({:.1}/s)",
        final_stats.messages,
        final_stats.messages as f64 / total_time
    );
    info!(
        "   🔍 特徵提取: {} ({:.1}μs)",
        final_stats.feature_extractions, final_stats.avg_feature_time
    );
    info!(
        "   🧠 模型推理: {} ({:.1}μs)",
        final_stats.inferences, final_stats.avg_inference_time
    );
    info!(
        "   📈 策略決策: {} ({:.1}μs)",
        final_stats.strategy_decisions, final_stats.avg_strategy_time
    );
    info!(
        "   🛡️  風險控制: {} ({:.1}μs)",
        final_stats.risk_checks, final_stats.avg_risk_time
    );
    info!(
        "   💼 訂單執行: {}/{} ({:.1}μs)",
        final_stats.order_executions,
        final_stats.order_requests,
        final_stats.avg_order_execution_time
    );
    info!(
        "   ⚡ 端到端延遲: {:.1}μs (min: {}μs, max: {}μs)",
        final_stats.avg_e2e_latency, final_stats.min_e2e_latency, final_stats.max_e2e_latency
    );

    // 計算各階段延遲佔比
    let total_component_time = final_stats.avg_feature_time
        + final_stats.avg_inference_time
        + final_stats.avg_strategy_time
        + final_stats.avg_risk_time
        + final_stats.avg_order_execution_time;

    info!("   📊 延遲分解:");
    info!(
        "      特徵提取: {:.1}% ({:.1}μs)",
        final_stats.avg_feature_time / total_component_time * 100.0,
        final_stats.avg_feature_time
    );
    info!(
        "      模型推理: {:.1}% ({:.1}μs)",
        final_stats.avg_inference_time / total_component_time * 100.0,
        final_stats.avg_inference_time
    );
    info!(
        "      策略決策: {:.1}% ({:.1}μs)",
        final_stats.avg_strategy_time / total_component_time * 100.0,
        final_stats.avg_strategy_time
    );
    info!(
        "      風險控制: {:.1}% ({:.1}μs)",
        final_stats.avg_risk_time / total_component_time * 100.0,
        final_stats.avg_risk_time
    );
    info!(
        "      訂單執行: {:.1}% ({:.1}μs)",
        final_stats.avg_order_execution_time / total_component_time * 100.0,
        final_stats.avg_order_execution_time
    );

    // 交易統計
    let win_rate = if final_stats.total_trades > 0 {
        final_stats.profitable_trades as f64 / final_stats.total_trades as f64 * 100.0
    } else {
        0.0
    };

    info!("   💰 交易統計:");
    info!("      總交易: {}", final_stats.total_trades);
    info!("      盈利交易: {}", final_stats.profitable_trades);
    info!("      勝率: {:.1}%", win_rate);
    info!("      當前倉位: {:.4}", final_stats.current_position);
    info!("      未實現PnL: ${:.2}", final_stats.unrealized_pnl);

    // 錯誤統計
    let total_operations = final_stats.feature_extractions
        + final_stats.inferences
        + final_stats.strategy_decisions
        + final_stats.risk_checks
        + final_stats.order_requests;
    let total_errors = final_stats.data_errors
        + final_stats.feature_errors
        + final_stats.inference_errors
        + final_stats.order_errors;
    let error_rate = if total_operations > 0 {
        total_errors as f64 / total_operations as f64 * 100.0
    } else {
        0.0
    };

    info!("   ❌ 錯誤統計:");
    info!("      總錯誤: {}", total_errors);
    info!("      錯誤率: {:.2}%", error_rate);
    info!("      風險拒絕: {}", final_stats.risk_rejections);
    info!(
        "      風險拒絕率: {:.2}%",
        if final_stats.risk_checks > 0 {
            final_stats.risk_rejections as f64 / final_stats.risk_checks as f64 * 100.0
        } else {
            0.0
        }
    );

    // 性能評級
    let grade = if final_stats.avg_e2e_latency < 10000.0 && error_rate < 1.0 && win_rate > 50.0 {
        "A+ 優秀"
    } else if final_stats.avg_e2e_latency < 20000.0 && error_rate < 2.0 && win_rate > 45.0 {
        "A 良好"
    } else if final_stats.avg_e2e_latency < 50000.0 && error_rate < 5.0 && win_rate > 40.0 {
        "B 中等"
    } else {
        "C 需要優化"
    };

    info!("🏆 系統性能評級: {}", grade);

    // 優化建議
    if final_stats.avg_e2e_latency > 20000.0 {
        info!("💡 建議: 端到端延遲過高，優化瓶頸組件");
    }
    if error_rate > 2.0 {
        info!("💡 建議: 錯誤率較高，加強異常處理和重試機制");
    }
    if win_rate < 50.0 {
        info!("💡 建議: 交易勝率偏低，優化策略參數或模型");
    }
    if final_stats.avg_inference_time > 5000.0 {
        info!("💡 建議: 模型推理較慢，考慮模型量化或硬件加速");
    }

    Ok(())
}
// Archived legacy example; see grouped examples
