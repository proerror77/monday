//! 公有行情統一介面（骨架）
//! - adapters 實作 `ports::MarketStream`
//! - 此處可放 venue 無關的正規化輔助（暫空）

pub mod capabilities {
    #[derive(Debug, Clone, Default)]
    pub struct VenueCapabilities {
        pub snapshot_crc: bool,
        pub dynamic_subscriptions: bool,
    }
}
