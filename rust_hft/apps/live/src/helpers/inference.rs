//! ONNX 推理 Worker - 支持模型熱加載
//!
//! 使用 ModelManager 監視模型變更，自動重新加載

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use engine::Engine;
use hft_infer_onnx::OnnxPredictor;
use hft_model_manager::{ModelManager, ModelWatcher};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{info, warn, error};

// Type alias for complex window data structure
type WindowEntry = Vec<(Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>)>;

/// 推理配置
pub struct InferenceConfig {
    pub model_dir: PathBuf,
    pub k: usize,      // Top-K 檔數
    pub l: usize,      // 序列長度
    pub step_ms: u64,  // 推理步長
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            model_dir: PathBuf::from("models"),
            k: 20,
            l: 100,
            step_ms: 100,
        }
    }
}

/// 啟動帶模型熱加載的推理 Worker
/// (保留供未來 gRPC 熱加載接口使用)
#[allow(dead_code)]
pub fn spawn_inference_worker_with_hot_reload(
    engine_arc: Arc<Mutex<Engine>>,
    config: InferenceConfig,
) -> JoinHandle<()> {
    info!(
        "啟用 ONNX 推理 (熱加載): model_dir={:?}, K={}, L={}, step_ms={}",
        config.model_dir, config.k, config.l, config.step_ms
    );

    tokio::spawn(async move {
        if let Err(e) = run_infer_worker_hot_reload(engine_arc, config).await {
            error!("推理 worker 失敗: {}", e);
        }
    })
}

/// 原始推理 Worker (無熱加載，保持向後兼容)
pub fn spawn_inference_worker(
    engine_arc: Arc<Mutex<Engine>>,
    model_path: String,
    k: usize,
    l: usize,
    step_ms: u64,
) -> JoinHandle<()> {
    info!(
        "啟用 ONNX 推理: model={}, K={}, L={}, step_ms={}",
        model_path, k, l, step_ms
    );

    tokio::spawn(async move {
        if let Err(e) = run_infer_worker(engine_arc, &model_path, k, l, step_ms).await {
            warn!("推理 worker 失敗: {}", e);
        }
    })
}

