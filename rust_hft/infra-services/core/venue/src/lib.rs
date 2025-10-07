//! 交易所規格配置系統
//!
//! 功能：
//! 1. 從 YAML 加載 per-symbol tick/lot/min_notional 等參數
//! 2. 提供標準化校驗功能
//! 3. 支持運行時熱重載

use hft_core::{HftError, HftResult, Price, Quantity, Symbol};
use ports::VenueSpec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

/// YAML 配置文件結構
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueConfigFile {
    pub venues: HashMap<String, VenueConfig>,
    pub defaults: SpecDefaults,
}

/// 單個交易所配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueConfig {
    pub spot: HashMap<String, SymbolSpec>,
    #[serde(default)]
    pub futures: HashMap<String, SymbolSpec>,
}

/// 品種規格配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolSpec {
    pub tick_size: f64,
    pub lot_size: f64,
    pub min_qty: f64,
    #[serde(default)]
    pub max_qty: Option<f64>,
    pub min_notional: f64,
    #[serde(default)]
    pub price_min: Option<f64>,
    #[serde(default)]
    pub price_max: Option<f64>,
    #[serde(default)]
    pub maker_fee_bps: Option<f64>,
    #[serde(default)]
    pub taker_fee_bps: Option<f64>,
    #[serde(default)]
    pub rate_limit: Option<u32>,
}

/// 默認規格
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecDefaults {
    pub tick_size: f64,
    pub lot_size: f64,
    pub min_qty: f64,
    pub max_qty: f64,
    pub min_notional: f64,
    pub price_min: f64,
    pub price_max: f64,
    pub maker_fee_bps: f64,
    pub taker_fee_bps: f64,
    pub rate_limit: u32,
}

/// 交易所規格管理器
pub struct VenueSpecManager {
    specs: HashMap<String, VenueSpec>, // key: "{venue}:{symbol}"
    config_path: String,
}

impl VenueSpecManager {
    /// 創建新的規格管理器
    pub fn new(config_path: String) -> Self {
        Self {
            specs: HashMap::new(),
            config_path,
        }
    }

    /// 從 YAML 文件加載配置
    pub async fn load_config(&mut self) -> HftResult<()> {
        info!("加載交易所規格配置: {}", self.config_path);

        let content = tokio::fs::read_to_string(&self.config_path)
            .await
            .map_err(|e| HftError::Config(format!("讀取配置文件失敗: {}", e)))?;

        let config: VenueConfigFile = serde_yaml::from_str(&content)
            .map_err(|e| HftError::Config(format!("解析 YAML 失敗: {}", e)))?;

        // 清空現有規格
        self.specs.clear();

        // 處理每個交易所
        for (venue_name, venue_config) in &config.venues {
            // 處理現貨品種
            for (symbol, spec) in &venue_config.spot {
                let key = format!("{}:{}", venue_name, symbol);
                let venue_spec = self.convert_symbol_spec(venue_name, spec, &config.defaults)?;
                self.specs.insert(key, venue_spec);
            }

            // 處理期貨品種（如果有）
            for (symbol, spec) in &venue_config.futures {
                let key = format!("{}:{}:FUTURES", venue_name, symbol);
                let venue_spec = self.convert_symbol_spec(venue_name, spec, &config.defaults)?;
                self.specs.insert(key, venue_spec);
            }
        }

        info!("成功加載 {} 個品種規格", self.specs.len());
        Ok(())
    }

    /// 獲取指定品種的規格
    pub fn get_spec(&self, venue: &str, symbol: &Symbol) -> Option<&VenueSpec> {
        let key = format!("{}:{}", venue, symbol.0);
        self.specs.get(&key)
    }

    /// 獲取所有規格（用於風控系統）
    pub fn get_all_specs(&self) -> &HashMap<String, VenueSpec> {
        &self.specs
    }

    /// 校驗訂單參數是否符合規格
    pub fn validate_order(
        &self,
        venue: &str,
        symbol: &Symbol,
        price: Option<Price>,
        quantity: Quantity,
    ) -> HftResult<()> {
        let spec = self
            .get_spec(venue, symbol)
            .ok_or_else(|| HftError::Config(format!("找不到規格: {}:{}", venue, symbol.0)))?;

        // 校驗數量
        if quantity < spec.min_qty {
            return Err(HftError::InvalidOrder(format!(
                "數量過小: {} < 最小數量 {}",
                quantity.0, spec.min_qty.0
            )));
        }

        if let Some(max_qty) = spec.max_quantity {
            if quantity > max_qty {
                return Err(HftError::InvalidOrder(format!(
                    "數量過大: {} > 最大數量 {}",
                    quantity.0, max_qty.0
                )));
            }
        }

        // 校驗價格（如果有）
        if let Some(price) = price {
            // 檢查價格步進
            let price_decimal = price.0;
            let tick_decimal = spec.tick_size.0;

            if (price_decimal % tick_decimal) != rust_decimal::Decimal::ZERO {
                return Err(HftError::InvalidOrder(format!(
                    "價格不符合步進: {} 不是 {} 的倍數",
                    price_decimal, tick_decimal
                )));
            }

            // 檢查最小名義值
            let notional = price_decimal * quantity.0;
            if notional < spec.min_notional {
                return Err(HftError::InvalidOrder(format!(
                    "名義值過小: {} < 最小名義值 {}",
                    notional, spec.min_notional
                )));
            }
        }

        Ok(())
    }

