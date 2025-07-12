/*!
 * 📊 LOB回放系统演示 - TimeTravelManager + TorchScript推理引擎
 * 
 * 这个例子展示了如何使用新的LOB回放系统来:
 * 1. 使用TimeTravelManager进行精确时间控制
 * 2. 验证数据完整性
 * 3. 集成TorchScript推理引擎实现<100μs推理
 * 4. 演示完整的历史数据回放流程
 * 
 * 使用方法:
 * cargo run --example 02_lob_replay_demo --features="database,ml-pytorch"
 */

use rust_hft::core::*;
use rust_hft::replay::*;
use rust_hft::ml::*;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use tracing::{info, warn, error, debug};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .init();
    
    info!("🚀 启动LOB回放系统演示");
    
    // 创建演示配置
    let replay_config = create_demo_replay_config().await?;
    
    // 演示1: TimeTravelManager 时间控制
    demo_time_travel_manager(&replay_config).await?;
    
    // 演示2: TorchScript推理引擎
    demo_torchscript_inference().await?;
    
    // 演示3: 完整回放系统
    demo_complete_replay_system(&replay_config).await?;
    
    info!("✅ LOB回放系统演示完成");
    Ok(())
}

/// 创建演示用的回放配置
async fn create_demo_replay_config() -> Result<ReplayConfig, Box<dyn std::error::Error>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_micros() as u64;
    
    Ok(ReplayConfig {
        speed_multiplier: 1.0,
        start_timestamp_us: now - 3600_000_000, // 1小时前
        end_timestamp_us: now,
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        max_memory_mb: 512,
        buffer_size: 1000,
        batch_size: 100,
        enable_validation: true,
        output_format: ReplayOutputFormat::RawLobEvents,
    })
}

/// 演示TimeTravelManager的功能
async fn demo_time_travel_manager(config: &ReplayConfig) -> Result<(), Box<dyn std::error::Error>> {
    info!("🕐 演示TimeTravelManager功能");
    
    // 创建时间管理器
    let time_manager = TimeTravelManager::new(config.clone());
    
    // 初始化
    time_manager.initialize(config.start_timestamp_us, 2.0).await?;
    info!("初始化完成，速度: 2.0x");
    
    // 订阅时间更新
    let mut time_receiver = time_manager.subscribe_time_updates();
    
    // 启动时间推进
    time_manager.start().await?;
    info!("时间推进已开始");
    
    // 监听时间变化
    tokio::spawn(async move {
        while time_receiver.changed().await.is_ok() {
            let time = *time_receiver.borrow();
            debug!("时间更新: {} ({}x速度)", 
                   utils::timestamp_to_string(time.timestamp_us),
                   time.speed_multiplier);
        }
    });
    
    // 等待一段时间
    sleep(Duration::from_millis(100)).await;
    
    let current_time = time_manager.current_time().await;
    info!("当前虚拟时间: {}", utils::timestamp_to_string(current_time));
    
    // 测试暂停和恢复
    time_manager.pause().await?;
    info!("时间已暂停");
    
    sleep(Duration::from_millis(50)).await;
    
    time_manager.start().await?;
    info!("时间已恢复");
    
    // 测试速度调整
    time_manager.set_speed(10.0).await?;
    info!("速度调整为10.0x");
    
    sleep(Duration::from_millis(50)).await;
    
    // 测试时间跳转
    let jump_time = config.start_timestamp_us + 1800_000_000; // +30分钟
    time_manager.seek_to(jump_time).await?;
    info!("跳转到: {}", utils::timestamp_to_string(jump_time));
    
    // 测试书签功能
    time_manager.add_bookmark(
        "demo_checkpoint".to_string(),
        None,
        Some("演示检查点".to_string())
    ).await?;
    info!("添加书签: demo_checkpoint");
    
    // 获取统计信息
    let stats = time_manager.get_stats().await;
    info!("时间管理器统计: 总运行时间={:?}, 书签数={}", 
          stats.total_runtime, stats.bookmark_count);
    
    info!("✅ TimeTravelManager演示完成");
    Ok(())
}

