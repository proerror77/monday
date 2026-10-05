//! Signed, scoped runtime observations shared with governance readers.
use crate::runtime_bundle::canonical_hash;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AttributionError {
    #[error("{0} cannot be empty")]
    EmptyField(&'static str),
    #[error("runtime attribution metrics must be finite")]
    InvalidAttributionMetric,
    #[error("runtime attribution outcome does not match its event kind")]
    InvalidAttributionOutcome,
    #[error("runtime attribution payload hash does not match")]
    AttributionPayloadHashMismatch,
    #[error("runtime attribution signing key is not trusted")]
    UnknownAttributionSigningKey,
    #[error("runtime attribution signature is invalid")]
    InvalidAttributionSignature,
    #[error("runtime attribution signature encoding is invalid")]
    InvalidAttributionSignatureEncoding,
    #[error("runtime attribution serialization failed")]
    Serialization,
}
impl From<crate::runtime_bundle::RuntimeBundleError> for AttributionError {
    fn from(_: crate::runtime_bundle::RuntimeBundleError) -> Self {
        Self::Serialization
    }
}
fn require_text(name: &'static str, value: &str) -> Result<(), AttributionError> {
    if value.trim().is_empty() {
        Err(AttributionError::EmptyField(name))
    } else {
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributionMode {
    Paper,
    Shadow,
    LiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributionOutcome {
    Activated,
    Healthy,
    Decayed,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AttributionKind {
    #[default]
    Activation,
    Fill,
    Reject,
    Cancel,
    PortfolioSnapshot,
    StreamGap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAttributionEvent {
    pub event_id: String,
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub mission_id: Option<String>,
    pub mode: AttributionMode,
    pub outcome: AttributionOutcome,
    #[serde(default)]
    pub kind: AttributionKind,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    pub metrics: BTreeMap<String, f64>,
    pub reason: Option<String>,
    pub observed_at: DateTime<Utc>,
}

impl RuntimeAttributionEvent {
    pub fn validate(&self) -> Result<(), AttributionError> {
        require_text("attribution event_id", &self.event_id)?;
        require_text("attribution deployment_id", &self.deployment_id)?;
        require_text("attribution asset_revision_id", &self.asset_revision_id)?;
        if self.metrics.values().any(|value| !value.is_finite()) {
            return Err(AttributionError::InvalidAttributionMetric);
        }
        for value in [
            self.strategy_id.as_deref(),
            self.order_id.as_deref(),
            self.account_id.as_deref(),
            self.venue.as_deref(),
            self.symbol.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.trim().is_empty() {
                return Err(AttributionError::EmptyField("attribution scope"));
            }
        }
        match self.kind {
            AttributionKind::Fill | AttributionKind::Reject | AttributionKind::Cancel
                if self.strategy_id.is_none()
                    || self.order_id.is_none()
                    || self.account_id.is_none()
                    || self.venue.is_none()
                    || self.symbol.is_none() =>
            {
                return Err(AttributionError::EmptyField("attribution order scope"));
            }
            AttributionKind::PortfolioSnapshot
                if self.strategy_id.is_none()
                    || self.account_id.is_none()
                    || self.venue.is_none() =>
            {
                return Err(AttributionError::EmptyField("attribution portfolio scope"));
            }
            AttributionKind::StreamGap if self.reason.as_deref().is_none_or(str::is_empty) => {
                return Err(AttributionError::EmptyField(
                    "attribution stream gap reason",
                ));
            }
            _ => {}
        }
        match (&self.kind, &self.outcome) {
            (AttributionKind::Fill | AttributionKind::Cancel, AttributionOutcome::Healthy)
            | (AttributionKind::Reject | AttributionKind::StreamGap, AttributionOutcome::Failed)
            | (
                AttributionKind::PortfolioSnapshot,
                AttributionOutcome::Healthy
                | AttributionOutcome::Decayed
                | AttributionOutcome::RolledBack
                | AttributionOutcome::Failed,
            )
            | (AttributionKind::Activation, _) => Ok(()),
            _ => Err(AttributionError::InvalidAttributionOutcome),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedRuntimeAttributionEvent {
    pub event: RuntimeAttributionEvent,
    pub key_id: String,
    pub content_hash: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedRuntimeAttributionEvent(RuntimeAttributionEvent);

impl VerifiedRuntimeAttributionEvent {
    pub fn event(&self) -> &RuntimeAttributionEvent {
        &self.0
    }

    pub fn into_event(self) -> RuntimeAttributionEvent {
        self.0
    }
}

impl std::ops::Deref for VerifiedRuntimeAttributionEvent {
    type Target = RuntimeAttributionEvent;

    fn deref(&self) -> &Self::Target {
        self.event()
    }
}

pub fn sign_runtime_attribution_event(
    event: RuntimeAttributionEvent,
    key_id: impl Into<String>,
    signing_key: &SigningKey,
) -> Result<SignedRuntimeAttributionEvent, AttributionError> {
    event.validate()?;
    let key_id = key_id.into();
    require_text("runtime attribution key_id", &key_id)?;
    let content_hash = canonical_hash(&event)?;
    let signature = signing_key.sign(content_hash.as_bytes());
    Ok(SignedRuntimeAttributionEvent {
        event,
        key_id,
        content_hash,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_runtime_attribution_event(
    signed: &SignedRuntimeAttributionEvent,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
) -> Result<VerifiedRuntimeAttributionEvent, AttributionError> {
    signed.event.validate()?;
    require_text("runtime attribution key_id", &signed.key_id)?;
    let expected_hash = canonical_hash(&signed.event)?;
    if expected_hash != signed.content_hash {
        return Err(AttributionError::AttributionPayloadHashMismatch);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(AttributionError::UnknownAttributionSigningKey)?;
    let signature_bytes = hex::decode(&signed.signature_hex)
        .map_err(|_| AttributionError::InvalidAttributionSignatureEncoding)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| AttributionError::InvalidAttributionSignatureEncoding)?;
    key.verify(signed.content_hash.as_bytes(), &signature)
        .map_err(|_| AttributionError::InvalidAttributionSignature)?;
    Ok(VerifiedRuntimeAttributionEvent(signed.event.clone()))
}
