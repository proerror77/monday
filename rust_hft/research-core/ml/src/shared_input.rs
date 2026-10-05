//! Consume a verified prepared Train view using the existing native CPU trainer.
//! No JSON row spool, database client, cluster client, or scientific authorization.
use crate::{
    train_parsed_contract_model, validate_bound_training_rows, ContractTrainingError,
    ContractTrainingRow, SealedTrainingRequest, TimestampMs, TrainedContractModel,
};
use hft_cex_research_input::data::{SharedInput, Split, TypedBlock};

fn binding(message: &str) -> ContractTrainingError {
    ContractTrainingError::DatasetBinding(message.into())
}
fn exact_ms(ns: i64) -> Result<i64, ContractTrainingError> {
    if ns <= 0 || ns % 1_000_000 != 0 {
        return Err(binding(
            "native millisecond contract requires exact view boundaries/horizon",
        ));
    }
    Ok(ns / 1_000_000)
}
fn available_ms(ns: i64) -> TimestampMs {
    // Never make a nanosecond observation or mature label available early.
    TimestampMs::new((i128::from(ns) + 999_999).div_euclid(1_000_000) as i64)
}

/// For this typed exit, rows/dataset/labels all bind the independently verified
/// PublishedView identity. Features bind its reviewed SQL and exact column order.
/// The caller seals the request once; the immutable SharedInput already owns
/// manifest/block/order admission. Reuse that proof while crossing this seam.
pub fn train_shared_contract_model(
    input: &SharedInput,
    request: &SealedTrainingRequest,
) -> Result<TrainedContractModel, ContractTrainingError> {
    let rows = training_rows(input, request)?;
    train_parsed_contract_model(&rows, request)
}