/// 演示TorchScript推理引擎
async fn demo_torchscript_inference() -> Result<(), Box<dyn std::error::Error>> {
    info!("🧠 演示TorchScript推理引擎");
    
    // 创建推理配置
    let inference_config = InferenceConfig {
        max_concurrent_inferences: 4,
        inference_timeout_ms: 1000,
        enable_batching: true,
        max_batch_size: 16,
        enable_gpu: false, // 对于演示，使用CPU
        enable_warmup: true,
        warmup_iterations: 3,
        ..Default::default()
    };
    
    // 创建推理引擎
    let inference_engine = TorchScriptInferenceEngine::new(inference_config).await?;
    info!("推理引擎创建成功");
    
    // 创建模拟推理请求
    let request = create_mock_inference_request();
    
    // 执行推理 (注意: 这将使用CPU fallback，因为没有实际的模型)
    match inference_engine.infer(request).await {
        Ok(result) => {
            info!("推理成功: 请求ID={}, 耗时={:?}", 
                  result.request_id, result.inference_time);
            info!("输出数量: {}", result.outputs.len());
            
            // 检查是否达到<100μs的目标
            let inference_time_us = result.inference_time.as_micros() as u64;
            if inference_time_us < 100 {
                info!("🎯 推理时间 {}μs - 达到<100μs目标!", inference_time_us);
            } else {
                warn!("⚠️ 推理时间 {}μs - 超过100μs目标", inference_time_us);
            }
        }
        Err(e) => {
            warn!("推理失败 (预期的，因为没有实际模型): {}", e);
        }
    }
    
    // 演示批量推理
    info!("🔄 演示批量推理");
    let batch_requests = create_batch_inference_requests(8);
    let batch_start = std::time::Instant::now();
    
    match inference_engine.batch_infer(batch_requests).await {
        Ok(results) => {
            let batch_time = batch_start.elapsed();
            info!("批量推理完成: {} 个请求, 总耗时 {:?}", results.len(), batch_time);
            
            let avg_time_per_request = batch_time.as_micros() as f64 / results.len() as f64;
            info!("平均每个请求: {:.2}μs", avg_time_per_request);
            
            if avg_time_per_request < 100.0 {
                info!("🎯 批量推理平均时间达到<100μs目标!");
            }
        }
        Err(e) => {
            warn!("批量推理失败: {}", e);
        }
    }
    
    // 获取推理统计
    let stats = inference_engine.get_stats().await;
    info!("推理引擎统计: 总推理数={}, 成功数={}, 平均时间={:.2}μs",
          stats.total_inferences, stats.successful_inferences, stats.average_inference_time_us);
    
    info!("✅ TorchScript推理引擎演示完成");
    Ok(())
}

/// 创建模拟推理请求
fn create_mock_inference_request() -> InferenceRequest {
    InferenceRequest {
        request_id: "demo_request_001".to_string(),
        model_id: "demo_model".to_string(),
        inputs: vec![
            InferenceInput {
                name: "features".to_string(),
                data: InputData::Float32(vec![1.0, 2.0, 3.0, 4.0, 5.0]),
                shape: vec![1, 5],
            }
        ],
        options: InferenceOptions {
            force_gpu: false,
            enable_batching: false,
            timeout_ms: 1000,
            priority: InferencePriority::Normal,
        },
        created_at: std::time::Instant::now(),
    }
}

/// 创建批量推理请求
fn create_batch_inference_requests(count: usize) -> Vec<InferenceRequest> {
    (0..count)
        .map(|i| InferenceRequest {
            request_id: format!("batch_request_{:03}", i),
            model_id: "demo_model".to_string(),
            inputs: vec![
                InferenceInput {
                    name: "features".to_string(),
                    data: InputData::Float32(vec![i as f32; 10]),
                    shape: vec![1, 10],
                }
            ],
            options: InferenceOptions {
                force_gpu: false,
                enable_batching: true,
                timeout_ms: 1000,
                priority: InferencePriority::Normal,
            },
            created_at: std::time::Instant::now(),
        })
        .collect()
}

/// 演示完整的回放系统
async fn demo_complete_replay_system(config: &ReplayConfig) -> Result<(), Box<dyn std::error::Error>> {
    info!("🔄 演示完整回放系统集成");
    
    // 注意: 这里需要实际的ClickHouse连接
    // 为了演示，我们创建一个模拟的数据块
    let demo_chunk = create_demo_data_chunk(config);
    
    // 创建数据验证器
    let mut validator = ReplayValidator::default();
    
    // 验证数据块
    let validation_results = validator.validate_chunk(&demo_chunk);
    info!("数据验证结果: {} 个结果", validation_results.len());
    
    for result in &validation_results {
        match result.level {
            ValidationLevel::Error => error!("验证错误: {}", result.message),
            ValidationLevel::Warning => warn!("验证警告: {}", result.message),
            ValidationLevel::Info => info!("验证信息: {}", result.message),
            ValidationLevel::Anomaly => warn!("检测到异常: {}", result.message),
        }
    }
    
    // 获取验证统计
    let validation_stats = validator.get_stats();
    info!("验证统计: 总事件={}, 有效事件={}, 通过率={:.2}%",
          validation_stats.total_events,
          validation_stats.valid_events,
          validation_stats.validation_pass_rate * 100.0);
    
    // 生成验证报告
    let report = validator.generate_report();
    info!("验证报告:\n{}", report);
    
    // 演示实时推理 + 回放结合
    info!("🧠 演示实时推理与回放结合");
    demo_realtime_inference_with_replay(&demo_chunk).await?;
    
    info!("✅ 完整回放系统演示完成");
    Ok(())
}

