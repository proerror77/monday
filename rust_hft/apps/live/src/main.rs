//! HFT 真盤交易應用
//!
//! 特性開關示例：
//! - cargo run --features="bitget,trend-strategy,metrics"
//! - cargo run --features="full"  # 開啟所有特性

mod helpers;

use alpha_domain::{
    AttributionKind, AttributionMode, AttributionOutcome, RuntimeAttributionEvent,
    SignedDeploymentEnvelope, StrategyBundle,
};
use clap::Parser;
use hft_live::deployment_envelope::{
    decode_trusted_keys, ActivationRequest, DeploymentIntake, DeploymentReservation,
    RuntimeAuditLog, RuntimeFeedbackLog, RuntimeNonceLedger, RuntimePolicyDocument,
    SystemConfigActivationAdapter,
};
use hft_live::runtime_attribution::RuntimeAttributionObserver;
use runtime::{ShardConfig, ShardStrategy, SystemBuilder, SystemConfig};
use tracing::info;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置檔案路徑
    /// Production starts from a v2 quotes-only configuration.
    #[arg(short, long, default_value = "config/prod/system.yaml")]
    config: String,

    /// 啟動後自動退出的毫秒數（用於CI/sandbox）
    #[arg(long)]
    exit_after_ms: Option<u64>,

    /// 僅行情（不啟動執行端、不連私有WS、不下單）
    #[arg(long)]
    quotes_only: bool,

    /// Print deterministic runtime/risk hashes and exit without starting the system.
    #[arg(long)]
    deployment_hashes_only: bool,

    /// Signed deployment envelope produced by alpha-harness.
    #[arg(long)]
    deployment_envelope: Option<std::path::PathBuf>,

    /// Content-addressed StrategyBundle referenced by the deployment envelope.
    #[arg(long, requires = "deployment_envelope")]
    strategy_bundle: Option<std::path::PathBuf>,

    /// Runtime-owned deployment policy document.
    #[arg(long, requires = "deployment_envelope")]
    deployment_policy: Option<std::path::PathBuf>,

    /// Runtime-owned JSON map of key id to Ed25519 public key hex.
    #[arg(long, requires = "deployment_envelope")]
    deployment_trusted_keys: Option<std::path::PathBuf>,

    /// Runtime-owned durable nonce ledger.
    #[arg(long, requires = "deployment_envelope")]
    deployment_nonce_ledger: Option<std::path::PathBuf>,

    /// Runtime-owned append-only deployment audit log.
    #[arg(long, requires = "deployment_envelope")]
    deployment_audit_log: Option<std::path::PathBuf>,

    /// Runtime-owned append-only paper/shadow/live attribution log.
    #[arg(long, requires = "deployment_envelope")]
    deployment_feedback_log: Option<std::path::PathBuf>,

    /// Runtime-owned Ed25519 private key used only to sign attribution records.
    #[arg(long, requires = "deployment_envelope")]
    deployment_feedback_signing_key: Option<std::path::PathBuf>,

    /// Public identifier for the runtime attribution signing key.
    #[arg(long, requires = "deployment_envelope")]
    deployment_feedback_key_id: Option<String>,

    /// Metrics HTTP 服務器端口（啟用 metrics feature 時生效）
    #[cfg(feature = "metrics")]
    #[arg(long, default_value = "9090")]
    metrics_port: u16,

    /// 靜態分片：當前分片索引 (0-based)
    #[arg(long)]
    shard_index: Option<u32>,

    /// 靜態分片：總分片數量
    #[arg(long)]
    shard_count: Option<u32>,

    /// 靜態分片：使用自定義分片策略 (symbol-hash, venue-round-robin, hybrid)
    #[arg(long, default_value = "symbol-hash")]
    shard_strategy: String,

    /// 啟用 Sentinel 自動化風控
    #[arg(long, default_value_t = true)]
    sentinel_enable: bool,
    /// Sentinel 檢查間隔（毫秒）
    #[arg(long, default_value_t = 100)]
    sentinel_interval_ms: u64,
    /// Sentinel 延遲警告閾值（微秒）
    #[arg(long, default_value_t = 15000)]
    sentinel_latency_warn_us: u64,
    /// Sentinel 回撤停止閾值（百分比）
    #[arg(long, default_value_t = 5.0)]
    sentinel_drawdown_stop_pct: f64,

    /// gRPC 控制服務端口（啟用 grpc feature 時生效）
    #[cfg(feature = "grpc")]
    #[arg(long, default_value = "9092")]
    grpc_port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if !args.deployment_hashes_only {
        tracing_subscriber::fmt().with_env_filter("info").init();
    }

    info!("啟動 HFT 真盤交易系統");
    info!("配置檔案: {}", args.config);

    // Quotes-only 模式：透過環境變量關閉執行端
    if args.quotes_only {
        std::env::set_var("HFT_QUOTES_ONLY", "1");
        info!("已啟用 quotes-only 模式（不下單）");
    }

    // 展示可用的適配器
    show_available_adapters();

    let builder = SystemBuilder::from_yaml(&args.config)
        .map_err(|error| anyhow::anyhow!("runtime configuration is required: {error}"))?;
    info!("成功載入配置檔案: {}", args.config);
    let mut cfg = builder.config().clone();
    let (runtime_config_hash, risk_policy_hash) = deployment_hashes(&cfg)?;
    if args.deployment_hashes_only {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "runtime_config_hash": runtime_config_hash,
                "risk_policy_hash": risk_policy_hash,
            }))?
        );
        return Ok(());
    }
    let mut activation =
        apply_deployment_if_present(&args, &mut cfg, runtime_config_hash, risk_policy_hash)?;
    if let Some(deployment) = &activation {
        info!(
            deployment_id = deployment.request.deployment_id,
            mode = ?deployment.request.mode,
            "verified deployment applied to runtime startup config"
        );
    }
    validate_startup_authority(&cfg, activation.is_some())?;
    info!(
        queue_capacity = cfg.engine.queue_capacity,
        stale_us = cfg.engine.stale_us,
        top_n = cfg.engine.top_n,
        "Engine 配置已載入"
    );
    let market_stale_us = cfg.engine.stale_us;
    let builder = SystemBuilder::new(cfg);
    let builder = if let (Some(shard_index), Some(shard_count)) =
        (args.shard_index, args.shard_count)
    {
        let strategy = ShardStrategy::from(args.shard_strategy.as_str());
        builder.with_sharding(ShardConfig::new(shard_index, shard_count, strategy))
    } else if args.shard_index.is_some() || args.shard_count.is_some() {
        return Err(
            anyhow::anyhow!("--shard-index and --shard-count must be supplied together").into(),
        );
    } else {
        builder
    };
    let builder = match builder.auto_register_adapters_strict() {
        Ok(builder) => builder,
        Err(error) => {
            if let Some(deployment) = activation.as_mut() {
                deployment
                    .reservation
                    .record_startup_failed(chrono::Utc::now(), error.to_string())?;
            }
            return Err(error.into());
        }
    };
    if let Some(deployment) = activation.as_mut() {
        deployment
            .reservation
            .commit_configuration(chrono::Utc::now())?;
    }
    let mut system = builder.build();
    let attribution_observer = if let Some(deployment) = activation.as_ref() {
        let feedback_log = open_feedback_log(&args)?;
        let (receiver, market_reader, account_reader, runtime_truth_reader) = {
            let engine = system.engine.lock().await;
            (
                engine.subscribe_execution_events(),
                engine.market_reader(),
                engine.account_reader(),
                engine.runtime_truth_reader(),
            )
        };
        Some(RuntimeAttributionObserver::new(
            receiver,
            market_reader,
            account_reader,
            runtime_truth_reader,
            deployment.request.clone(),
            feedback_log,
            market_stale_us,
        ))
    } else {
        None
    };

    // 啟動系統
    if let Err(error) = system.start().await {
        if let Some(deployment) = activation.as_mut() {
            deployment
                .reservation
                .record_startup_failed(chrono::Utc::now(), error.to_string())?;
        }
        let _ = system.stop().await;
        return Err(error);
    }
    if let Some(deployment) = activation.as_mut() {
        if let Err(error) = append_activation_feedback(&args, &deployment.request) {
            let _ = deployment.reservation.record_startup_failed(
                chrono::Utc::now(),
                format!("activation feedback failed: {error}"),
            );
            let _ = system.execution_control_handle().emergency_stop(true).await;
            let _ = system.stop().await;
            return Err(error.into());
        }
        if let Err(error) = deployment.reservation.record_activated(chrono::Utc::now()) {
            let _ = system.execution_control_handle().emergency_stop(true).await;
            let _ = system.stop().await;
            return Err(error.into());
        }
    }
    let (attribution_shutdown, mut attribution_handle) =
        if let Some(observer) = attribution_observer {
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            (
                Some(shutdown_tx),
                Some(tokio::spawn(observer.run(shutdown_rx))),
            )
        } else {
            (None, None)
        };

    info!("系統正在運行...");

    // 啟動 Sentinel 風控 worker（默認啟用）
    let _sentinel_handle = if args.sentinel_enable {
        let sentinel_config = helpers::SentinelWorkerConfig {
            check_interval_ms: args.sentinel_interval_ms,
            latency_warn_us: args.sentinel_latency_warn_us,
            latency_degrade_us: args.sentinel_latency_warn_us * 2, // 降頻閾值 = 警告閾值 * 2
            drawdown_warn_pct: args.sentinel_drawdown_stop_pct * 0.4, // 警告 = 停止 * 0.4
            drawdown_stop_pct: args.sentinel_drawdown_stop_pct,
        };
        Some(helpers::spawn_sentinel_worker(
            system.execution_control_handle(),
            sentinel_config,
        ))
    } else {
        None
    };

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    let _metrics_handle = helpers::spawn_metrics_server(system.engine.clone(), args.metrics_port);

    // 啟動 gRPC 控制服務（如果啟用 grpc feature）
    #[cfg(feature = "grpc")]
    let _grpc_handle =
        helpers::spawn_grpc_server(system.execution_control_handle(), args.grpc_port);

    // 保持運行直到收到停止信號、到達自動退出時間或歸因 observer 退出。
    let shutdown_signal = async {
        if let Some(ms) = args.exit_after_ms {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => signal,
                _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => Ok(()),
            }
        } else {
            tokio::signal::ctrl_c().await
        }
    };
    tokio::pin!(shutdown_signal);
    let attribution_result = if let Some(handle) = attribution_handle.as_mut() {
        tokio::select! {
            result = handle => Some(result),
            signal = &mut shutdown_signal => {
                signal?;
                None
            }
        }
    } else {
        shutdown_signal.await?;
        None
    };
    info!("收到停止信號，正在關閉系統...");

    // Keep attribution alive while the runtime cancels and reconciles outstanding orders.
    let stop_result = system.stop().await;

    let attribution_result = if attribution_result.is_some() {
        attribution_result.map(|result| (false, result))
    } else if let Some(handle) = attribution_handle.take() {
        if let Some(shutdown) = attribution_shutdown {
            let _ = shutdown.send(());
        }
        Some((true, handle.await))
    } else {
        None
    };

    stop_result?;

    if let Some((expected_shutdown, result)) = attribution_result {
        match (expected_shutdown, result) {
            (true, Ok(Ok(()))) => {}
            (false, Ok(Ok(()))) => {
                return Err(
                    anyhow::anyhow!("runtime attribution observer exited unexpectedly").into(),
                )
            }
            (_, Ok(Err(error))) => {
                return Err(anyhow::anyhow!("runtime attribution observer failed: {error}").into())
            }
            (_, Err(error)) => {
                return Err(
                    anyhow::anyhow!("runtime attribution observer task failed: {error}").into(),
                )
            }
        }
    }

    Ok(())
}

