//! A session lock owns one normalized scope; fences survive process restart.
//! Publications and attempt events are append-only, watermarks advance in the
//! same PG transaction as the verified CH publication receipt.
use anyhow::{ensure, Result};
use data::market_columnar::Manifest;
use serde::{Deserialize, Serialize};
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row};
use sqlx_postgres::{PgConnection, PgPool, PgPoolOptions};

pub const MIGRATION: &str = include_str!("../sql/market_postgres.sql");
// The pilot's CH table/partition budget has one writer across source scopes.
// Scope keys use only 60 bits, so this lock cannot alias a source scope.
const PILOT_WRITER_LOCK: i64 = (1_i64 << 62) | 0x4d4f4e44;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u16,
    pub batch_id: String,
    pub manifest_sha256: String,
    pub logical_sha256: String,
    pub parquet_sha256: String,
    pub rows: u64,
    pub scope_sha256: String,
    pub watermark_ns: u64,
    pub generation: String,
    pub fence: i64,
    pub clickhouse_instance_receipt_sha256: String,
}
impl Receipt {
    pub fn id(&self) -> Result<String> {
        ensure!(
            self.schema == 1
                && self.fence > 0
                && [
                    self.batch_id.as_str(),
                    self.manifest_sha256.as_str(),
                    self.logical_sha256.as_str(),
                    self.parquet_sha256.as_str(),
                    self.scope_sha256.as_str(),
                    self.generation.as_str(),
                    self.clickhouse_instance_receipt_sha256.as_str()
                ]
                .into_iter()
                .all(crate::valid_digest),
            "invalid data publication receipt"
        );
        crate::identity(self)
    }
}
pub enum Claim {
    Published(Receipt),
    Permit(Box<Permit>),
}
#[derive(Clone)]
pub struct DataLedger {
    pool: PgPool,
}
pub struct Permit {
    connection: PgConnection,
    manifest: Manifest,
    generation: String,
    fence: i64,
}
impl Permit {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn fence(&self) -> i64 {
        self.fence
    }
    pub async fn release(mut self) -> Result<()> {
        use sqlx_core::connection::Connection;
        let key = i64::from_str_radix(&self.manifest.spec.scope()?[..15], 16)?;
        let unlocked: bool = query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(key)
            .fetch_one(&mut self.connection)
            .await?;
        ensure!(unlocked, "data scope lock was lost");
        let unlocked: bool = query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(PILOT_WRITER_LOCK)
            .fetch_one(&mut self.connection)
            .await?;
        ensure!(unlocked, "pilot data writer lock was lost");
        self.connection.close().await?;
        Ok(())
    }
    pub async fn check(&mut self) -> Result<()> {
        let row=query("SELECT a.mode,(SELECT max(fence) FROM market_data.attempts WHERE batch_id=$1) AS fence FROM market_data.authority a WHERE singleton")
            .bind(&self.manifest.batch_id).fetch_one(&mut self.connection).await?;
        ensure!(
            row.get::<String, _>("mode") == "import"
                && row.get::<Option<i64>, _>("fence") == Some(self.fence),
            "data writer paused or attempt superseded"
        );
        Ok(())
    }
    pub async fn event(&mut self, name: &str) -> Result<()> {
        ensure!(
            matches!(
                name,
                "stage_created"
                    | "stage_verified"
                    | "ch_published"
                    | "ch_receipt_recovered"
                    | "transport_unknown"
                    | "failed"
            ),
            "invalid data attempt event"
        );
        query("INSERT INTO market_data.events(fence,event) VALUES($1,$2)")
            .bind(self.fence)
            .bind(name)
            .execute(&mut self.connection)
            .await?;
        Ok(())
    }
    pub(crate) async fn publish(
        &mut self,
        proof: crate::market_clickhouse::TargetProof,
    ) -> Result<Receipt> {
        proof.matches(&self.manifest)?;
        let instance_sha = proof.instance_sha256();
        self.check().await?;
        ensure!(
            crate::valid_digest(instance_sha),
            "CH service identity is not admitted"
        );
        use sqlx_core::connection::Connection;
        let mut tx = self.connection.begin().await?;
        let mode: String =
            query_scalar("SELECT mode FROM market_data.authority WHERE singleton FOR SHARE")
                .fetch_one(&mut *tx)
                .await?;
        let current: i64 =
            query_scalar("SELECT max(fence) FROM market_data.attempts WHERE batch_id=$1")
                .bind(&self.manifest.batch_id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            mode == "import" && current == self.fence,
            "data publication attempt no longer admitted"
        );
        let start = self
            .manifest
            .spec
            .sources
            .first()
            .unwrap()
            .start_received_ns as i64;
        let end = self.manifest.spec.sources.last().unwrap().end_received_ns as i64;
        let scope = self.manifest.spec.scope()?;
        let watermark: Option<i64> = query_scalar(
            "SELECT source_end_ns FROM market_data.watermarks WHERE scope_sha256=$1 FOR UPDATE",
        )
        .bind(&scope)
        .fetch_optional(&mut *tx)
        .await?;
        ensure!(
            watermark.is_none_or(|last| last == start),
            "gap/overlap/late batch cannot advance data watermark"
        );
        let receipt = Receipt {
            schema: 1,
            batch_id: self.manifest.batch_id.clone(),
            manifest_sha256: self.manifest.id()?,
            logical_sha256: self.manifest.logical_sha256.clone(),
            parquet_sha256: self.manifest.parquet_sha256.clone(),
            rows: self.manifest.rows,
            scope_sha256: scope.clone(),
            watermark_ns: end as u64,
            generation: self.generation.clone(),
            fence: self.fence,
            clickhouse_instance_receipt_sha256: instance_sha.into(),
        };
        query("INSERT INTO market_data.publications(batch_id,receipt_sha256,fence,receipt) VALUES($1,$2,$3,$4)")
            .bind(&receipt.batch_id).bind(receipt.id()?).bind(self.fence).bind(serde_json::to_value(&receipt)?).execute(&mut *tx).await?;
        query("INSERT INTO market_data.watermarks(scope_sha256,source_end_ns,batch_id,revision) VALUES($1,$2,$3,1) ON CONFLICT(scope_sha256) DO UPDATE SET source_end_ns=EXCLUDED.source_end_ns,batch_id=EXCLUDED.batch_id,revision=market_data.watermarks.revision+1")
            .bind(&scope).bind(end).bind(&receipt.batch_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(receipt)
    }
}
impl DataLedger {
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self {
            pool: PgPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect(url)
                .await?,
        })
    }
    pub async fn claim(&self, manifest: &Manifest) -> Result<Claim> {
        manifest.validate()?;
        let scope = manifest.spec.scope()?;
        let mut connection = self.pool.acquire().await?.detach();
        let pilot_locked: bool = query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(PILOT_WRITER_LOCK)
            .fetch_one(&mut connection)
            .await?;
        ensure!(pilot_locked, "pilot data import already has a writer");
        let key = i64::from_str_radix(&scope[..15], 16)?;
        let locked: bool = query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut connection)
            .await?;
        ensure!(locked, "normalized data scope already has a writer");
        let mode: String = query_scalar("SELECT mode FROM market_data.authority WHERE singleton")
            .fetch_one(&mut connection)
            .await?;
        ensure!(mode == "import", "data import authority is paused");
        if let Some(row)=query("SELECT p.receipt,p.receipt_sha256,b.manifest_sha256 FROM market_data.publications p JOIN market_data.batches b USING(batch_id) WHERE batch_id=$1")
            .bind(&manifest.batch_id).fetch_optional(&mut connection).await? {
            let receipt:Receipt=serde_json::from_value(row.get("receipt"))?;
            ensure!(row.get::<String,_>("manifest_sha256")==manifest.id()? && row.get::<String,_>("receipt_sha256")==receipt.id()? && receipt.manifest_sha256==manifest.id()? && receipt.logical_sha256==manifest.logical_sha256 && receipt.parquet_sha256==manifest.parquet_sha256 && receipt.rows==manifest.rows,"published batch conflicts with selected manifest");
            let unlocked:bool=query_scalar("SELECT pg_advisory_unlock($1)").bind(key).fetch_one(&mut connection).await?;
            ensure!(unlocked,"published scope lock was lost");
            let unlocked:bool=query_scalar("SELECT pg_advisory_unlock($1)").bind(PILOT_WRITER_LOCK).fetch_one(&mut connection).await?;
            ensure!(unlocked,"published pilot writer lock was lost");
            use sqlx_core::connection::Connection;
            connection.close().await?;
            return Ok(Claim::Published(receipt));
        }
        let start = manifest.spec.sources.first().unwrap().start_received_ns as i64;
        let end = manifest.spec.sources.last().unwrap().end_received_ns as i64;
        let watermark: Option<i64> =
            query_scalar("SELECT source_end_ns FROM market_data.watermarks WHERE scope_sha256=$1")
                .bind(&scope)
                .fetch_optional(&mut connection)
                .await?;
        ensure!(
            watermark.is_none_or(|last| last == start),
            "gap/overlap/late input rejected before CH write"
        );
        query("INSERT INTO market_data.batches(batch_id,scope_sha256,manifest_sha256,manifest,source_start_ns,source_end_ns) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(batch_id) DO NOTHING")
            .bind(&manifest.batch_id).bind(&scope).bind(manifest.id()?).bind(serde_json::to_value(manifest)?).bind(start).bind(end).execute(&mut connection).await?;
        let registered: String =
            query_scalar("SELECT manifest_sha256 FROM market_data.batches WHERE batch_id=$1")
                .bind(&manifest.batch_id)
                .fetch_one(&mut connection)
                .await?;
        ensure!(
            registered == manifest.id()?,
            "immutable batch produced conflicting Parquet bytes"
        );
        let fence: i64 = query_scalar("SELECT nextval('market_data.attempts_fence_seq')")
            .fetch_one(&mut connection)
            .await?;
        let generation = crate::identity(&(&manifest.batch_id, fence))?;
        query("INSERT INTO market_data.attempts(fence,generation,batch_id) VALUES($1,$2,$3)")
            .bind(fence)
            .bind(&generation)
            .bind(&manifest.batch_id)
            .execute(&mut connection)
            .await?;
        let mut permit = Permit {
            connection,
            manifest: manifest.clone(),
            generation,
            fence,
        };
        permit.check().await?;
        Ok(Claim::Permit(Box::new(permit)))
    }
    pub async fn receipt(&self, batch_id: &str) -> Result<Receipt> {
        ensure!(crate::valid_digest(batch_id), "invalid batch identity");
        let row =
            query("SELECT receipt,receipt_sha256 FROM market_data.publications WHERE batch_id=$1")
                .bind(batch_id)
                .fetch_one(&self.pool)
                .await?;
        let receipt: Receipt = serde_json::from_value(row.get("receipt"))?;
        ensure!(
            receipt.id()? == row.get::<String, _>("receipt_sha256"),
            "PG publication receipt changed"
        );
        Ok(receipt)
    }
}
