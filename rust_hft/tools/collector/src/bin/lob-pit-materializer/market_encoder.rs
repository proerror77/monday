//! Feature construction is independent of label values and future endpoints.
use super::*;
use hft_research_manifest::market_encoder::{
    MarketFeatureDatasetV1, MarketFeatureFrameV1, MarketTargetDatasetV1, MarketTargetFrameV1,
    FEATURE_SCHEMA, TARGET_SCHEMA, TASK_HORIZON_MS,
};

#[derive(Debug, Serialize)]
pub(super) struct PublishedArtifact {
    pub file: String,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
pub(super) struct PublishedDatasets {
    pub schema_version: String,
    pub features: PublishedArtifact,
    pub targets: PublishedArtifact,
}

fn stable_series(replay: &Replay, sample: &BookSample) -> Result<u64> {
    replay
        .cont_ofi
        .series_started_at_ns
        .get(&sample.series_id)
        .copied()
        .context("market encoder recovery identity is missing")
}

pub(super) fn feature_frames(
    replay: &Replay,
    trades: &[AggregateTrade],
    args: &Args,
    symbol: &str,
) -> Result<Vec<MarketFeatureFrameV1>> {
    let window = output_window(args)?;
    let trades = trades
        .iter()
        .filter(|t| t.symbol == symbol)
        .collect::<Vec<_>>();
    if trades
        .windows(2)
        .any(|w| w[0].received_at_ns > w[1].received_at_ns)
    {
        bail!("market feature trades are out of receive-time order");
    }
    let mut frames = Vec::new();
    for pair in replay.samples.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.series_id != current.series_id
            || previous.time_ns.checked_add(1_000_000_000) != Some(current.time_ns)
            || window.is_some_and(|(start, end)| current.time_ns < start || current.time_ns >= end)
        {
            continue;
        }
        if previous.mid_price <= 0.0 || current.mid_price <= 0.0 {
            bail!("invalid market feature mid-price");
        }
        let levels = current
            .levels
            .as_ref()
            .context("market feature export did not capture Top5 levels")?;
        let start = trades.partition_point(|t| t.received_at_ns <= previous.time_ns);
        let end = trades.partition_point(|t| t.received_at_ns <= current.time_ns);
        let (base, signed) = trades[start..end].iter().try_fold(
            (Decimal::ZERO, Decimal::ZERO),
            |(base, signed), t| {
                if t.quantity < Decimal::ZERO {
                    bail!("negative market feature trade quantity");
                }
                let sign = if t.is_buyer_maker {
                    -t.quantity
                } else {
                    t.quantity
                };
                Ok::<_, anyhow::Error>((
                    base.checked_add(t.quantity)
                        .context("market volume overflow")?,
                    signed
                        .checked_add(sign)
                        .context("market signed volume overflow")?,
                ))
            },
        )?;
        let mut channels = vec![(current.mid_price / previous.mid_price - 1.0) as f32];
        for [price, quantity] in levels.iter() {
            if *price <= 0.0 || *quantity < 0.0 {
                bail!("invalid market Top5 level");
            }
            channels.push(((price / current.mid_price - 1.0) * 10_000.0) as f32);
            channels.push(quantity.ln_1p() as f32);
        }
        channels.extend([
            decimal_f64(base)?.ln_1p() as f32,
            decimal_f64(signed)?.asinh() as f32,
            ((end - start) as f64).ln_1p() as f32,
        ]);
        let time =
            i64::try_from(current.time_ns / 1_000_000).context("market feature clock overflow")?;
        let frame = MarketFeatureFrameV1 {
            series_id: stable_series(replay, current)?,
            observed_at_ms: time,
            feature_max_available_at_ms: time,
            channels,
        };
        frame
            .validate(&SequenceInputSpecV1::sol_lob())
            .map_err(anyhow::Error::msg)?;
        frames.push(frame);
        if frames.len() > 14 * 86_400 {
            bail!("market feature export exceeds 14-day row budget");
        }
    }
    if frames.is_empty() {
        bail!("market feature export has no continuous observations");
    }
    Ok(frames)
}

