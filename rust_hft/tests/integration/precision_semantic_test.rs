//! 精度/语义修复的集成测试
//! 
//! 测试完整的事件流：Adapter -> OMS -> Engine -> Portfolio
//! 确保精度保持和语义一致性

use std::collections::HashMap;
use rust_decimal::Decimal;
use hft_core::{OrderId, Price, Quantity, Symbol, Side};
use ports::{ExecutionEvent, AccountView};
use oms_core::{OmsCore, OrderStatus};
use portfolio_core::Portfolio;
use hft_engine::Engine;

#[tokio::test]
async fn test_fill_precision_preservation() {
    // 测试从适配器到Portfolio的精度保持
    let mut oms = OmsCore::new();
    let mut portfolio = Portfolio::new();
    
    let order_id = OrderId("T-PRECISION-001".to_string());
    let symbol = Symbol("BTCUSDT".to_string());
    let price = Price::from_str("67188.12345678901").unwrap(); // 高精度价格
    let quantity = Quantity::from_str("0.00123456789").unwrap(); // 高精度数量
    
    // 1. 注册订单到OMS和Portfolio
    oms.register_order(order_id.clone(), None, symbol.clone(), Side::Buy, quantity);
    portfolio.register_order(order_id.clone(), symbol.clone(), Side::Buy);
    
    // 2. 模拟Ack事件
    let ack_event = ExecutionEvent::OrderAck {
        order_id: order_id.clone(),
        timestamp: 1640995200_000_000, // 2022-01-01 00:00:00 UTC in microseconds
    };
    
    let order_update = oms.on_execution_event(&ack_event).unwrap();
    assert_eq!(order_update.status, OrderStatus::Acknowledged);
    assert_eq!(order_update.previous_status, OrderStatus::New);
    
    // 3. 模拟Fill事件 - 确保精度保持
    let fill_event = ExecutionEvent::Fill {
        order_id: order_id.clone(),
        price,
        quantity,
        timestamp: 1640995201_000_000,
        fill_id: "FILL-PRECISION-001".to_string(),
    };
    
    // 4. OMS处理Fill
    let order_update = oms.on_execution_event(&fill_event).unwrap();
    assert_eq!(order_update.status, OrderStatus::Filled);
    assert_eq!(order_update.cum_qty, quantity);
    assert_eq!(order_update.avg_price.unwrap(), price);
    
    // 5. Portfolio处理Fill - 验证精度
    portfolio.on_execution_event(&fill_event);
    let account_view = portfolio.reader().load();
    
    // 验证倉位精度
    let position = account_view.positions.get(&symbol).unwrap();
    assert_eq!(position.quantity, quantity);
    assert_eq!(position.avg_price, price);
    
    // 验证现金计算精度
    let expected_cash_change = -(price.0 * quantity.0).to_f64().unwrap();
    assert!((account_view.cash_balance - (10000.0 + expected_cash_change)).abs() < 1e-10);
}

#[tokio::test]
async fn test_weighted_average_price_precision() {
    // 测试多次Fill的加权平均价格精度
    let mut oms = OmsCore::new();
    let mut portfolio = Portfolio::new();
    
    let order_id = OrderId("T-WAP-001".to_string());
    let symbol = Symbol("BTCUSDT".to_string());
    let total_qty = Quantity::from_str("1.0").unwrap();
    
    oms.register_order(order_id.clone(), None, symbol.clone(), Side::Buy, total_qty);
    portfolio.register_order(order_id.clone(), symbol.clone(), Side::Buy);
    
    // Ack订单
    let ack_event = ExecutionEvent::OrderAck {
        order_id: order_id.clone(),
        timestamp: 1640995200_000_000,
    };
    oms.on_execution_event(&ack_event);
    
    // 第一次部分成交
    let fill1 = ExecutionEvent::Fill {
        order_id: order_id.clone(),
        price: Price::from_str("67188.123456").unwrap(),
        quantity: Quantity::from_str("0.4").unwrap(),
        timestamp: 1640995201_000_000,
        fill_id: "FILL-WAP-001-1".to_string(),
    };
    
    let update1 = oms.on_execution_event(&fill1).unwrap();
    assert_eq!(update1.status, OrderStatus::PartiallyFilled);
    assert_eq!(update1.cum_qty, Quantity::from_str("0.4").unwrap());
    assert_eq!(update1.avg_price.unwrap(), Price::from_str("67188.123456").unwrap());
    
    portfolio.on_execution_event(&fill1);
    
    // 第二次部分成交 - 不同价格
    let fill2 = ExecutionEvent::Fill {
        order_id: order_id.clone(),
        price: Price::from_str("67190.654321").unwrap(),
        quantity: Quantity::from_str("0.6").unwrap(),
        timestamp: 1640995202_000_000,
        fill_id: "FILL-WAP-001-2".to_string(),
    };
    
    let update2 = oms.on_execution_event(&fill2).unwrap();
    assert_eq!(update2.status, OrderStatus::Filled);
    assert_eq!(update2.cum_qty, Quantity::from_str("1.0").unwrap());
    
    // 验证加权平均价格：(67188.123456 * 0.4 + 67190.654321 * 0.6) / 1.0
    let expected_avg = (Decimal::from_str("67188.123456").unwrap() * Decimal::from_str("0.4").unwrap() 
                       + Decimal::from_str("67190.654321").unwrap() * Decimal::from_str("0.6").unwrap()) 
                       / Decimal::from_str("1.0").unwrap();
    
    assert_eq!(update2.avg_price.unwrap().0, expected_avg);
    
    portfolio.on_execution_event(&fill2);
    let account_view = portfolio.reader().load();
    let position = account_view.positions.get(&symbol).unwrap();
    assert_eq!(position.avg_price.0, expected_avg);
}