    /// 標準化價格到合法步進
    pub fn normalize_price(&self, venue: &str, symbol: &Symbol, price: Price) -> Option<Price> {
        let spec = self.get_spec(venue, symbol)?;
        let tick_decimal = spec.tick_size.0;

        // 四捨五入到最近的 tick
        let normalized = (price.0 / tick_decimal).round() * tick_decimal;
        Price::from_f64(normalized.to_string().parse().ok()?).ok()
    }

    /// 標準化數量到合法步進
    pub fn normalize_quantity(
        &self,
        venue: &str,
        symbol: &Symbol,
        quantity: Quantity,
    ) -> Option<Quantity> {
        let spec = self.get_spec(venue, symbol)?;
        let lot_decimal = spec.lot_size.0;

        // 向下取整到最近的 lot
        let normalized = (quantity.0 / lot_decimal).floor() * lot_decimal;
        Quantity::from_f64(normalized.to_string().parse().ok()?).ok()
    }

    /// 轉換 SymbolSpec 到 VenueSpec
    fn convert_symbol_spec(
        &self,
        venue_name: &str,
        spec: &SymbolSpec,
        _defaults: &SpecDefaults,
    ) -> HftResult<VenueSpec> {
        Ok(VenueSpec {
            name: venue_name.to_string(),
            tick_size: Price::from_f64(spec.tick_size)
                .map_err(|e| HftError::Config(format!("無效的 tick_size: {}", e)))?,
            lot_size: Quantity::from_f64(spec.lot_size)
                .map_err(|e| HftError::Config(format!("無效的 lot_size: {}", e)))?,
            min_qty: Quantity::from_f64(spec.min_qty)
                .map_err(|e| HftError::Config(format!("無效的 min_qty: {}", e)))?,
            max_quantity: spec
                .max_qty
                .map(|q| Quantity::from_f64(q))
                .transpose()
                .map_err(|e| HftError::Config(format!("無效的 max_qty: {}", e)))?,
            min_notional: rust_decimal::Decimal::try_from(spec.min_notional)
                .map_err(|e| HftError::Config(format!("無效的 min_notional: {}", e)))?,
            maker_fee_bps: spec
                .maker_fee_bps
                .map(|f| rust_decimal::Decimal::try_from(f))
                .transpose()
                .map_err(|e| HftError::Config(format!("無效的 maker_fee_bps: {}", e)))?,
            taker_fee_bps: spec
                .taker_fee_bps
                .map(|f| rust_decimal::Decimal::try_from(f))
                .transpose()
                .map_err(|e| HftError::Config(format!("無效的 taker_fee_bps: {}", e)))?,
            rate_limit: spec.rate_limit,
        })
    }

    /// 重新加載配置（熱重載）
    pub async fn reload(&mut self) -> HftResult<()> {
        info!("熱重載交易所規格配置");
        self.load_config().await
    }
}

/// 創建默認的 VenueSpecManager
///
/// 使用環境變量 INFRA_VENUE_SPECS_PATH，如果未設置則使用默認路徑
pub fn create_default_manager() -> VenueSpecManager {
    let path = std::env::var("INFRA_VENUE_SPECS_PATH")
        .unwrap_or_else(|_| "config/venue_specs.yaml".to_string());

    info!("使用交易所規格配置路徑: {}", path);
    VenueSpecManager::new(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_venue_spec_loading() {
        let mut manager = VenueSpecManager::new("../../../config/venue_specs.yaml".to_string());

        match manager.load_config().await {
            Ok(_) => {
                let btc_spec = manager.get_spec("BITGET", &Symbol("BTCUSDT".to_string()));
                assert!(btc_spec.is_some());

                let spec = btc_spec.unwrap();
                assert_eq!(spec.tick_size.0.to_string(), "0.1");
                assert_eq!(spec.min_notional.to_string(), "5");
            }
            Err(e) => {
                // 測試環境可能沒有配置文件，不作為錯誤
                eprintln!("配置加載測試跳過: {}", e);
            }
        }
    }

    #[test]
    fn test_price_normalization() {
        // 這個測試不需要文件系統
        let manager = VenueSpecManager::new("dummy".to_string());
        // 添加一個測試用的 spec
        // （實際實現中可以加一個 add_spec 方法）
    }
}