fn training_rows(
    input: &SharedInput,
    sealed: &SealedTrainingRequest,
) -> Result<Vec<ContractTrainingRow>, ContractTrainingError> {
    let request = sealed.request();
    let dataset = request.dataset();
    let spec = input.spec();
    let horizon_ms =
        i64::try_from(dataset.horizon_ms().get()).map_err(|_| binding("horizon overflow"))?;
    let horizon_ns = horizon_ms
        .checked_mul(1_000_000)
        .ok_or_else(|| binding("horizon overflow"))?;
    if spec.split != Split::Train
        || request.rows_artifact_sha256().as_str() != input.manifest_sha256()
        || dataset.dataset_manifest_sha256().as_str() != input.manifest_sha256()
        || dataset.label_manifest_sha256().as_str() != input.manifest_sha256()
        || dataset.feature_manifest_sha256().as_str() != spec.feature_sql_sha256
        || dataset.symbol().as_str() != spec.instrument
        || dataset.venue().as_str() != spec.venue
        || dataset
            .ordered_features()
            .iter()
            .map(|f| f.as_str())
            .ne(spec.feature_names.iter().map(String::as_str))
        || !spec.horizons_ns.contains(&horizon_ns)
        || request.split().start_at_ms().get() != exact_ms(spec.window.start_ns)?
        || request.split().training_cutoff_ms().get() != exact_ms(spec.fit_cutoff_ns)?
    {
        return Err(binding(
            "training request changed the verified view/columns/clock/horizon",
        ));
    }
    exact_ms(spec.window.end_ns)?;
    let mut rows = Vec::new();
    for block in input.blocks() {
        let TypedBlock::Training(frames) = block.as_ref() else {
            return Err(binding("native fit requires the prepared training exit"));
        };
        for frame in frames {
            let label = frame
                .labels
                .iter()
                .find(|l| l.horizon_ns == horizon_ns)
                .ok_or_else(|| binding("requested horizon lacks a prepared label"))?;
            rows.push(ContractTrainingRow {
                observed_at_ms: available_ms(frame.feature.available_ns),
                feature_max_available_at_ms: available_ms(frame.feature.available_ns),
                label_available_at_ms: available_ms(label.mature_ns),
                features: frame.feature.values.iter().map(|v| *v as f32).collect(),
                forward_return: label.value as f32,
            });
        }
    }
    // Native numeric precision, purge, embargo and causality remain enforced.
    // Quantization cannot silently merge rows or move a label before its clock.
    validate_bound_training_rows(&rows, request)?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContractDatasetBinding, ContractTrainingConfig, FeatureName, PositiveDurationMs,
        PurgedWalkForwardSplit, Sha256Digest, SplitId, SplitRole, Symbol, TrainingRequest, Venue,
    };
    use hft_cex_research_input::{
        data::{
            memory_bytes, BlockRef, BlockSource, DataViewSpec, Exit, FeatureFrame, Label,
            PublishedView, TrainingFrame, VerifiedCache, Window,
        },
        identity, prepared, sha256,
    };
    struct Source(Vec<u8>);
    impl BlockSource for Source {
        fn read(&mut self, _: &BlockRef) -> anyhow::Result<Vec<u8>> {
            Ok(self.0.clone())
        }
    }
    fn fixture() -> anyhow::Result<(SharedInput, TrainingRequest)> {
        fixture_with_overflow(false)
    }
    fn fixture_with_overflow(overflow: bool) -> anyhow::Result<(SharedInput, TrainingRequest)> {
        let block = TypedBlock::Training(
            (2..=4)
                .enumerate()
                .map(|(i, ms)| TrainingFrame {
                    feature: FeatureFrame {
                        segment: "one".into(),
                        ordinal: i as u64,
                        event_ns: ms * 1_000_000 - 100_000,
                        available_ns: ms * 1_000_000,
                        values: vec![
                            if overflow && i == 0 {
                                f64::MAX
                            } else {
                                ms as f64 / 10.0
                            },
                            1.0,
                        ],
                    },
                    labels: vec![Label {
                        horizon_ns: 2_000_000,
                        target_event_ns: (ms + 2) * 1_000_000,
                        mature_ns: (ms + 2) * 1_000_000,
                        value: ms as f64 / 1000.0,
                    }],
                })
                .collect(),
        );
        let bytes = prepared::encode(&block)?;
        let view = PublishedView {
            prepared_id: "a".repeat(64),
            source_receipt_sha256: "c".repeat(64),
            producer_image: format!("fixture@sha256:{}", "a".repeat(64)),
            spec: DataViewSpec {
                schema: 1,
                venue: "binance".into(),
                instrument: "BTCUSDT".into(),
                market: "usdm".into(),
                depth: 1,
                sources: vec!["a".repeat(64)],
                normalizer_sha256: "a".repeat(64),
                feature_sql_sha256: "b".repeat(64),
                feature_names: vec!["x".into(), "y".into()],
                window: Window {
                    start_ns: 1_000_000,
                    end_ns: 10_000_000,
                },
                lookback_ns: 0,
                horizons_ns: vec![2_000_000],
                label_tolerance_ns: 0,
                fit_cutoff_ns: 12_000_000,
                split: Split::Train,
            },
            blocks: vec![BlockRef {
                sha256: sha256(&bytes),
                bytes: bytes.len() as u64,
                rows: 3,
                decoded_bytes: memory_bytes(&block),
                exit: Exit::Training,
            }],
        };
        let view_id: Sha256Digest = identity(&view)?.parse()?;
        let input = VerifiedCache::new(1024 * 1024)?.load(
            &view,
            view_id.as_str(),
            Exit::Training,
            &mut Source(bytes),
        )?;
        let dataset = ContractDatasetBinding::new(
            view_id.clone(),
            "b".repeat(64).parse()?,
            view_id.clone(),
            vec![FeatureName::new("x")?, FeatureName::new("y")?],
            Symbol::new("BTCUSDT")?,
            Venue::new("binance")?,
            PositiveDurationMs::new(2)?,
        )?;
        let split = PurgedWalkForwardSplit::new(
            SplitId::new("prepared-train")?,
            SplitRole::Train,
            TimestampMs::new(1),
            TimestampMs::new(12),
            TimestampMs::new(15),
            PositiveDurationMs::new(2)?,
            PositiveDurationMs::new(2)?,
        )?;
        let config = ContractTrainingConfig {
            input_dim: 2,
            hidden_dim: 2,
            epochs: 2,
            min_rows: 3,
            ..Default::default()
        };
        Ok((
            input,
            TrainingRequest::new(view_id, dataset, split, config)?,
        ))
    }
    fn seal(request: TrainingRequest) -> Result<SealedTrainingRequest, ContractTrainingError> {
        let bytes = serde_json::to_vec(&request).unwrap();
        SealedTrainingRequest::from_bytes(&bytes, &Sha256Digest::of_bytes(&bytes))
    }
    #[test]
    fn shared_fit_reuses_native_training_and_matches_identical_numeric_rows() -> anyhow::Result<()>
    {
        let (input, request) = fixture()?;
        let sealed = seal(request.clone())?;
        let rows = training_rows(&input, &sealed)?;
        let model = train_shared_contract_model(&input, &sealed)?;
        let bytes = serde_json::to_vec(&rows)?;
        let mut reference = request;
        reference.rows_artifact_sha256 = Sha256Digest::of_bytes(&bytes);
        let reference = crate::train_contract_model(&bytes, &seal(reference)?)?;
        assert_eq!(
            model.diagnostics().semantic_model_sha256,
            reference.diagnostics().semantic_model_sha256
        );
        assert_eq!(model.diagnostics().mse, reference.diagnostics().mse);
        assert_eq!(model.diagnostics().row_count, 3);
        assert_eq!(
            model.diagnostics().authority,
            crate::EvidenceAuthority::FitDiagnosticsOnly
        );
        Ok(())
    }
    #[test]
    fn finite_prepared_f64_cannot_overflow_native_f32_training() -> anyhow::Result<()> {
        let (input, request) = fixture_with_overflow(true)?;
        assert!(matches!(
            train_shared_contract_model(&input, &seal(request)?),
            Err(ContractTrainingError::NonFiniteValue { .. })
        ));
        Ok(())
    }
    #[test]
    fn sealed_request_cannot_change_verified_view_columns_or_cutoff() -> anyhow::Result<()> {
        let (input, request) = fixture()?;
        for changed in [0, 1, 2, 3] {
            let mut foreign = request.clone();
            match changed {
                0 => foreign.rows_artifact_sha256 = "f".repeat(64).parse()?,
                1 => foreign.dataset.ordered_features.swap(0, 1),
                2 => foreign.dataset.symbol = Symbol::new("ETHUSDT")?,
                _ => foreign.split.training_cutoff_ms = TimestampMs::new(11),
            }
            assert!(train_shared_contract_model(&input, &seal(foreign)?).is_err());
        }
        assert_eq!(available_ms(1_000_001).get(), 2);
        assert!(exact_ms(1_000_001).is_err());
        Ok(())
    }
}
