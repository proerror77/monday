/*!
 * 風險服務 (Risk Service)
 *
 * 專注於風險控制和風險指標監控，包括預交易風控、實時風險監控和風險告警。
 * 從 CompleteOMS 中分離出專門的風險管理職責。
 */

use anyhow::Result;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::app::services::order_service::OrderRequest;
use crate::domains::trading::{AccountId, Portfolio};

/// 風險檢查結果
#[derive(Debug, Clone)]
pub struct RiskCheckResult {
    /// 是否通過風險檢查
    pub approved: bool,

    /// 拒絕原因 (如果被拒絕)
    pub reason: Option<String>,

    /// 風險評分 (0.0 - 1.0)
    pub risk_score: f64,

    /// 檢查的風險項目
    pub checked_items: Vec<RiskItem>,

    /// 建議的調整
    pub suggestions: Vec<String>,
}

/// 風險檢查項目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskItem {
    /// 風險項目名稱
    pub name: String,

    /// 當前值
    pub current_value: f64,

    /// 限制值
    pub limit_value: f64,

    /// 是否通過
    pub passed: bool,

    /// 利用率 (百分比)
    pub utilization_pct: f64,
}

/// 風險配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// 最大單筆訂單金額 (USD)
    pub max_order_value_usd: f64,

    /// 最大持倉金額 (USD)
    pub max_position_value_usd: f64,

    /// 最大總風險敞口 (USD)
    pub max_total_exposure_usd: f64,

    /// 最大回撤百分比
    pub max_drawdown_pct: f64,

    /// 最大槓桿倍數
    pub max_leverage: f64,

    /// 單日最大虧損額度 (USD)
    pub daily_loss_limit_usd: f64,

    /// 每小時最大交易次數
    pub max_trades_per_hour: u32,

    /// 最大集中度 (單一交易對佔比)
    pub max_concentration_pct: f64,

    /// 風險評分閾值 (超過此值將被拒絕)
    pub risk_score_threshold: f64,

    /// 是否啟用實時風控
    pub enable_realtime_risk: bool,

    /// 是否啟用預交易風控
    pub enable_pre_trade_risk: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_order_value_usd: 100_000.0,      // 10萬美元
            max_position_value_usd: 500_000.0,   // 50萬美元
            max_total_exposure_usd: 1_000_000.0, // 100萬美元
            max_drawdown_pct: 5.0,               // 5%
            max_leverage: 3.0,                   // 3倍槓桿
            daily_loss_limit_usd: 10_000.0,      // 日虧損限額1萬美元
            max_trades_per_hour: 100,            // 每小時最多100筆交易
            max_concentration_pct: 25.0,         // 單一交易對最多25%
            risk_score_threshold: 0.8,           // 風險評分閾值
            enable_realtime_risk: true,
            enable_pre_trade_risk: true,
        }
    }
}

/// 風險統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RiskStats {
    /// 總風險檢查次數
    pub total_checks: u64,

    /// 通過的風險檢查次數
    pub passed_checks: u64,

    /// 被拒絕的風險檢查次數
    pub rejected_checks: u64,

    /// 當前風險評分
    pub current_risk_score: f64,

    /// 最高風險評分
    pub max_risk_score: f64,

    /// 風險告警次數
    pub risk_alerts: u64,

    /// 最後風險檢查時間
    pub last_check_time: u64,
}

/// 風險告警
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAlert {
    /// 告警ID
    pub alert_id: String,

    /// 賬戶ID
    pub account_id: AccountId,

    /// 告警類型
    pub alert_type: RiskAlertType,

    /// 告警級別
    pub severity: AlertSeverity,

    /// 告警消息
    pub message: String,

    /// 相關數據
    pub data: HashMap<String, f64>,

    /// 告警時間
    pub timestamp: u64,

    /// 是否已確認
    pub acknowledged: bool,
}

/// 風險告警類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskAlertType {
    /// 超額風險敞口
    ExcessiveExposure,

    /// 集中度過高
    HighConcentration,

    /// 回撤過大
    ExcessiveDrawdown,

    /// 交易頻率過高
    HighTradingFrequency,

    /// 槓桿過高
    ExcessiveLeverage,

    /// 日虧損超限
    DailyLossLimit,

    /// 風險評分過高
    HighRiskScore,
}

