/*!
 * ClickHouse 客戶端實現
 * 
 * 高性能時序數據寫入和查詢
 */

use clickhouse::Client;
use anyhow::Result;
use tracing::{info, error};

/// ClickHouse 客戶端封裝
pub struct ClickHouseClient {
    client: Client,
    url: String,
}

impl ClickHouseClient {
    /// 創建新的 ClickHouse 客戶端
    pub fn new(url: String) -> Self {
        let client = Client::default().with_url(url.clone());
        
        Self {
            client,
            url,
        }
    }
    
    /// 測試連接
    pub async fn test_connection(&self) -> Result<()> {
        let result: Vec<u8> = self.client
            .query("SELECT 1")
            .fetch_all()
            .await?;
            
        info!("ClickHouse 連接測試成功");
        Ok(())
    }
    
    /// 創建表
    pub async fn create_table(&self, table_name: &str, schema: &str) -> Result<()> {
        let query = format!("CREATE TABLE IF NOT EXISTS {} {}", table_name, schema);
        
        self.client.query(&query).execute().await?;
        
        info!("表 {} 創建成功", table_name);
        Ok(())
    }
    
    /// 插入數據
    pub async fn insert_data(&self, table_name: &str, data: &str) -> Result<()> {
        let query = format!("INSERT INTO {} FORMAT JSONEachRow", table_name);
        
        self.client
            .query(&query)
            .with_option("input_format_import_nested_json", "1")
            .with_option("allow_experimental_object_type", "1")
            .bind(data)
            .execute()
            .await?;
            
        Ok(())
    }
    
    /// 查詢數據
    pub async fn query(&self, sql: &str) -> Result<String> {
        let result = self.client
            .query(sql)
            .with_option("output_format_json_named_tuples_as_objects", "1")
            .fetch_all::<String>()
            .await?;
            
        Ok(result.join("\n"))
    }
}