fn deployment_hashes(config: &SystemConfig) -> anyhow::Result<(String, String)> {
    use sha2::{Digest, Sha256};

    let runtime = serde_json::to_vec(&serde_json::to_value(config)?)?;
    let risk = serde_json::to_vec(&serde_json::to_value(&config.risk)?)?;
    Ok((
        hex::encode(Sha256::digest(runtime)),
        hex::encode(Sha256::digest(risk)),
    ))
}

fn validate_startup_authority(
    config: &SystemConfig,
    has_signed_activation: bool,
) -> anyhow::Result<()> {
    // A venue-authenticated process with no strategies is an operator control session: it can
    // inspect and cancel orders, but there is no producer for a new OrderIntent. Any configured
    // strategy remains deployment-governed, including Paper and Shadow modes.
    if !has_signed_activation && !config.strategies.is_empty() {
        anyhow::bail!("strategy-capable hft-live startup requires a signed deployment envelope");
    }
    Ok(())
}

struct PreparedDeployment {
    request: ActivationRequest,
    reservation: DeploymentReservation,
}

fn apply_deployment_if_present(
    args: &Args,
    config: &mut SystemConfig,
    runtime_config_hash: String,
    risk_policy_hash: String,
) -> anyhow::Result<Option<PreparedDeployment>> {
    let Some(envelope_path) = args.deployment_envelope.as_ref() else {
        return Ok(None);
    };
    let bundle_path = required_path(&args.strategy_bundle, "--strategy-bundle")?;
    let policy_path = required_path(&args.deployment_policy, "--deployment-policy")?;
    let trusted_keys_path =
        required_path(&args.deployment_trusted_keys, "--deployment-trusted-keys")?;
    let nonce_path = required_path(&args.deployment_nonce_ledger, "--deployment-nonce-ledger")?;
    let audit_path = required_path(&args.deployment_audit_log, "--deployment-audit-log")?;
    open_feedback_log(args)?;
    let signed: SignedDeploymentEnvelope = read_json(envelope_path)?;
    let bundle: StrategyBundle = read_json(bundle_path)?;
    let policy_document: RuntimePolicyDocument = read_json(policy_path)?;
    let trusted_keys = decode_trusted_keys(read_json(trusted_keys_path)?)?;
    let policy = policy_document.bind(runtime_config_hash, risk_policy_hash);
    let ledger = RuntimeNonceLedger::open(nonce_path)?;
    let audit = RuntimeAuditLog::open(audit_path)?;
    let mut adapter = SystemConfigActivationAdapter::new(config, &bundle, bundle_path);
    let observed_at = chrono::Utc::now();
    let (request, reservation) = DeploymentIntake::new(
        &trusted_keys,
        &policy,
        policy_document.runtime_paused,
        ledger,
        audit,
        &mut adapter,
    )
    .prepare(&signed, observed_at)?;
    Ok(Some(PreparedDeployment {
        request,
        reservation,
    }))
}