/// 告警嚴重程度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertSeverity {
    /// 信息
    Info,

    /// 警告
    Warning,

    /// 錯誤
    Error,

    /// 嚴重
    Critical,
}

/// 風險服務 - 專注於風險控制和監控
pub struct RiskService {
    /// 風險配置 (按賬戶分組)
    risk_configs: Arc<RwLock<HashMap<AccountId, RiskConfig>>>,

    /// 投資組合快照 (用於風險計算)
    portfolios: Arc<RwLock<HashMap<AccountId, Portfolio>>>,

    /// 風險統計信息
    stats: Arc<RwLock<RiskStats>>,

    /// 活躍的風險告警
    active_alerts: Arc<RwLock<HashMap<String, RiskAlert>>>,

    /// 交易歷史計數器 (用於頻率控制)
    trade_counters: Arc<RwLock<HashMap<AccountId, TradeCounter>>>,
}

/// 交易計數器
#[derive(Debug, Clone)]
struct TradeCounter {
    /// 每小時交易次數
    hourly_count: u32,

    /// 最後重置時間
    last_reset: u64,

    /// 當日虧損
    daily_loss: f64,

    /// 當日重置時間
    daily_reset: u64,
}

impl Default for TradeCounter {
    fn default() -> Self {
        let now = crate::core::types::now_micros();
        Self {
            hourly_count: 0,
            last_reset: now,
            daily_loss: 0.0,
            daily_reset: now,
        }
    }
}

