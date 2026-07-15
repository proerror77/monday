//! Read-only, fail-closed market-data diagnostics for prediction research.
//!
//! The typed report in this module is the hand-off between raw collection and
//! immutable prediction snapshots. It deliberately has no execution authority.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "audit")]
use sqlx::{postgres::PgPoolOptions, PgConnection, PgPool};
#[cfg(feature = "audit")]
use std::time::{Duration as StdDuration, Instant};

pub const PREDICTION_MARKET_DATA_AUDIT_SCHEMA_VERSION: &str = "prediction_market_data_gap_audit.v1";
pub const PREDICTION_EVENT_HORIZON_SECS: i64 = 300;
pub const RESEARCH_SNAPSHOT_WARMUP_SECS: i64 = 3_900;

pub const BINANCE_PRICE_SURFACE: &str = "binance_price_ticks";
pub const BINANCE_AGG_TRADE_SURFACE: &str = "binance_agg_trade_ticks";
pub const BINANCE_LOB_SURFACE: &str = "binance_lob_ticks";
pub const CHAINLINK_REFERENCE_SURFACE: &str = "chainlink_reference_ticks";
pub const PM_ORDERBOOK_SURFACE: &str = "clob_orderbook_snapshots";
pub const PM_SETTLEMENT_SURFACE: &str = "pm_token_settlements";

pub const REQUIRED_PREDICTION_SURFACES: [&str; 6] = [
    BINANCE_PRICE_SURFACE,
    BINANCE_AGG_TRADE_SURFACE,
    BINANCE_LOB_SURFACE,
    CHAINLINK_REFERENCE_SURFACE,
    PM_ORDERBOOK_SURFACE,
    PM_SETTLEMENT_SURFACE,
];

const SUPPORTED_PREDICTION_SYMBOLS: [&str; 2] = ["BTCUSDT", "SOLUSDT"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    Ok,
    Critical,
}

impl AuditStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditFindingCode {
    InvalidRequest,
    QueryFailed,
    NoExpectedBuckets,
    NoUsableRows,
    CoverageBelowMinimum,
    GapExceedsMaximum,
    CausalityViolation,
    NoEligibleEvents,
    IncompleteOfficialSettlement,
    RequiredSurfaceMissing,
    DuplicateSurfaceResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditFinding {
    pub code: AuditFindingCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub message: String,
}

impl AuditFinding {
    fn request(message: impl Into<String>) -> Self {
        Self {
            code: AuditFindingCode::InvalidRequest,
            surface: None,
            symbol: None,
            message: message.into(),
        }
    }

    fn surface(
        code: AuditFindingCode,
        surface: impl Into<String>,
        symbol: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            surface: Some(surface.into()),
            symbol: Some(symbol.into()),
            message: message.into(),
        }
    }

