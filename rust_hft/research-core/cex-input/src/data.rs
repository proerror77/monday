//! One immutable DataView, three typed exits, and bounded shared verified inputs.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{identity, sha256, valid_digest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub start_ns: i64,
    pub end_ns: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DataViewSpec {
    pub schema: u32,
    pub venue: String,
    pub instrument: String,
    pub market: String,
    pub depth: u16,
    pub sources: Vec<String>,
    pub normalizer_sha256: String,
    pub feature_sql_sha256: String,
    pub feature_names: Vec<String>,
    pub window: Window,
    pub lookback_ns: i64,
    pub horizons_ns: Vec<i64>,
    pub label_tolerance_ns: i64,
    pub fit_cutoff_ns: i64,
    pub split: Split,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Validation,
    Holdout,
}

impl DataViewSpec {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "unsupported DataView schema");
        ensure!(
            !self.venue.is_empty() && !self.instrument.is_empty() && !self.market.is_empty(),
            "missing instrument"
        );
        ensure!(self.depth > 0 && self.depth <= 4096, "invalid depth");
        ensure!(
            self.window.start_ns > 0 && self.window.start_ns < self.window.end_ns,
            "invalid window"
        );
        ensure!(
            self.lookback_ns >= 0 && self.window.start_ns >= self.lookback_ns,
            "invalid lookback"
        );
        ensure!(
            self.fit_cutoff_ns >= self.window.end_ns,
            "cutoff precedes split end"
        );
        ensure!(self.label_tolerance_ns >= 0, "negative label tolerance");
        ensure!(
            !self.horizons_ns.is_empty() && self.horizons_ns.len() <= 64,
            "invalid horizon count"
        );
        ensure!(
            self.horizons_ns[0] > 0 && self.horizons_ns.windows(2).all(|w| w[0] < w[1]),
            "horizons must be positive, unique and sorted"
        );
        self.window
            .end_ns
            .checked_add(*self.horizons_ns.last().unwrap())
            .and_then(|v| v.checked_add(self.label_tolerance_ns))
            .context("label clock overflow")?;
        ensure!(
            !self.sources.is_empty()
                && self.sources.windows(2).all(|w| w[0] < w[1])
                && self.sources.iter().all(|v| valid_digest(v)),
            "sources must be sorted unique immutable identities"
        );
        ensure!(
            valid_digest(&self.normalizer_sha256) && valid_digest(&self.feature_sql_sha256),
            "invalid producer identity"
        );
        ensure!(
            !self.feature_names.is_empty() && self.feature_names.len() <= 4096,
            "invalid feature schema"
        );
        let mut names = self.feature_names.clone();
        names.sort();
        names.dedup();
        ensure!(
            names.len() == self.feature_names.len() && names.iter().all(|s| !s.is_empty()),
            "duplicate feature columns"
        );
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity(self)
    }
}