impl RiskService {
    /// 創建新的風險服務
    pub fn new() -> Self {
        Self {
            risk_configs: Arc::new(RwLock::new(HashMap::new())),
            portfolios: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(RiskStats::default())),
            active_alerts: Arc::new(RwLock::new(HashMap::new())),
            trade_counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 添加賬戶風險配置
    pub async fn add_account(&self, account_id: AccountId, config: RiskConfig) {
        self.risk_configs
            .write()
            .await
            .insert(account_id.clone(), config);
        self.trade_counters
            .write()
            .await
            .insert(account_id, TradeCounter::default());
    }

    /// 更新投資組合 (用於風險計算)
    pub async fn update_portfolio(&self, account_id: AccountId, portfolio: Portfolio) {
        self.portfolios.write().await.insert(account_id, portfolio);
    }

    /// 預交易風險檢查
    pub async fn pre_trade_risk_check(&self, request: &OrderRequest) -> Result<RiskCheckResult> {
        let account_id = &request.account_id;

        // 獲取風險配置
        let risk_config = self
            .risk_configs
            .read()
            .await
            .get(account_id)
            .cloned()
            .unwrap_or_default();

        if !risk_config.enable_pre_trade_risk {
            return Ok(RiskCheckResult {
                approved: true,
                reason: None,
                risk_score: 0.0,
                checked_items: vec![],
                suggestions: vec![],
            });
        }

        // 獲取當前投資組合
        let portfolio = self
            .portfolios
            .read()
            .await
            .get(account_id)
            .cloned()
            .unwrap_or_else(|| Portfolio::new(account_id.clone(), 0.0));

        let mut checked_items = Vec::new();
        let mut total_risk_score = 0.0;
        let mut is_approved = true;
        let mut rejection_reasons = Vec::new();

        // 1. 單筆訂單金額檢查
        let order_value = self.calculate_order_value(request);
        let order_check = RiskItem {
            name: "單筆訂單金額".to_string(),
            current_value: order_value,
            limit_value: risk_config.max_order_value_usd,
            passed: order_value <= risk_config.max_order_value_usd,
            utilization_pct: (order_value / risk_config.max_order_value_usd) * 100.0,
        };

        if !order_check.passed {
            is_approved = false;
            rejection_reasons.push(format!(
                "單筆訂單金額超限: ${:.2} > ${:.2}",
                order_value, risk_config.max_order_value_usd
            ));
        }

        total_risk_score += order_check.utilization_pct / 100.0 * 0.2; // 20% 權重
        checked_items.push(order_check);

        // 2. 交易頻率檢查
        let trade_frequency_check = self.check_trade_frequency(account_id, &risk_config).await;
        if !trade_frequency_check.passed {
            is_approved = false;
            rejection_reasons.push("交易頻率超限".to_string());
        }
        total_risk_score += trade_frequency_check.utilization_pct / 100.0 * 0.1; // 10% 權重
        checked_items.push(trade_frequency_check);

        // 3. 預計持倉檢查 (模擬成交後的持倉)
        let projected_exposure = self.calculate_projected_exposure(&portfolio, request).await;
        let exposure_check = RiskItem {
            name: "預計總風險敞口".to_string(),
            current_value: projected_exposure,
            limit_value: risk_config.max_total_exposure_usd,
            passed: projected_exposure <= risk_config.max_total_exposure_usd,
            utilization_pct: (projected_exposure / risk_config.max_total_exposure_usd) * 100.0,
        };

        if !exposure_check.passed {
            is_approved = false;
            rejection_reasons.push(format!(
                "預計風險敞口超限: ${:.2} > ${:.2}",
                projected_exposure, risk_config.max_total_exposure_usd
            ));
        }

        total_risk_score += exposure_check.utilization_pct / 100.0 * 0.3; // 30% 權重
        checked_items.push(exposure_check);

        // 4. 集中度檢查
        let concentration_check = self
            .check_concentration(&portfolio, request, &risk_config)
            .await;
        if !concentration_check.passed {
            is_approved = false;
            rejection_reasons.push("交易對集中度過高".to_string());
        }
        total_risk_score += concentration_check.utilization_pct / 100.0 * 0.2; // 20% 權重
        checked_items.push(concentration_check);

        // 5. 日虧損限額檢查
        let daily_loss_check = self.check_daily_loss_limit(account_id, &risk_config).await;
        if !daily_loss_check.passed {
            is_approved = false;
            rejection_reasons.push("日虧損限額超限".to_string());
        }
        total_risk_score += daily_loss_check.utilization_pct / 100.0 * 0.2; // 20% 權重
        checked_items.push(daily_loss_check);

        // 6. 風險評分檢查
        if total_risk_score > risk_config.risk_score_threshold {
            is_approved = false;
            rejection_reasons.push(format!(
                "綜合風險評分過高: {:.3} > {:.3}",
                total_risk_score, risk_config.risk_score_threshold
            ));
        }

        // 更新統計
        let mut stats = self.stats.write().await;
        stats.total_checks += 1;
        stats.current_risk_score = total_risk_score;
        stats.max_risk_score = stats.max_risk_score.max(total_risk_score);
        stats.last_check_time = crate::core::types::now_micros();

        if is_approved {
            stats.passed_checks += 1;
        } else {
            stats.rejected_checks += 1;
        }

        // 生成風險告警 (如果需要)
        if total_risk_score > 0.7 {
            self.generate_risk_alert(
                account_id.clone(),
                RiskAlertType::HighRiskScore,
                AlertSeverity::Warning,
                format!("風險評分較高: {:.3}", total_risk_score),
                HashMap::from([("risk_score".to_string(), total_risk_score)]),
            )
            .await;
        }

        Ok(RiskCheckResult {
            approved: is_approved,
            reason: if is_approved {
                None
            } else {
                Some(rejection_reasons.join("; "))
            },
            risk_score: total_risk_score,
            suggestions: self.generate_suggestions(&checked_items),
            checked_items,
        })
    }

    /// 實時風險監控
    pub async fn realtime_risk_monitor(&self, account_id: &AccountId) -> Result<()> {
        let risk_config = self
            .risk_configs
            .read()
            .await
            .get(account_id)
            .cloned()
            .unwrap_or_default();

        if !risk_config.enable_realtime_risk {
            return Ok(());
        }

        let portfolio = self
            .portfolios
            .read()
            .await
            .get(account_id)
            .cloned()
            .unwrap_or_else(|| Portfolio::new(account_id.clone(), 0.0));

        // 檢查當前總風險敞口
        let total_exposure = portfolio.total_exposure();
        if total_exposure > risk_config.max_total_exposure_usd {
            self.generate_risk_alert(
                account_id.clone(),
                RiskAlertType::ExcessiveExposure,
                AlertSeverity::Critical,
                format!(
                    "總風險敞口超限: ${:.2} > ${:.2}",
                    total_exposure, risk_config.max_total_exposure_usd
                ),
                HashMap::from([
                    ("current_exposure".to_string(), total_exposure),
                    (
                        "limit_exposure".to_string(),
                        risk_config.max_total_exposure_usd,
                    ),
                ]),
            )
            .await;
        }

        // 檢查回撤
        // TODO: 實現回撤計算邏輯

        Ok(())
    }

    /// 獲取風險統計信息
    pub async fn get_stats(&self) -> RiskStats {
        self.stats.read().await.clone()
    }

    /// 獲取活躍告警
    pub async fn get_active_alerts(&self, account_id: Option<&AccountId>) -> Vec<RiskAlert> {
        let alerts = self.active_alerts.read().await;

        if let Some(account_id) = account_id {
            alerts
                .values()
                .filter(|alert| &alert.account_id == account_id)
                .cloned()
                .collect()
        } else {
            alerts.values().cloned().collect()
        }
    }

    /// 確認告警
    pub async fn acknowledge_alert(&self, alert_id: &str) -> Result<()> {
        let mut alerts = self.active_alerts.write().await;
        if let Some(alert) = alerts.get_mut(alert_id) {
            alert.acknowledged = true;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Alert not found: {}", alert_id))
        }
    }

    // ====================== 內部輔助方法 ======================

    /// 計算訂單價值
    fn calculate_order_value(&self, request: &OrderRequest) -> f64 {
        let price = request.price.map(|p| p.0).unwrap_or(50000.0); // 使用默認價格或最新市價

        let quantity = request.quantity.to_f64().unwrap_or(0.0);
        price * quantity
    }

    /// 檢查交易頻率
    async fn check_trade_frequency(&self, account_id: &AccountId, config: &RiskConfig) -> RiskItem {
        let mut counters = self.trade_counters.write().await;
        let counter = counters.entry(account_id.clone()).or_default();

        // 重置每小時計數器 (如果需要)
        let now = crate::core::types::now_micros();
        if now - counter.last_reset > 3600_000_000 {
            // 1小時 = 3600 * 1000 * 1000 微秒
            counter.hourly_count = 0;
            counter.last_reset = now;
        }

        counter.hourly_count += 1;

        RiskItem {
            name: "每小時交易頻率".to_string(),
            current_value: counter.hourly_count as f64,
            limit_value: config.max_trades_per_hour as f64,
            passed: counter.hourly_count <= config.max_trades_per_hour,
            utilization_pct: (counter.hourly_count as f64 / config.max_trades_per_hour as f64)
                * 100.0,
        }
    }

    /// 計算預計風險敞口
    async fn calculate_projected_exposure(
        &self,
        portfolio: &Portfolio,
        request: &OrderRequest,
    ) -> f64 {
        let current_exposure = portfolio.total_exposure();
        let order_value = self.calculate_order_value(request);

        // 簡化計算：當前敞口 + 新訂單價值
        current_exposure + order_value
    }

    /// 檢查集中度
    async fn check_concentration(
        &self,
        portfolio: &Portfolio,
        request: &OrderRequest,
        config: &RiskConfig,
    ) -> RiskItem {
        let total_value = portfolio.total_value;

        // 計算該交易對的當前持倉價值
        let current_symbol_value = portfolio
            .positions
            .get(&request.symbol)
            .map(|pos| pos.market_value())
            .unwrap_or(0.0);

        // 加上新訂單的價值
        let order_value = self.calculate_order_value(request);
        let projected_symbol_value = current_symbol_value + order_value;

        let concentration_pct = if total_value > 0.0 {
            (projected_symbol_value / total_value) * 100.0
        } else {
            0.0
        };

        RiskItem {
            name: format!("{} 集中度", request.symbol),
            current_value: concentration_pct,
            limit_value: config.max_concentration_pct,
            passed: concentration_pct <= config.max_concentration_pct,
            utilization_pct: (concentration_pct / config.max_concentration_pct) * 100.0,
        }
    }

    /// 檢查日虧損限額
    async fn check_daily_loss_limit(
        &self,
        account_id: &AccountId,
        config: &RiskConfig,
    ) -> RiskItem {
        let counters = self.trade_counters.read().await;
        let counter = counters.get(account_id).cloned().unwrap_or_default();

        // 重置日計數器 (如果需要)
        let now = crate::core::types::now_micros();
        let daily_loss = if now - counter.daily_reset > 86400_000_000 {
            // 1天 = 86400 * 1000 * 1000 微秒
            0.0
        } else {
            counter.daily_loss
        };

        RiskItem {
            name: "日虧損額度".to_string(),
            current_value: daily_loss,
            limit_value: config.daily_loss_limit_usd,
            passed: daily_loss <= config.daily_loss_limit_usd,
            utilization_pct: (daily_loss / config.daily_loss_limit_usd) * 100.0,
        }
    }

    /// 生成風險告警
    async fn generate_risk_alert(
        &self,
        account_id: AccountId,
        alert_type: RiskAlertType,
        severity: AlertSeverity,
        message: String,
        data: HashMap<String, f64>,
    ) {
        let alert_id = format!(
            "RISK_{}_{}",
            crate::core::types::now_micros(),
            account_id.as_str()
        );

        let alert = RiskAlert {
            alert_id: alert_id.clone(),
            account_id,
            alert_type,
            severity,
            message,
            data,
            timestamp: crate::core::types::now_micros(),
            acknowledged: false,
        };

        self.active_alerts.write().await.insert(alert_id, alert);
        self.stats.write().await.risk_alerts += 1;
    }

    /// 生成風險建議
    fn generate_suggestions(&self, checked_items: &[RiskItem]) -> Vec<String> {
        let mut suggestions = Vec::new();

        for item in checked_items {
            if item.utilization_pct > 80.0 {
                suggestions.push(format!(
                    "{} 利用率較高 ({:.1}%)，建議減少交易規模",
                    item.name, item.utilization_pct
                ));
            }
        }

        if suggestions.is_empty() {
            suggestions.push("風險水平正常".to_string());
        }

        suggestions
    }
}

impl Default for RiskService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::trading::{OrderSide, OrderType, TimeInForce};

