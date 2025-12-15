//! Model Manager - 模型熱加載管理器
//!
//! 負責監控模型目錄，自動熱加載新模型到交易系統。
//! 設計原則：
//! - 文件系統取代複雜的 Redis/gRPC 通信
//! - 原子切換，無縫替換運行中的模型
//! - 版本管理和自動回滾

mod error;
mod metadata;
mod watcher;

pub use error::{ModelError, ModelResult};
pub use metadata::{ModelMetadata, ModelVersion};
pub use watcher::ModelWatcher;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// 模型句柄 - 包含模型路徑和元數據
#[derive(Debug, Clone)]
pub struct ModelHandle {
    pub path: PathBuf,
    pub version: ModelVersion,
    pub metadata: ModelMetadata,
    pub loaded_at: chrono::DateTime<chrono::Utc>,
}

/// 模型管理器 - 核心組件
///
/// 功能：
/// - 監控 models/current/ 目錄的文件變化
/// - 自動熱加載新模型
/// - 版本管理和回滾
pub struct ModelManager {
    /// 模型目錄根路徑
    model_dir: PathBuf,

    /// 當前加載的模型
    current_model: Arc<RwLock<Option<ModelHandle>>>,

    /// 模型加載回調（用於通知策略層）
    on_model_loaded: Option<Box<dyn Fn(&ModelHandle) + Send + Sync>>,
}

impl ModelManager {
    /// 創建新的模型管理器
    ///
    /// # Arguments
    /// * `model_dir` - 模型目錄根路徑 (e.g., "/opt/hft/models")
    ///
    /// # 目錄結構
    /// ```text
    /// models/
    /// ├── current/           # 當前使用的模型
    /// │   └── strategy_dl.pt
    /// ├── archive/           # 歷史版本
    /// │   ├── v1_20250101.pt
    /// │   └── v2_20250102.pt
    /// └── metadata.json      # 模型元數據
    /// ```
    pub fn new<P: AsRef<Path>>(model_dir: P) -> Self {
        Self {
            model_dir: model_dir.as_ref().to_path_buf(),
            current_model: Arc::new(RwLock::new(None)),
            on_model_loaded: None,
        }
    }

    /// 設置模型加載回調
    pub fn on_model_loaded<F>(mut self, callback: F) -> Self
    where
        F: Fn(&ModelHandle) + Send + Sync + 'static,
    {
        self.on_model_loaded = Some(Box::new(callback));
        self
    }

    /// 獲取當前目錄路徑
    pub fn current_dir(&self) -> PathBuf {
        self.model_dir.join("current")
    }

    /// 獲取歸檔目錄路徑
    pub fn archive_dir(&self) -> PathBuf {
        self.model_dir.join("archive")
    }

    /// 獲取元數據文件路徑
    pub fn metadata_path(&self) -> PathBuf {
        self.model_dir.join("metadata.json")
    }

    /// 初始化目錄結構
    pub async fn init(&self) -> ModelResult<()> {
        tokio::fs::create_dir_all(self.current_dir()).await?;
        tokio::fs::create_dir_all(self.archive_dir()).await?;

        // 創建默認元數據文件（如果不存在）
        if !self.metadata_path().exists() {
            let metadata = ModelMetadata::default();
            let json = serde_json::to_string_pretty(&metadata)?;
            tokio::fs::write(self.metadata_path(), json).await?;
        }

        info!("ModelManager initialized at {:?}", self.model_dir);
        Ok(())
    }

