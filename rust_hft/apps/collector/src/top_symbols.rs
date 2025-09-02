use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct TopSymbol {
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub volume: f64,
    pub price: f64,
    pub change_24h: f64,
}

// Binance API 響應結構
#[derive(Deserialize, Debug)]
struct BinanceTicker {
    symbol: String,
    #[serde(rename = "lastPrice")]
    last_price: String,
    #[serde(rename = "priceChangePercent")]
    price_change_percent: String,
    #[serde(rename = "quoteVolume")]
    quote_volume: Option<String>,
    volume: Option<String>,
}

// Bitget API 響應結構
#[derive(Deserialize, Debug)]
struct BitgetResponse {
    code: String,
    data: Option<Vec<BitgetTicker>>,
}

#[derive(Deserialize, Debug)]
struct BitgetTicker {
    symbol: String,
    #[serde(rename = "lastPr")]
    last_pr: String,
    #[serde(rename = "change24h")]
    change24h: String,
    #[serde(rename = "quoteVolume")]
    quote_volume: String,
}

pub struct TopSymbolsFetcher {
    client: Client,
}

impl TopSymbolsFetcher {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub async fn get_binance_top_symbols(
        &self,
        market: &str,
        limit: usize,
    ) -> Result<Vec<TopSymbol>> {
        let url = match market.to_lowercase().as_str() {
            "spot" => "https://api.binance.com/api/v3/ticker/24hr",
            "usdt-futures" => "https://fapi.binance.com/fapi/v1/ticker/24hr",
            "coin-futures" => "https://dapi.binance.com/dapi/v1/ticker/24hr",
            _ => return Err(anyhow::anyhow!("不支援的市場類型: {}", market)),
        };

        info!("獲取 Binance {} 市場數據: {}", market, url);

        let response = self.client.get(url).send().await?;
        let tickers: Vec<BinanceTicker> = response.json().await?;

        // 過濾 USDT 配對並按交易量排序
        let mut filtered: Vec<_> = tickers
            .into_iter()
            .filter(|ticker| {
                if market.to_lowercase() == "spot" || market.to_lowercase() == "usdt-futures" {
                    ticker.symbol.ends_with("USDT")
                } else {
                    true // coin-futures 不過濾
                }
            })
            .collect();

        // 按交易量排序（現貨用 quoteVolume，合約用 volume）
        filtered.sort_by(|a, b| {
            let vol_a = if market.to_lowercase() == "spot" {
                a.quote_volume.as_ref().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
            } else {
                a.volume.as_ref().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
            };
            
            let vol_b = if market.to_lowercase() == "spot" {
                b.quote_volume.as_ref().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
            } else {
                b.volume.as_ref().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
            };
            
            vol_b.partial_cmp(&vol_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        let top_symbols: Vec<TopSymbol> = filtered
            .into_iter()
            .take(limit)
            .map(|ticker| {
                let volume = if market.to_lowercase() == "spot" {
                    ticker.quote_volume.as_ref().and_then(|v| v.parse().ok()).unwrap_or(0.0)
                } else {
                    ticker.volume.as_ref().and_then(|v| v.parse().ok()).unwrap_or(0.0)
                };

                TopSymbol {
                    venue: "binance".to_string(),
                    market: market.to_string(),
                    symbol: ticker.symbol,
                    volume,
                    price: ticker.last_price.parse().unwrap_or(0.0),
                    change_24h: ticker.price_change_percent.parse().unwrap_or(0.0),
                }
            })
            .collect();

        info!("獲取到 {} 個 Binance {} 標的", top_symbols.len(), market);
        Ok(top_symbols)
    }

    pub async fn get_bitget_top_symbols(
        &self,
        market: &str,
        limit: usize,
    ) -> Result<Vec<TopSymbol>> {
        let url = match market.to_lowercase().as_str() {
            "spot" => "https://api.bitget.com/api/v2/spot/market/tickers",
            "usdt-futures" => "https://api.bitget.com/api/v2/mix/market/tickers?productType=USDT-FUTURES",
            "coin-futures" => "https://api.bitget.com/api/v2/mix/market/tickers?productType=COIN-FUTURES",
            _ => return Err(anyhow::anyhow!("不支援的市場類型: {}", market)),
        };

        info!("獲取 Bitget {} 市場數據: {}", market, url);

        let response = self.client.get(url).send().await?;
        let bitget_response: BitgetResponse = response.json().await?;

        if bitget_response.code != "00000" {
            return Err(anyhow::anyhow!("Bitget API 返回錯誤: {}", bitget_response.code));
        }

        let tickers = bitget_response.data.unwrap_or_default();

        // 過濾 USDT 配對並按交易量排序
        let mut filtered: Vec<_> = tickers
            .into_iter()
            .filter(|ticker| {
                if market.to_lowercase() == "spot" || market.to_lowercase() == "usdt-futures" {
                    ticker.symbol.ends_with("USDT")
                } else {
                    true // coin-futures 不過濾
                }
            })
            .collect();

        // 按 quoteVolume 排序
        filtered.sort_by(|a, b| {
            let vol_a = a.quote_volume.parse::<f64>().unwrap_or(0.0);
            let vol_b = b.quote_volume.parse::<f64>().unwrap_or(0.0);
            vol_b.partial_cmp(&vol_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        let top_symbols: Vec<TopSymbol> = filtered
            .into_iter()
            .take(limit)
            .map(|ticker| TopSymbol {
                venue: "bitget".to_string(),
                market: market.to_string(),
                symbol: ticker.symbol.clone(),
                volume: ticker.quote_volume.parse().unwrap_or(0.0),
                price: ticker.last_pr.parse().unwrap_or(0.0),
                change_24h: ticker.change24h.parse().unwrap_or(0.0),
            })
            .collect();

        info!("獲取到 {} 個 Bitget {} 標的", top_symbols.len(), market);
        Ok(top_symbols)
    }

    pub async fn get_all_top_symbols(
        &self,
        market: &str,
        limit: usize,
        venues: &[&str],
    ) -> Result<Vec<TopSymbol>> {
        let mut all_symbols = Vec::new();

        for venue in venues {
            let symbols = match *venue {
                "binance" => {
                    match self.get_binance_top_symbols(market, limit).await {
                        Ok(symbols) => symbols,
                        Err(e) => {
                            warn!("獲取 Binance 標的失敗: {}", e);
                            Vec::new()
                        }
                    }
                }
                "bitget" => {
                    match self.get_bitget_top_symbols(market, limit).await {
                        Ok(symbols) => symbols,
                        Err(e) => {
                            warn!("獲取 Bitget 標的失敗: {}", e);
                            Vec::new()
                        }
                    }
                }
                _ => {
                    warn!("不支援的交易所: {}", venue);
                    Vec::new()
                }
            };
            all_symbols.extend(symbols);
        }

        Ok(all_symbols)
    }

    pub fn format_symbols_by_venue(&self, symbols: &[TopSymbol]) -> HashMap<String, Vec<String>> {
        let mut grouped = HashMap::new();
        
        for symbol in symbols {
            grouped.entry(symbol.venue.clone())
                .or_insert_with(Vec::new)
                .push(symbol.symbol.clone());
        }

        grouped
    }

    pub fn print_statistics(&self, symbols: &[TopSymbol], top_n: usize) {
        info!("=== 獲取到的熱門標的統計 ===");
        
        let binance_count = symbols.iter().filter(|s| s.venue == "binance").count();
        let bitget_count = symbols.iter().filter(|s| s.venue == "bitget").count();
        
        info!("總計: {} 個標的", symbols.len());
        info!("Binance: {} 個", binance_count);
        info!("Bitget: {} 個", bitget_count);
        
        info!("=== 前{}大交易量標的 ===", top_n.min(symbols.len()));
        for (i, symbol) in symbols.iter().enumerate().take(top_n) {
            info!("{:>2}. {:>8} {:>12} vol={:>15.0} price={:>10.4} change={:>6.2}%",
                i + 1,
                symbol.venue.to_uppercase(),
                symbol.symbol,
                symbol.volume,
                symbol.price,
                symbol.change_24h
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_binance_spot_symbols() {
        let fetcher = TopSymbolsFetcher::new();
        let symbols = fetcher.get_binance_top_symbols("spot", 5).await;
        assert!(symbols.is_ok());
        let symbols = symbols.unwrap();
        assert!(!symbols.is_empty());
        assert!(symbols.len() <= 5);
        
        for symbol in &symbols {
            assert_eq!(symbol.venue, "binance");
            assert_eq!(symbol.market, "spot");
            assert!(symbol.symbol.ends_with("USDT"));
        }
    }

    #[tokio::test]
    async fn test_get_bitget_futures_symbols() {
        let fetcher = TopSymbolsFetcher::new();
        let symbols = fetcher.get_bitget_top_symbols("usdt-futures", 3).await;
        assert!(symbols.is_ok());
        let symbols = symbols.unwrap();
        assert!(!symbols.is_empty());
        assert!(symbols.len() <= 3);
        
        for symbol in &symbols {
            assert_eq!(symbol.venue, "bitget");
            assert_eq!(symbol.market, "usdt-futures");
        }
    }
}