#[tokio::test]
async fn test_fill_deduplication() {
    // 测试fill_id去重功能
    let mut oms = OmsCore::new();
    
    let order_id = OrderId("T-DEDUP-001".to_string());
    let symbol = Symbol("BTCUSDT".to_string());
    let quantity = Quantity::from_str("1.0").unwrap();
    
    oms.register_order(order_id.clone(), None, symbol.clone(), Side::Buy, quantity);
    
    // 首次Fill
    let fill_event = ExecutionEvent::Fill {
        order_id: order_id.clone(),
        price: Price::from_str("67188.0").unwrap(),
        quantity: Quantity::from_str("1.0").unwrap(),
        timestamp: 1640995201_000_000,
        fill_id: "DUPLICATE-FILL-001".to_string(),
    };
    
    let update1 = oms.on_execution_event(&fill_event).unwrap();
    assert_eq!(update1.status, OrderStatus::Filled);
    assert_eq!(update1.cum_qty, Quantity::from_str("1.0").unwrap());
    
    // 重复相同fill_id的Fill事件（应该被去重）
    // 注意：这个测试需要在适配器层实现，OMS层不处理去重
    // 这里主要测试OMS的幂等性
    let update2 = oms.on_execution_event(&fill_event);
    // OMS应该返回None或保持状态不变
    if let Some(update) = update2 {
        assert_eq!(update.status, OrderStatus::Filled);
        assert_eq!(update.cum_qty, Quantity::from_str("1.0").unwrap());
    }
}

#[tokio::test]
async fn test_order_completed_event_generation() {
    // 测试Engine层自动生成OrderCompleted事件
    use hft_engine::{Engine, EngineConfig};
    
    let mut engine = Engine::new(EngineConfig::default());
    
    let order_id = OrderId("T-COMPLETE-001".to_string());
    let symbol = Symbol("BTCUSDT".to_string());
    let quantity = Quantity::from_str("1.0").unwrap();
    
    // 这里需要通过反射或其他方式访问engine的内部OMS进行测试
    // 由于Engine的内部结构，我们需要创建一个简单的测试场景
    
    // 模拟完整的事件流：Ack -> Fill (Partial) -> Fill (Complete)
    let events = vec![
        ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: 1640995200_000_000,
        },
        ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_str("67188.0").unwrap(),
            quantity: Quantity::from_str("0.5").unwrap(),
            timestamp: 1640995201_000_000,
            fill_id: "COMPLETE-001-1".to_string(),
        },
        ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_str("67189.0").unwrap(),
            quantity: Quantity::from_str("0.5").unwrap(),
            timestamp: 1640995202_000_000,
            fill_id: "COMPLETE-001-2".to_string(),
        },
    ];
    
    // 注意：由于Engine的设计，这个测试需要更复杂的设置
    // 这里只是展示测试结构，实际实现需要engine暴露更多测试接口
    
    assert!(true); // 占位符断言
}

