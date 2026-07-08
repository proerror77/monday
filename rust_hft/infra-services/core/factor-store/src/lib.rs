//! Typed Factor Bank storage ports.

use hft_factor_bank::{FactorAsset, FactorBankError, FactorStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FactorStoreError {
    #[error("factor not found")]
    NotFound,
    #[error("invalid factor asset: {0}")]
    InvalidAsset(#[from] FactorBankError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorQuery {
    pub status: Option<FactorStatus>,
    pub limit: usize,
}

pub trait FactorStore {
    fn upsert_factor(&mut self, asset: FactorAsset) -> Result<(), FactorStoreError>;
    fn get_factor(&self, factor_id: &str) -> Result<FactorAsset, FactorStoreError>;
    fn list_factors(&self, query: FactorQuery) -> Result<Vec<FactorAsset>, FactorStoreError>;
}

pub fn validate_before_store(asset: &FactorAsset) -> Result<(), FactorStoreError> {
    asset.validate()?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct InMemoryFactorStore {
    assets: BTreeMap<String, FactorAsset>,
}

impl FactorStore for InMemoryFactorStore {
    fn upsert_factor(&mut self, asset: FactorAsset) -> Result<(), FactorStoreError> {
        validate_before_store(&asset)?;
        self.assets.insert(asset.factor_id.clone(), asset);
        Ok(())
    }

    fn get_factor(&self, factor_id: &str) -> Result<FactorAsset, FactorStoreError> {
        self.assets
            .get(factor_id)
            .cloned()
            .ok_or(FactorStoreError::NotFound)
    }

    fn list_factors(&self, query: FactorQuery) -> Result<Vec<FactorAsset>, FactorStoreError> {
        let mut assets = self
            .assets
            .values()
            .filter(|asset| {
                query
                    .status
                    .as_ref()
                    .map(|status| &asset.promotion_status == status)
                    .unwrap_or(true)
            })
            .take(query.limit)
            .cloned()
            .collect::<Vec<_>>();
        assets.sort_by(|left, right| left.factor_id.cmp(&right.factor_id));
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_bank::{FactorLineage, FactorMetrics, FactorType};
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_research_manifest::{ManifestId, ManifestRef};
    use std::collections::BTreeMap;

    fn reference(id: &str, kind: &str) -> ManifestRef {
        ManifestRef::new(ManifestId::new(id).unwrap(), kind).unwrap()
    }

    #[test]
    fn rejects_invalid_asset_before_store() {
        let now = chrono::Utc::now();
        let asset = FactorAsset {
            factor_id: " ".to_string(),
            factor_type: FactorType::Formula,
            ast: FactorAst::Terminal(FactorTerminal::Field("oi".to_string())),
            lineage: FactorLineage {
                parent_factor_ids: vec![],
                source_engine: "manual".to_string(),
                search_manifest_id: ManifestId::new("search-1").unwrap(),
            },
            data_manifest: reference("data-1", "data_manifest"),
            feature_manifest: reference("feature-1", "feature_manifest"),
            label_manifest: reference("label-1", "label_manifest"),
            evaluation_manifests: vec![reference("eval-1", "evaluation_manifest")],
            metrics: FactorMetrics {
                rank_ic: None,
                icir: None,
                net_sharpe: None,
                max_drawdown: None,
                turnover: None,
                custom: BTreeMap::new(),
            },
            correlation_cluster: None,
            regime_metrics: BTreeMap::new(),
            symbol_metrics: BTreeMap::new(),
            promotion_status: FactorStatus::Generated,
            live_decay_state: None,
            created_at: now,
            updated_at: now,
        };

        assert!(matches!(
            validate_before_store(&asset).unwrap_err(),
            FactorStoreError::InvalidAsset(_)
        ));
    }

    #[test]
    fn in_memory_store_round_trips_valid_assets() {
        let mut asset = {
            let now = chrono::Utc::now();
            FactorAsset {
                factor_id: "factor-1".to_string(),
                factor_type: FactorType::Formula,
                ast: FactorAst::Terminal(FactorTerminal::Field("oi".to_string())),
                lineage: FactorLineage {
                    parent_factor_ids: vec![],
                    source_engine: "manual".to_string(),
                    search_manifest_id: ManifestId::new("search-1").unwrap(),
                },
                data_manifest: reference("data-1", "data_manifest"),
                feature_manifest: reference("feature-1", "feature_manifest"),
                label_manifest: reference("label-1", "label_manifest"),
                evaluation_manifests: vec![reference("eval-1", "evaluation_manifest")],
                metrics: FactorMetrics {
                    rank_ic: Some(0.04),
                    icir: Some(1.3),
                    net_sharpe: Some(1.5),
                    max_drawdown: Some(0.04),
                    turnover: None,
                    custom: BTreeMap::new(),
                },
                correlation_cluster: None,
                regime_metrics: BTreeMap::new(),
                symbol_metrics: BTreeMap::new(),
                promotion_status: FactorStatus::PaperTrading,
                live_decay_state: None,
                created_at: now,
                updated_at: now,
            }
        };
        let mut store = InMemoryFactorStore::default();
        store.upsert_factor(asset.clone()).unwrap();

        asset.live_decay_state = Some("healthy".to_string());
        store.upsert_factor(asset.clone()).unwrap();

        assert_eq!(store.get_factor("factor-1").unwrap(), asset);
        assert_eq!(
            store
                .list_factors(FactorQuery {
                    status: Some(FactorStatus::PaperTrading),
                    limit: 10,
                })
                .unwrap()
                .len(),
            1
        );
    }
}