fn append_activation_feedback(args: &Args, request: &ActivationRequest) -> anyhow::Result<()> {
    let observed_at = chrono::Utc::now();
    open_feedback_log(args)?.append(&RuntimeAttributionEvent {
        event_id: format!("activation:{}", request.deployment_id),
        deployment_id: request.deployment_id.clone(),
        asset_revision_id: request.asset_revision_id.clone(),
        mission_id: None,
        mode: match request.mode {
            hft_live::deployment_envelope::ActivationMode::Paper => AttributionMode::Paper,
            hft_live::deployment_envelope::ActivationMode::Shadow => AttributionMode::Shadow,
            hft_live::deployment_envelope::ActivationMode::LiveSmall => AttributionMode::LiveSmall,
        },
        outcome: AttributionOutcome::Activated,
        kind: AttributionKind::Activation,
        strategy_id: None,
        order_id: None,
        account_id: Some(request.account_id.clone()),
        venue: Some(request.venue.clone()),
        symbol: None,
        metrics: std::collections::BTreeMap::from([(
            "sealed_execution_cost_coverage_required".to_string(),
            if request.cex_execution_costs.is_some() {
                1.0
            } else {
                0.0
            },
        )]),
        reason: None,
        observed_at,
    })?;
    Ok(())
}

fn open_feedback_log(args: &Args) -> anyhow::Result<RuntimeFeedbackLog> {
    let feedback_path = required_path(&args.deployment_feedback_log, "--deployment-feedback-log")?;
    let signing_key_path = required_path(
        &args.deployment_feedback_signing_key,
        "--deployment-feedback-signing-key",
    )?;
    let key_id = args
        .deployment_feedback_key_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("--deployment-feedback-key-id is required with --deployment-envelope")
        })?;
    let encoded = std::fs::read_to_string(signing_key_path).map_err(|error| {
        anyhow::anyhow!(
            "failed to read feedback signing key {}: {error}",
            signing_key_path.display()
        )
    })?;
    let key_bytes = hex::decode(encoded.trim())
        .map_err(|_| anyhow::anyhow!("feedback signing key must be hex encoded"))?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("feedback signing key must contain exactly 32 bytes"))?;
    RuntimeFeedbackLog::open(
        feedback_path,
        key_id,
        ed25519_dalek::SigningKey::from_bytes(&key_bytes),
    )
    .map_err(Into::into)
}

