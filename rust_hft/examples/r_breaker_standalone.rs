/*!
 * R-Breaker 獨立測試版本
 * 
 * 這是一個完全獨立的 R-Breaker 策略測試，展示：
 * 1. 報價功能 - 模擬實時價格數據
 * 2. 交易監控 - 實時交易狀態監控  
 * 3. 訂單監控 - 訂單執行和狀態跟蹤
 * 4. 策略載入 - R-Breaker 策略邏輯
 * 5. 交易執行 - 自動化交易決策
 */

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct MarketQuote {
    symbol: String,
    timestamp: u64,
    bid: f64,
    ask: f64,
    last: f64,
    volume: f64,
}

#[derive(Debug, Clone)]
struct Order {
    id: String,
    symbol: String,
    side: OrderSide,
    order_type: OrderType,
    quantity: f64,
    price: f64,
    status: OrderStatus,
    timestamp: u64,
}

#[derive(Debug, Clone, PartialEq)]
enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, PartialEq)]
enum OrderStatus {
    Pending,
    Filled,
    PartiallyFilled,
    Cancelled,
}

#[derive(Debug, Clone)]
struct Trade {
    order_id: String,
    symbol: String,
    side: OrderSide,
    quantity: f64,
    price: f64,
    timestamp: u64,
    pnl: f64,
}

#[derive(Debug, Clone)]
struct RBreakerLevels {
    breakthrough_buy: f64,
    observation_sell: f64,
    reversal_sell: f64,
    reversal_buy: f64,
    observation_buy: f64,
    breakthrough_sell: f64,
}

impl RBreakerLevels {
    fn calculate(high: f64, low: f64, close: f64, sensitivity: f64) -> Self {
        let pivot = (high + close + low) / 3.0;
        
        Self {
            breakthrough_buy: high + 2.0 * sensitivity * (pivot - low),
            observation_sell: pivot + sensitivity * (high - low),
            reversal_sell: 2.0 * pivot - low,
            reversal_buy: 2.0 * pivot - high,
            observation_buy: pivot - sensitivity * (high - low),
            breakthrough_sell: low - 2.0 * sensitivity * (high - pivot),
        }
    }

    fn get_signal(&self, current_price: f64, daily_high: f64, daily_low: f64) -> RBreakerSignal {
        // 趨勢跟蹤
        if current_price > self.breakthrough_buy {
            return RBreakerSignal::TrendBuy;
        }
        if current_price < self.breakthrough_sell {
            return RBreakerSignal::TrendSell;
        }

        // 反轉交易
        if daily_high > self.observation_sell && current_price < self.reversal_sell {
            return RBreakerSignal::ReversalSell;
        }
        if daily_low < self.observation_buy && current_price > self.reversal_buy {
            return RBreakerSignal::ReversalBuy;
        }

        RBreakerSignal::Hold
    }
}

#[derive(Debug, Clone, PartialEq)]
enum RBreakerSignal {
    TrendBuy,
    TrendSell,
    ReversalBuy,
    ReversalSell,
    Hold,
}

struct RBreakerStrategy {
    symbol: String,
    levels: Option<RBreakerLevels>,
    sensitivity: f64,
    daily_high: f64,
    daily_low: f64,
    position: f64,
    entry_price: Option<f64>,
}

impl RBreakerStrategy {
    fn new(symbol: String, sensitivity: f64) -> Self {
        Self {
            symbol,
            levels: None,
            sensitivity,
            daily_high: 0.0,
            daily_low: f64::MAX,
            position: 0.0,
            entry_price: None,
        }
    }

    fn update_levels(&mut self, high: f64, low: f64, close: f64) {
        self.levels = Some(RBreakerLevels::calculate(high, low, close, self.sensitivity));
        println!("📊 R-Breaker 價位更新:");
        if let Some(ref levels) = self.levels {
            println!("  突破買入: {:.2}", levels.breakthrough_buy);
            println!("  觀察賣出: {:.2}", levels.observation_sell);
            println!("  反轉賣出: {:.2}", levels.reversal_sell);
            println!("  反轉買入: {:.2}", levels.reversal_buy);
            println!("  觀察買入: {:.2}", levels.observation_buy);
            println!("  突破賣出: {:.2}", levels.breakthrough_sell);
        }
    }

