/*!
 * 應用服務層
 *
 * 包含從 CompleteOMS 拆分出來的獨立應用服務，每個服務專注於特定的業務職責。
 * 遵循單一職責原則，提供清晰的服務邊界和接口。
 */

pub mod execution_service;
pub mod order_service;
pub mod risk_service;

// 重新導出核心服務類型
pub use order_service::{
    Fill, OrderCancelRequest, OrderRequest, OrderService, OrderServiceConfig, OrderStateMachine,
    OrderStats, OrderUpdate,
};

pub use risk_service::{
    AlertSeverity, RiskAlert, RiskAlertType, RiskCheckResult, RiskConfig, RiskItem, RiskService,
    RiskStats,
};

pub use execution_service::{
    ExecutionConfig, ExecutionEvent, ExecutionQualityReport, ExecutionRecord, ExecutionService,
    ExecutionStats, ExecutionStatus,
};

// 預留其他應用服務
// pub mod portfolio_service;    // 投資組合管理服務
// pub mod position_service;     // 持倉管理服務
// pub mod reporting_service;    // 報告生成服務
// pub mod audit_service;        // 審計和合規服務
