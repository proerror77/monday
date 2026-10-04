//! Fixed-schema Parquet import. A generation receives one INSERT; an unknown
//! transport outcome is never resent. Only a fully verified, quiescent staging
//! partition can replace the target batch atomically.
use crate::market_postgres::Permit;
use anyhow::{ensure, Context, Result};
use data::market_columnar::{MarketRow, SealedParquet, MAX_ROWS};

pub const MIGRATION: &str = include_str!("../sql/market_clickhouse.sql");
const TARGET: &str = "market_data.events_v1";
const READBACK:&str="source_sha256,event_kind,ordinal,source_row,source_received_ns,available_ns,exchange_event_ns,transaction_ns,first_update_id,final_update_id,previous_update_id,aggregate_trade_id,first_trade_id,last_trade_id,toInt64(toDecimal128(price,8)*100000000) AS price_units,toInt64(toDecimal128(quantity,8)*100000000) AS quantity_units,is_buyer_maker,arrayMap(x->toInt64(toDecimal128(x,8)*100000000),bid_price) AS bid_price_units,arrayMap(x->toInt64(toDecimal128(x,8)*100000000),bid_quantity) AS bid_quantity_units,arrayMap(x->toInt64(toDecimal128(x,8)*100000000),ask_price) AS ask_price_units,arrayMap(x->toInt64(toDecimal128(x,8)*100000000),ask_quantity) AS ask_quantity_units";
pub struct DataClickHouse {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    user: String,
    password: String,
    instance_sha: String,
}
pub struct StageProof {
    manifest_sha: String,
    generation: String,
}
/// Only a completed CH readback constructs this proof. PG cannot publish from
/// a row count, an INSERT response, or a caller-authored success flag.
pub struct TargetProof {
    manifest_sha: String,
    logical_sha: String,
    rows: u64,
    instance_sha: String,
}
impl TargetProof {
    pub(crate) fn matches(&self, manifest: &data::market_columnar::Manifest) -> Result<()> {
        ensure!(
            self.manifest_sha == manifest.id()?
                && self.logical_sha == manifest.logical_sha256
                && self.rows == manifest.rows,
            "target proof does not bind this manifest"
        );
        Ok(())
    }
    pub(crate) fn instance_sha256(&self) -> &str {
        &self.instance_sha
    }
}
impl DataClickHouse {
    pub fn new(
        endpoint: &str,
        user: String,
        password: String,
        instance_sha: String,
    ) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint)?;
        ensure!(endpoint.scheme() == "https", "CH importer requires HTTPS");
        Self::connect(endpoint, user, password, instance_sha)
    }
    #[cfg(debug_assertions)]
    pub fn disposable_fixture(endpoint: &str, password: String) -> Result<Self> {
        ensure!(
            endpoint == "http://127.0.0.1:18123",
            "CH fixture must be exact disposable loopback"
        );
        Self::connect(
            reqwest::Url::parse(endpoint)?,
            "fixture".into(),
            password,
            "f".repeat(64),
        )
    }
    fn connect(
        endpoint: reqwest::Url,
        user: String,
        password: String,
        instance_sha: String,
    ) -> Result<Self> {
        ensure!(
            endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.path() == "/"
                && !user.is_empty()
                && !password.is_empty()
                && crate::valid_digest(&instance_sha),
            "invalid CH importer identity"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            endpoint,
            user,
            password,
            instance_sha,
        })
    }
    pub fn instance_sha256(&self) -> &str {
        &self.instance_sha
    }
    async fn query(
        &self,
        sql: &str,
        source: Option<&SealedParquet>,
        query_id: &str,
        max: usize,
    ) -> Result<Vec<u8>> {
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("query_id", query_id)
            .append_pair("max_execution_time", "45")
            .append_pair("wait_end_of_query", "1")
            .append_pair("replace_running_query", "0")
            .append_pair("output_format_json_quote_64bit_integers", "0")
            .append_pair("max_result_rows", &(MAX_ROWS + 1).to_string())
            .append_pair("max_result_bytes", &(128 * 1024 * 1024).to_string())
            .append_pair("result_overflow_mode", "throw");
        if let Some(source) = source {
            let spec = &source.manifest().spec;
            for (name, value) in [
                ("param_batch", source.manifest().batch_id.as_str()),
                ("param_venue", spec.venue.as_str()),
                ("param_market", spec.market.as_str()),
                ("param_instrument", spec.instrument.as_str()),
                ("param_session", spec.session.as_str()),
                ("param_normalizer", spec.normalizer_sha256.as_str()),
            ] {
                url.query_pairs_mut().append_pair(name, value);
            }
        }
        let request = self
            .client
            .post(url)
            .basic_auth(&self.user, Some(&self.password))
            .body(sql.to_owned());
        self.response(request, max).await
    }
    async fn response(&self, request: reqwest::RequestBuilder, max: usize) -> Result<Vec<u8>> {
        let mut response = request.send().await.map_err(|_| {
            anyhow::anyhow!("CH transport outcome unknown; do not resend this generation")
        })?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "CH rejected operation: HTTP {}, native code {}",
            status.as_u16(),
            response
                .headers()
                .get("x-clickhouse-exception-code")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("CH response interrupted; outcome unknown"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= max,
                "CH readback exceeds pilot byte bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    async fn readback(&self, table: &str, source: &SealedParquet, identity: &str) -> Result<bool> {
        let sql=format!("SELECT count() AS rows,countIf(venue!={{venue:String}} OR market!={{market:String}} OR instrument!={{instrument:String}} OR session!={{session:String}} OR normalizer_sha256!={{normalizer:FixedString(64)}}) AS wrong_scope FROM {table} WHERE batch_id={{batch:FixedString(64)}} FORMAT JSONEachRow");
        #[derive(serde::Deserialize)]
        struct Counts {
            rows: u64,
            wrong_scope: u64,
        }
        let counts: Counts = serde_json::from_slice(
            &self
                .query(&sql, Some(source), &format!("{identity}-scope"), 4096)
                .await?,
        )?;
        if counts.rows == 0 {
            return Ok(false);
        }
        ensure!(
            counts.rows == source.manifest().rows && counts.wrong_scope == 0,
            "CH target has partial/duplicate/conflicting batch contents"
        );
        let sql=format!("SELECT {READBACK} FROM {table} WHERE batch_id={{batch:FixedString(64)}} ORDER BY ordinal FORMAT JSONEachRow");
        let bytes = self
            .query(
                &sql,
                Some(source),
                &format!("{identity}-rows"),
                128 * 1024 * 1024,
            )
            .await?;
        let mut rows = Vec::new();
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            ensure!(
                rows.len() < MAX_ROWS && line.len() <= 16 * 1024 * 1024,
                "CH row decode budget exceeded"
            );
            rows.push(serde_json::from_slice::<MarketRow>(line)?);
        }
        ensure!(
            rows == source.rows(),
            "CH ordered columns differ from independent Parquet decode"
        );
        Ok(true)
    }
    pub async fn verify_target(
        &self,
        source: &SealedParquet,
        identity: &str,
    ) -> Result<Option<TargetProof>> {
        ensure!(
            crate::valid_digest(identity),
            "invalid CH readback operation identity"
        );
        if !self.readback(TARGET, source, identity).await? {
            return Ok(None);
        }
        Ok(Some(TargetProof {
            manifest_sha: source.manifest().id()?,
            logical_sha: source.manifest().logical_sha256.clone(),
            rows: source.manifest().rows,
            instance_sha: self.instance_sha.clone(),
        }))
    }
    pub async fn stage(&self, permit: &mut Permit, source: &SealedParquet) -> Result<StageProof> {
        ensure!(
            permit.manifest().id()? == source.manifest().id()?,
            "staging attempt does not own this manifest"
        );
        permit.check().await?;
        let generation = permit.generation().to_owned();
        ensure!(
            crate::valid_digest(&generation),
            "invalid staging generation"
        );
        let budgets=self.query("SELECT count() FROM system.tables WHERE database='market_data' AND startsWith(name,'stage_') FORMAT TabSeparated",None,&format!("{generation}-stages"),4096).await?;
        ensure!(
            std::str::from_utf8(&budgets)?.trim().parse::<u64>()? < 64,
            "pilot staging table budget exhausted; operator review required"
        );
        let partitions=self.query("SELECT uniqExact(partition_id) FROM system.parts WHERE active AND database='market_data' AND table='events_v1' FORMAT TabSeparated",None,&format!("{generation}-partitions"),4096).await?;
        ensure!(
            std::str::from_utf8(&partitions)?.trim().parse::<u64>()? < 64,
            "pilot target partition budget exhausted"
        );
        let table = format!("market_data.stage_{generation}");
        self.query(
            &format!("CREATE TABLE {table} AS {TARGET}"),
            None,
            &format!("{generation}-create"),
            4096,
        )
        .await?;
        permit.event("stage_created").await?;
        permit.check().await?;
        let insert_id = format!("{generation}-insert");
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("query", &format!("INSERT INTO {table} FORMAT Parquet"))
            .append_pair("query_id", &insert_id)
            .append_pair("max_execution_time", "45")
            .append_pair("wait_end_of_query", "1")
            .append_pair("replace_running_query", "0")
            .append_pair("async_insert", "0");
        let request = self
            .client
            .post(url)
            .basic_auth(&self.user, Some(&self.password))
            .body(source.bytes());
        if let Err(error) = self.response(request, 4096).await {
            let _ = permit.event("transport_unknown").await;
            return Err(error);
        }
        let count=self.query(&format!("SELECT count() FROM system.processes WHERE query_id='{insert_id}' AND user=currentUser() FORMAT TabSeparated"),None,&format!("{generation}-quiescent"),4096).await?;
        ensure!(
            std::str::from_utf8(&count)?.trim() == "0",
            "staging insert has not terminated; publication blocked"
        );
        ensure!(
            self.readback(&table, source, &generation).await?,
            "staging batch is empty"
        );
        permit.check().await?;
        permit.event("stage_verified").await?;
        Ok(StageProof {
            manifest_sha: source.manifest().id()?,
            generation,
        })
    }
    pub async fn publish_staged(
        &self,
        permit: &mut Permit,
        source: &SealedParquet,
        stage: StageProof,
    ) -> Result<TargetProof> {
        ensure!(
            permit.manifest().id()? == source.manifest().id()?,
            "publication attempt does not own this manifest"
        );
        permit.check().await?;
        let generation = permit.generation().to_owned();
        ensure!(
            crate::valid_digest(&generation),
            "invalid staging generation"
        );
        ensure!(
            stage.manifest_sha == source.manifest().id()? && stage.generation == generation,
            "staging proof does not own this source/attempt"
        );
        // Read the complete frozen generation again. No subsequent writer API
        // exists for it; all attempts use distinct generations and source bytes.
        ensure!(
            self.readback(
                &format!("market_data.stage_{generation}"),
                source,
                &format!("{generation}-sealed")
            )
            .await?,
            "sealed stage missing"
        );
        permit.check().await?;
        self.query(&format!("ALTER TABLE {TARGET} REPLACE PARTITION {{batch:String}} FROM market_data.stage_{generation}"),Some(source),&format!("{generation}-replace"),4096).await?;
        let proof = self
            .verify_target(source, &generation)
            .await?
            .context("target publication disappeared")?;
        permit.event("ch_published").await?;
        Ok(proof)
    }
}