/// 演示实时推理与回放结合
async fn demo_realtime_inference_with_replay(chunk: &DataChunk) -> Result<(), Box<dyn std::error::Error>> {
    // 创建简化的推理引擎
    let inference_config = InferenceConfig {
        max_concurrent_inferences: 2,
        inference_timeout_ms: 100,
        enable_batching: false,
        enable_gpu: false,
        ..Default::default()
    };
    
    let inference_engine = TorchScriptInferenceEngine::new(inference_config).await?;
    
    // 模拟实时处理每个LOB事件
    for (i, event) in chunk.events.iter().enumerate() {
        if let ReplayEvent::LobUpdate { timestamp_us, symbol, bids, asks, .. } = event {
            // 提取特征
            let features = extract_lob_features(bids, asks);
            
            // 创建推理请求
            let request = InferenceRequest {
                request_id: format!("realtime_{}", i),
                model_id: "lob_predictor".to_string(),
                inputs: vec![
                    InferenceInput {
                        name: "lob_features".to_string(),
                        data: InputData::Float32(features),
                        shape: vec![1, 8], // bid/ask价格和数量特征
                    }
                ],
                options: InferenceOptions {
                    force_gpu: false,
                    enable_batching: false,
                    timeout_ms: 50,
                    priority: InferencePriority::High,
                },
                created_at: std::time::Instant::now(),
            };
            
            // 执行超低延迟推理
            let inference_start = std::time::Instant::now();
            match inference_engine.infer(request).await {
                Ok(result) => {
                    let inference_time_us = inference_start.elapsed().as_micros() as u64;
                    if inference_time_us < 100 {
                        debug!("⚡ {} - 推理成功 {}μs", symbol, inference_time_us);
                    } else {
                        warn!("⚠️ {} - 推理耗时 {}μs (超过100μs)", symbol, inference_time_us);
                    }
                }
                Err(_) => {
                    // 预期的错误，因为没有实际模型
                }
            }
            
            // 模拟实时处理间隔
            sleep(Duration::from_millis(10)).await;
        }
    }
    
    // 获取最终统计
    let stats = inference_engine.get_stats().await;
    info!("实时推理统计: 总推理={}, 平均时间={:.2}μs", 
          stats.total_inferences, stats.average_inference_time_us);
    
    Ok(())
}

