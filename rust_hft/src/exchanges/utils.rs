//! 交易所工具函數
//! 
//! 提供交易所相關的通用工具函數

/// 標準化交易對名稱
pub fn normalize_symbol(symbol: &str) -> String {
    symbol.to_uppercase().replace("-", "").replace("_", "")
}

/// 反標準化交易對名稱（轉為特定交易所格式）
pub fn denormalize_symbol(symbol: &str, exchange: &str) -> String {
    match exchange.to_lowercase().as_str() {
        "bitget" => symbol.to_uppercase(),
        "binance" => symbol.to_uppercase(),
        "okx" => symbol.to_uppercase().replace("USDT", "-USDT"),
        "bybit" => symbol.to_uppercase(),
        _ => symbol.to_uppercase(),
    }
}

/// 計算價格精度
pub fn get_price_precision(price: f64) -> u32 {
    if price >= 1.0 {
        2
    } else if price >= 0.1 {
        3
    } else if price >= 0.01 {
        4
    } else if price >= 0.001 {
        5
    } else {
        8
    }
}

/// 計算數量精度
pub fn get_quantity_precision(quantity: f64) -> u32 {
    if quantity >= 1.0 {
        3
    } else if quantity >= 0.1 {
        4
    } else if quantity >= 0.01 {
        5
    } else {
        8
    }
}

/// 格式化價格字符串
pub fn format_price(price: f64, precision: u32) -> String {
    format!("{:.prec$}", price, prec = precision as usize)
}

/// 格式化數量字符串
pub fn format_quantity(quantity: f64, precision: u32) -> String {
    format!("{:.prec$}", quantity, prec = precision as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_symbol() {
        assert_eq!(normalize_symbol("BTC-USDT"), "BTCUSDT");
        assert_eq!(normalize_symbol("btc_usdt"), "BTCUSDT");
        assert_eq!(normalize_symbol("BTCUSDT"), "BTCUSDT");
    }

    #[test]
    fn test_denormalize_symbol() {
        assert_eq!(denormalize_symbol("BTCUSDT", "okx"), "BTC-USDT");
        assert_eq!(denormalize_symbol("BTCUSDT", "binance"), "BTCUSDT");
        assert_eq!(denormalize_symbol("BTCUSDT", "bitget"), "BTCUSDT");
    }

    #[test]
    fn test_precision_calculation() {
        assert_eq!(get_price_precision(50000.0), 2);
        assert_eq!(get_price_precision(0.5), 3);
        assert_eq!(get_price_precision(0.05), 4);
        assert_eq!(get_price_precision(0.005), 5);
        assert_eq!(get_price_precision(0.0005), 8);
    }
}