/// Normalizer output: exchange sequence rebuilding stays outside SQL. A segment
/// ends at a gap or session boundary; it is never stitched by SQL interpolation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeatureFrame {
    pub segment: String,
    pub ordinal: u64,
    pub event_ns: i64,
    pub available_ns: i64,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub horizon_ns: i64,
    pub target_event_ns: i64,
    pub mature_ns: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrainingFrame {
    pub feature: FeatureFrame,
    pub labels: Vec<Label>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayPayload {
    Snapshot {
        bids: Vec<[f64; 2]>,
        asks: Vec<[f64; 2]>,
    },
    Delta {
        bids: Vec<[f64; 2]>,
        asks: Vec<[f64; 2]>,
    },
    Trade {
        price: f64,
        quantity: f64,
        buyer_initiated: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReplayEvent {
    pub segment: String,
    pub ordinal: u64,
    pub event_ns: i64,
    pub available_ns: i64,
    pub payload: ReplayPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedBlock {
    Features(Vec<FeatureFrame>),
    Training(Vec<TrainingFrame>),
    Replay(Vec<ReplayEvent>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    Features,
    Training,
    Replay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlockRef {
    pub sha256: String,
    pub bytes: u64,
    pub rows: u64,
    pub decoded_bytes: u64,
    pub exit: Exit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishedView {
    /// Physical CH generation, isolated per preparation attempt.
    pub prepared_id: String,
    pub spec: DataViewSpec,
    pub blocks: Vec<BlockRef>,
    pub producer_image: String,
    pub source_receipt_sha256: String,
}

impl PublishedView {
    /// `expected` comes from the authoritative published PG record, never from
    /// a colocated untrusted manifest or a worker-supplied boolean.
    pub fn verify(&self, expected: &str) -> Result<()> {
        self.spec.validate()?;
        ensure!(
            valid_digest(&self.prepared_id),
            "invalid prepared generation"
        );
        ensure!(
            valid_digest(expected) && identity(self)? == expected,
            "untrusted DataView manifest"
        );
        ensure!(
            valid_digest(&self.source_receipt_sha256),
            "missing source verification receipt"
        );
        ensure!(
            self.producer_image
                .rsplit_once("@sha256:")
                .is_some_and(|(_, h)| valid_digest(h)),
            "producer image is not pinned"
        );
        ensure!(!self.blocks.is_empty(), "empty publication");
        let mut seen = std::collections::BTreeSet::new();
        for block in &self.blocks {
            ensure!(
                valid_digest(&block.sha256) && seen.insert(&block.sha256),
                "invalid or repeated block"
            );
            ensure!(
                block.bytes > 0
                    && block.bytes <= 16 * 1024 * 1024
                    && block.rows > 0
                    && block.rows <= 4096
                    && block.decoded_bytes > 0
                    && block.decoded_bytes <= 64 * 1024 * 1024,
                "unbounded block"
            );
        }
        Ok(())
    }
}

/// Transport supplies bytes only. The verified publisher codec is fixed here;
/// an untrusted transport cannot substitute different rows after a hash check.
pub trait BlockSource {
    fn read(&mut self, block: &BlockRef) -> Result<Vec<u8>>;
}

/// Cache retains immutable, already hash-checked bytes/typed values. A miss is
/// reverified. Nothing trusts mutable inode/mtime or skips hashes on disk reads.
pub struct VerifiedCache {
    max_bytes: u64,
    used_bytes: u64,
    blocks: BTreeMap<String, (BlockRef, Arc<TypedBlock>)>,
    verified_view: Option<(String, PublishedView)>,
}

impl VerifiedCache {
    pub fn new(max_bytes: u64) -> Result<Self> {
        ensure!(max_bytes > 0, "zero cache budget");
        Ok(Self {
            max_bytes,
            used_bytes: 0,
            blocks: BTreeMap::new(),
            verified_view: None,
        })
    }

    pub fn load(
        &mut self,
        view: &PublishedView,
        manifest_sha: &str,
        exit: Exit,
        source: &mut impl BlockSource,
    ) -> Result<SharedInput> {
        if let Some((known_id, known_view)) = &self.verified_view {
            if known_id == manifest_sha {
                ensure!(
                    known_view == view,
                    "validated view identity reused with changed metadata"
                );
            } else {
                view.verify(manifest_sha)?;
                self.verified_view = Some((manifest_sha.into(), view.clone()));
            }
        } else {
            view.verify(manifest_sha)?;
            self.verified_view = Some((manifest_sha.into(), view.clone()));
        }
        let selected: Vec<_> = view.blocks.iter().filter(|b| b.exit == exit).collect();
        ensure!(!selected.is_empty(), "requested exit is absent");
        let budget = selected.iter().try_fold(0_u64, |n, b| {
            n.checked_add(b.decoded_bytes)
                .context("input byte overflow")
        })?;
        ensure!(
            budget <= self.max_bytes,
            "bounded batch exceeds cache budget"
        );
        let mut output = Vec::new();
        for block in selected {
            if let Some((cached_ref, cached)) = self.blocks.get(&block.sha256) {
                ensure!(
                    cached_ref == block,
                    "same bytes claimed with a different block contract"
                );
                validate_block(cached, block, &view.spec)?;
                output.push(Arc::clone(cached));
                continue;
            }
            let bytes = source.read(block)?;
            ensure!(
                bytes.len() as u64 == block.bytes && sha256(&bytes) == block.sha256,
                "block integrity failure"
            );
            let typed = crate::prepared::decode(&bytes)?;
            validate_block(&typed, block, &view.spec)?;
            // External SharedInput Arcs may retain blocks. Their batch budgets
            // must be accounted by the orchestrator; eviction cannot free them.
            if self
                .used_bytes
                .checked_add(block.decoded_bytes)
                .context("cache overflow")?
                > self.max_bytes
            {
                self.blocks.clear();
                self.used_bytes = 0;
            }
            let typed = Arc::new(typed);
            self.used_bytes += block.decoded_bytes;
            self.blocks
                .insert(block.sha256.clone(), (block.clone(), Arc::clone(&typed)));
            output.push(typed);
        }
        let input = SharedInput {
            spec: view.spec.clone(),
            manifest_sha256: manifest_sha.into(),
            blocks: output,
        };
        input.validate_order()?;
        Ok(input)
    }
}

#[derive(Clone)]
pub struct SharedInput {
    spec: DataViewSpec,
    pub(crate) manifest_sha256: String,
    pub(crate) blocks: Vec<Arc<TypedBlock>>,
}

impl SharedInput {
    pub fn spec(&self) -> &DataViewSpec {
        &self.spec
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    pub fn blocks(&self) -> &[Arc<TypedBlock>] {
        &self.blocks
    }
    fn validate_order(&self) -> Result<()> {
        let mut order = BlockOrder::default();
        for block in &self.blocks {
            order.observe(block)?;
        }
        Ok(())
    }
}

/// Streaming validation retains only segment identities and the preceding key,
/// rather than retaining a whole prepared view during publication readback.
#[derive(Default)]
pub struct BlockOrder {
    previous: Option<(String, u64, i64)>,
    closed: std::collections::BTreeSet<String>,
}
impl BlockOrder {
    pub fn observe(&mut self, block: &TypedBlock) -> Result<()> {
        let keys: Vec<_> = match block {
            TypedBlock::Features(rows) => rows
                .iter()
                .map(|r| (&r.segment, r.ordinal, r.available_ns, false))
                .collect(),
            TypedBlock::Training(rows) => rows
                .iter()
                .map(|r| {
                    (
                        &r.feature.segment,
                        r.feature.ordinal,
                        r.feature.available_ns,
                        false,
                    )
                })
                .collect(),
            TypedBlock::Replay(rows) => rows
                .iter()
                .map(|r| {
                    (
                        &r.segment,
                        r.ordinal,
                        r.available_ns,
                        matches!(r.payload, ReplayPayload::Snapshot { .. }),
                    )
                })
                .collect(),
        };
        for (segment, ordinal, clock, snapshot) in keys {
            if self.previous.as_ref().is_some_and(|p| &p.0 == segment) {
                let p = self.previous.as_ref().unwrap();
                let ordered = if matches!(block, TypedBlock::Replay(_)) {
                    ordinal == p.1.checked_add(1).context("ordinal overflow")?
                } else {
                    ordinal > p.1
                };
                ensure!(
                    ordered && clock >= p.2,
                    "gap or reversed clock across blocks"
                );
            } else {
                ensure!(!self.closed.contains(segment), "reopened segment");
                if matches!(block, TypedBlock::Replay(_)) {
                    ensure!(snapshot, "replay segment lacks initial snapshot");
                }
                if let Some(p) = &self.previous {
                    ensure!(clock >= p.2, "segment availability reversed");
                    self.closed.insert(p.0.clone());
                }
            }
            self.previous = Some((segment.clone(), ordinal, clock));
        }
        Ok(())
    }
}

fn validate_feature(frame: &FeatureFrame, spec: &DataViewSpec) -> Result<()> {
    ensure!(
        !frame.segment.is_empty() && frame.event_ns > 0 && frame.available_ns >= frame.event_ns,
        "invalid feature clock"
    );
    ensure!(
        frame.available_ns >= spec.window.start_ns - spec.lookback_ns
            && frame.available_ns < spec.window.end_ns,
        "feature outside admitted lookback/split"
    );
    ensure!(
        frame.values.len() == spec.feature_names.len()
            && frame.values.iter().all(|v| v.is_finite()),
        "feature schema mismatch"
    );
    Ok(())
}

pub fn validate_block(block: &TypedBlock, reference: &BlockRef, spec: &DataViewSpec) -> Result<()> {
    ensure!(
        memory_bytes(block) <= reference.decoded_bytes,
        "decoded allocation exceeds admitted block budget"
    );
    let count = match (block, &reference.exit) {
        (TypedBlock::Features(rows), Exit::Features) => {
            for row in rows {
                validate_feature(row, spec)?;
            }
            rows.len()
        }
        (TypedBlock::Training(rows), Exit::Training) => {
            ensure!(
                spec.split == Split::Train,
                "labels cannot be read through the training exit of a sealed split"
            );
            for row in rows {
                validate_feature(&row.feature, spec)?;
                ensure!(
                    row.feature.available_ns >= spec.window.start_ns,
                    "lookback cannot become a fit sample"
                );
                ensure!(
                    row.labels.len() == spec.horizons_ns.len(),
                    "incomplete horizon labels"
                );
                for (label, horizon) in row.labels.iter().zip(&spec.horizons_ns) {
                    let target = row
                        .feature
                        .available_ns
                        .checked_add(*horizon)
                        .context("label target overflow")?;
                    ensure!(
                        label.horizon_ns == *horizon
                            && label.target_event_ns >= target
                            && label.target_event_ns <= target + spec.label_tolerance_ns,
                        "label uses row offset or wrong target clock"
                    );
                    ensure!(
                        label.target_event_ns < spec.window.end_ns
                            && label.mature_ns >= label.target_event_ns
                            && label.mature_ns <= spec.fit_cutoff_ns
                            && label.value.is_finite(),
                        "immature or cross-split label"
                    );
                }
            }
            rows.len()
        }
        (TypedBlock::Replay(rows), Exit::Replay) => {
            for row in rows {
                ensure!(
                    !row.segment.is_empty()
                        && row.event_ns > 0
                        && row.available_ns >= row.event_ns
                        && row.available_ns >= spec.window.start_ns - spec.lookback_ns
                        && row.available_ns < spec.window.end_ns,
                    "invalid replay clock"
                );
                match &row.payload {
                    ReplayPayload::Snapshot { bids, asks }
                    | ReplayPayload::Delta { bids, asks } => {
                        ensure!(
                            bids.len() <= usize::from(spec.depth)
                                && asks.len() <= usize::from(spec.depth),
                            "replay depth exceeds contract"
                        );
                        ensure!(
                            bids.iter().chain(asks).all(|p| p[0].is_finite()
                                && p[0] > 0.0
                                && p[1].is_finite()
                                && p[1] >= 0.0),
                            "invalid level"
                        );
                        if matches!(row.payload, ReplayPayload::Snapshot { .. }) {
                            ensure!(!bids.is_empty() && !asks.is_empty(), "empty snapshot");
                        }
                    }
                    ReplayPayload::Trade {
                        price, quantity, ..
                    } => ensure!(
                        price.is_finite()
                            && *price > 0.0
                            && quantity.is_finite()
                            && *quantity > 0.0,
                        "invalid trade"
                    ),
                }
            }
            rows.len()
        }
        _ => bail!("decoder returned the wrong typed exit"),
    };
    ensure!(count as u64 == reference.rows, "block row count mismatch");
    Ok(())
}

pub fn memory_bytes(block: &TypedBlock) -> u64 {
    use std::mem::size_of;
    let feature = |r: &FeatureFrame| r.segment.capacity() + r.values.capacity() * size_of::<f64>();
    let allocated = match block {
        TypedBlock::Features(rows) => {
            rows.capacity() * size_of::<FeatureFrame>() + rows.iter().map(feature).sum::<usize>()
        }
        TypedBlock::Training(rows) => {
            rows.capacity() * size_of::<TrainingFrame>()
                + rows
                    .iter()
                    .map(|r| feature(&r.feature) + r.labels.capacity() * size_of::<Label>())
                    .sum::<usize>()
        }
        TypedBlock::Replay(rows) => {
            rows.capacity() * size_of::<ReplayEvent>()
                + rows
                    .iter()
                    .map(|r| {
                        r.segment.capacity()
                            + match &r.payload {
                                ReplayPayload::Snapshot { bids, asks }
                                | ReplayPayload::Delta { bids, asks } => {
                                    (bids.capacity() + asks.capacity()) * size_of::<[f64; 2]>()
                                }
                                ReplayPayload::Trade { .. } => 0,
                            }
                    })
                    .sum::<usize>()
        }
    };
    (allocated + size_of::<TypedBlock>()) as u64
}

/// Reference temporal join for contract tests and non-SQL adapters. Production
/// SQL prepares all horizons together; no per-trial feature recomputation.
pub fn temporal_labels(
    anchor: &FeatureFrame,
    observations: &[(String, i64, i64, f64)],
    spec: &DataViewSpec,
) -> Result<Option<Vec<Label>>> {
    spec.validate()?;
    validate_feature(anchor, spec)?;
    ensure!(
        anchor.available_ns >= spec.window.start_ns,
        "anchor is only lookback context"
    );
    ensure!(anchor.values[0] > 0.0, "nonpositive anchor mid");
    let mut labels = Vec::new();
    for horizon in &spec.horizons_ns {
        let target = anchor
            .available_ns
            .checked_add(*horizon)
            .context("target clock overflow")?;
        let candidate = observations
            .iter()
            .filter(|(segment, event, available, value)| {
                segment == &anchor.segment
                    && *event >= target
                    && *event <= target + spec.label_tolerance_ns
                    && *event < spec.window.end_ns
                    && *available >= *event
                    && *available <= spec.fit_cutoff_ns
                    && value.is_finite()
            })
            .min_by_key(|(_, event, available, _)| (*event, *available));
        let Some((_, event, available, value)) = candidate else {
            return Ok(None);
        };
        labels.push(Label {
            horizon_ns: *horizon,
            target_event_ns: *event,
            mature_ns: *available,
            value: *value / anchor.values[0] - 1.0,
        });
    }
    Ok(Some(labels))
}