/// 从LOB数据提取特征
fn extract_lob_features(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> Vec<f32> {
    let mut features = Vec::with_capacity(8);
    
    // 最佳买卖价和数量
    if let (Some(&(bid_price, bid_qty)), Some(&(ask_price, ask_qty))) = (bids.first(), asks.first()) {
        features.extend([
            bid_price as f32,
            bid_qty as f32,
            ask_price as f32,
            ask_qty as f32,
        ]);
        
        // 价差和中间价
        let spread = ask_price - bid_price;
        let mid_price = (bid_price + ask_price) / 2.0;
        features.extend([
            spread as f32,
            mid_price as f32,
        ]);
        
        // 买卖盘不平衡
        let imbalance = bid_qty / (bid_qty + ask_qty);
        features.push(imbalance as f32);
        
        // 深度特征 (总量)
        let total_bid_qty: f64 = bids.iter().map(|(_, qty)| qty).sum();
        let total_ask_qty: f64 = asks.iter().map(|(_, qty)| qty).sum();
        let depth_ratio = total_bid_qty / (total_bid_qty + total_ask_qty);
        features.push(depth_ratio as f32);
    } else {
        // 如果没有数据，填充默认值
        features.extend([0.0; 8]);
    }
    
    features
}

/// 创建演示数据块
fn create_demo_data_chunk(config: &ReplayConfig) -> DataChunk {
    let time_range = TimeRange::new(
        config.start_timestamp_us,
        config.start_timestamp_us + 60_000_000 // +1分钟
    );
    
    let mut chunk = DataChunk::new(time_range);
    
    // 添加一些模拟的LOB更新事件
    for i in 0..10 {
        let timestamp = config.start_timestamp_us + (i * 6_000_000); // 每6秒一个事件
        
        let event = ReplayEvent::LobUpdate {
            timestamp_us: timestamp,
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                (50000.0 - i as f64, 1.0 + i as f64 * 0.1),
                (49999.0 - i as f64, 2.0 + i as f64 * 0.1),
            ],
            asks: vec![
                (50001.0 + i as f64, 1.5 + i as f64 * 0.1),
                (50002.0 + i as f64, 2.5 + i as f64 * 0.1),
            ],
            sequence: i as u64 + 1,
        };
        
        chunk.add_event(event);
    }
    
    // 添加一些交易事件
    for i in 0..5 {
        let timestamp = config.start_timestamp_us + (i * 12_000_000) + 3_000_000; // 偏移3秒
        
        let event = ReplayEvent::Trade {
            timestamp_us: timestamp,
            symbol: "BTCUSDT".to_string(),
            price: 50000.5 + i as f64 * 0.1,
            quantity: 0.1 + i as f64 * 0.01,
            side: if i % 2 == 0 { "buy" } else { "sell" }.to_string(),
            trade_id: format!("trade_{:06}", i + 1),
        };
        
        chunk.add_event(event);
    }
    
    // 确保事件按时间戳排序
    chunk.sort_by_timestamp();
    
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_demo_config_creation() {
        let config = create_demo_replay_config().await.unwrap();
        assert!(!config.symbols.is_empty());
        assert!(config.start_timestamp_us < config.end_timestamp_us);
    }
    
    #[tokio::test]
    async fn test_time_manager_basic_operations() {
        let config = create_demo_replay_config().await.unwrap();
        let time_manager = TimeTravelManager::new(config.clone());
        
        // 测试初始化
        let result = time_manager.initialize(config.start_timestamp_us, 1.0).await;
        assert!(result.is_ok());
        
        // 测试状态获取
        let status = time_manager.get_status().await;
        assert_eq!(status, TimeStatus::Paused);
        
        // 测试时间获取
        let current_time = time_manager.current_time().await;
        assert_eq!(current_time, config.start_timestamp_us);
    }
    
    #[test]
    fn test_mock_inference_request() {
        let request = create_mock_inference_request();
        assert_eq!(request.model_id, "demo_model");
        assert_eq!(request.inputs.len(), 1);
        assert_eq!(request.inputs[0].shape, vec![1, 5]);
    }
    
    #[test]
    fn test_lob_feature_extraction() {
        let bids = vec![(50000.0, 1.0), (49999.0, 2.0)];
        let asks = vec![(50001.0, 1.5), (50002.0, 2.5)];
        
        let features = extract_lob_features(&bids, &asks);
        assert_eq!(features.len(), 8);
        
        // 验证特征值
        assert_eq!(features[0], 50000.0); // best bid price
        assert_eq!(features[1], 1.0);     // best bid quantity
        assert_eq!(features[2], 50001.0); // best ask price
        assert_eq!(features[3], 1.5);     // best ask quantity
        assert_eq!(features[4], 1.0);     // spread
        assert_eq!(features[5], 50000.5); // mid price
    }
    
    #[test]
    fn test_demo_data_chunk_creation() {
        let config_future = create_demo_replay_config();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let config = rt.block_on(config_future).unwrap();
        
        let chunk = create_demo_data_chunk(&config);
        assert!(!chunk.is_empty());
        assert_eq!(chunk.event_count(), 15); // 10 LOB + 5 Trade events
        
        // 验证事件时间戳顺序
        let mut last_timestamp = 0u64;
        for event in &chunk.events {
            let timestamp = match event {
                ReplayEvent::LobUpdate { timestamp_us, .. } => *timestamp_us,
                ReplayEvent::Trade { timestamp_us, .. } => *timestamp_us,
                _ => 0,
            };
            
            if last_timestamp > 0 {
                assert!(timestamp >= last_timestamp, "事件时间戳应该按升序排列");
            }
            last_timestamp = timestamp;
        }
    }
    
    #[tokio::test]
    async fn test_batch_inference_requests() {
        let requests = create_batch_inference_requests(5);
        assert_eq!(requests.len(), 5);
        
        for (i, request) in requests.iter().enumerate() {
            assert_eq!(request.request_id, format!("batch_request_{:03}", i));
            assert_eq!(request.inputs[0].shape, vec![1, 10]);
        }
    }
}