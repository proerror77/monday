use anyhow::Result;
use clap::Parser;
use clickhouse::Client as ChClient;
use hft_infer_onnx::OnnxPredictor;
use ndarray::Array4;
use tracing::info;

#[derive(Parser, Debug)]
#[command(author, version, about = "Run ONNX inference on dataset_samples", long_about = None)]
struct Args {
    #[arg(long)]
    model: String,
    #[arg(long, default_value = "binance")]
    venue: String,
    #[arg(long)]
    symbol: String,
    #[arg(long)]
    start_us: u64,
    #[arg(long)]
    end_us: u64,
    #[arg(long, default_value_t = 20)]
    k: usize,
    #[arg(long, default_value_t = 100)]
    L: usize,
    #[arg(long, default_value = "http://localhost:8123")]
    ch_url: String,
    #[arg(long, default_value = "hft")]
    database: String,
}

#[derive(clickhouse::Row, serde::Deserialize)]
struct SampleRow {
    bid_px_rel: Vec<f32>,
    bid_qty_log: Vec<f32>,
    ask_px_rel: Vec<f32>,
    ask_qty_log: Vec<f32>,
    update_flag: Vec<u8>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();
    let ch = ChClient::default()
        .with_url(&args.ch_url)
        .with_database(&args.database);

    // 準備模型（固定 shape：N=1,C=4或5，L,K）這裡先用 4 通道
    let n = 1usize;
    let c = 4usize;
    let l = args.L;
    let k = args.k;
    let predictor = OnnxPredictor::load(&args.model, (n, c, l, k))?;

    // 讀一批樣本
    let q = "SELECT bid_px_rel, bid_qty_log, ask_px_rel, ask_qty_log, update_flag FROM dataset_samples \
            WHERE symbol=? AND venue=? AND ts>=? AND ts<=? ORDER BY ts LIMIT 10";
    let mut cur = ch
        .query(q)
        .bind(&args.symbol)
        .bind(&args.venue)
        .bind(args.start_us)
        .bind(args.end_us)
        .fetch::<SampleRow>()?;

    let mut i = 0usize;
    while let Some(r) = cur.next().await? {
        // 將 L*K 向量 reshape 成 (C,L,K)
        let mut x = Array4::<f32>::zeros((1, c, l, k));
        fill_channel(
            x.slice_mut(s![0, 0, .., ..]).as_array_mut(),
            &r.bid_px_rel,
            l,
            k,
        );
        fill_channel(
            x.slice_mut(s![0, 1, .., ..]).as_array_mut(),
            &r.bid_qty_log,
            l,
            k,
        );
        fill_channel(
            x.slice_mut(s![0, 2, .., ..]).as_array_mut(),
            &r.ask_px_rel,
            l,
            k,
        );
        fill_channel(
            x.slice_mut(s![0, 3, .., ..]).as_array_mut(),
            &r.ask_qty_log,
            l,
            k,
        );
        let probs = predictor.infer(&x)?; // 假設輸出為 [1,3] 類別概率
        info!("sample {} => probs={:?}", i, probs);
        i += 1;
    }

    Ok(())
}

use ndarray::{s, ArrayViewMut2};
fn fill_channel(mut dst: ArrayViewMut2<f32>, flat: &Vec<f32>, l: usize, k: usize) {
    assert_eq!(flat.len(), l * k, "flat length mismatch");
    for t in 0..l {
        let offset = t * k;
        for j in 0..k {
            dst[[t, j]] = flat[offset + j];
        }
    }
}
