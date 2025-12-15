//! File system watcher for model hot-reload

use crate::{ModelError, ModelHandle, ModelLoadedCallback, ModelMetadata, ModelResult};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// 文件監控器 - 監控模型目錄變化
pub struct ModelWatcher {
    /// 監控的目錄
    watch_dir: PathBuf,

    /// 當前模型引用
    current_model: Arc<RwLock<Option<ModelHandle>>>,

    /// 內部 watcher
    _watcher: RecommendedWatcher,

    /// 事件接收通道
    event_rx: mpsc::Receiver<ModelWatchEvent>,
}

/// 監控事件
#[derive(Debug, Clone)]
pub enum ModelWatchEvent {
    /// 新模型文件創建
    Created(PathBuf),
    /// 模型文件更新
    Modified(PathBuf),
    /// 模型文件刪除
    Removed(PathBuf),
    /// 錯誤
    Error(String),
}

impl ModelWatcher {
    /// 創建新的文件監控器
    pub fn new(
        watch_dir: PathBuf,
        current_model: Arc<RwLock<Option<ModelHandle>>>,
        _on_loaded: Option<ModelLoadedCallback>,
    ) -> ModelResult<Self> {
        let (event_tx, event_rx) = mpsc::channel(100);

        // 創建 notify watcher
        let tx = event_tx.clone();
        let watch_dir_clone = watch_dir.clone();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                let event = match res {
                    Ok(event) => Self::convert_event(&watch_dir_clone, event),
                    Err(e) => Some(ModelWatchEvent::Error(e.to_string())),
                };

                if let Some(evt) = event {
                    let _ = tx.blocking_send(evt);
                }
            },
            Config::default(),
        )?;

        // 開始監控
        watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

        info!("ModelWatcher started monitoring {:?}", watch_dir);

        Ok(Self {
            watch_dir,
            current_model,
            _watcher: watcher,
            event_rx,
        })
    }

    /// 轉換 notify 事件為內部事件
    fn convert_event(_watch_dir: &PathBuf, event: Event) -> Option<ModelWatchEvent> {
        // 只關心 .pt 和 .onnx 文件
        let is_model_file = |path: &PathBuf| {
            path.extension()
                .map(|ext| ext == "pt" || ext == "onnx")
                .unwrap_or(false)
        };

        let model_paths: Vec<_> = event
            .paths
            .iter()
            .filter(|p| is_model_file(p))
            .cloned()
            .collect();

        if model_paths.is_empty() {
            return None;
        }

        let path = model_paths.first()?.clone();

        match event.kind {
            EventKind::Create(_) => {
                debug!("Model file created: {:?}", path);
                Some(ModelWatchEvent::Created(path))
            }
            EventKind::Modify(_) => {
                debug!("Model file modified: {:?}", path);
                Some(ModelWatchEvent::Modified(path))
            }
            EventKind::Remove(_) => {
                debug!("Model file removed: {:?}", path);
                Some(ModelWatchEvent::Removed(path))
            }
            _ => None,
        }
    }

    /// 運行監控循環
    pub async fn run(mut self) -> ModelResult<()> {
        info!("ModelWatcher running...");

        while let Some(event) = self.event_rx.recv().await {
            match event {
                ModelWatchEvent::Created(path) | ModelWatchEvent::Modified(path) => {
                    info!("Detected model change: {:?}", path);
                    if let Err(e) = self.handle_model_update(&path).await {
                        error!("Failed to handle model update: {}", e);
                    }
                }
                ModelWatchEvent::Removed(path) => {
                    warn!("Model file removed: {:?}", path);
                    // 可選：清除當前模型引用
                }
                ModelWatchEvent::Error(msg) => {
                    error!("Watch error: {}", msg);
                }
            }
        }

        Ok(())
    }

    /// 處理模型更新
    async fn handle_model_update(&self, path: &PathBuf) -> ModelResult<()> {
        // 驗證文件存在且可讀
        if !path.exists() {
            return Err(ModelError::NotFound(path.display().to_string()));
        }

        // 讀取元數據
        let metadata_path = self.watch_dir.parent()
            .map(|p| p.join("metadata.json"))
            .unwrap_or_else(|| self.watch_dir.join("../metadata.json"));

        let metadata = if metadata_path.exists() {
            let content = tokio::fs::read_to_string(&metadata_path).await?;
            serde_json::from_str(&content)?
        } else {
            ModelMetadata::default()
        };

        // 創建新的 ModelHandle
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

        info!(
            "Model hot-reloaded: {:?} (version: {})",
            path, handle.version
        );

        Ok(())
    }

    /// 獲取當前模型
    pub async fn get_current(&self) -> Option<ModelHandle> {
        self.current_model.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_watcher_creation() {
        let temp_dir = TempDir::new().unwrap();
        let current_model = Arc::new(RwLock::new(None));

        let watcher = ModelWatcher::new(
            temp_dir.path().to_path_buf(),
            current_model,
            None,
        );

        assert!(watcher.is_ok());
    }
}
