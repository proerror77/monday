//! Native Campaign inputs. Development verification grants no Run or budget authority.
use crate::{
    data::{
        BlockSource, Exit, FeatureFrame, PublishedView, SharedInput, Split, TypedBlock,
        VerifiedCache, Window,
    },
    identity, valid_digest,
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};

pub const CAMPAIGN_PREPARED_INPUTS_SCHEMA: &str = "monday.cex_campaign_prepared_inputs.v1";
pub const NATIVE_LABEL_RECIPE: &str =
    "native-lob-pit-v5-actual-future-mid-over-current-mid-minus-one";

/// Wire-compatible with the original ResearchRow. A feature block never contains this label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeResearchRowV1 {
    pub series_id: u64,
    pub available_time: DateTime<Utc>,
    pub label_available_time: DateTime<Utc>,
    pub signal: f64,
    #[serde(default)]
    pub features: BTreeMap<String, f64>,
    pub label: f64,
    pub fee_bps: f64,
    pub funding_bps: f64,
    #[serde(default)]
    pub pit_funding: bool,
    pub latency_bps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBuildRefV1 {
    pub source_revision: String,
    pub image_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSourceBindingV1 {
    pub build: SourceBuildRefV1,
    /// The original native input preparation Run, not a scientific Platform Run.
    pub preparation_run_id: String,
    /// Immutable preparation-attempt receipt. No generated Platform Attempt is claimed.
    pub preparation_receipt_sha256: String,
    pub feature_sha256: String,
    pub materialization_sha256: String,
    pub replay_artifact_sha256: String,
    pub replay_manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpaqueWithheldPartitionV1 {
    pub original_rows: Range<usize>,
    pub window: Window,
    pub source_content_sha256: String,
}

/// The original full-data schedule, verified before the preparer discards withheld bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDatasetMetadataV1 {
    pub total_rows: usize,
    pub original_window: Window,
    pub original_rows_sha256: String,
    pub protocol_json: String,
    pub protocol_sha256: String,
    pub search_rows: Range<usize>,
    pub visible_rows: Range<usize>,
    pub development_window: Window,
    /// Exclusive limit for every real price/event needed by development labels/replay.
    pub authorized_context_end_ns: i64,
    pub selection: Option<OpaqueWithheldPartitionV1>,
    pub holdout: OpaqueWithheldPartitionV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLabelAnchorV1 {
    pub ordinal: u64,
    pub series_id: u64,
    pub segment: String,
    pub observed_at_ns: i64,
    /// Actual native future mark clock, frozen from the verified source row.
    pub future_at_ns: i64,
    pub mature_at_ns: i64,
    pub signal: f64,
    pub fee_bps: f64,
    pub funding_bps: f64,
    pub pit_funding: bool,
    pub latency_bps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedViewRefV1 {
    pub manifest_sha256: String,
    pub manifest: PublishedView,
}

/// Encode actual bounded blocks. The returned manifest is untrusted until independent readback.
pub fn encode_prepared_view(
    spec: crate::data::DataViewSpec,
    blocks: Vec<TypedBlock>,
    source: &NativeSourceBindingV1,
) -> Result<(PreparedViewRefV1, BTreeMap<String, Vec<u8>>)> {
    spec.validate()?;
    let mut references = Vec::new();
    let mut bytes = BTreeMap::new();
    for block in blocks {
        let (rows, exit) = match &block {
            TypedBlock::Features(rows) => (rows.len(), Exit::Features),
            TypedBlock::Replay(rows) => (rows.len(), Exit::Replay),
            TypedBlock::Training(_) => {
                anyhow::bail!("native collection cannot publish a Training exit")
            }
        };
        let encoded = crate::prepared::encode(&block)?;
        let sha256 = crate::sha256(&encoded);
        let reference = crate::data::BlockRef {
            sha256: sha256.clone(),
            bytes: encoded.len() as u64,
            rows: rows as u64,
            decoded_bytes: crate::data::memory_bytes(&block),
            exit,
        };
        crate::data::validate_block(&block, &reference, &spec)?;
        references.push(reference);
        ensure!(
            bytes.insert(sha256, encoded).is_none(),
            "repeated native prepared block"
        );
    }
    let prepared_id = identity(&(source, &spec, &references))?;
    let manifest = PublishedView {
        prepared_id,
        spec,
        blocks: references,
        producer_image: source.build.image_identity.clone(),
        source_receipt_sha256: source.preparation_receipt_sha256.clone(),
    };
    let manifest_sha256 = identity(&manifest)?;
    manifest.verify(&manifest_sha256)?;
    Ok((
        PreparedViewRefV1 {
            manifest_sha256,
            manifest,
        },
        bytes,
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPreparedInputsV1 {
    pub schema_version: String,
    pub source: NativeSourceBindingV1,
    pub original: NativeDatasetMetadataV1,
    pub label_recipe: String,
    pub anchors: Vec<NativeLabelAnchorV1>,
    pub development_rows_sha256: String,
    pub replay_rows_sha256: String,
    pub features: PreparedViewRefV1,
    /// Genuine native price observations. This label-free view has only mid_price.
    pub future_marks: PreparedViewRefV1,
    pub replay: PreparedViewRefV1,
}

/// Complete data expectation from the independently admitted native input/request record.
/// This describes data identity; it is not a substitute for a signed grant or PG witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedNativeCampaignInputsV1 {
    pub source: NativeSourceBindingV1,
    pub original_metadata_sha256: String,
    pub native_protocol_sha256: String,
    pub development_rows_sha256: String,
}

/// Only actual block decoding and native semantic equality can construct this value.
pub struct VerifiedCampaignPreparedInputsV1 {
    manifest: CampaignPreparedInputsV1,
    id: String,
    expected: ExpectedNativeCampaignInputsV1,
    rows: Vec<NativeResearchRowV1>,
    replay: SharedInput,
}

impl VerifiedCampaignPreparedInputsV1 {
    pub fn manifest(&self) -> &CampaignPreparedInputsV1 {
        &self.manifest
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn expected_native(&self) -> &ExpectedNativeCampaignInputsV1 {
        &self.expected
    }
    pub fn rows(&self) -> &[NativeResearchRowV1] {
        &self.rows
    }
    pub fn replay(&self) -> &SharedInput {
        &self.replay
    }
    pub fn source_receipt_sha256(&self) -> &str {
        &self.manifest.source.preparation_receipt_sha256
    }
    pub fn source_build(&self) -> &SourceBuildRefV1 {
        &self.manifest.source.build
    }
    pub fn original_metadata(&self) -> &NativeDatasetMetadataV1 {
        &self.manifest.original
    }
    pub fn development_rows_sha256(&self) -> &str {
        &self.manifest.development_rows_sha256
    }
    pub fn native_protocol_sha256(&self) -> &str {
        &self.manifest.original.protocol_sha256
    }
    pub fn view_ids(&self) -> [&str; 3] {
        [
            &self.manifest.features.manifest_sha256,
            &self.manifest.future_marks.manifest_sha256,
            &self.manifest.replay.manifest_sha256,
        ]
    }
}

impl CampaignPreparedInputsV1 {
    pub fn id(&self) -> Result<String> {
        self.validate_metadata()?;
        identity(self)
    }
    pub fn expected_native(&self) -> Result<ExpectedNativeCampaignInputsV1> {
        self.validate_metadata()?;
        Ok(ExpectedNativeCampaignInputsV1 {
            source: self.source.clone(),
            original_metadata_sha256: identity(&self.original)?,
            native_protocol_sha256: self.original.protocol_sha256.clone(),
            development_rows_sha256: self.development_rows_sha256.clone(),
        })
    }
    pub fn verify(
        self,
        independently_expected_id: &str,
        expected: &ExpectedNativeCampaignInputsV1,
        source: &mut impl BlockSource,
        max_bytes: u64,
    ) -> Result<VerifiedCampaignPreparedInputsV1> {
        let id = self.id()?;
        ensure!(
            valid_digest(independently_expected_id) && id == independently_expected_id,
            "native prepared collection differs from authoritative identity"
        );
        ensure!(
            self.expected_native()? == *expected,
            "native prepared source/protocol/rows binding changed"
        );
        let mut cache = VerifiedCache::new(max_bytes)?;
        let features = cache.load(
            &self.features.manifest,
            &self.features.manifest_sha256,
            Exit::Features,
            source,
        )?;
        let marks = cache.load(
            &self.future_marks.manifest,
            &self.future_marks.manifest_sha256,
            Exit::Features,
            source,
        )?;
        let replay = cache.load(
            &self.replay.manifest,
            &self.replay.manifest_sha256,
            Exit::Replay,
            source,
        )?;
        let rows = decode_rows(&self, &features, &marks)?;
        ensure!(
            identity(&rows)? == self.development_rows_sha256,
            "native development ResearchRow values or clock changed"
        );
        verify_replay_boundaries(&self, &replay)?;
        Ok(VerifiedCampaignPreparedInputsV1 {
            manifest: self,
            id,
            expected: expected.clone(),
            rows,
            replay,
        })
    }

    pub fn validate_metadata(&self) -> Result<()> {
        ensure!(
            self.schema_version == CAMPAIGN_PREPARED_INPUTS_SCHEMA
                && self.label_recipe == NATIVE_LABEL_RECIPE,
            "unsupported native prepared schema or label recipe"
        );
        let source = &self.source;
        ensure!(
            source.build.source_revision.len() == 40
                && source
                    .build
                    .source_revision
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "invalid producer source revision"
        );
        ensure!(
            source
                .build
                .image_identity
                .rsplit_once("@sha256:")
                .is_some_and(|(_, h)| valid_digest(h)),
            "producer Build image is not pinned"
        );
        ensure!(
            !source.preparation_run_id.is_empty() && source.preparation_run_id.len() <= 256,
            "missing preparation Run identity"
        );
        ensure!(
            [
                &source.preparation_receipt_sha256,
                &source.feature_sha256,
                &source.materialization_sha256,
                &source.replay_artifact_sha256,
                &source.replay_manifest_sha256,
                &self.development_rows_sha256
            ]
            .iter()
            .all(|v| valid_digest(v)),
            "invalid source/row content reference"
        );
        let meta = &self.original;
        ensure!(
            meta.total_rows > 0
                && meta.visible_rows.start == 0
                && meta.search_rows.start == 0
                && meta.search_rows.end <= meta.visible_rows.end
                && meta.visible_rows.end < meta.total_rows
                && !meta.visible_rows.is_empty(),
            "native visible/search/full row ranges differ"
        );
        ensure!(
            meta.protocol_json.len() <= 1_048_576
                && valid_digest(&meta.protocol_sha256)
                && crate::sha256(meta.protocol_json.as_bytes()) == meta.protocol_sha256
                && valid_digest(&meta.original_rows_sha256),
            "original protocol/row identity changed"
        );
        ensure!(
            meta.original_window.start_ns > 0
                && meta.original_window.start_ns < meta.development_window.end_ns
                && meta.development_window.start_ns >= meta.original_window.start_ns
                && meta.development_window.end_ns <= meta.authorized_context_end_ns
                && meta.authorized_context_end_ns <= meta.original_window.end_ns,
            "development context exceeds original window"
        );
        let mut boundary = meta.holdout.window.start_ns;
        for withheld in meta.selection.iter().chain(std::iter::once(&meta.holdout)) {
            ensure!(
                !withheld.original_rows.is_empty()
                    && withheld.original_rows.start >= meta.visible_rows.end
                    && withheld.original_rows.end <= meta.total_rows
                    && withheld.window.start_ns < withheld.window.end_ns
                    && valid_digest(&withheld.source_content_sha256),
                "withheld metadata is invalid"
            );
            boundary = boundary.min(withheld.window.start_ns);
        }
        ensure!(
            meta.authorized_context_end_ns <= boundary
                && meta.holdout.original_rows.end == meta.total_rows,
            "development/future context enters a withheld partition"
        );
        ensure!(
            self.anchors.len() == meta.visible_rows.len()
                && self.anchors.len() <= 2_000_000
                && !meta.search_rows.is_empty(),
            "native anchor count cannot be cropped or filled"
        );
        for (ordinal, anchor) in self.anchors.iter().enumerate() {
            ensure!(
                anchor.ordinal == ordinal as u64
                    && anchor.series_id > 0
                    && !anchor.segment.is_empty()
                    && anchor.observed_at_ns >= meta.development_window.start_ns
                    && anchor.observed_at_ns < meta.development_window.end_ns
                    && anchor.future_at_ns > anchor.observed_at_ns
                    && anchor.mature_at_ns >= anchor.future_at_ns
                    && anchor.future_at_ns < meta.authorized_context_end_ns
                    && anchor.mature_at_ns < meta.authorized_context_end_ns,
                "native label requires an unauthorized future price or maturity"
            );
            ensure!(
                [
                    anchor.signal,
                    anchor.fee_bps,
                    anchor.funding_bps,
                    anchor.latency_bps
                ]
                .iter()
                .all(|v| v.is_finite()),
                "nonfinite native row facts"
            );
        }
        for (view, exit) in [
            (&self.features, Exit::Features),
            (&self.future_marks, Exit::Features),
            (&self.replay, Exit::Replay),
        ] {
            view.manifest.verify(&view.manifest_sha256)?;
            ensure!(
                view.manifest.spec.split == Split::Validation
                    && view.manifest.blocks.iter().all(|b| b.exit == exit),
                "native collection cannot expose a Train/Training exit"
            );
            ensure!(
                view.manifest.source_receipt_sha256 == source.preparation_receipt_sha256
                    && view.manifest.producer_image == source.build.image_identity
                    && view.manifest.spec.window.start_ns >= meta.original_window.start_ns
                    && view.manifest.spec.window.end_ns <= meta.authorized_context_end_ns
                    && view.manifest.spec.lookback_ns == 0,
                "native view source or context changed"
            );
        }
        ensure!(
            self.features.manifest.spec.window == meta.development_window
                && self.future_marks.manifest.spec.feature_names == ["mid_price"]
                && self
                    .features
                    .manifest
                    .spec
                    .feature_names
                    .contains(&"mid_price".into()),
            "native feature/price view schema changed"
        );
        ensure!(
            self.features.manifest.spec.venue == self.future_marks.manifest.spec.venue
                && self.features.manifest.spec.instrument
                    == self.future_marks.manifest.spec.instrument
                && self.features.manifest.spec.market == self.future_marks.manifest.spec.market
                && self.features.manifest.spec.venue == self.replay.manifest.spec.venue
                && self.features.manifest.spec.instrument == self.replay.manifest.spec.instrument
                && self.features.manifest.spec.market == self.replay.manifest.spec.market,
            "native views do not share one instrument"
        );
        Ok(())
    }
}

fn feature_rows(input: &SharedInput) -> Result<Vec<&FeatureFrame>> {
    let mut rows = Vec::new();
    for block in input.blocks() {
        let TypedBlock::Features(frames) = block.as_ref() else {
            anyhow::bail!("native feature/price exit changed its type")
        };
        rows.extend(frames);
    }
    Ok(rows)
}
fn decode_rows(
    manifest: &CampaignPreparedInputsV1,
    features: &SharedInput,
    marks: &SharedInput,
) -> Result<Vec<NativeResearchRowV1>> {
    let frames = feature_rows(features)?;
    ensure!(
        frames.len() == manifest.anchors.len(),
        "native development row coverage changed"
    );
    let prices = feature_rows(marks)?
        .into_iter()
        .map(|row| ((row.segment.as_str(), row.available_ns), row.values[0]))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::with_capacity(frames.len());
    for (frame, anchor) in frames.into_iter().zip(&manifest.anchors) {
        ensure!(
            frame.ordinal == anchor.ordinal
                && frame.segment == anchor.segment
                && frame.available_ns == anchor.observed_at_ns
                && frame.event_ns == anchor.observed_at_ns,
            "native row/anchor order or clock changed"
        );
        let feature_map = features
            .spec()
            .feature_names
            .iter()
            .cloned()
            .zip(frame.values.iter().copied())
            .collect::<BTreeMap<_, _>>();
        let current = feature_map
            .get("mid_price")
            .context("native current price missing")?;
        let future = prices
            .get(&(anchor.segment.as_str(), anchor.future_at_ns))
            .context("actual native future price missing")?;
        ensure!(
            current.is_finite() && *current > 0.0 && future.is_finite() && *future > 0.0,
            "invalid actual native prices"
        );
        let label = future / current - 1.0;
        ensure!(label.is_finite(), "nonfinite derived native label");
        rows.push(NativeResearchRowV1 {
            series_id: anchor.series_id,
            available_time: DateTime::from_timestamp_nanos(anchor.observed_at_ns),
            label_available_time: DateTime::from_timestamp_nanos(anchor.mature_at_ns),
            signal: anchor.signal,
            features: feature_map,
            label,
            fee_bps: anchor.fee_bps,
            funding_bps: anchor.funding_bps,
            pit_funding: anchor.pit_funding,
            latency_bps: anchor.latency_bps,
        });
    }
    Ok(rows)
}
fn verify_replay_boundaries(
    manifest: &CampaignPreparedInputsV1,
    input: &SharedInput,
) -> Result<()> {
    let mut previous = None;
    let mut first = true;
    let mut segment = None;
    let mut actual = Vec::new();
    for block in input.blocks() {
        let TypedBlock::Replay(rows) = block.as_ref() else {
            anyhow::bail!("native replay exit changed its type")
        };
        for row in rows {
            ensure!(
                row.available_ns < manifest.original.authorized_context_end_ns
                    && row.event_ns == row.available_ns,
                "native replay exposes withheld price or wrong availability"
            );
            ensure!(
                segment.is_none_or(|s| s == row.segment.as_str())
                    && previous
                        .is_none_or(|(ordinal, clock)| row.ordinal == ordinal + 1
                            && row.available_ns >= clock),
                "native replay gap/session continuity changed"
            );
            if first {
                ensure!(
                    matches!(row.payload, crate::data::ReplayPayload::Snapshot { .. }),
                    "native replay is not snapshot seeded"
                );
                first = false;
            }
            previous = Some((row.ordinal, row.available_ns));
            segment = Some(row.segment.as_str());
            actual.push(row);
        }
    }
    ensure!(!first, "missing native replay bytes");
    ensure!(
        valid_digest(&manifest.replay_rows_sha256)
            && identity(&actual)? == manifest.replay_rows_sha256,
        "native replay rows differ from the frozen source projection"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        data::{BlockRef, DataViewSpec, ReplayEvent, ReplayPayload},
        prepared::{encode, AcquiredBlocks},
        sha256,
    };
    const SECOND: i64 = 1_000_000_000;

    fn frame(ordinal: u64, second: i64, price: f64) -> FeatureFrame {
        FeatureFrame {
            segment: "native-series-1".into(),
            ordinal,
            event_ns: second * SECOND,
            available_ns: second * SECOND,
            values: vec![price],
        }
    }
    fn view(
        block: TypedBlock,
        names: Vec<String>,
        end: i64,
        source: &NativeSourceBindingV1,
        bytes: &mut BTreeMap<String, Vec<u8>>,
    ) -> PreparedViewRefV1 {
        let content = encode(&block).unwrap();
        let digest = sha256(&content);
        let (rows, exit) = match &block {
            TypedBlock::Features(rows) => (rows.len(), Exit::Features),
            TypedBlock::Replay(rows) => (rows.len(), Exit::Replay),
            _ => unreachable!(),
        };
        let manifest = PublishedView {
            prepared_id: "5".repeat(64),
            spec: DataViewSpec {
                schema: 1,
                venue: "binance".into(),
                instrument: "BTCUSDT".into(),
                market: "spot".into(),
                depth: 5,
                sources: vec![source.feature_sha256.clone()],
                normalizer_sha256: "6".repeat(64),
                feature_sql_sha256: "7".repeat(64),
                feature_names: names,
                window: Window {
                    start_ns: SECOND,
                    end_ns: end * SECOND,
                },
                lookback_ns: 0,
                horizons_ns: vec![SECOND],
                label_tolerance_ns: 0,
                fit_cutoff_ns: end * SECOND,
                split: Split::Validation,
            },
            blocks: vec![BlockRef {
                sha256: digest.clone(),
                bytes: content.len() as u64,
                rows: rows as u64,
                decoded_bytes: 1_000_000,
                exit,
            }],
            producer_image: source.build.image_identity.clone(),
            source_receipt_sha256: source.preparation_receipt_sha256.clone(),
        };
        bytes.insert(digest, content);
        PreparedViewRefV1 {
            manifest_sha256: identity(&manifest).unwrap(),
            manifest,
        }
    }
    fn fixture() -> (
        CampaignPreparedInputsV1,
        Vec<NativeResearchRowV1>,
        AcquiredBlocks,
    ) {
        let source = NativeSourceBindingV1 {
            build: SourceBuildRefV1 {
                source_revision: "b".repeat(40),
                image_identity: format!("registry/source@sha256:{}", "a".repeat(64)),
            },
            preparation_run_id: "native-preparation-1".into(),
            preparation_receipt_sha256: "c".repeat(64),
            feature_sha256: "1".repeat(64),
            materialization_sha256: "2".repeat(64),
            replay_artifact_sha256: "3".repeat(64),
            replay_manifest_sha256: "4".repeat(64),
        };
        let mut bytes = BTreeMap::new();
        let features = view(
            TypedBlock::Features(vec![frame(0, 1, 20.0), frame(1, 2, 22.0)]),
            vec!["mid_price".into()],
            3,
            &source,
            &mut bytes,
        );
        let future_marks = view(
            TypedBlock::Features(vec![frame(0, 2, 22.0), frame(1, 3, 24.0)]),
            vec!["mid_price".into()],
            4,
            &source,
            &mut bytes,
        );
        let replay_rows = (0_u64..2)
            .map(|ordinal| ReplayEvent {
                segment: "native-series-1".into(),
                ordinal,
                event_ns: (ordinal as i64 + 1) * SECOND,
                available_ns: (ordinal as i64 + 1) * SECOND,
                payload: ReplayPayload::Snapshot {
                    bids: vec![[19.9 + ordinal as f64, 1.0]],
                    asks: vec![[20.1 + ordinal as f64, 1.0]],
                },
            })
            .collect::<Vec<_>>();
        let replay = view(
            TypedBlock::Replay(replay_rows.clone()),
            vec!["mid_price".into()],
            4,
            &source,
            &mut bytes,
        );
        let rows = [(1, 20.0, 22.0), (2, 22.0, 24.0)]
            .into_iter()
            .map(|(second, price, future)| NativeResearchRowV1 {
                series_id: 1,
                available_time: DateTime::from_timestamp_nanos(second * SECOND),
                label_available_time: DateTime::from_timestamp_nanos((second + 1) * SECOND),
                signal: 0.0,
                features: BTreeMap::from([("mid_price".into(), price)]),
                label: future / price - 1.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.5,
            })
            .collect::<Vec<_>>();
        let protocol_json = "{\"fixture\":\"clock-bound-native-protocol\"}".to_string();
        let original = NativeDatasetMetadataV1 {
            total_rows: 8,
            original_window: Window {
                start_ns: SECOND,
                end_ns: 9 * SECOND,
            },
            original_rows_sha256: "8".repeat(64),
            protocol_sha256: sha256(protocol_json.as_bytes()),
            protocol_json,
            search_rows: 0..2,
            visible_rows: 0..2,
            development_window: Window {
                start_ns: SECOND,
                end_ns: 3 * SECOND,
            },
            authorized_context_end_ns: 4 * SECOND,
            selection: Some(OpaqueWithheldPartitionV1 {
                original_rows: 4..5,
                window: Window {
                    start_ns: 5 * SECOND,
                    end_ns: 6 * SECOND,
                },
                source_content_sha256: "9".repeat(64),
            }),
            holdout: OpaqueWithheldPartitionV1 {
                original_rows: 6..8,
                window: Window {
                    start_ns: 7 * SECOND,
                    end_ns: 9 * SECOND,
                },
                source_content_sha256: "a".repeat(64),
            },
        };
        let anchors = (0_u64..2)
            .map(|ordinal| NativeLabelAnchorV1 {
                ordinal,
                series_id: 1,
                segment: "native-series-1".into(),
                observed_at_ns: (ordinal as i64 + 1) * SECOND,
                future_at_ns: (ordinal as i64 + 2) * SECOND,
                mature_at_ns: (ordinal as i64 + 2) * SECOND,
                signal: 0.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.5,
            })
            .collect();
        (
            CampaignPreparedInputsV1 {
                schema_version: CAMPAIGN_PREPARED_INPUTS_SCHEMA.into(),
                source,
                original,
                label_recipe: NATIVE_LABEL_RECIPE.into(),
                anchors,
                development_rows_sha256: identity(&rows).unwrap(),
                replay_rows_sha256: identity(&replay_rows).unwrap(),
                features,
                future_marks,
                replay,
            },
            rows,
            AcquiredBlocks { bytes },
        )
    }
    #[test]
    fn native_decoder_reconstructs_exact_research_rows_without_training_exit() {
        let (manifest, rows, mut source) = fixture();
        let id = manifest.id().unwrap();
        let expected = manifest.expected_native().unwrap();
        let verified = manifest
            .verify(&id, &expected, &mut source, 4_000_000)
            .unwrap();
        assert_eq!(verified.rows(), rows);
        assert_eq!(verified.id(), id);
        assert!(verified
            .manifest()
            .features
            .manifest
            .blocks
            .iter()
            .all(|b| b.exit == Exit::Features));
        assert_eq!(verified.original_metadata().total_rows, 8);
        assert_eq!(verified.rows().len(), 2);
    }
    #[test]
    fn native_source_identity_changed_bytes_and_train_role_are_rejected() {
        let (manifest, _, mut source) = fixture();
        let id = manifest.id().unwrap();
        let expected = manifest.expected_native().unwrap();
        source
            .bytes
            .get_mut(&manifest.features.manifest.blocks[0].sha256)
            .unwrap()[0] ^= 1;
        assert!(manifest
            .clone()
            .verify(&id, &expected, &mut source, 4_000_000)
            .is_err());
        let mut altered = manifest.clone();
        altered.features.manifest.spec.split = Split::Train;
        altered.features.manifest_sha256 = identity(&altered.features.manifest).unwrap();
        assert!(altered.id().is_err());
        let mut expected = expected;
        expected.source.preparation_receipt_sha256 = "d".repeat(64);
        let (_, _, mut source) = fixture();
        assert!(manifest
            .verify(&id, &expected, &mut source, 4_000_000)
            .is_err());
    }
    #[test]
    fn native_future_price_maturity_and_coverage_cannot_cross_or_hide_withheld_data() {
        let (manifest, _, _) = fixture();
        let mut altered = manifest.clone();
        altered.anchors[1].future_at_ns = 5 * SECOND;
        altered.anchors[1].mature_at_ns = 5 * SECOND;
        assert!(altered.id().is_err());
        let mut altered = manifest.clone();
        altered.anchors[1].mature_at_ns = 4 * SECOND;
        assert!(altered.id().is_err());
        let mut altered = manifest;
        altered.anchors.pop();
        assert!(altered.id().is_err());
    }
    #[test]
    fn native_decoder_requires_original_semantic_prices_even_after_block_rehash() {
        let (mut manifest, _, mut source) = fixture();
        let expected = manifest.expected_native().unwrap();
        let old = manifest.future_marks.manifest.blocks[0].sha256.clone();
        let mut block = crate::prepared::decode(source.bytes.get(&old).unwrap()).unwrap();
        let TypedBlock::Features(ref mut frames) = block else {
            unreachable!()
        };
        frames[0].values[0] += 1.0;
        let bytes = encode(&block).unwrap();
        let hash = sha256(&bytes);
        source.bytes.insert(hash.clone(), bytes.clone());
        manifest.future_marks.manifest.blocks[0].sha256 = hash;
        manifest.future_marks.manifest.blocks[0].bytes = bytes.len() as u64;
        manifest.future_marks.manifest_sha256 = identity(&manifest.future_marks.manifest).unwrap();
        let id = manifest.id().unwrap();
        let error = manifest
            .verify(&id, &expected, &mut source, 4_000_000)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("ResearchRow"), "{error}");
    }
}
