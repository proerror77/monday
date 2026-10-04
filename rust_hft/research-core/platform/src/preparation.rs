//! Reviewed preparation plans remain owned by the controller/data publisher.
use crate::{identity, sha256, valid_digest};
use anyhow::{ensure, Result};
use hft_research_input::data::DataViewSpec;
use serde::{Deserialize, Serialize};

pub const CLICKHOUSE_SCHEMA: &str = include_str!("../sql/clickhouse.sql");
pub const PREPARE_SQL: &str = include_str!("../sql/prepare.sql");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparationPlan {
    pub spec: DataViewSpec,
    pub source_receipt_sha256: String,
    pub producer_image: String,
    /// Optional immutable, reviewed SQL. Native admission binds this plan hash;
    /// this is not an Agent-supplied free-form query interface or SQL sandbox.
    #[serde(default)]
    pub recipe_sql: Option<String>,
}

impl PreparationPlan {
    pub fn validate(&self) -> Result<()> {
        self.spec.validate()?;
        let recipe = self.recipe_sql.as_deref().unwrap_or(PREPARE_SQL);
        ensure!(
            recipe.len() <= 64 * 1024 && sha256(recipe.as_bytes()) == self.spec.feature_sql_sha256,
            "preparation SQL identity mismatch"
        );
        let statements: Vec<_> = recipe.split(';').filter(|s| !s.trim().is_empty()).collect();
        ensure!(
            statements.len() == 2,
            "preparation requires two fixed-schema inserts"
        );
        for (sql, table) in statements
            .iter()
            .zip(["research.prepared_features", "research.prepared_labels"])
        {
            let body = sql
                .lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(
                body.trim_start()
                    .starts_with(&format!("INSERT INTO {table}\n")),
                "preparation target changed"
            );
        }
        ensure!(
            valid_digest(&self.source_receipt_sha256)
                && self
                    .producer_image
                    .rsplit_once("@sha256:")
                    .is_some_and(|(_, h)| valid_digest(h)),
            "unverified preparation producer/source"
        );
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity(self)
    }
}
