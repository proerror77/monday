//! Sharded runtime launcher: start N engine shards in a single process.
//! Each shard builds its own SystemRuntime, optionally pins to a CPU core,
//! and filters symbols by a simple hash-based partitioner.

use runtime::{SystemBuilder, SystemConfig, StrategyConfig};
use hft_core::Symbol;
use clap::Parser;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about = "Sharded HFT Runtime", long_about = None)]
struct Args {
    /// Path to YAML config
    #[arg(long, default_value = "config/dev/system.yaml")]
    config: String,

    /// Number of shards
    #[arg(long, default_value_t = 2)]
    shards: usize,

    /// Enable CPU affinity pinning per shard
    #[arg(long, default_value_t = true)]
    cpu_affinity: bool,
}

fn shard_index_for_symbol(sym: &str, shards: usize) -> usize {
    // FNV-1a 32-bit
    let mut hash: u32 = 2166136261;
    for b in sym.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    (hash as usize) % shards
}

fn filter_config_for_shard(mut cfg: SystemConfig, shard: usize, shards: usize) -> SystemConfig {
    // 對 strategies 內的 symbols 依 hash 分片
    let mut new_strats: Vec<StrategyConfig> = Vec::new();
    for mut sc in cfg.strategies.into_iter() {
        sc.symbols.retain(|s| shard_index_for_symbol(&s.0, shards) == shard);
        if !sc.symbols.is_empty() {
            new_strats.push(sc);
        }
    }
    cfg.strategies = new_strats;
    cfg
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 使用 with_max_level 避免依賴 env-filter feature
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    let args = Args::parse();

    info!("Loading config: {}", args.config);
    let base_builder = SystemBuilder::from_yaml(&args.config)?;
    let base_cfg = base_builder.config().clone();

    let mut handles = Vec::new();
    for shard in 0..args.shards {
        let cfg = filter_config_for_shard(base_cfg.clone(), shard, args.shards);
        let mut builder = SystemBuilder::new(cfg).auto_register_adapters();
        let mut system = builder.build();

        // 每個 shard 啟動獨立任務
        let enable_affinity = args.cpu_affinity;
        let handle = tokio::spawn(async move {
            if enable_affinity {
                // 若可用，綁定 CPU 親和
                if let Some(core) = core_affinity::get_core_ids().and_then(|mut ids| ids.get(shard).cloned()) {
                    let _ = core_affinity::set_for_current(core);
                    info!("Shard {} pinned to core {}", shard, core.id);
                } else {
                    warn!("CPU affinity not available or core index out of range for shard {}", shard);
                }
            }
            if let Err(e) = system.start().await { warn!("Shard {} start error: {}", shard, e); }
            // Run until Ctrl-C (in real use) – here keep alive briefly
            tokio::signal::ctrl_c().await.expect("ctrl-c");
            let _ = system.stop().await;
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.await;
    }
    Ok(())
}
// Archived legacy example