fn required_path<'a>(
    path: &'a Option<std::path::PathBuf>,
    flag: &str,
) -> anyhow::Result<&'a std::path::Path> {
    path.as_deref()
        .ok_or_else(|| anyhow::anyhow!("{flag} is required with --deployment-envelope"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> anyhow::Result<T> {
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("invalid JSON in {}: {error}", path.display()))
}

fn show_available_adapters() {
    info!("可用適配器:");

    #[cfg(feature = "bitget")]
    info!("  ✓ Bitget 交易所");
    #[cfg(feature = "ondo-perps")]
    info!("  ✓ Ondo Perps 行情與執行適配器");

    #[cfg(feature = "binance")]
    info!("  ✓ Binance 交易所");

    #[cfg(feature = "okx")]
    info!("  ✓ OKX 交易所");

    #[cfg(feature = "mock")]
    info!("  ✓ 模擬交易所");

    #[cfg(feature = "trend-strategy")]
    info!("  ✓ 趨勢策略");

    #[cfg(feature = "arbitrage-strategy")]
    info!("  ✓ 套利策略");

    #[cfg(feature = "dl-strategy")]
    info!("  ✓ 深度學習策略");

    #[cfg(feature = "metrics")]
    info!("  ✓ 指標監控");

    #[cfg(feature = "clickhouse")]
    info!("  ✓ ClickHouse 存儲");

    #[cfg(feature = "redis")]
    info!("  ✓ Redis 快取");
}

// Legacy config structs removed - now using SystemBuilder from YAML directly

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_startup_allows_empty_operator_control_but_never_a_strategy() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/prod/system.yaml");
        let mut config = SystemBuilder::from_yaml(path.to_str().unwrap())
            .unwrap()
            .config()
            .clone();
        assert!(validate_startup_authority(&config, false).is_ok());

        config.quotes_only = false;
        assert!(validate_startup_authority(&config, false).is_ok());
        config.strategies.push(runtime::StrategyConfig {
            name: "unsigned".to_string(),
            strategy_type: runtime::StrategyType::Imbalance,
            symbols: vec![hft_core::Symbol::new("BTCUSDT")],
            params: runtime::StrategyParams::Imbalance {
                obi_threshold: 0.1,
                lot: rust_decimal::Decimal::ONE,
                top_levels: 1,
            },
            risk_limits: runtime::StrategyRiskLimits {
                max_notional: rust_decimal::Decimal::ONE,
                max_position: rust_decimal::Decimal::ONE,
                daily_loss_limit: rust_decimal::Decimal::ONE,
                cooldown_ms: 1,
            },
        });
        assert!(validate_startup_authority(&config, false).is_err());
        assert!(validate_startup_authority(&config, true).is_ok());
    }
}
