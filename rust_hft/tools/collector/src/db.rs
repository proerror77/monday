use anyhow::Result;
#[cfg(feature = "duckdb")]
use duckdb::{params, Connection};
use once_cell::sync::{Lazy, OnceCell};
use reqwest::Client;
use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "duckdb")]
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::Duration;

type DynSink = dyn DbSink;

static SINK_CACHE: Lazy<AsyncMutex<HashMap<String, Arc<DynSink>>>> =
    Lazy::new(|| AsyncMutex::new(HashMap::new()));

static DB_CONFIG: OnceCell<DbConfig> = OnceCell::new();

#[derive(Clone)]
pub enum DbBackend {
    ClickHouse,
    #[cfg(feature = "duckdb")]
    DuckDb,
}

#[derive(Clone)]
pub struct DbConfig {
    pub backend: DbBackend,
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
    pub dry_run: bool,
}

impl DbConfig {
    pub fn clickhouse(
        url: String,
        database: String,
        user: String,
        password: String,
        dry_run: bool,
    ) -> Self {
        Self {
            backend: DbBackend::ClickHouse,
            url,
            database,
            user,
            password,
            dry_run,
        }
    }

    #[cfg(feature = "duckdb")]
    pub fn duckdb(path: String, database: String, dry_run: bool) -> Self {
        Self {
            backend: DbBackend::DuckDb,
            url: path,
            database,
            user: String::new(),
            password: String::new(),
            dry_run,
        }
    }

    pub fn from_backend(
        backend: &str,
        ch_url: String,
        _duckdb_path: String,
        database: String,
        user: String,
        password: String,
        dry_run: bool,
    ) -> Result<Self> {
        match backend.to_ascii_lowercase().as_str() {
            "clickhouse" | "ch" => Ok(Self::clickhouse(ch_url, database, user, password, dry_run)),
            #[cfg(feature = "duckdb")]
            "duckdb" | "duck" => Ok(Self::duckdb(_duckdb_path, database, dry_run)),
            #[cfg(not(feature = "duckdb"))]
            "duckdb" | "duck" => anyhow::bail!(
                "DuckDB backend requires building hft-collector with --features duckdb"
            ),
            other => anyhow::bail!("unsupported DB backend: {}", other),
        }
    }

    pub fn is_clickhouse(&self) -> bool {
        matches!(self.backend, DbBackend::ClickHouse)
    }

    fn from_env(default_db: &str) -> Self {
        let url =
            std::env::var("CLICKHOUSE_URL").unwrap_or_else(|_| "http://localhost:8123".to_string());
        let database = std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| default_db.to_string());
        let user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string());
        let password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_default();
        let dry_run = matches!(
            std::env::var("COLLECTOR_DRY_RUN")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );
        Self::from_backend(
            &std::env::var("DB_BACKEND").unwrap_or_else(|_| "clickhouse".to_string()),
            url,
            std::env::var("DUCKDB_PATH").unwrap_or_else(|_| "data/hft.duckdb".to_string()),
            database,
            user,
            password,
            dry_run,
        )
        .unwrap_or_else(|err| {
            tracing::warn!("DB_BACKEND invalid, falling back to ClickHouse: {}", err);
            Self::clickhouse(
                "http://localhost:8123".to_string(),
                default_db.to_string(),
                "default".to_string(),
                String::new(),
                dry_run,
            )
        })
    }
}

pub fn init_db_config(config: DbConfig) {
    let _ = DB_CONFIG.set(config);
}

fn current_config(default_db: &str) -> DbConfig {
    DB_CONFIG
        .get()
        .cloned()
        .unwrap_or_else(|| DbConfig::from_env(default_db))
}

/// 簡單的資料庫抽象，目前僅實作 ClickHouse HTTP(JSONEachRow)，
/// 後續可擴充為不同後端（如 Kafka、S3、其他時序庫）。
pub trait DbSink: Send + Sync {
    /// 將多筆 JSONEachRow 寫入指定資料表（不含資料庫前綴）。
    fn database(&self) -> &str;
    fn table_full_name(&self, table: &str) -> String {
        format!("{}.{}", self.database(), table)
    }

    /// 插入一批 JSONEachRow。rows 為已序列化的 JSONEachRow 字串，每列一行。
    /// 回傳已嘗試寫入的筆數（不保證成功筆數）。
    fn insert_json_rows<'a>(
        &'a self,
        table: &'a str,
        rows: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<usize>> + Send + 'a>>;
}

/// 從環境變數生成預設資料庫 sink。
pub async fn get_sink_async(default_db: &str) -> Result<Arc<DynSink>> {
    let config = current_config(default_db);
    let db_name = if config.database.is_empty() {
        default_db.to_string()
    } else {
        config.database.clone()
    };

    if config.dry_run {
        return Ok(Arc::new(NoopSink::new(&db_name)) as Arc<DynSink>);
    }

    let mut cache = SINK_CACHE.lock().await;
    if let Some(existing) = cache.get(&db_name) {
        return Ok(existing.clone());
    }

    let sink: Arc<DynSink> = match config.backend {
        DbBackend::ClickHouse => Arc::new(ClickHouseHttpSink::from_config(&config, &db_name)),
        #[cfg(feature = "duckdb")]
        DbBackend::DuckDb => Arc::new(DuckDbSink::from_config(&config, &db_name)?),
    };
    cache.insert(db_name.clone(), sink.clone());
    Ok(sink)
}

#[cfg(feature = "duckdb")]
pub struct DuckDbSink {
    db: String,
    conn: Mutex<Connection>,
}

