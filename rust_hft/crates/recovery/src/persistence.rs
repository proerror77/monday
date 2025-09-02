//! State persistence module
//! 
//! Handles saving and loading system snapshots to/from disk

use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use chrono::{DateTime, Utc};
use tracing::{info, warn, error, debug};

use crate::{SystemSnapshot, RecoveryConfig, RecoveryError, RecoveryResult};

/// Handles persistence of system snapshots
pub struct StatePersistence {
    config: RecoveryConfig,
}

impl StatePersistence {
    pub fn new(config: RecoveryConfig) -> Self {
        Self { config }
    }
    
    /// Initialize state directory
    pub async fn init(&self) -> RecoveryResult<()> {
        if !self.config.state_dir.exists() {
            fs::create_dir_all(&self.config.state_dir).await?;
            info!("Created state directory: {:?}", self.config.state_dir);
        }
        Ok(())
    }
    
    /// Save system snapshot to disk
    pub async fn save_snapshot(&self, snapshot: &SystemSnapshot) -> RecoveryResult<PathBuf> {
        let filename = format!("snapshot_{}.json", snapshot.timestamp.format("%Y%m%d_%H%M%S"));
        let file_path = self.config.state_dir.join(&filename);
        
        debug!("Saving snapshot to: {:?}", file_path);
        
        // Serialize snapshot to JSON
        let json_data = serde_json::to_string_pretty(snapshot)?;
        
        // Write to temporary file first, then rename (atomic operation)
        let temp_path = file_path.with_extension("json.tmp");
        let mut file = fs::File::create(&temp_path).await?;
        file.write_all(json_data.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        
        // Atomic rename
        fs::rename(&temp_path, &file_path).await?;
        
        info!("Snapshot saved: {} (ID: {}, {} orders, {} positions)",
              filename,
              snapshot.snapshot_id,
              snapshot.oms_state.len(),
              snapshot.portfolio_state.account_view.positions.len());
        
        // Cleanup old snapshots
        self.cleanup_old_snapshots().await?;
        
        Ok(file_path)
    }
    
    /// Load the most recent valid snapshot
    pub async fn load_latest_snapshot(&self) -> RecoveryResult<Option<SystemSnapshot>> {
        let snapshots = self.list_snapshots().await?;
        
        for snapshot_path in snapshots {
            match self.load_snapshot(&snapshot_path).await {
                Ok(snapshot) => {
                    if snapshot.is_recent(self.config.max_snapshot_age) {
                        info!("Loaded snapshot: {:?} (age: {} minutes)",
                              snapshot_path.file_name(),
                              snapshot.age().num_minutes());
                        return Ok(Some(snapshot));
                    } else {
                        warn!("Snapshot too old, skipping: {:?} (age: {} hours)",
                              snapshot_path.file_name(),
                              snapshot.age().num_hours());
                    }
                }
                Err(e) => {
                    error!("Failed to load snapshot {:?}: {}", snapshot_path, e);
                }
            }
        }
        
        Ok(None)
    }
    
    /// Load specific snapshot from file
    pub async fn load_snapshot<P: AsRef<Path>>(&self, path: P) -> RecoveryResult<SystemSnapshot> {
        let path = path.as_ref();
        debug!("Loading snapshot from: {:?}", path);
        
        let mut file = fs::File::open(path).await?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).await?;
        
        let snapshot: SystemSnapshot = serde_json::from_str(&contents)?;
        
        Ok(snapshot)
    }
    
    /// List all snapshots sorted by modification time (newest first)
    pub async fn list_snapshots(&self) -> RecoveryResult<Vec<PathBuf>> {
        let mut snapshots = Vec::new();
        
        if !self.config.state_dir.exists() {
            return Ok(snapshots);
        }
        
        let mut dir = fs::read_dir(&self.config.state_dir).await?;
        
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            
            // Only consider .json files that look like snapshots
            if let Some(extension) = path.extension() {
                if extension == "json" {
                    if let Some(filename) = path.file_name() {
                        if filename.to_string_lossy().starts_with("snapshot_") {
                            snapshots.push(path);
                        }
                    }
                }
            }
        }
        
        // Sort by modification time, newest first
        snapshots.sort_by(|a, b| {
            let a_meta = std::fs::metadata(a).ok();
            let b_meta = std::fs::metadata(b).ok();
            
            match (a_meta, b_meta) {
                (Some(a_meta), Some(b_meta)) => {
                    b_meta.modified().unwrap_or(std::time::UNIX_EPOCH)
                        .cmp(&a_meta.modified().unwrap_or(std::time::UNIX_EPOCH))
                }
                _ => std::cmp::Ordering::Equal,
            }
        });
        