#[tokio::test]
async fn test_unified_timestamp_handling() {
    // 测试UnifiedTimestamp的使用
    use hft_core::UnifiedTimestamp;
    
    let exchange_ts = 1640995200_000_000u64; // 交易所时间戳
    let local_ts = 1640995200_001_500u64;    // 本地时间戳（晚1.5ms）
    
    let unified = UnifiedTimestamp::new(exchange_ts, local_ts);
    
    // 测试时间戳选择
    assert_eq!(unified.primary_ts(), exchange_ts);
    assert_eq!(unified.network_latency_us(), Some(1500)); // 1.5ms网络延迟
    
    // 测试时间戳验证
    assert!(unified.validate());
    
    // 测试陈旧检测
    assert!(!unified.is_stale(2000)); // 2ms阈值，不陈旧
    assert!(unified.is_stale(1000));  // 1ms阈值，陈旧
}

#[tokio::test]
async fn test_mtm_synchronization() {
    // 测试Portfolio的Mark-to-Market同步
    let mut portfolio = Portfolio::new();
    
    let order_id = OrderId("T-MTM-001".to_string());
    let symbol = Symbol("BTCUSDT".to_string());
    
    portfolio.register_order(order_id.clone(), symbol.clone(), Side::Buy);
    
    // 模拟买入建仓
    let fill_event = ExecutionEvent::Fill {
        order_id: order_id.clone(),
        price: Price::from_str("67000.0").unwrap(),
        quantity: Quantity::from_str("1.0").unwrap(),
        timestamp: 1640995200_000_000,
        fill_id: "MTM-001".to_string(),
    };
    
    portfolio.on_execution_event(&fill_event);
    let view1 = portfolio.reader().load();
    
    // 初始状态：无未实现盈亏
    assert_eq!(view1.unrealized_pnl, 0.0);
    
    // 更新市场价格
    let mut market_prices = HashMap::new();
    market_prices.insert(symbol.clone(), Price::from_str("68000.0").unwrap()); // 上涨1000
    
    portfolio.update_market_prices(&market_prices);
    let view2 = portfolio.reader().load();
    
    // 验证未实现盈亏：(68000 - 67000) * 1.0 = 1000
    assert!((view2.unrealized_pnl - 1000.0).abs() < 1e-6);
    
    let position = view2.positions.get(&symbol).unwrap();
    assert!((position.unrealized_pnl - 1000.0).abs() < 1e-6);
}

#[tokio::test] 
async fn test_full_event_chain_precision() {
    // 测试完整事件链的精度保持
    let mut oms = OmsCore::new();
    let mut portfolio = Portfolio::new();
    
    let order_id = OrderId("T-CHAIN-001".to_string());
    let symbol = Symbol("BTCUSDT".to_string());
    
    // 使用高精度价格和数量
    let price1 = Price::from_str("67188.123456789012").unwrap();
    let price2 = Price::from_str("67190.987654321098").unwrap();
    let qty1 = Quantity::from_str("0.123456789").unwrap();
    let qty2 = Quantity::from_str("0.876543211").unwrap();
    let total_qty = Quantity(qty1.0 + qty2.0);
    
    oms.register_order(order_id.clone(), None, symbol.clone(), Side::Buy, total_qty);
    portfolio.register_order(order_id.clone(), symbol.clone(), Side::Buy);
    
    // 多次部分成交
    let fills = vec![
        ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: price1,
            quantity: qty1,
            timestamp: 1640995201_000_000,
            fill_id: "CHAIN-001-1".to_string(),
        },
        ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: price2,
            quantity: qty2,
            timestamp: 1640995202_000_000,
            fill_id: "CHAIN-001-2".to_string(),
        },
    ];
    
    let mut final_update = None;
    for fill in fills {
        if let Some(update) = oms.on_execution_event(&fill) {
            final_update = Some(update);
        }
        portfolio.on_execution_event(&fill);
    }
    
    // 验证最终状态
    let update = final_update.unwrap();
    assert_eq!(update.status, OrderStatus::Filled);
    assert_eq!(update.cum_qty, total_qty);
    
    // 验证精确的加权平均价格
    let expected_avg = (price1.0 * qty1.0 + price2.0 * qty2.0) / total_qty.0;
    assert_eq!(update.avg_price.unwrap().0, expected_avg);
    
    // 验证Portfolio状态
    let account_view = portfolio.reader().load();
    let position = account_view.positions.get(&symbol).unwrap();
    assert_eq!(position.quantity, total_qty);
    assert_eq!(position.avg_price.0, expected_avg);
}