#[cfg(feature = "duckdb")]
impl DuckDbSink {
    pub fn from_config(config: &DbConfig, database: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(&config.url).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(Self {
            db: database.to_string(),
            conn: Mutex::new(Connection::open(&config.url)?),
        })
    }

    fn safe_table_name(table: &str) -> String {
        let mut out = String::with_capacity(table.len());
        for ch in table.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        if out.is_empty() || out.as_bytes()[0].is_ascii_digit() {
            out.insert_str(0, "t_");
        }
        out
    }
}

#[cfg(feature = "duckdb")]
impl DbSink for DuckDbSink {
    fn database(&self) -> &str {
        &self.db
    }

    fn table_full_name(&self, table: &str) -> String {
        Self::safe_table_name(table)
    }

    fn insert_json_rows<'a>(
        &'a self,
        table: &'a str,
        rows: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            if rows.is_empty() {
                return Ok(0);
            }

            let table = Self::safe_table_name(table);
            let mut conn = self
                .conn
                .lock()
                .map_err(|_| anyhow::anyhow!("DuckDB connection lock poisoned"))?;
            conn.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {} (ingested_at_ms BIGINT, payload TEXT)",
                    table
                ),
                [],
            )?;
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(&format!(
                    "INSERT INTO {} (ingested_at_ms, payload) VALUES (?, ?)",
                    table
                ))?;
                let now_ms = chrono::Utc::now().timestamp_millis();
                for row in rows {
                    stmt.execute(params![now_ms, row])?;
                }
            }
            tx.commit()?;
            Ok(rows.len())
        })
    }
}

/// ClickHouse HTTP JSONEachRow 後端
pub struct ClickHouseHttpSink {
    url: String,
    db: String,
    user: String,
    password: String,
    http: Client,
}

impl ClickHouseHttpSink {
    pub fn from_config(config: &DbConfig, database: &str) -> Self {
        let http = Client::builder()
            .pool_max_idle_per_host(10)
            .tcp_keepalive(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            url: config.url.clone(),
            db: database.to_string(),
            user: config.user.clone(),
            password: config.password.clone(),
            http,
        }
    }

    fn build_body(rows: &[String]) -> String {
        let estimated: usize = rows.iter().map(|r| r.len() + 1).sum();
        let mut body = String::with_capacity(estimated);
        for line in rows {
            body.push_str(line);
            body.push('\n');
        }
        body
    }

    /// LZ4压缩数据（用于减少网络传输）
    fn compress_lz4(data: &str) -> Result<Vec<u8>> {
        let mut encoder = lz4::EncoderBuilder::new()
            .level(1) // 使用最快压缩级别，适合实时数据
            .build(Vec::new())?;
        encoder.write_all(data.as_bytes())?;
        let (compressed, result) = encoder.finish();
        result?;
        Ok(compressed)
    }
}

impl DbSink for ClickHouseHttpSink {
    fn database(&self) -> &str {
        &self.db
    }

    fn insert_json_rows<'a>(
        &'a self,
        table: &'a str,
        rows: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            if rows.is_empty() {
                return Ok(0);
            }
            let body_str = Self::build_body(rows);
            let table_full = self.table_full_name(table);
            let query = format!("INSERT INTO {} FORMAT JSONEachRow", table_full);

            // 使用LZ4压缩数据
            let body = match Self::compress_lz4(&body_str) {
                Ok(compressed) => compressed,
                Err(e) => {
                    tracing::warn!("LZ4压缩失败，使用未压缩数据: {}", e);
                    body_str.into_bytes()
                }
            };

            // Best Effort：5s 超时，不重试；失败计数并继续
            let resp = self
                .http
                .post(format!(
                    "{}/?query={}",
                    self.url,
                    urlencoding::encode(&query)
                ))
                .basic_auth(
                    &self.user,
                    if self.password.is_empty() {
                        None
                    } else {
                        Some(&self.password)
                    },
                )
                .header("Content-Encoding", "lz4") // 告知ClickHouse使用LZ4解压
                .body(body)
                .send()
                .await;

            match resp {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        tracing::error!(
                            "Failed to insert {} rows to {}: HTTP {} - {}",
                            rows.len(),
                            table_full,
                            status,
                            body
                        );
                        Ok(0)
                    } else {
                        Ok(rows.len())
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Request error inserting {} rows to {}: {}",
                        rows.len(),
                        table_full,
                        e
                    );
                    Ok(0)
                }
            }
        })
    }
}

/// 不執行任何寫入的假 Sink（dry-run）
pub struct NoopSink {
    db: String,
}

impl NoopSink {
    pub fn new(default_db: &str) -> Self {
        Self {
            db: default_db.to_string(),
        }
    }
}

impl DbSink for NoopSink {
    fn database(&self) -> &str {
        &self.db
    }
    fn insert_json_rows<'a>(
        &'a self,
        _table: &'a str,
        rows: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<usize>> + Send + 'a>> {
        Box::pin(async move { Ok(rows.len()) })
    }
}

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn duckdb_sink_writes_json_rows() {
        let path = std::env::temp_dir().join(format!(
            "hft_collector_test_{}_{}.duckdb",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let config = DbConfig::duckdb(path.to_string_lossy().to_string(), "hft_test".into(), false);
        let sink = DuckDbSink::from_config(&config, "hft_test").expect("open duckdb");
        let rows = vec![r#"{"symbol":"TSLABUSDT","price":396.24}"#.to_string()];

        assert_eq!(
            sink.insert_json_rows("binance_orderbook", &rows)
                .await
                .expect("insert"),
            1
        );

        let count: i64 = sink
            .conn
            .lock()
            .expect("lock")
            .query_row("SELECT count(*) FROM binance_orderbook", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 1);

        let _ = std::fs::remove_file(path);
    }
}