    pub fn producer_failure(surface: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: AuditFindingCode::QueryFailed,
            surface: Some(surface.into()),
            symbol: None,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMarketDataAuditRequest {
    pub symbols: Vec<String>,
    pub snapshot_start: DateTime<Utc>,
    pub snapshot_end: DateTime<Utc>,
    pub horizon_secs: i64,
    pub warmup_secs: i64,
    pub bucket_secs: i64,
    pub minimum_coverage_bps: u16,
    pub maximum_gap_secs: i64,
    pub maximum_source_delay_secs: i64,
}

impl PredictionMarketDataAuditRequest {
    pub fn btc_sol_five_minute(snapshot_start: DateTime<Utc>, snapshot_end: DateTime<Utc>) -> Self {
        Self {
            symbols: SUPPORTED_PREDICTION_SYMBOLS
                .iter()
                .map(|symbol| (*symbol).to_string())
                .collect(),
            snapshot_start,
            snapshot_end,
            horizon_secs: PREDICTION_EVENT_HORIZON_SECS,
            warmup_secs: RESEARCH_SNAPSHOT_WARMUP_SECS,
            bucket_secs: 60,
            minimum_coverage_bps: 9_900,
            maximum_gap_secs: 120,
            maximum_source_delay_secs: 30,
        }
    }

    pub fn coverage_start(&self) -> Option<DateTime<Utc>> {
        Duration::try_seconds(self.warmup_secs)
            .and_then(|warmup| self.snapshot_start.checked_sub_signed(warmup))
    }

    pub fn validation_findings(&self) -> Vec<AuditFinding> {
        let mut findings = Vec::new();
        if self.symbols.is_empty() {
            findings.push(AuditFinding::request("symbols must not be empty"));
        }
        let mut seen = BTreeSet::new();
        for symbol in &self.symbols {
            if !SUPPORTED_PREDICTION_SYMBOLS.contains(&symbol.as_str()) {
                findings.push(AuditFinding::request(format!(
                    "unsupported prediction symbol {symbol}; expected BTCUSDT or SOLUSDT"
                )));
            }
            if !seen.insert(symbol.as_str()) {
                findings.push(AuditFinding::request(format!(
                    "duplicate prediction symbol {symbol}"
                )));
            }
        }
        if self.snapshot_start >= self.snapshot_end {
            findings.push(AuditFinding::request(
                "snapshot_start must be before snapshot_end",
            ));
        }
        if self.horizon_secs != PREDICTION_EVENT_HORIZON_SECS {
            findings.push(AuditFinding::request(format!(
                "horizon_secs must be {PREDICTION_EVENT_HORIZON_SECS} for the governed prediction lane"
            )));
        }
        if self.warmup_secs < RESEARCH_SNAPSHOT_WARMUP_SECS {
            findings.push(AuditFinding::request(format!(
                "warmup_secs must cover the snapshot lookback ({RESEARCH_SNAPSHOT_WARMUP_SECS}s)"
            )));
        }
        if self.coverage_start().is_none() {
            findings.push(AuditFinding::request(
                "warmup window overflows timestamp range",
            ));
        }
        if self.bucket_secs <= 0 || self.bucket_secs > self.horizon_secs {
            findings.push(AuditFinding::request(
                "bucket_secs must be positive and no greater than horizon_secs",
            ));
        } else if self.horizon_secs % self.bucket_secs != 0 {
            findings.push(AuditFinding::request(
                "bucket_secs must divide the 300-second prediction horizon",
            ));
        }
        if self.minimum_coverage_bps == 0 || self.minimum_coverage_bps > 10_000 {
            findings.push(AuditFinding::request(
                "minimum_coverage_bps must be in 1..=10000",
            ));
        }
        if self.maximum_gap_secs < 0 {
            findings.push(AuditFinding::request(
                "maximum_gap_secs must not be negative",
            ));
        }
        if self.maximum_source_delay_secs <= 0 {
            findings.push(AuditFinding::request(
                "maximum_source_delay_secs must be positive",
            ));
        }
        findings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeSeriesCoverageObservation {
    pub surface: String,
    pub symbol: String,
    pub row_count: u64,
    pub usable_row_count: u64,
    pub source_delay_rejected_rows: u64,
    pub invalid_payload_rows: u64,
    pub causality_violations: u64,
    pub first_at: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
    pub expected_buckets: u64,
    pub present_buckets: u64,
    pub max_gap_secs: u64,
    pub query_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCompletenessObservation {
    pub surface: String,
    pub symbol: String,
    pub eligible_events: u64,
    pub complete_events: u64,
    pub expected_tokens: u64,
    pub resolved_tokens: u64,
    pub query_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditSurfaceMetrics {
    TimeSeries {
        row_count: u64,
        usable_row_count: u64,
        source_delay_rejected_rows: u64,
        invalid_payload_rows: u64,
        causality_violations: u64,
        first_at: Option<DateTime<Utc>>,
        last_at: Option<DateTime<Utc>>,
        expected_buckets: u64,
        present_buckets: u64,
        coverage_bps: u16,
        max_gap_secs: u64,
        query_ms: u64,
    },
    EventCompleteness {
        eligible_events: u64,
        complete_events: u64,
        expected_tokens: u64,
        resolved_tokens: u64,
        query_ms: u64,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSurfaceResult {
    pub surface: String,
    pub symbol: String,
    pub status: AuditStatus,
    pub metrics: AuditSurfaceMetrics,
    pub findings: Vec<AuditFinding>,
}

pub fn evaluate_time_series_coverage(
    request: &PredictionMarketDataAuditRequest,
    observation: TimeSeriesCoverageObservation,
) -> AuditSurfaceResult {
    let coverage_bps = if observation.expected_buckets == 0 {
        0
    } else {
        let scaled = u128::from(observation.present_buckets)
            .saturating_mul(10_000)
            .checked_div(u128::from(observation.expected_buckets))
            .unwrap_or_default()
            .min(10_000);
        u16::try_from(scaled).unwrap_or(10_000)
    };

    let mut findings = Vec::new();
    if observation.expected_buckets == 0 {
        findings.push(AuditFinding::surface(
            AuditFindingCode::NoExpectedBuckets,
            &observation.surface,
            &observation.symbol,
            "no expected buckets were derived for the bounded audit window",
        ));
    }
    if observation.usable_row_count == 0 {
        findings.push(AuditFinding::surface(
            AuditFindingCode::NoUsableRows,
            &observation.surface,
            &observation.symbol,
            "no causally usable rows were found in the bounded audit window",
        ));
    }
    if observation.causality_violations > 0 {
        findings.push(AuditFinding::surface(
            AuditFindingCode::CausalityViolation,
            &observation.surface,
            &observation.symbol,
            format!(
                "{} rows have received_at before source time",
                observation.causality_violations
            ),
        ));
    }
    if observation.expected_buckets > 0 && coverage_bps < request.minimum_coverage_bps {
        findings.push(AuditFinding::surface(
            AuditFindingCode::CoverageBelowMinimum,
            &observation.surface,
            &observation.symbol,
            format!(
                "coverage {coverage_bps}bps is below required {}bps",
                request.minimum_coverage_bps
            ),
        ));
    }
    if observation.max_gap_secs > request.maximum_gap_secs.max(0) as u64 {
        findings.push(AuditFinding::surface(
            AuditFindingCode::GapExceedsMaximum,
            &observation.surface,
            &observation.symbol,
            format!(
                "maximum gap {}s exceeds allowed {}s",
                observation.max_gap_secs, request.maximum_gap_secs
            ),
        ));
    }

    AuditSurfaceResult {
        surface: observation.surface,
        symbol: observation.symbol,
        status: if findings.is_empty() {
            AuditStatus::Ok
        } else {
            AuditStatus::Critical
        },
        metrics: AuditSurfaceMetrics::TimeSeries {
            row_count: observation.row_count,
            usable_row_count: observation.usable_row_count,
            source_delay_rejected_rows: observation.source_delay_rejected_rows,
            invalid_payload_rows: observation.invalid_payload_rows,
            causality_violations: observation.causality_violations,
            first_at: observation.first_at,
            last_at: observation.last_at,
            expected_buckets: observation.expected_buckets,
            present_buckets: observation.present_buckets,
            coverage_bps,
            max_gap_secs: observation.max_gap_secs,
            query_ms: observation.query_ms,
        },
        findings,
    }
}

pub fn evaluate_event_completeness(
    observation: EventCompletenessObservation,
) -> AuditSurfaceResult {
    let mut findings = Vec::new();
    if observation.eligible_events == 0 {
        findings.push(AuditFinding::surface(
            AuditFindingCode::NoEligibleEvents,
            &observation.surface,
            &observation.symbol,
            "no eligible five-minute prediction events were found",
        ));
    }
    if observation.complete_events != observation.eligible_events
        || observation.resolved_tokens != observation.expected_tokens
    {
        findings.push(AuditFinding::surface(
            AuditFindingCode::IncompleteOfficialSettlement,
            &observation.surface,
            &observation.symbol,
            format!(
                "official settlement complete for {}/{} events and {}/{} tokens",
                observation.complete_events,
                observation.eligible_events,
                observation.resolved_tokens,
                observation.expected_tokens
            ),
        ));
    }

    AuditSurfaceResult {
        surface: observation.surface,
        symbol: observation.symbol,
        status: if findings.is_empty() {
            AuditStatus::Ok
        } else {
            AuditStatus::Critical
        },
        metrics: AuditSurfaceMetrics::EventCompleteness {
            eligible_events: observation.eligible_events,
            complete_events: observation.complete_events,
            expected_tokens: observation.expected_tokens,
            resolved_tokens: observation.resolved_tokens,
            query_ms: observation.query_ms,
        },
        findings,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMarketDataAuditReport {
    pub schema_version: String,
    pub generated_at: DateTime<Utc>,
    pub producer: String,
    pub read_only: bool,
    pub data_audit_status: AuditStatus,
    pub request: PredictionMarketDataAuditRequest,
    pub required_surfaces: Vec<String>,
    pub surface_results: Vec<AuditSurfaceResult>,
    pub blockers: Vec<AuditFinding>,
}

fn canonical_surface_result(
    request: &PredictionMarketDataAuditRequest,
    result: &AuditSurfaceResult,
) -> Result<AuditSurfaceResult, String> {
    if !REQUIRED_PREDICTION_SURFACES.contains(&result.surface.as_str()) {
        return Err(format!(
            "unknown prediction data audit surface {}",
            result.surface
        ));
    }
    if !request.symbols.contains(&result.symbol) {
        return Err(format!(
            "prediction data audit contains out-of-scope symbol {}",
            result.symbol
        ));
    }

    match &result.metrics {
        AuditSurfaceMetrics::Unavailable => Err(format!(
            "required prediction surface {}/{} is unavailable",
            result.surface, result.symbol
        )),
        AuditSurfaceMetrics::TimeSeries {
            row_count,
            usable_row_count,
            source_delay_rejected_rows,
            invalid_payload_rows,
            causality_violations,
            first_at,
            last_at,
            expected_buckets,
            present_buckets,
            coverage_bps: _,
            max_gap_secs,
            query_ms,
        } => {
            if result.surface == PM_SETTLEMENT_SURFACE {
                return Err("official settlement must use event-completeness metrics".to_string());
            }
            if result.surface == PM_ORDERBOOK_SURFACE {
                if *source_delay_rejected_rows != 0 || *causality_violations != 0 {
                    return Err(
                        "Polymarket order-book audit has impossible clock rejection counts"
                            .to_string(),
                    );
                }
            } else if *invalid_payload_rows != 0 {
                return Err(format!(
                    "{} audit has impossible invalid-payload rows",
                    result.surface
                ));
            }
            let classified_rows = usable_row_count
                .checked_add(*source_delay_rejected_rows)
                .and_then(|count| count.checked_add(*invalid_payload_rows))
                .and_then(|count| count.checked_add(*causality_violations))
                .ok_or_else(|| "time-series audit row counts overflow".to_string())?;
            if classified_rows != *row_count {
                return Err(format!(
                    "time-series audit row counts are inconsistent for {}/{}",
                    result.surface, result.symbol
                ));
            }
            if present_buckets > expected_buckets {
                return Err(format!(
                    "present buckets exceed expected buckets for {}/{}",
                    result.surface, result.symbol
                ));
            }
            let bucket_secs = u64::try_from(request.bucket_secs)
                .map_err(|_| "audit bucket_secs is not positive".to_string())?;
            if *max_gap_secs % bucket_secs != 0 {
                return Err(format!(
                    "maximum gap is not bucket-aligned for {}/{}",
                    result.surface, result.symbol
                ));
            }
            match (first_at, last_at, *usable_row_count) {
                (Some(first), Some(last), usable) if usable > 0 => {
                    let coverage_start = request
                        .coverage_start()
                        .ok_or_else(|| "audit coverage window overflows".to_string())?;
                    if first > last || *first < coverage_start || *last >= request.snapshot_end {
                        return Err(format!(
                            "usable time-series bounds are outside the audit window for {}/{}",
                            result.surface, result.symbol
                        ));
                    }
                }
                (None, None, 0) => {}
                _ => {
                    return Err(format!(
                        "usable row count and time bounds disagree for {}/{}",
                        result.surface, result.symbol
                    ))
                }
            }

            let canonical = evaluate_time_series_coverage(
                request,
                TimeSeriesCoverageObservation {
                    surface: result.surface.clone(),
                    symbol: result.symbol.clone(),
                    row_count: *row_count,
                    usable_row_count: *usable_row_count,
                    source_delay_rejected_rows: *source_delay_rejected_rows,
                    invalid_payload_rows: *invalid_payload_rows,
                    causality_violations: *causality_violations,
                    first_at: *first_at,
                    last_at: *last_at,
                    expected_buckets: *expected_buckets,
                    present_buckets: *present_buckets,
                    max_gap_secs: *max_gap_secs,
                    query_ms: *query_ms,
                },
            );
            if canonical != *result {
                return Err(format!(
                    "serialized status, findings, or coverage metrics are not canonical for {}/{}",
                    result.surface, result.symbol
                ));
            }
            Ok(canonical)
        }
        AuditSurfaceMetrics::EventCompleteness {
            eligible_events,
            complete_events,
            expected_tokens,
            resolved_tokens,
            query_ms,
        } => {
            if result.surface != PM_SETTLEMENT_SURFACE {
                return Err(format!("{} must use time-series metrics", result.surface));
            }
            let binary_tokens = eligible_events
                .checked_mul(2)
                .ok_or_else(|| "settlement token count overflows".to_string())?;
            if complete_events > eligible_events
                || resolved_tokens > expected_tokens
                || *expected_tokens != binary_tokens
            {
                return Err(format!(
                    "official settlement counts are inconsistent for {}",
                    result.symbol
                ));
            }
            let canonical = evaluate_event_completeness(EventCompletenessObservation {
                surface: result.surface.clone(),
                symbol: result.symbol.clone(),
                eligible_events: *eligible_events,
                complete_events: *complete_events,
                expected_tokens: *expected_tokens,
                resolved_tokens: *resolved_tokens,
                query_ms: *query_ms,
            });
            if canonical != *result {
                return Err(format!(
                    "serialized status, findings, or settlement metrics are not canonical for {}",
                    result.symbol
                ));
            }
            Ok(canonical)
        }
    }
}

impl PredictionMarketDataAuditReport {
    pub fn validate_for_prediction_snapshot(
        &self,
        symbols: &[String],
        snapshot_start: DateTime<Utc>,
        snapshot_end: DateTime<Utc>,
    ) -> Result<(), String> {
        if self.schema_version != PREDICTION_MARKET_DATA_AUDIT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported data audit schema {}; expected {}",
                self.schema_version, PREDICTION_MARKET_DATA_AUDIT_SCHEMA_VERSION
            ));
        }
        if self.producer != "ploy-market-data/diagnostics" {
            return Err(format!(
                "unsupported prediction data audit producer {}",
                self.producer
            ));
        }
        if self.request.snapshot_start != snapshot_start
            || self.request.snapshot_end != snapshot_end
        {
            return Err("data audit window does not match prediction snapshot window".to_string());
        }
        if self.generated_at < snapshot_end {
            return Err(
                "data audit was generated before the prediction snapshot window ended".to_string(),
            );
        }
        let report_symbols = self
            .request
            .symbols
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let snapshot_symbols = symbols.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if report_symbols != snapshot_symbols || self.request.symbols.len() != symbols.len() {
            return Err("data audit symbols do not match prediction snapshot symbols".to_string());
        }
        if !self.read_only {
            return Err("prediction data audit must be produced in read-only mode".to_string());
        }
        if !self.request.validation_findings().is_empty() {
            return Err("prediction data audit contains an invalid request".to_string());
        }
        if self.data_audit_status != AuditStatus::Ok || !self.blockers.is_empty() {
            return Err(format!(
                "prediction data audit is {}; blockers={}",
                self.data_audit_status.as_str(),
                self.blockers.len()
            ));
        }
        let declared_surfaces = self
            .required_surfaces
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected_surfaces = REQUIRED_PREDICTION_SURFACES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if declared_surfaces != expected_surfaces
            || self.required_surfaces.len() != REQUIRED_PREDICTION_SURFACES.len()
        {
            return Err("data audit required-surface contract is incomplete".to_string());
        }
        let expected_result_count = symbols
            .len()
            .checked_mul(REQUIRED_PREDICTION_SURFACES.len())
            .ok_or_else(|| "prediction data audit result count overflows".to_string())?;
        if self.surface_results.len() != expected_result_count {
            return Err(format!(
                "prediction data audit has {} surface results; expected {expected_result_count}",
                self.surface_results.len()
            ));
        }
        let canonical_results = self
            .surface_results
            .iter()
            .map(|result| canonical_surface_result(&self.request, result))
            .collect::<Result<Vec<_>, _>>()?;
        let canonical_report = assemble_prediction_market_data_audit(
            self.request.clone(),
            self.generated_at,
            canonical_results,
            Vec::new(),
        );
        if canonical_report != *self {
            return Err(
                "prediction data audit is not the canonical typed report for its metrics"
                    .to_string(),
            );
        }
        for symbol in symbols {
            for surface in REQUIRED_PREDICTION_SURFACES {
                let matches = self
                    .surface_results
                    .iter()
                    .filter(|result| result.surface == surface && result.symbol == *symbol)
                    .collect::<Vec<_>>();
                if matches.len() != 1 || matches[0].status != AuditStatus::Ok {
                    return Err(format!(
                        "required prediction surface {surface}/{symbol} is not audited ok"
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn assemble_prediction_market_data_audit(
    request: PredictionMarketDataAuditRequest,
    generated_at: DateTime<Utc>,
    mut surface_results: Vec<AuditSurfaceResult>,
    mut producer_findings: Vec<AuditFinding>,
) -> PredictionMarketDataAuditReport {
    producer_findings.extend(request.validation_findings());
    surface_results
        .sort_by(|left, right| (&left.symbol, &left.surface).cmp(&(&right.symbol, &right.surface)));

    let mut result_counts = BTreeMap::<(&str, &str), usize>::new();
    for result in &surface_results {
        *result_counts
            .entry((result.surface.as_str(), result.symbol.as_str()))
            .or_default() += 1;
        if !result.findings.is_empty() {
            producer_findings.extend(result.findings.clone());
        } else if result.status == AuditStatus::Critical {
            producer_findings.push(AuditFinding::surface(
                AuditFindingCode::QueryFailed,
                &result.surface,
                &result.symbol,
                "surface reported critical without a typed finding",
            ));
        }
    }
    for symbol in &request.symbols {
        for surface in REQUIRED_PREDICTION_SURFACES {
            match result_counts.get(&(surface, symbol.as_str())).copied() {
                None | Some(0) => producer_findings.push(AuditFinding::surface(
                    AuditFindingCode::RequiredSurfaceMissing,
                    surface,
                    symbol,
                    "required prediction surface has no audit result",
                )),
                Some(1) => {}
                Some(count) => producer_findings.push(AuditFinding::surface(
                    AuditFindingCode::DuplicateSurfaceResult,
                    surface,
                    symbol,
                    format!("required prediction surface has {count} audit results"),
                )),
            }
        }
    }

    let data_audit_status = if producer_findings.is_empty() {
        AuditStatus::Ok
    } else {
        AuditStatus::Critical
    };
    PredictionMarketDataAuditReport {
        schema_version: PREDICTION_MARKET_DATA_AUDIT_SCHEMA_VERSION.to_string(),
        generated_at,
        producer: "ploy-market-data/diagnostics".to_string(),
        read_only: true,
        data_audit_status,
        request,
        required_surfaces: REQUIRED_PREDICTION_SURFACES
            .iter()
            .map(|surface| (*surface).to_string())
            .collect(),
        surface_results,
        blockers: producer_findings,
    }
}

#[cfg(feature = "audit")]
fn unavailable_surface_result(
    surface: &str,
    symbol: &str,
    message: &'static str,
) -> AuditSurfaceResult {
    let finding = AuditFinding::surface(AuditFindingCode::QueryFailed, surface, symbol, message);
    AuditSurfaceResult {
        surface: surface.to_string(),
        symbol: symbol.to_string(),
        status: AuditStatus::Critical,
        metrics: AuditSurfaceMetrics::Unavailable,
        findings: vec![finding],
    }
}

#[cfg(feature = "audit")]
struct TimeSeriesSpec {
    surface: &'static str,
    raw_source_sql: &'static str,
    reference_symbol: bool,
}

#[cfg(feature = "audit")]
const TIME_SERIES_SPECS: [TimeSeriesSpec; 4] = [
    TimeSeriesSpec {
        surface: BINANCE_PRICE_SURFACE,
        raw_source_sql: "SELECT trade_time AS source_time, received_at FROM binance_price_ticks WHERE upper(symbol) = $1",
        reference_symbol: false,
    },
    TimeSeriesSpec {
        surface: BINANCE_AGG_TRADE_SURFACE,
        raw_source_sql: "SELECT trade_time AS source_time, received_at FROM binance_agg_trade_ticks WHERE upper(symbol) = $1",
        reference_symbol: false,
    },
    TimeSeriesSpec {
        surface: BINANCE_LOB_SURFACE,
        raw_source_sql: "SELECT event_time AS source_time, received_at FROM binance_lob_ticks WHERE upper(symbol) = $1",
        reference_symbol: false,
    },
    TimeSeriesSpec {
        surface: CHAINLINK_REFERENCE_SURFACE,
        raw_source_sql: "SELECT price_time AS source_time, received_at FROM reference_price_ticks WHERE lower(symbol) = $1 AND lower(source) = 'chainlink' UNION ALL SELECT source_timestamp AS source_time, received_at FROM chainlink_price_ticks WHERE lower(symbol) = $1",
        reference_symbol: true,
    },
];

#[cfg(feature = "audit")]
fn reference_symbol(symbol: &str) -> String {
    symbol
        .strip_suffix("USDT")
        .or_else(|| symbol.strip_suffix("USD"))
        .unwrap_or(symbol)
        .to_ascii_lowercase()
        + "/usd"
}

#[cfg(feature = "audit")]
fn time_series_coverage_sql(raw_source_sql: &str) -> String {
    format!(
        r#"
WITH params AS (
    SELECT
        $2::timestamptz AS start_at,
        $3::timestamptz AS end_at,
        $4::bigint AS bucket_secs,
        ($4::double precision * interval '1 second') AS bucket_width,
        $5::bigint AS max_source_delay_secs
),
raw_source AS (
    {raw_source_sql}
),
raw AS (
    SELECT source.source_time, source.received_at
    FROM raw_source source
    CROSS JOIN params p
    WHERE source.source_time >= p.start_at
      AND source.source_time < p.end_at
),
usable AS (
    SELECT raw.*
    FROM raw
    CROSS JOIN params p
    WHERE raw.received_at >= raw.source_time
      AND extract(epoch FROM raw.received_at - raw.source_time) <= p.max_source_delay_secs
),
expected AS (
    SELECT generate_series(
        p.start_at,
        p.end_at - interval '1 microsecond',
        p.bucket_width
    ) AS bucket
    FROM params p
),
present AS (
    SELECT DISTINCT
        p.start_at
            + floor(
                extract(epoch FROM (usable.source_time - p.start_at))
                    / p.bucket_secs::numeric
              )::double precision * p.bucket_width AS bucket
    FROM usable
    CROSS JOIN params p
),
coverage AS (
    SELECT expected.bucket, present.bucket IS NOT NULL AS present
    FROM expected
    LEFT JOIN present USING (bucket)
),
missing_indexed AS (
    SELECT
        bucket,
        bucket - row_number() OVER (ORDER BY bucket)::double precision
            * (SELECT bucket_width FROM params) AS gap_group
    FROM coverage
    WHERE NOT present
),
missing_runs AS (
    SELECT count(*)::bigint AS bucket_count
    FROM missing_indexed
    GROUP BY gap_group
)
SELECT
    (SELECT count(*)::bigint FROM raw) AS row_count,
    (SELECT count(*)::bigint FROM usable) AS usable_row_count,
    (SELECT count(*)::bigint FROM raw CROSS JOIN params p
        WHERE raw.received_at >= raw.source_time
          AND extract(epoch FROM raw.received_at - raw.source_time) > p.max_source_delay_secs
    ) AS source_delay_rejected_rows,
    0::bigint AS invalid_payload_rows,
    (SELECT count(*)::bigint FROM raw WHERE received_at < source_time) AS causality_violations,
    (SELECT min(source_time) FROM usable) AS first_at,
    (SELECT max(source_time) FROM usable) AS last_at,
    (SELECT count(*)::bigint FROM coverage) AS expected_buckets,
    (SELECT count(*)::bigint FROM coverage WHERE present) AS present_buckets,
    coalesce((SELECT max(bucket_count) FROM missing_runs), 0)::bigint * $4::bigint
        AS max_gap_secs
"#
    )
}

#[cfg(feature = "audit")]
type CoverageRow = (
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    i64,
    i64,
    i64,
);

#[cfg(feature = "audit")]
fn nonnegative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

#[cfg(feature = "audit")]
fn coverage_observation(
    surface: &str,
    symbol: &str,
    row: CoverageRow,
    elapsed: StdDuration,
) -> TimeSeriesCoverageObservation {
    TimeSeriesCoverageObservation {
        surface: surface.to_string(),
        symbol: symbol.to_string(),
        row_count: nonnegative(row.0),
        usable_row_count: nonnegative(row.1),
        source_delay_rejected_rows: nonnegative(row.2),
        invalid_payload_rows: nonnegative(row.3),
        causality_violations: nonnegative(row.4),
        first_at: row.5,
        last_at: row.6,
        expected_buckets: nonnegative(row.7),
        present_buckets: nonnegative(row.8),
        max_gap_secs: nonnegative(row.9),
        query_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    }
}

#[cfg(feature = "audit")]
async fn query_time_series_coverage(
    connection: &mut PgConnection,
    request: &PredictionMarketDataAuditRequest,
    spec: &TimeSeriesSpec,
    symbol: &str,
) -> Result<TimeSeriesCoverageObservation, sqlx::Error> {
    let started = Instant::now();
    let filter_symbol = if spec.reference_symbol {
        reference_symbol(symbol)
    } else {
        symbol.to_string()
    };
    let row: CoverageRow = sqlx::query_as(&time_series_coverage_sql(spec.raw_source_sql))
        .bind(filter_symbol)
        .bind(request.coverage_start().unwrap_or(request.snapshot_start))
        .bind(request.snapshot_end)
        .bind(request.bucket_secs)
        .bind(request.maximum_source_delay_secs)
        .fetch_one(&mut *connection)
        .await?;
    Ok(coverage_observation(
        spec.surface,
        symbol,
        row,
        started.elapsed(),
    ))
}

#[cfg(feature = "audit")]
const PM_ORDERBOOK_COVERAGE_SQL: &str = r#"
WITH params AS (
    SELECT
        $2::timestamptz AS start_at,
        $3::timestamptz AS end_at,
        $4::bigint AS bucket_secs,
        ($4::double precision * interval '1 second') AS bucket_width,
        $5::bigint AS horizon_secs
),
token_windows AS (
    SELECT DISTINCT
        m.market_slug,
        trim(both '"' from token.value::text) AS token_id,
        greatest(p.start_at, m.start_time - p.bucket_width) AS window_start,
        least(p.end_at, m.end_time + p.bucket_width) AS window_end
    FROM pm_market_metadata m
    CROSS JOIN params p
    CROSS JOIN LATERAL jsonb_array_elements(
        (m.raw_market->'markets'->0->>'clobTokenIds')::jsonb
    ) token(value)
    WHERE m.symbol = $1
      AND m.start_time IS NOT NULL
      AND m.end_time IS NOT NULL
      AND m.end_time >= p.start_at
      AND m.start_time < p.end_at
      AND abs(extract(epoch FROM (m.end_time - m.start_time)) - p.horizon_secs) <= 1
      AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
),
expected AS (
    SELECT
        window.market_slug,
        window.token_id,
        bucket
    FROM token_windows window
    CROSS JOIN params p
    CROSS JOIN LATERAL generate_series(
        window.window_start,
        window.window_end - interval '1 microsecond',
        p.bucket_width
    ) bucket
),
raw AS (
    SELECT
        window.market_slug,
        window.token_id,
        window.window_start,
        snapshot.received_at AS source_time,
        snapshot.bids,
        snapshot.asks
    FROM token_windows window
    JOIN clob_orderbook_snapshots snapshot
      ON snapshot.token_id = window.token_id
     AND snapshot.received_at >= window.window_start
     AND snapshot.received_at < window.window_end
),
usable AS (
    SELECT *
    FROM raw
    WHERE jsonb_typeof(bids) = 'array'
      AND jsonb_typeof(asks) = 'array'
      AND (jsonb_array_length(bids) > 0 OR jsonb_array_length(asks) > 0)
),
present AS (
    SELECT DISTINCT
        usable.market_slug,
        usable.token_id,
        usable.window_start
            + floor(
                extract(epoch FROM (usable.source_time - usable.window_start))
                    / p.bucket_secs::numeric
              )::double precision * p.bucket_width AS bucket
    FROM usable
    CROSS JOIN params p
),
coverage AS (
    SELECT
        expected.market_slug,
        expected.token_id,
        expected.bucket,
        present.bucket IS NOT NULL AS present
    FROM expected
    LEFT JOIN present USING (market_slug, token_id, bucket)
),
missing_indexed AS (
    SELECT
        market_slug,
        token_id,
        bucket,
        bucket - row_number() OVER (
            PARTITION BY market_slug, token_id ORDER BY bucket
        )::double precision * (SELECT bucket_width FROM params) AS gap_group
    FROM coverage
    WHERE NOT present
),
missing_runs AS (
    SELECT count(*)::bigint AS bucket_count
    FROM missing_indexed
    GROUP BY market_slug, token_id, gap_group
)
SELECT
    (SELECT count(*)::bigint FROM raw) AS row_count,
    (SELECT count(*)::bigint FROM usable) AS usable_row_count,
    0::bigint AS source_delay_rejected_rows,
    (SELECT count(*)::bigint FROM raw) - (SELECT count(*)::bigint FROM usable)
        AS invalid_payload_rows,
    0::bigint AS causality_violations,
    (SELECT min(source_time) FROM usable) AS first_at,
    (SELECT max(source_time) FROM usable) AS last_at,
    (SELECT count(*)::bigint FROM coverage) AS expected_buckets,
    (SELECT count(*)::bigint FROM coverage WHERE present) AS present_buckets,
    coalesce((SELECT max(bucket_count) FROM missing_runs), 0)::bigint * $4::bigint
        AS max_gap_secs
"#;

#[cfg(feature = "audit")]
async fn query_pm_orderbook_coverage(
    connection: &mut PgConnection,
    request: &PredictionMarketDataAuditRequest,
    symbol: &str,
) -> Result<TimeSeriesCoverageObservation, sqlx::Error> {
    let started = Instant::now();
    let row: CoverageRow = sqlx::query_as(PM_ORDERBOOK_COVERAGE_SQL)
        .bind(symbol)
        .bind(request.coverage_start().unwrap_or(request.snapshot_start))
        .bind(request.snapshot_end)
        .bind(request.bucket_secs)
        .bind(request.horizon_secs)
        .fetch_one(&mut *connection)
        .await?;
    Ok(coverage_observation(
        PM_ORDERBOOK_SURFACE,
        symbol,
        row,
        started.elapsed(),
    ))
}

#[cfg(feature = "audit")]
const PM_SETTLEMENT_COMPLETENESS_SQL: &str = r#"
WITH params AS (
    SELECT
        $2::timestamptz AS start_at,
        $3::timestamptz AS end_at,
        $4::bigint AS horizon_secs
),
eligible_markets AS (
    SELECT
        m.market_slug,
        m.raw_market
    FROM pm_market_metadata m
    CROSS JOIN params p
    WHERE m.symbol = $1
      AND m.start_time IS NOT NULL
      AND m.end_time IS NOT NULL
      AND m.end_time > p.start_at
      AND m.end_time <= p.end_at
      AND abs(extract(epoch FROM (m.end_time - m.start_time)) - p.horizon_secs) <= 1
),
eligible_tokens AS (
    SELECT DISTINCT
        market.market_slug,
        trim(both '"' from token.value::text) AS token_id
    FROM eligible_markets market
    LEFT JOIN LATERAL jsonb_array_elements(
        CASE
            WHEN jsonb_typeof(
                (market.raw_market->'markets'->0->>'clobTokenIds')::jsonb
            ) = 'array'
            THEN (market.raw_market->'markets'->0->>'clobTokenIds')::jsonb
            ELSE '[]'::jsonb
        END
    ) token(value) ON true
),
market_status AS (
    SELECT
        eligible.market_slug,
        count(eligible.token_id)::bigint AS expected_tokens,
        count(eligible.token_id) FILTER (
            WHERE settlement.resolved = true
              AND settlement.settled_price IS NOT NULL
        )::bigint AS resolved_tokens,
        count(eligible.token_id) FILTER (
            WHERE settlement.resolved = true
              AND settlement.settled_price >= 0.999
        )::bigint AS winner_tokens,
        count(eligible.token_id) FILTER (
            WHERE settlement.resolved = true
              AND settlement.settled_price <= 0.001
        )::bigint AS loser_tokens
    FROM eligible_tokens eligible
    LEFT JOIN pm_token_settlements settlement
      ON settlement.token_id = eligible.token_id
    GROUP BY eligible.market_slug
)
SELECT
    count(*)::bigint AS eligible_events,
    count(*) FILTER (
        WHERE expected_tokens = 2
          AND resolved_tokens = 2
          AND winner_tokens = 1
          AND loser_tokens = 1
    )::bigint AS complete_events,
    coalesce(sum(expected_tokens), 0)::bigint AS expected_tokens,
    coalesce(sum(resolved_tokens), 0)::bigint AS resolved_tokens
FROM market_status
"#;

#[cfg(feature = "audit")]
async fn query_settlement_completeness(
    connection: &mut PgConnection,
    request: &PredictionMarketDataAuditRequest,
    symbol: &str,
) -> Result<EventCompletenessObservation, sqlx::Error> {
    let started = Instant::now();
    let row: (i64, i64, i64, i64) = sqlx::query_as(PM_SETTLEMENT_COMPLETENESS_SQL)
        .bind(symbol)
        .bind(request.snapshot_start)
        .bind(request.snapshot_end)
        .bind(request.horizon_secs)
        .fetch_one(&mut *connection)
        .await?;
    Ok(EventCompletenessObservation {
        surface: PM_SETTLEMENT_SURFACE.to_string(),
        symbol: symbol.to_string(),
        eligible_events: nonnegative(row.0),
        complete_events: nonnegative(row.1),
        expected_tokens: nonnegative(row.2),
        resolved_tokens: nonnegative(row.3),
        query_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(feature = "audit")]
async fn reset_audit_session(connection: &mut sqlx::pool::PoolConnection<sqlx::Postgres>) -> bool {
    if sqlx::query("RESET ALL")
        .execute(&mut **connection)
        .await
        .is_ok()
    {
        true
    } else {
        connection.close_on_drop();
        false
    }
}

#[cfg(feature = "audit")]
pub async fn audit_prediction_market_data(
    pool: &PgPool,
    request: PredictionMarketDataAuditRequest,
) -> PredictionMarketDataAuditReport {
    if !request.validation_findings().is_empty() {
        return assemble_prediction_market_data_audit(request, Utc::now(), Vec::new(), Vec::new());
    }

    let mut connection = match pool.acquire().await {
        Ok(connection) => connection,
        Err(_) => {
            return assemble_prediction_market_data_audit(
                request,
                Utc::now(),
                Vec::new(),
                vec![AuditFinding::producer_failure(
                    "database_connection",
                    "database connection failed",
                )],
            )
        }
    };
    let read_only_configured = sqlx::query("SET default_transaction_read_only = on")
        .execute(&mut *connection)
        .await
        .is_ok();
    let read_only_verified = if read_only_configured {
        sqlx::query_scalar::<_, String>("SHOW default_transaction_read_only")
            .fetch_one(&mut *connection)
            .await
            .is_ok_and(|value| value == "on")
    } else {
        false
    };
    if !read_only_verified {
        reset_audit_session(&mut connection).await;
        return assemble_prediction_market_data_audit(
            request,
            Utc::now(),
            Vec::new(),
            vec![AuditFinding::producer_failure(
                "database_session",
                "database read-only mode could not be verified",
            )],
        );
    }
    if sqlx::query("SET statement_timeout = '20s'")
        .execute(&mut *connection)
        .await
        .is_err()
    {
        reset_audit_session(&mut connection).await;
        return assemble_prediction_market_data_audit(
            request,
            Utc::now(),
            Vec::new(),
            vec![AuditFinding::producer_failure(
                "database_session",
                "database statement timeout could not be configured",
            )],
        );
    }

    let mut results = Vec::new();
    for symbol in &request.symbols {
        for spec in &TIME_SERIES_SPECS {
            let result =
                match query_time_series_coverage(&mut connection, &request, spec, symbol).await {
                    Ok(observation) => evaluate_time_series_coverage(&request, observation),
                    Err(_) => unavailable_surface_result(
                        spec.surface,
                        symbol,
                        "read-only coverage query failed",
                    ),
                };
            results.push(result);
        }
        let orderbook = match query_pm_orderbook_coverage(&mut connection, &request, symbol).await {
            Ok(observation) => evaluate_time_series_coverage(&request, observation),
            Err(_) => unavailable_surface_result(
                PM_ORDERBOOK_SURFACE,
                symbol,
                "read-only orderbook coverage query failed",
            ),
        };
        results.push(orderbook);
        let settlement =
            match query_settlement_completeness(&mut connection, &request, symbol).await {
                Ok(observation) => evaluate_event_completeness(observation),
                Err(_) => unavailable_surface_result(
                    PM_SETTLEMENT_SURFACE,
                    symbol,
                    "read-only official-settlement query failed",
                ),
            };
        results.push(settlement);
    }
    let producer_findings = if reset_audit_session(&mut connection).await {
        Vec::new()
    } else {
        vec![AuditFinding::producer_failure(
            "database_session",
            "database audit session cleanup failed; connection was discarded",
        )]
    };
    assemble_prediction_market_data_audit(request, Utc::now(), results, producer_findings)
}

#[cfg(feature = "audit")]
pub async fn audit_prediction_market_data_url(
    db_url: &str,
    request: PredictionMarketDataAuditRequest,
) -> PredictionMarketDataAuditReport {
    let pool = match PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(StdDuration::from_secs(30))
        .connect(db_url)
        .await
    {
        Ok(pool) => pool,
        Err(_) => {
            return assemble_prediction_market_data_audit(
                request,
                Utc::now(),
                Vec::new(),
                vec![AuditFinding::producer_failure(
                    "database_connection",
                    "database connection failed",
                )],
            )
        }
    };
    audit_prediction_market_data(&pool, request).await
}

/// Legacy human-readable DB inventory retained for the existing operator CLI.
#[cfg(feature = "audit")]
pub async fn check_database(db_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(db_url)
        .await?;

    println!("=== Database Data Completeness Check ===\n");
    let tables = [
        "sync_records",
        BINANCE_PRICE_SURFACE,
        "clob_quote_ticks",
        "pm_market_metadata",
        "pm_market_catalog",
        "reference_price_ticks",
        "sports_state_events",
        BINANCE_LOB_SURFACE,
        BINANCE_AGG_TRADE_SURFACE,
        PM_ORDERBOOK_SURFACE,
        PM_SETTLEMENT_SURFACE,
    ];
    for table in tables {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await?;
        println!(
            "Table '{table}': {}",
            if exists { "EXISTS" } else { "MISSING" }
        );
    }
    println!("\nUse the typed market_data_gap_audit example for prediction snapshot gating.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        assemble_prediction_market_data_audit, evaluate_event_completeness,
        evaluate_time_series_coverage, AuditFindingCode, AuditStatus, EventCompletenessObservation,
        PredictionMarketDataAuditRequest, TimeSeriesCoverageObservation, BINANCE_AGG_TRADE_SURFACE,
        BINANCE_LOB_SURFACE, BINANCE_PRICE_SURFACE, CHAINLINK_REFERENCE_SURFACE,
        PM_ORDERBOOK_SURFACE, PM_SETTLEMENT_SURFACE,
    };

    fn request() -> PredictionMarketDataAuditRequest {
        PredictionMarketDataAuditRequest {
            symbols: vec!["BTCUSDT".to_string(), "SOLUSDT".to_string()],
            snapshot_start: Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
            snapshot_end: Utc.with_ymd_and_hms(2026, 7, 2, 0, 0, 0).unwrap(),
            horizon_secs: 300,
            warmup_secs: 3_900,
            bucket_secs: 60,
            minimum_coverage_bps: 9_900,
            maximum_gap_secs: 120,
            maximum_source_delay_secs: 30,
        }
    }

    fn healthy_time_series(surface: &str, symbol: &str) -> TimeSeriesCoverageObservation {
        let (usable_row_count, source_delay_rejected_rows) = if surface == PM_ORDERBOOK_SURFACE {
            (10_000, 0)
        } else {
            (9_999, 1)
        };
        TimeSeriesCoverageObservation {
            surface: surface.to_string(),
            symbol: symbol.to_string(),
            row_count: 10_000,
            usable_row_count,
            source_delay_rejected_rows,
            invalid_payload_rows: 0,
            causality_violations: 0,
            first_at: Some(Utc.with_ymd_and_hms(2026, 6, 30, 22, 55, 0).unwrap()),
            last_at: Some(Utc.with_ymd_and_hms(2026, 7, 1, 23, 59, 59).unwrap()),
            expected_buckets: 1_505,
            present_buckets: 1_505,
            max_gap_secs: 0,
            query_ms: 4,
        }
    }

    #[test]
    fn coverage_gate_is_fail_closed_for_missing_or_non_causal_data() {
        let request = request();
        let mut missing = healthy_time_series(BINANCE_AGG_TRADE_SURFACE, "SOLUSDT");
        missing.usable_row_count = 0;
        missing.present_buckets = 0;
        missing.max_gap_secs = 86_400;
        let result = evaluate_time_series_coverage(&request, missing);
        assert_eq!(result.status, AuditStatus::Critical);
        assert!(result
            .findings
            .iter()
            .any(|finding| finding.code == AuditFindingCode::NoUsableRows));

        let mut non_causal = healthy_time_series(BINANCE_PRICE_SURFACE, "BTCUSDT");
        non_causal.causality_violations = 1;
        let result = evaluate_time_series_coverage(&request, non_causal);
        assert_eq!(result.status, AuditStatus::Critical);
        assert!(result
            .findings
            .iter()
            .any(|finding| finding.code == AuditFindingCode::CausalityViolation));
    }

    #[test]
    fn complete_prediction_report_serializes_snapshot_gate_status() {
        let request = request();
        let mut results = Vec::new();
        for symbol in &request.symbols {
            for surface in [
                BINANCE_PRICE_SURFACE,
                BINANCE_AGG_TRADE_SURFACE,
                BINANCE_LOB_SURFACE,
                CHAINLINK_REFERENCE_SURFACE,
                PM_ORDERBOOK_SURFACE,
            ] {
                results.push(evaluate_time_series_coverage(
                    &request,
                    healthy_time_series(surface, symbol),
                ));
            }
            results.push(evaluate_event_completeness(EventCompletenessObservation {
                surface: PM_SETTLEMENT_SURFACE.to_string(),
                symbol: symbol.clone(),
                eligible_events: 288,
                complete_events: 288,
                expected_tokens: 576,
                resolved_tokens: 576,
                query_ms: 3,
            }));
        }

        let report = assemble_prediction_market_data_audit(
            request.clone(),
            Utc.with_ymd_and_hms(2026, 7, 2, 0, 1, 0).unwrap(),
            results,
            Vec::new(),
        );
        assert_eq!(report.data_audit_status, AuditStatus::Ok);
        report
            .validate_for_prediction_snapshot(
                &request.symbols,
                request.snapshot_start,
                request.snapshot_end,
            )
            .unwrap();
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["data_audit_status"], "ok");
        assert_eq!(json["read_only"], true);
    }

    #[test]
    fn snapshot_validation_recomputes_metrics_instead_of_trusting_ok_status() {
        let request = request();
        let mut results = Vec::new();
        for symbol in &request.symbols {
            for surface in [
                BINANCE_PRICE_SURFACE,
                BINANCE_AGG_TRADE_SURFACE,
                BINANCE_LOB_SURFACE,
                CHAINLINK_REFERENCE_SURFACE,
                PM_ORDERBOOK_SURFACE,
            ] {
                results.push(evaluate_time_series_coverage(
                    &request,
                    healthy_time_series(surface, symbol),
                ));
            }
            results.push(evaluate_event_completeness(EventCompletenessObservation {
                surface: PM_SETTLEMENT_SURFACE.to_string(),
                symbol: symbol.clone(),
                eligible_events: 288,
                complete_events: 288,
                expected_tokens: 576,
                resolved_tokens: 576,
                query_ms: 3,
            }));
        }
        let report = assemble_prediction_market_data_audit(
            request.clone(),
            Utc.with_ymd_and_hms(2026, 7, 2, 0, 1, 0).unwrap(),
            results,
            Vec::new(),
        );

        let mut unavailable = report.clone();
        unavailable.surface_results[0].metrics = super::AuditSurfaceMetrics::Unavailable;
        unavailable.surface_results[0].status = AuditStatus::Ok;
        unavailable.surface_results[0].findings.clear();
        assert!(unavailable
            .validate_for_prediction_snapshot(
                &request.symbols,
                request.snapshot_start,
                request.snapshot_end,
            )
            .unwrap_err()
            .contains("unavailable"));

        let mut forged_coverage = report.clone();
        if let super::AuditSurfaceMetrics::TimeSeries {
            present_buckets,
            coverage_bps,
            ..
        } = &mut forged_coverage.surface_results[0].metrics
        {
            *present_buckets = 0;
            *coverage_bps = 10_000;
        } else {
            panic!("first canonical result must be time-series metrics");
        }
        assert!(forged_coverage
            .validate_for_prediction_snapshot(
                &request.symbols,
                request.snapshot_start,
                request.snapshot_end,
            )
            .unwrap_err()
            .contains("not canonical"));

        let mut forged_settlement = report;
        let settlement = forged_settlement
            .surface_results
            .iter_mut()
            .find(|result| result.surface == PM_SETTLEMENT_SURFACE)
            .expect("settlement result");
        if let super::AuditSurfaceMetrics::EventCompleteness {
            expected_tokens,
            resolved_tokens,
            ..
        } = &mut settlement.metrics
        {
            *expected_tokens = 0;
            *resolved_tokens = 0;
        } else {
            panic!("settlement result must use event-completeness metrics");
        }
        assert!(forged_settlement
            .validate_for_prediction_snapshot(
                &request.symbols,
                request.snapshot_start,
                request.snapshot_end,
            )
            .unwrap_err()
            .contains("settlement counts are inconsistent"));
    }

    #[test]
    fn report_rejects_missing_required_surface_and_window_mismatch() {
        let request = request();
        let report = assemble_prediction_market_data_audit(
            request.clone(),
            Utc.with_ymd_and_hms(2026, 7, 2, 0, 1, 0).unwrap(),
            vec![evaluate_time_series_coverage(
                &request,
                healthy_time_series(BINANCE_PRICE_SURFACE, "BTCUSDT"),
            )],
            Vec::new(),
        );
        assert_eq!(report.data_audit_status, AuditStatus::Critical);
        assert!(report
            .blockers
            .iter()
            .any(|finding| finding.code == AuditFindingCode::RequiredSurfaceMissing));
        assert!(report
            .validate_for_prediction_snapshot(
                &request.symbols,
                request.snapshot_start,
                request.snapshot_end,
            )
            .is_err());

        let mut complete = report;
        complete.data_audit_status = AuditStatus::Ok;
        assert!(complete
            .validate_for_prediction_snapshot(
                &request.symbols,
                request.snapshot_start + chrono::Duration::minutes(5),
                request.snapshot_end,
            )
            .unwrap_err()
            .contains("window"));
    }

    #[test]
    fn settlement_gate_requires_both_binary_tokens_for_every_event() {
        let result = evaluate_event_completeness(EventCompletenessObservation {
            surface: PM_SETTLEMENT_SURFACE.to_string(),
            symbol: "BTCUSDT".to_string(),
            eligible_events: 2,
            complete_events: 1,
            expected_tokens: 4,
            resolved_tokens: 3,
            query_ms: 1,
        });
        assert_eq!(result.status, AuditStatus::Critical);
        assert!(result
            .findings
            .iter()
            .any(|finding| { finding.code == AuditFindingCode::IncompleteOfficialSettlement }));
    }

    #[test]
    fn extreme_warmup_is_rejected_without_timestamp_overflow() {
        let mut request = request();
        request.warmup_secs = i64::MAX;
        assert!(request.coverage_start().is_none());
        assert!(request
            .validation_findings()
            .iter()
            .any(|finding| finding.code == AuditFindingCode::InvalidRequest));
    }

    #[cfg(feature = "audit")]
    #[test]
    fn database_audit_queries_are_select_only() {
        let mut queries = super::TIME_SERIES_SPECS
            .iter()
            .map(|spec| super::time_series_coverage_sql(spec.raw_source_sql))
            .collect::<Vec<_>>();
        queries.push(super::PM_ORDERBOOK_COVERAGE_SQL.to_string());
        queries.push(super::PM_SETTLEMENT_COMPLETENESS_SQL.to_string());
        for query in queries {
            let normalized = query.to_ascii_lowercase();
            for forbidden in [
                "insert into",
                "update ",
                "delete from",
                "truncate ",
                "drop ",
            ] {
                assert!(
                    !normalized.contains(forbidden),
                    "audit query contains forbidden mutation: {forbidden}"
                );
            }
        }
    }
}
