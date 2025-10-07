use once_cell::sync::OnceCell;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;

const DEFAULT_WHITELIST_PATH: &str = "config/symbol_whitelist.json";

const FALLBACK_BASE_TOKENS: &[&str] = &[
    "BTC", "ETH", "SOL", "SUI", "XRP", "XLM", "HYPE", "ASTER", "PUMP", "DOGE", "AVAX", "WLFI",
    "ADA", "ENA", "ONDO", "SEI", "MERL", "B", "BNB", "BGB", "OKB",
];

#[derive(Deserialize)]
struct WhitelistFile {
    #[serde(default)]
    base_tokens: Vec<String>,
}

static BASE_TOKENS_CACHE: OnceCell<Vec<String>> = OnceCell::new();

fn load_base_tokens_from_json() -> Option<Vec<String>> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("SYMBOLS_FILE") {
        if !path.trim().is_empty() {
            candidates.push(path);
        }
    }
    candidates.push(DEFAULT_WHITELIST_PATH.to_string());

    for path in candidates {
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<WhitelistFile>(&content) {
                Ok(cfg) => {
                    if cfg.base_tokens.is_empty() {
                        continue;
                    }
                    let mut tokens: Vec<String> = cfg
                        .base_tokens
                        .into_iter()
                        .map(|token| token.trim().to_uppercase())
                        .filter(|token| !token.is_empty())
                        .collect();
                    tokens.sort();
                    tokens.dedup();
                    if tokens.is_empty() {
                        continue;
                    }
                    return Some(tokens);
                }
                Err(err) => {
                    tracing::warn!("無法解析白名單 JSON {}: {}", path, err);
                }
            },
            Err(err) => {
                tracing::debug!("讀取白名單檔案 {} 失敗: {}", path, err);
            }
        }
    }
    None
}

fn base_tokens() -> Vec<String> {
    BASE_TOKENS_CACHE
        .get_or_init(|| {
            load_base_tokens_from_json().unwrap_or_else(|| {
                FALLBACK_BASE_TOKENS
                    .iter()
                    .map(|token| token.to_string())
                    .collect()
            })
        })
        .clone()
}

pub fn parse_symbols_override() -> Option<Vec<String>> {
    let csv = std::env::var("SYMBOLS").ok()?;
    let mut symbols: Vec<String> = csv
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .map(|mut s| {
            if s.contains(':') || s.contains('@') || s.contains('-') || s.contains('_') {
                s
            } else if !(s.ends_with("USDT")
                || s.ends_with("USDC")
                || s.ends_with("BUSD")
                || s.ends_with("FDUSD")
                || s.ends_with("TUSD"))
            {
                s.push_str("USDT");
                s
            } else {
                s
            }
        })
        .collect();
    symbols.sort();
    symbols.dedup();
    if symbols.is_empty() {
        None
    } else {
        Some(symbols)
    }
}

pub fn binance_spot_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}USDT", base))
        .collect()
}

pub fn binance_futures_pairs() -> Vec<String> {
    let mut overrides: HashMap<&'static str, &'static str> = HashMap::new();
    overrides.insert("BONK", "1000BONKUSDT");
    overrides.insert("PEPE", "1000PEPEUSDT");
    overrides.insert("SHIB", "1000SHIBUSDT");
    overrides.insert("SATS", "1000SATSUSDT");
    overrides.insert("CATE", "1000CATEUSDT");

    base_tokens()
        .into_iter()
        .map(|base| {
            overrides
                .get(base.as_str())
                .map(|&sym| sym.to_string())
                .unwrap_or_else(|| format!("{}USDT", base))
        })
        .collect()
}

pub fn bitget_spot_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}USDT:SPOT", base))
        .collect()
}

pub fn bitget_futures_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}USDT:USDT-FUTURES", base))
        .collect()
}

pub fn asterdex_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}USDT", base))
        .collect()
}

pub fn okx_spot_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}-USDT", base))
        .collect()
}

pub fn okx_swap_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}-USDT-SWAP", base))
        .collect()
}

pub fn hyperliquid_pairs() -> Vec<String> {
    base_tokens()
        .into_iter()
        .map(|base| format!("{}-USDT", base))
        .collect()
}
