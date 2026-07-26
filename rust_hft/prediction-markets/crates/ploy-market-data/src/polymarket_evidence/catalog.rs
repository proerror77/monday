use super::{
    authenticate_polymarket_evidence_object, seal_polymarket_evidence_candidate_triplet,
    verify_polymarket_evidence_candidate, PolymarketCandidateSurfaceCoverage,
    PolymarketEvidenceContract, PolymarketEvidenceSequence, PolymarketEvidenceTradeCompletion,
    PolymarketEvidenceTriplet, PolymarketEvidenceTrustAnchor, VerifiedPolymarketEvidenceCandidate,
};
use anyhow::{bail, ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

const QUALIFICATION_SCHEMA: &str = "monday.polymarket.event_qualification.v1";
const BTC_5M_SECS: i64 = 300;

/// Immutable identity of the verifier that classified an evidence receipt.
/// Paths and mutable version strings are deliberately not accepted as identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolymarketCatalogVerifier {
    source_sha256: String,
    binary_sha256: String,
    configuration_sha256: String,
    policy_sha256: String,
}

impl PolymarketCatalogVerifier {
    pub fn new(
        source_sha256: String,
        binary_sha256: String,
        configuration_sha256: String,
        policy_sha256: String,
    ) -> Result<Self> {
        for (label, digest) in [
            ("verifier source", &source_sha256),
            ("verifier binary", &binary_sha256),
            ("verifier configuration", &configuration_sha256),
            ("verifier policy", &policy_sha256),
        ] {
            if !is_sha256(digest) {
                bail!("{label} digest must be 64 lowercase hexadecimal characters");
            }
        }
        Ok(Self {
            source_sha256,
            binary_sha256,
            configuration_sha256,
            policy_sha256,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolymarketResearchTask {
    Btc5mBacktest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolymarketCatalogReceiptState {
    Ready,
    Partial,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolymarketCatalogReason {
    EvidenceVerificationFailed,
    QualificationVerificationFailed,
    IncompleteEvidence,
    QualificationMismatch,
    SequenceMismatch,
    UnsupportedProduct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolymarketEvidenceAvailability {
    pub contract: Option<DateTime<Utc>>,
    pub books: Option<DateTime<Utc>>,
    pub references: Option<DateTime<Utc>>,
    pub trades: Option<DateTime<Utc>>,
    pub settlement: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolymarketCatalogReceipt {
    pub receipt_sha256: String,
    pub market_id: String,
    pub content_sha256: String,
    pub manifest_sha256: String,
    pub qualification_sha256: String,
    pub success_sha256: Option<String>,
    pub verifier: PolymarketCatalogVerifier,
    pub event_start: Option<DateTime<Utc>>,
    pub event_end: Option<DateTime<Utc>>,
    pub up_token_id: Option<String>,
    pub down_token_id: Option<String>,
    pub sequence: Option<PolymarketEvidenceSequence>,
    pub coverage: Option<PolymarketCandidateSurfaceCoverage>,
    pub trade_completion: Option<PolymarketEvidenceTradeCompletion>,
    pub availability: Option<PolymarketEvidenceAvailability>,
    pub state: PolymarketCatalogReceiptState,
    pub reasons: Vec<PolymarketCatalogReason>,
    pub supported_tasks: Vec<PolymarketResearchTask>,
}

/// In-memory append-only projection. Persistence belongs to a later catalog
/// service; callers can only add an exact digest-keyed receipt here.
#[derive(Debug, Default)]
pub struct PolymarketReadyEventCatalog {
    receipts: BTreeMap<String, PolymarketCatalogReceipt>,
}

impl PolymarketReadyEventCatalog {
    pub fn verify_and_append(
        &mut self,
        market_id: &str,
        triplet: &PolymarketEvidenceTriplet,
        evidence_anchor: &PolymarketEvidenceTrustAnchor,
        qualification_path: &Path,
        qualification_anchor_sha256: &str,
        verifier: PolymarketCatalogVerifier,
    ) -> Result<&PolymarketCatalogReceipt> {
        ensure!(!market_id.is_empty(), "catalog market_id must not be empty");
        ensure!(
            is_sha256(qualification_anchor_sha256),
            "invalid qualification digest"
        );
        let qualification = match authenticate_polymarket_evidence_object(
            qualification_path,
            qualification_anchor_sha256,
        ) {
            Ok(value) => value,
            Err(_) => {
                return self.append(rejected_receipt(
                    market_id,
                    evidence_anchor,
                    qualification_anchor_sha256,
                    verifier,
                    PolymarketCatalogReason::QualificationVerificationFailed,
                ))
            }
        };
        let evidence = match seal_polymarket_evidence_candidate_triplet(triplet, evidence_anchor)
            .and_then(verify_polymarket_evidence_candidate)
        {
            Ok(value) => value,
            Err(_) => {
                return self.append(rejected_receipt(
                    market_id,
                    evidence_anchor,
                    qualification.sha256(),
                    verifier,
                    PolymarketCatalogReason::EvidenceVerificationFailed,
                ))
            }
        };
        let mut receipt =
            match serde_json::from_slice::<ProducerQualification>(qualification.bytes()) {
                Ok(carrier) if carrier.market_id != market_id => rejected_from_evidence(
                    &evidence,
                    qualification.sha256(),
                    verifier,
                    PolymarketCatalogReason::QualificationMismatch,
                ),
                Ok(carrier) => classify(
                    &carrier,
                    &evidence,
                    qualification.sha256(),
                    verifier.clone(),
                )
                .unwrap_or_else(|_| {
                    rejected_from_evidence(
                        &evidence,
                        qualification.sha256(),
                        verifier,
                        PolymarketCatalogReason::QualificationVerificationFailed,
                    )
                }),
                Err(_) => rejected_from_evidence(
                    &evidence,
                    qualification.sha256(),
                    verifier,
                    PolymarketCatalogReason::QualificationVerificationFailed,
                ),
            };
        receipt.receipt_sha256 = receipt_digest(&receipt)?;
        self.append(receipt)
    }

    fn append(
        &mut self,
        mut receipt: PolymarketCatalogReceipt,
    ) -> Result<&PolymarketCatalogReceipt> {
        if receipt.receipt_sha256.is_empty() {
            receipt.receipt_sha256 = receipt_digest(&receipt)?;
        }
        let digest = receipt.receipt_sha256.clone();
        self.receipts.entry(digest.clone()).or_insert(receipt);
        Ok(self.receipts.get(&digest).expect("receipt inserted"))
    }

    pub fn ready_for(&self, task: PolymarketResearchTask) -> Vec<&PolymarketCatalogReceipt> {
        self.receipts
            .values()
            .filter(|receipt| {
                receipt.state == PolymarketCatalogReceiptState::Ready
                    && receipt.supported_tasks.contains(&task)
            })
            .collect()
    }

    pub fn receipts(&self) -> impl Iterator<Item = &PolymarketCatalogReceipt> {
        self.receipts.values()
    }

    /// Restore only #319's authenticated Ready receipts. The caller supplies
    /// JSON values so this module can retain control of the receipt identity
    /// reconstruction and re-hash every persisted record before admission.
    pub fn from_persisted_ready_receipts(receipts: Vec<Value>) -> Result<Self> {
        let mut catalog = Self::default();
        for value in receipts {
            let persisted: PersistedReadyCatalogReceipt =
                serde_json::from_value(value).map_err(|error| {
                    anyhow::anyhow!("parse persisted ready catalog receipt: {error}")
                })?;
            let receipt = persisted.into_receipt()?;
            let digest = receipt.receipt_sha256.clone();
            ensure!(
                !catalog.receipts.contains_key(&digest),
                "persisted ready catalog contains duplicate receipt_sha256 {digest}"
            );
            ensure!(
                !catalog
                    .receipts
                    .values()
                    .any(|existing| existing.market_id == receipt.market_id),
                "persisted ready catalog contains duplicate market_id {}",
                receipt.market_id
            );
            catalog.receipts.insert(digest, receipt);
        }
        Ok(catalog)
    }

    /// Bound variable-length Ready receipt fields before a caller serializes an
    /// artifact. This preserves the caller's fixed byte budget without first
    /// allocating a canonical representation of unbounded producer text.
    pub fn validate_ready_artifact_bounds(
        &self,
        max_entries: usize,
        max_text_bytes: usize,
    ) -> Result<()> {
        let mut ready_count = 0_usize;
        for receipt in self.receipts.values() {
            if receipt.state != PolymarketCatalogReceiptState::Ready {
                continue;
            }
            ready_count = ready_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Ready catalog count overflow"))?;
            ensure!(
                ready_count <= max_entries,
                "ready-event catalog exceeds the bounded entry count"
            );
            for (field, value) in [
                ("market_id", receipt.market_id.as_str()),
                (
                    "up_token_id",
                    receipt.up_token_id.as_deref().unwrap_or_default(),
                ),
                (
                    "down_token_id",
                    receipt.down_token_id.as_deref().unwrap_or_default(),
                ),
            ] {
                ensure!(
                    value.len() <= max_text_bytes,
                    "ready catalog {field} exceeds {max_text_bytes} bytes"
                );
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedReadyCatalogReceipt {
    receipt_sha256: String,
    market_id: String,
    content_sha256: String,
    manifest_sha256: String,
    qualification_sha256: String,
    success_sha256: Option<String>,
    verifier: PolymarketCatalogVerifier,
    event_start: Option<DateTime<Utc>>,
    event_end: Option<DateTime<Utc>>,
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    sequence: Option<PersistedEvidenceSequence>,
    coverage: Option<PersistedSurfaceCoverage>,
    trade_completion: Option<PersistedTradeCompletion>,
    availability: Option<PolymarketEvidenceAvailability>,
    state: PolymarketCatalogReceiptState,
    reasons: Vec<PolymarketCatalogReason>,
    supported_tasks: Vec<PolymarketResearchTask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedEvidenceSequence {
    start: u64,
    end: u64,
    gaps: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedSurfaceCoverage {
    up_book: u64,
    down_book: u64,
    trades: u64,
    reference: u64,
    settlement: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedTradeCompletion {
    trade_count: u64,
    trade_record_ids_sha256: String,
}

impl PersistedReadyCatalogReceipt {
    fn into_receipt(self) -> Result<PolymarketCatalogReceipt> {
        ensure!(
            self.state == PolymarketCatalogReceiptState::Ready,
            "persisted catalog receipt {} is not Ready",
            self.receipt_sha256
        );
        ensure!(
            self.reasons.is_empty(),
            "persisted Ready catalog receipt {} has rejection reasons",
            self.receipt_sha256
        );
        ensure!(
            self.supported_tasks == [PolymarketResearchTask::Btc5mBacktest],
            "persisted Ready catalog receipt {} has unsupported tasks",
            self.receipt_sha256
        );
        ensure!(!self.market_id.trim().is_empty() && self.market_id.trim() == self.market_id);
        for (label, digest) in [
            ("receipt", &self.receipt_sha256),
            ("content", &self.content_sha256),
            ("manifest", &self.manifest_sha256),
            ("qualification", &self.qualification_sha256),
        ] {
            ensure!(is_sha256(digest), "persisted {label} digest is invalid");
        }
        let success_sha256 = self
            .success_sha256
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing success_sha256"))?;
        ensure!(
            is_sha256(success_sha256),
            "persisted success digest is invalid"
        );
        let verifier = PolymarketCatalogVerifier::new(
            self.verifier.source_sha256,
            self.verifier.binary_sha256,
            self.verifier.configuration_sha256,
            self.verifier.policy_sha256,
        )?;
        let event_start = self
            .event_start
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing event_start"))?;
        let event_end = self
            .event_end
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing event_end"))?;
        ensure!(
            event_start < event_end,
            "persisted Ready receipt has an invalid event window"
        );
        let up_token_id = self
            .up_token_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing Up token"))?;
        let down_token_id = self
            .down_token_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing Down token"))?;
        ensure!(
            up_token_id != down_token_id,
            "persisted Ready receipt has identical tokens"
        );
        let sequence = self
            .sequence
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing sequence"))?;
        ensure!(
            sequence.start <= sequence.end && sequence.gaps == 0,
            "persisted Ready receipt has invalid sequence"
        );
        let coverage = self
            .coverage
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing coverage"))?;
        ensure!(
            coverage.up_book > 0
                && coverage.down_book > 0
                && coverage.trades > 0
                && coverage.reference > 0
                && coverage.settlement > 0,
            "persisted Ready receipt has incomplete coverage"
        );
        let trade_completion = self.trade_completion.ok_or_else(|| {
            anyhow::anyhow!("persisted Ready receipt is missing trade completion")
        })?;
        ensure!(
            trade_completion.trade_count > 0
                && is_sha256(&trade_completion.trade_record_ids_sha256),
            "persisted Ready receipt has invalid trade completion"
        );
        let availability = self
            .availability
            .ok_or_else(|| anyhow::anyhow!("persisted Ready receipt is missing availability"))?;
        let settlement = availability.settlement.ok_or_else(|| {
            anyhow::anyhow!("persisted Ready receipt is missing settlement availability")
        })?;
        ensure!(
            settlement >= event_end,
            "persisted Ready receipt settlement predates event end"
        );
        let receipt = PolymarketCatalogReceipt {
            receipt_sha256: self.receipt_sha256,
            market_id: self.market_id,
            content_sha256: self.content_sha256,
            manifest_sha256: self.manifest_sha256,
            qualification_sha256: self.qualification_sha256,
            success_sha256: self.success_sha256,
            verifier,
            event_start: Some(event_start),
            event_end: Some(event_end),
            up_token_id: Some(up_token_id),
            down_token_id: Some(down_token_id),
            sequence: Some(PolymarketEvidenceSequence {
                start: sequence.start,
                end: sequence.end,
                gaps: sequence.gaps,
            }),
            coverage: Some(PolymarketCandidateSurfaceCoverage {
                up_book: coverage.up_book,
                down_book: coverage.down_book,
                trades: coverage.trades,
                reference: coverage.reference,
                settlement: coverage.settlement,
            }),
            trade_completion: Some(PolymarketEvidenceTradeCompletion {
                trade_count: trade_completion.trade_count,
                trade_record_ids_sha256: trade_completion.trade_record_ids_sha256,
            }),
            availability: Some(availability),
            state: self.state,
            reasons: self.reasons,
            supported_tasks: self.supported_tasks,
        };
        ensure!(
            receipt_digest(&receipt)? == receipt.receipt_sha256,
            "persisted Ready receipt digest does not match its canonical content"
        );
        Ok(receipt)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerQualification {
    schema: String,
    verifier_contract: String,
    market_id: String,
    symbol: String,
    event_start: DateTime<Utc>,
    event_end: DateTime<Utc>,
    up_token_id: String,
    down_token_id: String,
    verified_token_ids: Option<[String; 2]>,
    #[serde(rename = "state")]
    _state: ProducerState,
    #[serde(rename = "reasons")]
    _reasons: Vec<String>,
    #[serde(rename = "retry")]
    _retry: bool,
    producer: ProducerIdentity,
    source_closed: bool,
    up_book: ProducerSurface,
    down_book: ProducerSurface,
    trades: ProducerSurface,
    reference: ProducerSurface,
    settlement: ProducerSurface,
    request_outcomes: Option<Vec<ProducerRequest>>,
    source_clocks: Option<ProducerClocks>,
    sequence: ProducerSequence,
    evidence_digests: ProducerEvidenceDigests,
    token_identity_matches: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProducerState {
    Ready,
    Partial,
    Rejected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerIdentity {
    source_sha: String,
    image_digest: String,
    configuration_sha256: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProducerSurface {
    Complete,
    Incomplete,
    Contradictory,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerRequest {
    surface: String,
    status: String,
    completed_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerClocks {
    opened_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerSequence {
    start: u64,
    end: u64,
    gaps: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerEvidenceDigests {
    expected_content_sha256: String,
    expected_manifest_sha256: String,
}

fn classify(
    carrier: &ProducerQualification,
    evidence: &VerifiedPolymarketEvidenceCandidate,
    qualification_sha256: &str,
    verifier: PolymarketCatalogVerifier,
) -> Result<PolymarketCatalogReceipt> {
    ensure!(
        carrier.schema == QUALIFICATION_SCHEMA,
        "unsupported qualification schema"
    );
    ensure!(
        !carrier.verifier_contract.is_empty(),
        "qualification verifier contract is empty"
    );
    let contract = evidence
        .contracts()
        .first()
        .ok_or_else(|| anyhow::anyhow!("candidate has no contract"))?;
    ensure!(
        evidence.contracts().len() == 1,
        "candidate must contain one event-local contract"
    );
    let mut reasons = Vec::new();
    if contract.symbol != "BTCUSDT"
        || (contract.event_end - contract.event_start).num_seconds() != BTC_5M_SECS
    {
        reasons.push(PolymarketCatalogReason::UnsupportedProduct);
    }
    if !carrier_matches(carrier, evidence, contract) {
        reasons.push(PolymarketCatalogReason::QualificationMismatch);
    }
    let sequence = evidence.sequence();
    if carrier.sequence.start != sequence.start
        || carrier.sequence.end != sequence.end
        || carrier.sequence.gaps != sequence.gaps
    {
        reasons.push(PolymarketCatalogReason::SequenceMismatch);
    }
    let coverage = evidence.coverage();
    if coverage.up_book == 0
        || coverage.down_book == 0
        || coverage.trades == 0
        || coverage.reference == 0
        || coverage.settlement == 0
    {
        reasons.push(PolymarketCatalogReason::IncompleteEvidence);
    }
    if evidence.trade_completion().is_none() {
        reasons.push(PolymarketCatalogReason::IncompleteEvidence);
    }
    let rejected = reasons
        .iter()
        .any(|reason| !matches!(reason, PolymarketCatalogReason::IncompleteEvidence));
    let state = if rejected {
        PolymarketCatalogReceiptState::Rejected
    } else if reasons.is_empty() {
        PolymarketCatalogReceiptState::Ready
    } else {
        PolymarketCatalogReceiptState::Partial
    };
    Ok(PolymarketCatalogReceipt {
        receipt_sha256: String::new(),
        market_id: contract.market_id.clone(),
        content_sha256: evidence.identity().content_sha256.clone(),
        manifest_sha256: evidence.identity().manifest_sha256.clone(),
        qualification_sha256: qualification_sha256.to_owned(),
        success_sha256: Some(evidence.success_sha256().to_owned()),
        verifier,
        event_start: Some(contract.event_start),
        event_end: Some(contract.event_end),
        up_token_id: Some(contract.up_token_id.clone()),
        down_token_id: Some(contract.down_token_id.clone()),
        sequence: Some(sequence),
        coverage: Some(coverage),
        trade_completion: evidence.trade_completion().cloned(),
        availability: Some(availability(evidence)),
        state,
        reasons,
        supported_tasks: if state == PolymarketCatalogReceiptState::Ready {
            vec![PolymarketResearchTask::Btc5mBacktest]
        } else {
            Vec::new()
        },
    })
}

fn rejected_receipt(
    market_id: &str,
    evidence_anchor: &PolymarketEvidenceTrustAnchor,
    qualification_sha256: &str,
    verifier: PolymarketCatalogVerifier,
    reason: PolymarketCatalogReason,
) -> PolymarketCatalogReceipt {
    PolymarketCatalogReceipt {
        receipt_sha256: String::new(),
        market_id: market_id.to_owned(),
        content_sha256: evidence_anchor.expected_content_sha256(),
        manifest_sha256: evidence_anchor.expected_manifest_sha256(),
        qualification_sha256: qualification_sha256.to_owned(),
        success_sha256: None,
        verifier,
        event_start: None,
        event_end: None,
        up_token_id: None,
        down_token_id: None,
        sequence: None,
        coverage: None,
        trade_completion: None,
        availability: None,
        state: PolymarketCatalogReceiptState::Rejected,
        reasons: vec![reason],
        supported_tasks: Vec::new(),
    }
}

fn rejected_from_evidence(
    evidence: &VerifiedPolymarketEvidenceCandidate,
    qualification_sha256: &str,
    verifier: PolymarketCatalogVerifier,
    reason: PolymarketCatalogReason,
) -> PolymarketCatalogReceipt {
    let market_id = evidence
        .contracts()
        .first()
        .expect("candidate verification requires one contract")
        .market_id
        .as_str();
    let mut receipt = rejected_receipt(
        market_id,
        &PolymarketEvidenceTrustAnchor::from_lower_hex(
            &evidence.identity().content_sha256,
            &evidence.identity().manifest_sha256,
        )
        .expect("verified evidence digest is canonical"),
        qualification_sha256,
        verifier,
        reason,
    );
    receipt.success_sha256 = Some(evidence.success_sha256().to_owned());
    receipt.sequence = Some(evidence.sequence());
    receipt.coverage = Some(evidence.coverage());
    receipt.trade_completion = evidence.trade_completion().cloned();
    receipt.availability = Some(availability(evidence));
    receipt
}

fn availability(evidence: &VerifiedPolymarketEvidenceCandidate) -> PolymarketEvidenceAvailability {
    let latest = |values: Vec<DateTime<Utc>>| values.into_iter().max();
    PolymarketEvidenceAvailability {
        contract: latest(
            evidence
                .contracts()
                .iter()
                .map(|value| value.available_at)
                .collect(),
        ),
        books: latest(
            evidence
                .books()
                .iter()
                .map(|value| value.available_at)
                .collect(),
        ),
        references: latest(
            evidence
                .references()
                .iter()
                .map(|value| value.available_at)
                .collect(),
        ),
        trades: latest(
            evidence
                .trades()
                .iter()
                .map(|value| value.available_at)
                .collect(),
        ),
        settlement: latest(
            evidence
                .settlements()
                .iter()
                .map(|value| value.retrieved_at.max(value.observed_at))
                .collect(),
        ),
    }
}

fn carrier_matches(
    carrier: &ProducerQualification,
    evidence: &VerifiedPolymarketEvidenceCandidate,
    contract: &PolymarketEvidenceContract,
) -> bool {
    carrier.market_id == contract.market_id
        && carrier.symbol == contract.symbol
        && carrier.event_start == contract.event_start
        && carrier.event_end == contract.event_end
        && carrier.up_token_id == contract.up_token_id
        && carrier.down_token_id == contract.down_token_id
        && carrier.verified_token_ids.as_ref().is_some_and(|tokens| {
            tokens == &[contract.up_token_id.clone(), contract.down_token_id.clone()]
        })
        && carrier.token_identity_matches
        && carrier.evidence_digests.expected_content_sha256 == evidence.identity().content_sha256
        && carrier.evidence_digests.expected_manifest_sha256 == evidence.identity().manifest_sha256
        && carrier.source_closed
        && carrier.source_clocks.as_ref().is_some_and(|clocks| {
            clocks.opened_at <= contract.event_start && clocks.closed_at >= contract.event_end
        })
        && surface_matches(carrier.up_book, evidence.coverage().up_book)
        && surface_matches(carrier.down_book, evidence.coverage().down_book)
        && surface_matches(carrier.trades, evidence.coverage().trades)
        && surface_matches(carrier.reference, evidence.coverage().reference)
        && surface_matches(carrier.settlement, evidence.coverage().settlement)
        && carrier.request_outcomes.as_ref().is_some_and(|outcomes| {
            outcomes.len() == 5
                && outcomes.iter().all(|outcome| {
                    outcome.status == "succeeded" && outcome.completed_at >= contract.event_end
                })
                && outcomes
                    .iter()
                    .map(|outcome| outcome.surface.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    == std::collections::BTreeSet::from([
                        "up_book",
                        "down_book",
                        "trades",
                        "reference",
                        "settlement",
                    ])
        })
        && is_sha256(&carrier.producer.configuration_sha256)
        && is_sha256(
            carrier
                .producer
                .image_digest
                .strip_prefix("sha256:")
                .unwrap_or_default(),
        )
        && carrier.producer.source_sha.len() == 40
        && carrier
            .producer
            .source_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn surface_matches(surface: ProducerSurface, coverage: u64) -> bool {
    matches!(
        (surface, coverage > 0),
        (ProducerSurface::Complete, true) | (ProducerSurface::Incomplete, false)
    )
}

fn receipt_digest(receipt: &PolymarketCatalogReceipt) -> Result<String> {
    #[derive(Serialize)]
    struct ReceiptIdentity<'a> {
        market_id: &'a str,
        content_sha256: &'a str,
        manifest_sha256: &'a str,
        qualification_sha256: &'a str,
        success_sha256: &'a Option<String>,
        verifier: &'a PolymarketCatalogVerifier,
        event_start: Option<DateTime<Utc>>,
        event_end: Option<DateTime<Utc>>,
        up_token_id: &'a Option<String>,
        down_token_id: &'a Option<String>,
        sequence: &'a Option<PolymarketEvidenceSequence>,
        coverage: &'a Option<PolymarketCandidateSurfaceCoverage>,
        trade_completion: &'a Option<PolymarketEvidenceTradeCompletion>,
        availability: &'a Option<PolymarketEvidenceAvailability>,
        state: PolymarketCatalogReceiptState,
        reasons: &'a [PolymarketCatalogReason],
        supported_tasks: &'a [PolymarketResearchTask],
    }
    let bytes = serde_json::to_vec(&ReceiptIdentity {
        market_id: &receipt.market_id,
        content_sha256: &receipt.content_sha256,
        manifest_sha256: &receipt.manifest_sha256,
        qualification_sha256: &receipt.qualification_sha256,
        success_sha256: &receipt.success_sha256,
        verifier: &receipt.verifier,
        event_start: receipt.event_start,
        event_end: receipt.event_end,
        up_token_id: &receipt.up_token_id,
        down_token_id: &receipt.down_token_id,
        sequence: &receipt.sequence,
        coverage: &receipt.coverage,
        trade_completion: &receipt.trade_completion,
        availability: &receipt.availability,
        state: receipt.state,
        reasons: &receipt.reasons,
        supported_tasks: &receipt.supported_tasks,
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polymarket_evidence::{artifact::tests, verified::tests as verified_tests};
    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn digest(path: &Path) -> String {
        format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
    }

    fn qualification(
        path: &Path,
        triplet: &PolymarketEvidenceTriplet,
        symbol: &str,
        up_token_id: &str,
    ) -> String {
        let value = json!({
            "schema": QUALIFICATION_SCHEMA, "verifier_contract": "monday.polymarket.normalized_evidence.v2",
            "market_id": "market-1", "symbol": symbol, "event_start": "2026-07-17T05:30:00Z", "event_end": "2026-07-17T05:35:00Z",
            "up_token_id": up_token_id, "down_token_id": "down-token", "verified_token_ids": [up_token_id, "down-token"],
            "state": "ready", "reasons": [], "retry": false,
            "producer": {"source_sha": "a".repeat(40), "image_digest": format!("sha256:{}", "b".repeat(64)), "configuration_sha256": "c".repeat(64)},
            "source_closed": true, "up_book": "complete", "down_book": "complete", "trades": "complete", "reference": "complete", "settlement": "complete",
            "request_outcomes": [
                {"surface":"up_book","status":"succeeded","completed_at":"2026-07-17T05:35:01Z"},
                {"surface":"down_book","status":"succeeded","completed_at":"2026-07-17T05:35:01Z"},
                {"surface":"trades","status":"succeeded","completed_at":"2026-07-17T05:35:01Z"},
                {"surface":"reference","status":"succeeded","completed_at":"2026-07-17T05:35:01Z"},
                {"surface":"settlement","status":"succeeded","completed_at":"2026-07-17T05:35:01Z"}
            ],
            "source_clocks": {"opened_at":"2026-07-17T05:29:59Z", "closed_at":"2026-07-17T05:36:00Z"},
            "sequence": {"start":1,"end":7,"gaps":0},
            "evidence_digests": {"expected_content_sha256":digest(&triplet.data),"expected_manifest_sha256":digest(&triplet.manifest)},
            "token_identity_matches": true
        });
        fs::write(
            path,
            format!("{}\n", serde_json::to_string(&value).unwrap()),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
        digest(path)
    }

    #[test]
    fn only_independently_verified_btc_receipts_are_ready() {
        let rows = verified_tests::valid_rows();
        let (_temp, triplet) = verified_tests::candidate_triplet(&rows);
        let verifier = PolymarketCatalogVerifier::new(
            "d".repeat(64),
            "e".repeat(64),
            "f".repeat(64),
            "a".repeat(64),
        )
        .unwrap();
        let ready_path = triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-ready.json");
        let ready_anchor = qualification(&ready_path, &triplet, "BTCUSDT", "up-token");
        let mut catalog = PolymarketReadyEventCatalog::default();
        let ready = catalog
            .verify_and_append(
                "market-1",
                &triplet,
                &tests::trust(&triplet),
                &ready_path,
                &ready_anchor,
                verifier.clone(),
            )
            .unwrap();
        assert_eq!(ready.state, PolymarketCatalogReceiptState::Ready);
        assert!(ready.success_sha256.is_some());
        assert!(ready.sequence.is_some());
        assert!(ready.coverage.is_some());
        assert!(ready.availability.is_some());
        assert_eq!(
            catalog
                .ready_for(PolymarketResearchTask::Btc5mBacktest)
                .len(),
            1
        );

        let cross_wired = catalog
            .verify_and_append(
                "market-other",
                &triplet,
                &tests::trust(&triplet),
                &ready_path,
                &ready_anchor,
                verifier.clone(),
            )
            .unwrap();
        assert_eq!(cross_wired.state, PolymarketCatalogReceiptState::Rejected);
        assert_eq!(
            cross_wired.reasons,
            vec![PolymarketCatalogReason::QualificationMismatch]
        );

        let malformed_path = triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-malformed.json");
        qualification(&malformed_path, &triplet, "BTCUSDT", "up-token");
        let mut malformed: Value =
            serde_json::from_slice(&fs::read(&malformed_path).unwrap()).unwrap();
        malformed["schema"] = json!("unsupported");
        tests::rewrite_read_only(
            &malformed_path,
            format!("{}\n", serde_json::to_string(&malformed).unwrap()),
        );
        let malformed = catalog
            .verify_and_append(
                "market-1",
                &triplet,
                &tests::trust(&triplet),
                &malformed_path,
                &digest(&malformed_path),
                verifier.clone(),
            )
            .unwrap();
        assert_eq!(malformed.state, PolymarketCatalogReceiptState::Rejected);
        assert_eq!(
            malformed.reasons,
            vec![PolymarketCatalogReason::QualificationVerificationFailed]
        );

        let (_incomplete_temp, incomplete_triplet) = verified_tests::candidate_triplet(&rows);
        let mut incomplete_manifest: Value =
            serde_json::from_slice(&fs::read(&incomplete_triplet.manifest).unwrap()).unwrap();
        incomplete_manifest["validated_inputs"]["reference"]["trade_completions"] = json!({});
        tests::rewrite_read_only(
            &incomplete_triplet.manifest,
            format!("{}\n", serde_json::to_string(&incomplete_manifest).unwrap()),
        );
        let incomplete_path = incomplete_triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-no-trade-completion.json");
        let incomplete_anchor =
            qualification(&incomplete_path, &incomplete_triplet, "BTCUSDT", "up-token");
        let incomplete = catalog
            .verify_and_append(
                "market-1",
                &incomplete_triplet,
                &tests::trust(&incomplete_triplet),
                &incomplete_path,
                &incomplete_anchor,
                verifier.clone(),
            )
            .unwrap();
        assert_eq!(incomplete.state, PolymarketCatalogReceiptState::Partial);
        assert!(incomplete.trade_completion.is_none());

        let surface_rows = rows
            .iter()
            .filter(|row| row["surface"] != "polymarket_trade")
            .cloned()
            .collect::<Vec<_>>();
        let (_surface_temp, surface_triplet) = verified_tests::candidate_triplet(&surface_rows);
        let surface_path = surface_triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-incomplete-trades.json");
        qualification(&surface_path, &surface_triplet, "BTCUSDT", "up-token");
        let mut surface_carrier: Value =
            serde_json::from_slice(&fs::read(&surface_path).unwrap()).unwrap();
        surface_carrier["trades"] = json!("incomplete");
        surface_carrier["sequence"]["gaps"] = json!(1);
        tests::rewrite_read_only(
            &surface_path,
            format!("{}\n", serde_json::to_string(&surface_carrier).unwrap()),
        );
        let surface_partial = catalog
            .verify_and_append(
                "market-1",
                &surface_triplet,
                &tests::trust(&surface_triplet),
                &surface_path,
                &digest(&surface_path),
                verifier.clone(),
            )
            .unwrap();
        assert_eq!(
            surface_partial.state,
            PolymarketCatalogReceiptState::Partial
        );
        assert_eq!(
            surface_partial.reasons,
            vec![PolymarketCatalogReason::IncompleteEvidence]
        );

        let rejected_path = triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-symbol-mismatch.json");
        let rejected_anchor = qualification(&rejected_path, &triplet, "SOLUSDT", "up-token");
        let rejected = catalog
            .verify_and_append(
                "market-1",
                &triplet,
                &tests::trust(&triplet),
                &rejected_path,
                &rejected_anchor,
                verifier,
            )
            .unwrap();
        assert_eq!(rejected.state, PolymarketCatalogReceiptState::Rejected);

        let swapped_path = triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-token-swap.json");
        let swapped_anchor = qualification(&swapped_path, &triplet, "BTCUSDT", "down-token");
        let swapped = catalog
            .verify_and_append(
                "market-1",
                &triplet,
                &tests::trust(&triplet),
                &swapped_path,
                &swapped_anchor,
                PolymarketCatalogVerifier::new(
                    "d".repeat(64),
                    "e".repeat(64),
                    "f".repeat(64),
                    "a".repeat(64),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(swapped.state, PolymarketCatalogReceiptState::Rejected);

        let wrong_anchor = PolymarketEvidenceTrustAnchor::from_lower_hex(
            &"0".repeat(64),
            &digest(&triplet.manifest),
        )
        .unwrap();
        let failed = catalog
            .verify_and_append(
                "market-1",
                &triplet,
                &wrong_anchor,
                &ready_path,
                &ready_anchor,
                PolymarketCatalogVerifier::new(
                    "d".repeat(64),
                    "e".repeat(64),
                    "f".repeat(64),
                    "a".repeat(64),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(failed.state, PolymarketCatalogReceiptState::Rejected);
        assert_eq!(
            failed.reasons,
            vec![PolymarketCatalogReason::EvidenceVerificationFailed]
        );
        assert_eq!(
            catalog
                .ready_for(PolymarketResearchTask::Btc5mBacktest)
                .len(),
            1
        );
    }

    #[test]
    fn persisted_ready_catalog_rejects_non_ready_extra_and_rehashed_receipts() {
        let rows = verified_tests::valid_rows();
        let (_temp, triplet) = verified_tests::candidate_triplet(&rows);
        let ready_path = triplet
            .data
            .parent()
            .unwrap()
            .join("qualification-ready.json");
        let ready_anchor = qualification(&ready_path, &triplet, "BTCUSDT", "up-token");
        let mut catalog = PolymarketReadyEventCatalog::default();
        catalog
            .verify_and_append(
                "market-1",
                &triplet,
                &tests::trust(&triplet),
                &ready_path,
                &ready_anchor,
                PolymarketCatalogVerifier::new(
                    "d".repeat(64),
                    "e".repeat(64),
                    "f".repeat(64),
                    "a".repeat(64),
                )
                .unwrap(),
            )
            .unwrap();
        let receipt = serde_json::to_value(catalog.receipts().next().unwrap()).unwrap();
        assert_eq!(
            PolymarketReadyEventCatalog::from_persisted_ready_receipts(vec![receipt.clone()])
                .unwrap()
                .receipts()
                .count(),
            1
        );

        let mut non_ready = receipt.clone();
        non_ready["state"] = json!("partial");
        assert!(
            PolymarketReadyEventCatalog::from_persisted_ready_receipts(vec![non_ready])
                .expect_err("non-Ready receipts cannot enter persisted ready catalog")
                .to_string()
                .contains("not Ready")
        );

        let mut extra = receipt.clone();
        extra["unexpected"] = json!(true);
        assert!(
            PolymarketReadyEventCatalog::from_persisted_ready_receipts(vec![extra])
                .expect_err("extra receipt fields must fail closed")
                .to_string()
                .contains("unknown field")
        );

        let mut rehashed = receipt;
        rehashed["market_id"] = json!("market-rewritten");
        assert!(
            PolymarketReadyEventCatalog::from_persisted_ready_receipts(vec![rehashed])
                .expect_err("receipt content cannot move behind a fixed receipt identity")
                .to_string()
                .contains("digest does not match")
        );

        let mut oversized_receipt = catalog.receipts().next().unwrap().clone();
        oversized_receipt.market_id = "x".repeat(33);
        oversized_receipt.receipt_sha256 = receipt_digest(&oversized_receipt).unwrap();
        let mut oversized_catalog = PolymarketReadyEventCatalog::default();
        oversized_catalog.append(oversized_receipt).unwrap();
        assert!(oversized_catalog
            .validate_ready_artifact_bounds(1, 32)
            .expect_err("artifact serialization must reject oversized producer text")
            .to_string()
            .contains("market_id exceeds"));
    }
}