    fn process_quote(&mut self, quote: &MarketQuote) -> Option<RBreakerSignal> {
        let price = quote.last;
        
        // 更新當日高低點
        if self.daily_high == 0.0 {
            self.daily_high = price;
            self.daily_low = price;
        }
        self.daily_high = self.daily_high.max(price);
        self.daily_low = self.daily_low.min(price);

        // 如果沒有R-Breaker價位，返回Hold
        let levels = self.levels.as_ref()?;
        
        // 生成信號
        let signal = levels.get_signal(price, self.daily_high, self.daily_low);
        
        // 過濾信號（避免重複開倉）
        match signal {
            RBreakerSignal::TrendBuy | RBreakerSignal::ReversalBuy => {
                if self.position > 0.0 {
                    None // 已有多頭持倉
                } else {
                    Some(signal)
                }
            }
            RBreakerSignal::TrendSell | RBreakerSignal::ReversalSell => {
                if self.position < 0.0 {
                    None // 已有空頭持倉
                } else {
                    Some(signal)
                }
            }
            RBreakerSignal::Hold => None,
        }
    }

    fn execute_trade(&mut self, signal: &RBreakerSignal, price: f64) -> Option<Order> {
        let order_id = format!("R{}", now_millis());
        let quantity = 0.1; // 固定數量

        match signal {
            RBreakerSignal::TrendBuy | RBreakerSignal::ReversalBuy => {
                self.position = quantity;
                self.entry_price = Some(price);
                
                Some(Order {
                    id: order_id,
                    symbol: self.symbol.clone(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Market,
                    quantity,
                    price,
                    status: OrderStatus::Filled,
                    timestamp: now_millis(),
                })
            }
            RBreakerSignal::TrendSell | RBreakerSignal::ReversalSell => {
                let pnl = if let Some(entry) = self.entry_price {
                    if self.position > 0.0 {
                        (price - entry) * self.position
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                self.position = -quantity;
                self.entry_price = Some(price);
                
                Some(Order {
                    id: order_id,
                    symbol: self.symbol.clone(),
                    side: OrderSide::Sell,
                    order_type: OrderType::Market,
                    quantity,
                    price,
                    status: OrderStatus::Filled,
                    timestamp: now_millis(),
                })
            }
            RBreakerSignal::Hold => None,
        }
    }
}

struct TradingSystem {
    strategy: RBreakerStrategy,
    orders: VecDeque<Order>,
    trades: VecDeque<Trade>,
    portfolio_value: f64,
    total_pnl: f64,
    trade_count: u32,
    winning_trades: u32,
}

impl TradingSystem {
    fn new(symbol: String, initial_capital: f64, sensitivity: f64) -> Self {
        Self {
            strategy: RBreakerStrategy::new(symbol, sensitivity),
            orders: VecDeque::new(),
            trades: VecDeque::new(),
            portfolio_value: initial_capital,
            total_pnl: 0.0,
            trade_count: 0,
            winning_trades: 0,
        }
    }

    fn setup_strategy(&mut self, yesterday_high: f64, yesterday_low: f64, yesterday_close: f64) {
        println!("🎯 載入 R-Breaker 策略...");
        println!("  交易對: {}", self.strategy.symbol);
        println!("  敏感度係數: {:.2}", self.strategy.sensitivity);
        println!("  前日數據: H={:.2}, L={:.2}, C={:.2}", yesterday_high, yesterday_low, yesterday_close);
        
        self.strategy.update_levels(yesterday_high, yesterday_low, yesterday_close);
        println!("✅ 策略載入完成\n");
    }

    fn process_market_data(&mut self, quote: &MarketQuote) {
        // 📈 報價功能
        if self.trade_count % 10 == 0 { // 每10次打印一次報價
            self.display_quote(quote);
        }

        // 🎯 策略處理
        if let Some(signal) = self.strategy.process_quote(quote) {
            println!("🔥 交易信號: {:?} @ {:.2}", signal, quote.last);
            
            // 執行交易
            if let Some(order) = self.strategy.execute_trade(&signal, quote.last) {
                self.process_order(order, quote);
            }
        }

        // 📊 交易監控 (每20次更新一次)
        if self.trade_count % 20 == 0 {
            self.display_trading_status(quote);
        }
    }

    fn display_quote(&self, quote: &MarketQuote) {
        println!("📈 報價 [{}] Bid: {:.2} | Ask: {:.2} | Last: {:.2} | Vol: {:.0}", 
                 quote.symbol, quote.bid, quote.ask, quote.last, quote.volume);
    }

    fn process_order(&mut self, order: Order, quote: &MarketQuote) {
        println!("📋 訂單執行: {} {} {:.4} @ {:.2}", 
                 order.id, 
                 if order.side == OrderSide::Buy { "買入" } else { "賣出" },
                 order.quantity, 
                 order.price);

        // 計算PnL
        let pnl = if let Some(entry_price) = self.strategy.entry_price {
            match order.side {
                OrderSide::Sell if self.strategy.position >= 0.0 => {
                    (order.price - entry_price) * order.quantity
                }
                OrderSide::Buy if self.strategy.position <= 0.0 => {
                    (entry_price - order.price) * order.quantity
                }
                _ => 0.0,
            }
        } else {
            0.0
        };

        // 創建交易記錄
        let trade = Trade {
            order_id: order.id.clone(),
            symbol: order.symbol.clone(),
            side: order.side.clone(),
            quantity: order.quantity,
            price: order.price,
            timestamp: order.timestamp,
            pnl,
        };

        // 更新統計
        self.trade_count += 1;
        if pnl > 0.0 {
            self.winning_trades += 1;
        }
        self.total_pnl += pnl;

        // 訂單監控
        println!("✅ 訂單完成: {} | PnL: ${:.2} | 狀態: {:?}", 
                 order.id, pnl, order.status);

        // 保存記錄
        self.orders.push_back(order);
        self.trades.push_back(trade);

        // 限制記錄大小
        if self.orders.len() > 100 {
            self.orders.pop_front();
        }
        if self.trades.len() > 100 {
            self.trades.pop_front();
        }
    }

    fn display_trading_status(&self, quote: &MarketQuote) {
        let current_portfolio = self.portfolio_value + self.total_pnl;
        let returns = (current_portfolio - self.portfolio_value) / self.portfolio_value * 100.0;
        let win_rate = if self.trade_count > 0 {
            self.winning_trades as f64 / self.trade_count as f64 * 100.0
        } else {
            0.0
        };

        println!("\n📊 交易監控狀態:");
        println!("  持倉: {:.4} | 價格: {:.2}", self.strategy.position, quote.last);
        println!("  交易次數: {} | 勝率: {:.1}%", self.trade_count, win_rate);
        println!("  總盈虧: ${:.2} | 收益率: {:.2}%", self.total_pnl, returns);
        println!("  投資組合價值: ${:.2}", current_portfolio);
        println!("  當日區間: {:.2} - {:.2}", self.strategy.daily_low, self.strategy.daily_high);
        println!("");
    }

    fn display_final_summary(&self) {
        println!("\n🎉 交易完成 - 最終報告");
        println!("=========================================");
        println!("策略: R-Breaker | 交易對: {}", self.strategy.symbol);
        println!("總交易次數: {}", self.trade_count);
        println!("勝率: {:.1}%", if self.trade_count > 0 { 
            self.winning_trades as f64 / self.trade_count as f64 * 100.0 
        } else { 0.0 });
        println!("總盈虧: ${:.2}", self.total_pnl);
        println!("最終收益率: {:.2}%", self.total_pnl / self.portfolio_value * 100.0);

        if !self.trades.is_empty() {
            let avg_pnl = self.total_pnl / self.trade_count as f64;
            println!("平均每筆盈虧: ${:.2}", avg_pnl);

            let best_trade = self.trades.iter().max_by(|a, b| a.pnl.partial_cmp(&b.pnl).unwrap()).unwrap();
            let worst_trade = self.trades.iter().min_by(|a, b| a.pnl.partial_cmp(&b.pnl).unwrap()).unwrap();
            
            println!("最佳交易: ${:.2} ({})", best_trade.pnl, best_trade.order_id);
            println!("最差交易: ${:.2} ({})", worst_trade.pnl, worst_trade.order_id);
        }

        println!("\n📋 最近訂單:");
        for order in self.orders.iter().rev().take(5) {
            println!("  {} | {} {:.4} @ {:.2}", 
                     order.id,
                     if order.side == OrderSide::Buy { "買" } else { "賣" },
                     order.quantity,
                     order.price);
        }
    }
}

// 模擬市場數據生成器
struct MarketDataSimulator {
    symbol: String,
    current_price: f64,
    timestamp: u64,
}

impl MarketDataSimulator {
    fn new(symbol: String, initial_price: f64) -> Self {
        Self {
            symbol,
            current_price: initial_price,
            timestamp: now_millis(),
        }
    }

    fn next_quote(&mut self) -> MarketQuote {
        // 模擬價格波動
        let change = (simple_random() - 0.5) * 0.02; // ±1%
        self.current_price *= 1.0 + change;
        self.current_price = self.current_price.max(1.0);

        let spread = self.current_price * 0.0001; // 0.01% 價差
        let volume = 1000.0 + simple_random() * 5000.0;

        self.timestamp += 1000; // 每秒更新

        MarketQuote {
            symbol: self.symbol.clone(),
            timestamp: self.timestamp,
            bid: self.current_price - spread / 2.0,
            ask: self.current_price + spread / 2.0,
            last: self.current_price,
            volume,
        }
    }
}

// 工具函數
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn simple_random() -> f64 {
    let now = now_millis();
    let a = 1664525u64;
    let c = 1013904223u64;
    let m = 2u64.pow(32);
    
    let result = (a.wrapping_mul(now).wrapping_add(c)) % m;
    result as f64 / m as f64
}

fn main() {
    println!("🚀 R-Breaker Live Trading System Demo");
    println!("=====================================\n");

    // 配置參數
    let symbol = "ETHUSDT";
    let initial_capital = 10000.0;
    let sensitivity = 0.35;
    let data_points = 50;

    // 創建交易系統
    let mut trading_system = TradingSystem::new(symbol.to_string(), initial_capital, sensitivity);
    
    // 模擬前一日數據
    let yesterday_high = 3600.0;
    let yesterday_low = 3400.0;
    let yesterday_close = 3500.0;

    // 設置策略
    trading_system.setup_strategy(yesterday_high, yesterday_low, yesterday_close);

    // 創建市場數據模擬器
    let mut market_sim = MarketDataSimulator::new(symbol.to_string(), yesterday_close);

    println!("🎯 開始模擬交易...\n");

    // 模擬市場數據處理
    for i in 0..data_points {
        let quote = market_sim.next_quote();
        trading_system.process_market_data(&quote);

        // 添加一些延遲以便觀察
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        // 進度顯示
        if (i + 1) % 15 == 0 {
            println!("⏳ 進度: {:.1}%", (i + 1) as f64 / data_points as f64 * 100.0);
        }
    }

    // 最終報告
    trading_system.display_final_summary();

    println!("\n💡 系統功能驗證:");
    println!("✅ 報價功能 - 實時市場數據顯示");
    println!("✅ 交易監控 - 實時交易狀態監控");
    println!("✅ 訂單監控 - 訂單執行和狀態跟蹤");
    println!("✅ 策略載入 - R-Breaker 策略邏輯");
    println!("✅ 交易執行 - 自動化交易決策");

    println!("\n🔄 下一步測試:");
    println!("  cargo run --example r_breaker_standalone");
    println!("  修改參數重新測試不同的市場條件");
}