    /// 加載當前模型
    pub async fn load_current(&self) -> ModelResult<Option<ModelHandle>> {
        let current_dir = self.current_dir();

        // 查找 .pt 或 .onnx 文件
        let mut entries = tokio::fs::read_dir(&current_dir).await?;
        let mut model_path: Option<PathBuf> = None;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "pt" || ext == "onnx" {
                    model_path = Some(path);
                    break;
                }
            }
        }

        let Some(path) = model_path else {
            info!("No model found in {:?}", current_dir);
            return Ok(None);
        };

        // 讀取元數據
        let metadata = self.load_metadata().await?;

        let handle = ModelHandle {
            path: path.clone(),
            version: metadata.current_version.clone(),
            metadata,
            loaded_at: chrono::Utc::now(),
        };

        // 更新當前模型
        {
            let mut current = self.current_model.write().await;
            *current = Some(handle.clone());
        }

        info!("Loaded model: {:?} (version: {})", path, handle.version);

        // 觸發回調
        if let Some(ref callback) = self.on_model_loaded {
            callback(&handle);
        }

        Ok(Some(handle))
    }

    /// 加載元數據
    async fn load_metadata(&self) -> ModelResult<ModelMetadata> {
        let path = self.metadata_path();
        if path.exists() {
            let content = tokio::fs::read_to_string(&path).await?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(ModelMetadata::default())
        }
    }

    /// 保存元數據
    ///
    /// 用於模型部署時更新元數據，目前由外部部署流程調用
    #[allow(dead_code)]
    pub async fn save_metadata(&self, metadata: &ModelMetadata) -> ModelResult<()> {
        let json = serde_json::to_string_pretty(metadata)?;
        tokio::fs::write(self.metadata_path(), json).await?;
        Ok(())
    }

    /// 歸檔當前模型
    pub async fn archive_current(&self) -> ModelResult<Option<PathBuf>> {
        let current = self.current_model.read().await;
        let Some(handle) = current.as_ref() else {
            return Ok(None);
        };

        let archive_name = format!(
            "{}_{}.{}",
            handle.version,
            chrono::Utc::now().format("%Y%m%d_%H%M%S"),
            handle.path.extension().and_then(|s| s.to_str()).unwrap_or("pt")
        );

        let archive_path = self.archive_dir().join(&archive_name);
        tokio::fs::copy(&handle.path, &archive_path).await?;

        info!("Archived model to {:?}", archive_path);
        Ok(Some(archive_path))
    }

    /// 回滾到上一版本
    pub async fn rollback(&self) -> ModelResult<Option<ModelHandle>> {
        // 讀取歸檔目錄，找到最新的歸檔
        let archive_dir = self.archive_dir();
        let mut entries = tokio::fs::read_dir(&archive_dir).await?;

        let mut latest: Option<(PathBuf, std::time::SystemTime)> = None;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "pt" || ext == "onnx" {
                    let metadata = entry.metadata().await?;
                    let modified = metadata.modified()?;

                    match &latest {
                        None => latest = Some((path, modified)),
                        Some((_, prev_time)) if modified > *prev_time => {
                            latest = Some((path, modified));
                        }
                        _ => {}
                    }
                }
            }
        }

        let Some((archive_path, _)) = latest else {
            warn!("No archived model found for rollback");
            return Ok(None);
        };

        // 複製歸檔到 current
        let current_path = self.current_dir().join(
            archive_path.file_name().unwrap_or_default()
        );

        tokio::fs::copy(&archive_path, &current_path).await?;

        info!("Rolled back to {:?}", archive_path);

        // 重新加載
        self.load_current().await
    }

    /// 獲取當前模型句柄
    pub async fn get_current(&self) -> Option<ModelHandle> {
        self.current_model.read().await.clone()
    }

    /// 啟動文件監控
    pub async fn watch(&self) -> ModelResult<ModelWatcher> {
        let watcher = ModelWatcher::new(
            self.current_dir(),
            Arc::clone(&self.current_model),
            self.on_model_loaded.as_ref().map(|_| {
                // 創建一個簡單的通知通道
                Box::new(|_: &ModelHandle| {}) as Box<dyn Fn(&ModelHandle) + Send + Sync>
            }),
        )?;

        Ok(watcher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_model_manager_init() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ModelManager::new(temp_dir.path());

        manager.init().await.unwrap();

        assert!(manager.current_dir().exists());
        assert!(manager.archive_dir().exists());
        assert!(manager.metadata_path().exists());
    }

    #[tokio::test]
    async fn test_load_no_model() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ModelManager::new(temp_dir.path());
        manager.init().await.unwrap();

        let result = manager.load_current().await.unwrap();
        assert!(result.is_none());
    }
}
