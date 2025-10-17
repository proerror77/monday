use std::collections::HashMap;
use std::sync::Arc;

use engine::Engine;
use hft_infer_onnx::OnnxPredictor;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

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

async fn run_infer_worker(
    engine_arc: Arc<Mutex<Engine>>,
    model_path: &str,
    k: usize,
    l: usize,
    step_ms: u64,
) -> anyhow::Result<()> {
    let predictor = OnnxPredictor::load(model_path, (1, 4, l, k))?;
    let mut windows: HashMap<String, Vec<(Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>)>> =
        HashMap::new();
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
                bid_px_rel.push((bp - mid as f32) as f32);
                ask_px_rel.push((ap - mid as f32) as f32);
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
                for t in 0..l {
                    let (ref bpr, ref bql, ref apr, ref aql) = entry[t];
                    flat.extend_from_slice(&bpr[..]);
                    flat.extend_from_slice(&bql[..]);
                    flat.extend_from_slice(&apr[..]);
                    flat.extend_from_slice(&aql[..]);
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