        Ok(snapshots)
    }
    
    /// Remove old snapshots beyond the configured limit
    async fn cleanup_old_snapshots(&self) -> RecoveryResult<()> {
        let snapshots = self.list_snapshots().await?;
        
        if snapshots.len() > self.config.max_snapshots {
            let to_remove = &snapshots[self.config.max_snapshots..];
            
            for snapshot_path in to_remove {
                match fs::remove_file(snapshot_path).await {
                    Ok(_) => info!("Removed old snapshot: {:?}", snapshot_path.file_name()),
                    Err(e) => warn!("Failed to remove old snapshot {:?}: {}", snapshot_path, e),
                }
            }
        }
        
        Ok(())
    }
    
    /// Get snapshot statistics
    pub async fn get_stats(&self) -> RecoveryResult<SnapshotStats> {
        let snapshots = self.list_snapshots().await?;
        let mut total_size = 0u64;
        let mut oldest: Option<DateTime<Utc>> = None;
        let mut newest: Option<DateTime<Utc>> = None;
        
        for snapshot_path in &snapshots {
            if let Ok(metadata) = fs::metadata(snapshot_path).await {
                total_size += metadata.len();
            }
            
            if let Ok(snapshot) = self.load_snapshot(snapshot_path).await {
                if oldest.is_none() || snapshot.timestamp < oldest.unwrap() {
                    oldest = Some(snapshot.timestamp);
                }
                if newest.is_none() || snapshot.timestamp > newest.unwrap() {
                    newest = Some(snapshot.timestamp);
                }
            }
        }
        
        Ok(SnapshotStats {
            total_snapshots: snapshots.len(),
            total_size_bytes: total_size,
            oldest_snapshot: oldest,
            newest_snapshot: newest,
        })
    }
}

/// Statistics about stored snapshots
#[derive(Debug, Clone)]
pub struct SnapshotStats {
    pub total_snapshots: usize,
    pub total_size_bytes: u64,
    pub oldest_snapshot: Option<DateTime<Utc>>,
    pub newest_snapshot: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::collections::HashMap;
    
    async fn create_test_persistence() -> (StatePersistence, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let config = RecoveryConfig {
            state_dir: temp_dir.path().to_path_buf(),
            max_snapshots: 3,
            ..Default::default()
        };
        let persistence = StatePersistence::new(config);
        persistence.init().await.unwrap();
        (persistence, temp_dir)
    }
    
    #[tokio::test]
    async fn test_save_and_load_snapshot() {
        let (persistence, _temp_dir) = create_test_persistence().await;
        
        // Create test snapshot
        let oms_state = HashMap::new();
        let portfolio_state = portfolio_core::PortfolioState {
            account_view: ports::AccountView::default(),
            order_meta: HashMap::new(),
            market_prices: HashMap::new(),
        };
        let snapshot = SystemSnapshot::new(oms_state, portfolio_state, "test-v1.0.0".to_string());
        
        // Save snapshot
        let path = persistence.save_snapshot(&snapshot).await.unwrap();
        assert!(path.exists());
        
        // Load snapshot back
        let loaded = persistence.load_snapshot(&path).await.unwrap();
        assert_eq!(loaded.snapshot_id, snapshot.snapshot_id);
        assert_eq!(loaded.system_version, snapshot.system_version);
    }
    
    #[tokio::test]
    async fn test_load_latest_snapshot() {
        let (persistence, _temp_dir) = create_test_persistence().await;
        
        // Initially no snapshots
        let latest = persistence.load_latest_snapshot().await.unwrap();
        assert!(latest.is_none());
        
        // Save a snapshot
        let oms_state = HashMap::new();
        let portfolio_state = portfolio_core::PortfolioState {
            account_view: ports::AccountView::default(),
            order_meta: HashMap::new(),
            market_prices: HashMap::new(),
        };
        let snapshot = SystemSnapshot::new(oms_state, portfolio_state, "test-v1.0.0".to_string());
        persistence.save_snapshot(&snapshot).await.unwrap();
        
        // Should load the snapshot
        let latest = persistence.load_latest_snapshot().await.unwrap();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().snapshot_id, snapshot.snapshot_id);
    }
    
    #[tokio::test]
    async fn test_cleanup_old_snapshots() {
        let (persistence, _temp_dir) = create_test_persistence().await;
        
        // Save more snapshots than the limit (3)
        for i in 0..5 {
            let oms_state = HashMap::new();
            let portfolio_state = portfolio_core::PortfolioState {
                account_view: ports::AccountView::default(),
                order_meta: HashMap::new(),
                market_prices: HashMap::new(),
            };
            let snapshot = SystemSnapshot::new(oms_state, portfolio_state, format!("test-v1.{}.0", i));
            persistence.save_snapshot(&snapshot).await.unwrap();
            
            // Small delay to ensure different timestamps
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        
        // Should only have 3 snapshots (the limit)
        let snapshots = persistence.list_snapshots().await.unwrap();
        assert_eq!(snapshots.len(), 3);
    }
}