    #[tokio::test]
    async fn test_risk_service_creation() {
        let risk_service = RiskService::new();
        let stats = risk_service.get_stats().await;

        assert_eq!(stats.total_checks, 0);
        assert_eq!(stats.passed_checks, 0);
    }

    #[tokio::test]
    async fn test_pre_trade_risk_check() {
        let risk_service = RiskService::new();
        let account_id = AccountId::new("bitget", "main");

        // 添加風險配置
        risk_service
            .add_account(account_id.clone(), RiskConfig::default())
            .await;

        // 創建測試訂單請求
        let request = OrderRequest {
            client_order_id: "test_001".to_string(),
            account_id: account_id.clone(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: crate::domains::trading::ToQuantity::to_quantity(1.0),
            price: Some(crate::domains::trading::ToPrice::to_price(50000.0)),
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: HashMap::new(),
        };

        let result = risk_service.pre_trade_risk_check(&request).await.unwrap();

        // 50,000 USD 訂單應該通過默認配置的風險檢查
        assert!(result.approved);
        assert!(result.risk_score >= 0.0);
        assert!(!result.checked_items.is_empty());
    }

    #[tokio::test]
    async fn test_order_value_calculation() {
        let risk_service = RiskService::new();

        let request = OrderRequest {
            client_order_id: "test_001".to_string(),
            account_id: AccountId::new("bitget", "main"),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: crate::domains::trading::ToQuantity::to_quantity(2.0),
            price: Some(crate::domains::trading::ToPrice::to_price(50000.0)),
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: HashMap::new(),
        };

        let order_value = risk_service.calculate_order_value(&request);
        assert_eq!(order_value, 100000.0); // 2 BTC * 50,000 USD = 100,000 USD
    }
}