/// 帶熱加載的推理 Worker 主循環
#[allow(dead_code)]
async fn run_infer_worker_hot_reload(
    engine_arc: Arc<Mutex<Engine>>,
    config: InferenceConfig,
) -> anyhow::Result<()> {
    // 初始化 ModelManager
    let manager = ModelManager::new(&config.model_dir);
    manager.init().await?;

    // 嘗試加載當前模型
    let predictor: Arc<RwLock<Option<OnnxPredictor>>> = Arc::new(RwLock::new(None));

    if let Some(handle) = manager.load_current().await? {
        info!("載入初始模型: {:?}", handle.path);
        match OnnxPredictor::load(handle.path.to_str().unwrap(), (1, 4, config.l, config.k)) {
            Ok(p) => {
                *predictor.write().await = Some(p);
                info!("初始模型載入成功");
            }
            Err(e) => {
                warn!("初始模型載入失敗: {}", e);
            }
        }
    } else {
        info!("models/current 目錄下無模型，等待模型部署...");
    }

    // 創建共享的當前模型狀態
    let current_model = Arc::new(RwLock::new(None));

    // 啟動模型監視器
    let watch_dir = config.model_dir.join("current");
    let watcher = ModelWatcher::new(
        watch_dir.clone(),
        current_model.clone(),
        None,
    )?;

    let k = config.k;
    let l = config.l;

    // 監視器任務 - 檢測文件變化並重載模型
    tokio::spawn(async move {
        if let Err(e) = watcher.run().await {
            error!("ModelWatcher error: {}", e);
        }
    });

    // 模型重載輪詢任務 (每秒檢查一次 current_model 是否有變化)
    let current_model_for_reload = current_model.clone();
    let predictor_for_reload = predictor.clone();
    tokio::spawn(async move {
        let mut last_version = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            let model = current_model_for_reload.read().await;
            if let Some(ref handle) = *model {
                if handle.version.to_string() != last_version {
                    info!("檢測到新模型版本: {}", handle.version);

                    // 只處理 .onnx 文件
                    if handle.path.extension().map(|e| e == "onnx").unwrap_or(false) {
                        match OnnxPredictor::load(handle.path.to_str().unwrap(), (1, 4, l, k)) {
                            Ok(new_predictor) => {
                                *predictor_for_reload.write().await = Some(new_predictor);
                                last_version = handle.version.to_string();
                                info!("模型熱加載成功: {:?}", handle.path);
                            }
                            Err(e) => {
                                error!("模型熱加載失敗: {}", e);
                            }
                        }
                    }
                }
            }
        }
    });

    // 推理主循環
    let mut windows: HashMap<String, WindowEntry> = HashMap::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(config.step_ms));

    loop {
        interval.tick().await;

        // 檢查引擎狀態
        {
            let engine = engine_arc.lock().await;
            if !engine.get_statistics().is_running {
                break;
            }
        }

        // 獲取當前預測器
        let pred_guard = predictor.read().await;
        let Some(ref pred) = *pred_guard else {
            continue; // 無模型，跳過
        };

        // 獲取市場視圖
        let market_view = {
            let engine = engine_arc.lock().await;
            engine.get_market_view()
        };

        // 對每個交易對進行推理
        for (sym, ob) in &market_view.orderbooks {
            if ob.bid_prices.is_empty() || ob.ask_prices.is_empty() {
                continue;
            }

            let mid = (ob.bid_prices[0].to_f64() + ob.ask_prices[0].to_f64()) / 2.0;
            let mut bid_px_rel = Vec::with_capacity(config.k);
            let mut bid_qty_log = Vec::with_capacity(config.k);
            let mut ask_px_rel = Vec::with_capacity(config.k);
            let mut ask_qty_log = Vec::with_capacity(config.k);

            for i in 0..config.k {
                let bp = ob.bid_prices.get(i).map(|p| p.to_f64()).unwrap_or(0.0) as f32;
                let bq = ob.bid_quantities.get(i).map(|q| q.to_f64()).unwrap_or(0.0) as f32;
                let ap = ob.ask_prices.get(i).map(|p| p.to_f64()).unwrap_or(0.0) as f32;
                let aq = ob.ask_quantities.get(i).map(|q| q.to_f64()).unwrap_or(0.0) as f32;
                bid_px_rel.push(bp - mid as f32);
                ask_px_rel.push(ap - mid as f32);
                bid_qty_log.push((1.0 + bq).ln());
                ask_qty_log.push((1.0 + aq).ln());
            }

            let entry = windows.entry(sym.symbol.as_str().to_string()).or_default();
            entry.push((bid_px_rel, bid_qty_log, ask_px_rel, ask_qty_log));
            if entry.len() > config.l {
                entry.remove(0);
            }

            if entry.len() == config.l {
                let mut flat = Vec::with_capacity(4 * config.l * config.k);
                for (bpr, bql, apr, aql) in entry.iter() {
                    flat.extend_from_slice(bpr);
                    flat.extend_from_slice(bql);
                    flat.extend_from_slice(apr);
                    flat.extend_from_slice(aql);
                }

                match pred.infer(&flat) {
                    Ok(probs) => {
                        info!(symbol = %sym.symbol.as_str(), ts = market_view.timestamp, ?probs, "ONNX 推理");
                    }
                    Err(e) => {
                        warn!(symbol = %sym.symbol.as_str(), "推理失敗: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}

/// 原始推理 Worker (無熱加載)
async fn run_infer_worker(
    engine_arc: Arc<Mutex<Engine>>,
    model_path: &str,
    k: usize,
    l: usize,
    step_ms: u64,
) -> anyhow::Result<()> {
    let predictor = OnnxPredictor::load(model_path, (1, 4, l, k))?;
    let mut windows: HashMap<String, WindowEntry> = HashMap::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(step_ms));

    loop {
        interval.tick().await;

        {
            let engine = engine_arc.lock().await;
            if !engine.get_statistics().is_running {
                break;
            }
        }

        let market_view = {
            let engine = engine_arc.lock().await;
            engine.get_market_view()
        };

        for (sym, ob) in &market_view.orderbooks {
            if ob.bid_prices.is_empty() || ob.ask_prices.is_empty() {
                continue;
            }

            let mid = (ob.bid_prices[0].to_f64() + ob.ask_prices[0].to_f64()) / 2.0;
            let mut bid_px_rel = Vec::with_capacity(k);
            let mut bid_qty_log = Vec::with_capacity(k);
            let mut ask_px_rel = Vec::with_capacity(k);
            let mut ask_qty_log = Vec::with_capacity(k);

            for i in 0..k {
                let bp = ob.bid_prices.get(i).map(|p| p.to_f64()).unwrap_or(0.0) as f32;
                let bq = ob.bid_quantities.get(i).map(|q| q.to_f64()).unwrap_or(0.0) as f32;
                let ap = ob.ask_prices.get(i).map(|p| p.to_f64()).unwrap_or(0.0) as f32;
                let aq = ob.ask_quantities.get(i).map(|q| q.to_f64()).unwrap_or(0.0) as f32;
                bid_px_rel.push(bp - mid as f32);
                ask_px_rel.push(ap - mid as f32);
                bid_qty_log.push((1.0 + bq).ln());
                ask_qty_log.push((1.0 + aq).ln());
            }

            let entry = windows.entry(sym.symbol.as_str().to_string()).or_default();
            entry.push((bid_px_rel, bid_qty_log, ask_px_rel, ask_qty_log));
            if entry.len() > l {
                entry.remove(0);
            }

            if entry.len() == l {
                let mut flat = Vec::with_capacity(4 * l * k);
                for (bpr, bql, apr, aql) in entry.iter() {
                    flat.extend_from_slice(bpr);
                    flat.extend_from_slice(bql);
                    flat.extend_from_slice(apr);
                    flat.extend_from_slice(aql);
                }

                match predictor.infer(&flat) {
                    Ok(probs) => {
                        info!(symbol = %sym.symbol.as_str(), ts = market_view.timestamp, ?probs, "ONNX 推理");
                    }
                    Err(e) => {
                        warn!(symbol = %sym.symbol.as_str(), "推理失敗: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}