fn target_frames(
    replay: &Replay,
    features: &[MarketFeatureFrameV1],
    args: &Args,
) -> Result<Vec<MarketTargetFrameV1>> {
    let window = output_window(args)?;
    let mut targets = Vec::new();
    let horizon = (TASK_HORIZON_MS / 1000) as usize;
    for feature in features {
        let clock = u64::try_from(feature.observed_at_ms)
            .context("negative market clock")?
            .checked_mul(1_000_000)
            .context("market clock overflow")?;
        let i = replay.samples.partition_point(|s| s.time_ns < clock);
        let current = replay.samples.get(i).context("feature has no raw sample")?;
        if current.time_ns != clock || stable_series(replay, current)? != feature.series_id {
            bail!("feature differs from its raw replay");
        }
        let Some(future) = replay.samples.get(i + horizon) else {
            continue;
        };
        if current.series_id != future.series_id
            || clock.checked_add(TASK_HORIZON_MS as u64 * 1_000_000) != Some(future.time_ns)
            || window.is_some_and(|(_, end)| future.time_ns >= end)
        {
            continue;
        }
        let target = MarketTargetFrameV1 {
            series_id: feature.series_id,
            observed_at_ms: feature.observed_at_ms,
            available_at_ms: i64::try_from(future.time_ns / 1_000_000)
                .context("target clock overflow")?,
            simple_return: (future.mid_price / current.mid_price - 1.0) as f32,
            spread_bps: current.spread_bps,
        };
        target.validate().map_err(anyhow::Error::msg)?;
        targets.push(target);
    }
    if targets.is_empty() {
        bail!("market export has no mature targets for downstream comparison");
    }
    Ok(targets)
}

fn write_shards<T: Serialize>(
    rows: &[T],
    suffix: &str,
    output: &Path,
    clock: impl Fn(&T) -> i64,
) -> Result<Vec<SequenceShardV1>> {
    let mut result = Vec::new();
    let mut total_bytes = 0_u64;
    for chunk in rows.chunks(3600) {
        let mut bytes = Vec::new();
        for row in chunk {
            serde_json::to_writer(&mut bytes, row)?;
            bytes.push(b'\n');
        }
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .context("market export byte overflow")?;
        if total_bytes > 8 * 1024 * 1024 * 1024 || bytes.len() > 256 * 1024 * 1024 {
            bail!("market export exceeds byte budget");
        }
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let file = format!("{sha256}.{suffix}.jsonl");
        publish_immutable(&output.join(&file), &bytes)?;
        result.push(SequenceShardV1 {
            file,
            sha256,
            bytes: bytes.len() as u64,
            rows: chunk.len() as u64,
            first_observed_at_ms: clock(chunk.first().context("empty market shard")?),
            last_observed_at_ms: clock(chunk.last().context("empty market shard")?),
        });
    }
    Ok(result)
}

pub(super) fn publish(
    replay: &Replay,
    trades: &[AggregateTrade],
    args: &Args,
    symbol: &str,
    source_hash: &str,
) -> Result<PublishedDatasets> {
    // Build features first. Target omission cannot remove a feature observation.
    let features = feature_frames(replay, trades, args, symbol)?;
    let targets = target_frames(replay, &features, args)?;
    let feature_shards = write_shards(&features, "market-features", &args.artifact_dir, |f| {
        f.observed_at_ms
    })?;
    let feature_dataset = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: symbol.into(),
        source_manifest_sha256: source_hash.into(),
        input: SequenceInputSpecV1::sol_lob(),
        shards: feature_shards,
    };
    let feature_hash = feature_dataset.digest().map_err(anyhow::Error::msg)?;
    let feature_file = format!("{feature_hash}.market-features.json");
    publish_immutable(
        &args.artifact_dir.join(&feature_file),
        &serde_json::to_vec(&feature_dataset)?,
    )?;
    let target_dataset = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: feature_hash.clone(),
        horizon_ms: TASK_HORIZON_MS,
        shards: write_shards(&targets, "market-targets", &args.artifact_dir, |t| {
            t.observed_at_ms
        })?,
    };
    let target_hash = target_dataset.digest().map_err(anyhow::Error::msg)?;
    let target_file = format!("{target_hash}.market-targets.json");
    publish_immutable(
        &args.artifact_dir.join(&target_file),
        &serde_json::to_vec(&target_dataset)?,
    )?;
    Ok(PublishedDatasets {
        schema_version: "monday.market_encoder_export.v1".into(),
        features: PublishedArtifact {
            file: feature_file,
            sha256: feature_hash,
        },
        targets: PublishedArtifact {
            file: target_file,
            sha256: target_hash,
        },
    })
}
