#![cfg(feature = "legacy-sdk")]

/*!
 * Lock-Free OrderBook 內存安全測試
 */

use rust_hft::core::lockfree_orderbook::LockFreeOrderBook;
use rust_hft::types::{Price, Quantity, Side};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_lockfree_orderbook_memory_safety() {
    let ob = Arc::new(LockFreeOrderBook::new("BTCUSDT".to_string()));

    // 測試併發讀寫是否安全
    let mut handles = vec![];

    // 啟動多個寫入線程
    for i in 0..10 {
        let ob_clone = ob.clone();
        let handle = thread::spawn(move || {
            for j in 0..100 {
                let price = Price::from(100.0 + i as f64 + j as f64 * 0.01);
                let quantity = Quantity::try_from(10.0).unwrap();
                let _ = ob_clone.update_level(Side::Bid, price, quantity);
            }
        });
        handles.push(handle);
    }

    // 啟動多個讀取線程
    for _i in 0..5 {
        let ob_clone = ob.clone();
        let handle = thread::spawn(move || {
            for _j in 0..1000 {
                let _ = ob_clone.best_bid();
                let _ = ob_clone.best_ask();
                let _ = ob_clone.mid_price();
                thread::sleep(Duration::from_nanos(100));
            }
        });
        handles.push(handle);
    }

    // 等待所有線程完成
    for handle in handles {
        handle.join().unwrap();
    }

    // 驗證最終狀態
    let stats = ob.stats();
    println!("Final stats: bid_levels={}, ask_levels={}, updates={}",
             stats.bid_levels, stats.ask_levels, stats.update_count);

    assert!(stats.update_count > 0);
}

#[test]
fn test_lockfree_best_price_cache_consistency() {
    let ob = LockFreeOrderBook::new("TESTPAIR".to_string());

    // 添加一些價格級別
    let _ = ob.update_level(Side::Bid, Price::from(100.0), Quantity::try_from(10.0).unwrap());
    let _ = ob.update_level(Side::Bid, Price::from(99.0), Quantity::try_from(20.0).unwrap());
    let _ = ob.update_level(Side::Ask, Price::from(101.0), Quantity::try_from(15.0).unwrap());

    // 測試最佳價格
    assert_eq!(ob.best_bid(), Some(Price::from(100.0)));
    assert_eq!(ob.best_ask(), Some(Price::from(101.0)));

    // 更新價格級別後緩存應該失效
    let _ = ob.update_level(Side::Bid, Price::from(102.0), Quantity::try_from(5.0).unwrap());
    assert_eq!(ob.best_bid(), Some(Price::from(102.0)));

    // 移除價格級別
    let _ = ob.update_level(Side::Bid, Price::from(102.0), Quantity::try_from(0.0).unwrap());
    assert_eq!(ob.best_bid(), Some(Price::from(100.0)));
}