//! Archive coverage is independent of process health and upload success.
//! Manifest coverage is a candidate only; admission also requires native tape
//! verification and the research materializer's per-second calendar gate.
use anyhow::{Context, Result};
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape_archive, BinanceMarketTapeTriplet,
    BinanceMarketTapeTrustAnchor,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::PathBuf};

pub const EIGHT_HOURS_NS: u64 = 8 * 60 * 60 * 1_000_000_000;
pub const MAX_RECENT_SEGMENTS: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSegment {
    pub object: String,
    pub data_sha256: String,
    pub manifest_sha256: String,
    pub market: String,
    pub dataset: String,
    pub shard_id: String,
    pub symbols_sha256: String,
    pub capture_session_id: String,
    pub start_ns: u64,
    pub end_ns: u64,
    pub replay_safe: bool,
}

impl ArchiveSegment {
    pub fn from_manifest(
        manifest: &Value,
        object: String,
        manifest_sha256: String,
    ) -> Result<Self> {
        let string = |key: &str| -> Result<String> {
            Ok(manifest[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .with_context(|| format!("archive manifest missing {key}"))?
                .to_owned())
        };
        let symbols = manifest["symbols"]
            .as_array()
            .context("archive manifest missing symbols")?
            .iter()
            .map(|s| s.as_str().context("invalid archive symbol"))
            .collect::<Result<BTreeSet<_>>>()?;
        anyhow::ensure!(!symbols.is_empty(), "archive manifest has no symbols");
        let summary = &manifest["lob_continuity"];
        let capture_session_id = summary["capture_session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("archive manifest missing capture session")?
            .to_owned();
        let start_ns = manifest["start_received_at_ns"]
            .as_u64()
            .context("archive start missing")?;
        let end_ns = manifest["end_received_at_ns"]
            .as_u64()
            .context("archive end missing")?;
        anyhow::ensure!(end_ns > start_ns, "archive range is empty or reversed");
        let replay_safe = manifest["has_replay_safe_checkpoint"] == true
            && summary["sequence_gaps"].as_u64() == Some(0)
            && summary["source_time_rollbacks"].as_u64() == Some(0)
            && summary["missing_symbols"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && summary["covered_symbol_count"].as_u64() == Some(symbols.len() as u64)
            && (manifest["all_symbols_bridged"] == true
                || manifest["all_stream_coverage_verified"] == true);
        Ok(Self {
            object,
            data_sha256: string("sha256")?,
            manifest_sha256,
            market: string("market")?,
            dataset: string("dataset")?,
            shard_id: string("shard_id")?,
            symbols_sha256: hex::encode(Sha256::digest(serde_json::to_vec(&symbols)?)),
            capture_session_id,
            start_ns,
            end_ns,
            replay_safe,
        })
    }

    fn break_reason(&self, next: &Self) -> Option<&'static str> {
        if self.market != next.market
            || self.dataset != next.dataset
            || self.shard_id != next.shard_id
            || self.symbols_sha256 != next.symbols_sha256
        {
            Some("scope_changed")
        } else if self.capture_session_id != next.capture_session_id {
            Some("capture_session_changed")
        } else if next.start_ns > self.end_ns {
            Some("recording_gap")
        } else if next.start_ns < self.end_ns {
            Some("recording_overlap_or_rollback")
        } else {
            None
        }
    }
}

pub fn merge_recent_segments(current: &mut Vec<ArchiveSegment>, new: &[ArchiveSegment]) {
    for segment in new {
        if !current.iter().any(|old| {
            old.object == segment.object && old.manifest_sha256 == segment.manifest_sha256
        }) {
            current.push(segment.clone());
        }
    }
    current.sort_by_key(|s| (s.start_ns, s.end_ns));
    if current.len() > MAX_RECENT_SEGMENTS {
        current.drain(..current.len() - MAX_RECENT_SEGMENTS);
    }
}

pub fn coverage_report(segments: &[ArchiveSegment]) -> Value {
    let mut spans = Vec::new();
    let mut breaks = Vec::new();
    let mut start: Option<u64> = None;
    let mut end = 0;
    let mut previous: Option<&ArchiveSegment> = None;
    for segment in segments {
        let safe = segment.replay_safe && segment.end_ns > segment.start_ns;
        let reason = if !safe {
            Some("unsafe_segment")
        } else {
            previous.and_then(|p| p.break_reason(segment))
        };
        if let Some(reason) = reason {
            if let Some(start_ns) = start.take() {
                spans.push(json!({"start_ns":start_ns,"end_ns":end,"duration_ns":end-start_ns}));
            }
            breaks.push(json!({"reason":reason,"before_end_ns":previous.map(|p|p.end_ns),
                "next_start_ns":segment.start_ns,"object":segment.object,
                "manifest_sha256":segment.manifest_sha256,"capture_session_id":segment.capture_session_id}));
        }
        if safe {
            start.get_or_insert(segment.start_ns);
            end = segment.end_ns;
        }
        previous = Some(segment);
    }
    if let Some(start_ns) = start {
        spans.push(json!({"start_ns":start_ns,"end_ns":end,"duration_ns":end-start_ns}));
    }
    let longest = spans
        .iter()
        .filter_map(|s| s["duration_ns"].as_u64())
        .max()
        .unwrap_or(0);
    json!({"schema":"monday.archive_coverage.v1","evidence":"verified_upload_manifests",
        "segments":segments.len(),"longest_candidate_duration_ns":longest,
        "eight_hour_candidate_available":longest >= EIGHT_HOURS_NS,
        "native_tape_verification":"pending","calendar_admission":"pending",
        "spans":spans,"breaks":breaks})
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveIndex {
    schema: String,
    start_ns: u64,
    end_ns: u64,
    segments: Vec<ArchiveInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveInput {
    data_path: PathBuf,
    object: String,
    data_sha256: String,
    manifest_sha256: String,
}

impl ArchiveInput {
    fn triplet(&self) -> Result<BinanceMarketTapeTriplet> {
        let file = self
            .data_path
            .file_name()
            .and_then(|s| s.to_str())
            .context("archive filename invalid")?;
        Ok(BinanceMarketTapeTriplet {
            data: self.data_path.clone(),
            manifest: self
                .data_path
                .with_file_name(format!("{file}.manifest.json")),
            success: self.data_path.with_file_name(format!("{file}._SUCCESS")),
        })
    }
}

/// Uses external hashes and preserves the input index as the immutable scope.
/// A successful tape report never substitutes for materialization/calendar admission.
pub fn audit_archive_index(path: &std::path::Path) -> Result<Value> {
    let bytes = fs::read(path)?;
    let index_sha256 = hex::encode(Sha256::digest(&bytes));
    let index: ArchiveIndex = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        index.schema == "monday.archive_continuity_index.v1",
        "unsupported archive index"
    );
    anyhow::ensure!(
        index.end_ns > index.start_ns,
        "archive window must be nonempty"
    );
    anyhow::ensure!(
        !index.segments.is_empty() && index.segments.len() <= 4096,
        "archive requires 1..4096 segments"
    );
    let mut segments = Vec::new();
    let mut seen = BTreeSet::new();
    for input in &index.segments {
        anyhow::ensure!(seen.insert(&input.object), "duplicate archive object");
        let triplet = input.triplet()?;
        let manifest_bytes = fs::read(&triplet.manifest)?;
        anyhow::ensure!(
            hex::encode(Sha256::digest(&manifest_bytes)) == input.manifest_sha256,
            "archive manifest hash mismatch"
        );
        let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
        anyhow::ensure!(
            manifest["sha256"] == input.data_sha256,
            "archive data anchor mismatch"
        );
        segments.push(ArchiveSegment::from_manifest(
            &manifest,
            input.object.clone(),
            input.manifest_sha256.clone(),
        )?);
    }
    let mut report = coverage_report(&segments);
    report["schema"] = json!("monday.archive_continuity_readback.v1");
    report["index_sha256"] = json!(index_sha256);
    report["requested_start_ns"] = json!(index.start_ns);
    report["requested_end_ns"] = json!(index.end_ns);
    report["objects"] = serde_json::to_value(&segments)?;
    let spans = report["spans"]
        .as_array()
        .context("coverage spans missing")?;
    let covered = report["breaks"].as_array().is_some_and(Vec::is_empty)
        && spans.len() == 1
        && spans[0]["start_ns"]
            .as_u64()
            .is_some_and(|s| s <= index.start_ns)
        && spans[0]["end_ns"]
            .as_u64()
            .is_some_and(|e| e >= index.end_ns);
    let verification = if covered {
        verify_binance_market_tape_archive(index.segments.iter().map(|input| {
            let trust = BinanceMarketTapeTrustAnchor::from_lower_hex(
                &input.data_sha256,
                &input.manifest_sha256,
            )?;
            seal_binance_market_tape_triplet(&input.triplet()?, &trust)
        }))
        .map(|_| ())
    } else {
        Err(anyhow::anyhow!(
            "requested window lacks one uninterrupted manifest chain"
        ))
    };
    report["evidence"] = json!("independent_local_triplet_readback");
    report["native_tape_verification"] = json!(if verification.is_ok() {
        "passed"
    } else {
        "failed"
    });
    report["eight_hour_tape_available"] =
        json!(verification.is_ok() && index.end_ns - index.start_ns >= EIGHT_HOURS_NS);
    report["failure"] = verification
        .err()
        .map(|e| json!(e.to_string()))
        .unwrap_or(Value::Null);
    report["observed_at"] = json!(chrono::Utc::now().to_rfc3339());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(start_ns: u64, end_ns: u64) -> ArchiveSegment {
        ArchiveSegment {
            object: format!("oss://fixture/{start_ns}"),
            data_sha256: "a".repeat(64),
            manifest_sha256: "b".repeat(64),
            market: "usdm".into(),
            dataset: "test".into(),
            shard_id: "all".into(),
            symbols_sha256: "c".repeat(64),
            capture_session_id: "session".into(),
            start_ns,
            end_ns,
            replay_safe: true,
        }
    }
    #[test]
    fn archive_index_requires_raw_triplets_and_keeps_calendar_pending() {
        use crate::lob_archiver::{Market, Segment, SegmentConfig};
        let root = tempfile::tempdir().unwrap();
        let start = 1_789_430_400_000_000_000_u64;
        let mut segment = Segment::create(
            SegmentConfig {
                spool_dir: root.path().canonicalize().unwrap(),
                market: Market::Usdm,
                dataset: "usdm_perpetual_top100_lob".into(),
                shard_id: "all".into(),
                symbols: vec!["BTCUSDT".into()],
                security_token_symbols: vec![],
                excluded_symbols: vec![],
                snapshot_limit: 100,
                zstd_timeout: std::time::Duration::from_secs(30),
                stream_types: vec!["depth@100ms".into()],
            },
            start,
        )
        .unwrap();
        segment
            .write(
                "session_start",
                json!({"session_id":"s","market":"usdm","symbols":1,
            "websocket_shards":1,"websocket_streams":1,"stream_types":["depth@100ms"]}),
                start,
            )
            .unwrap();
        segment
            .write(
                "snapshot",
                json!({"session_id":"s","symbol":"BTCUSDT","request_started_at_ns":start,
            "snapshot":{"lastUpdateId":100,"bids":[["100","1"]],"asks":[["101","1"]]}}),
                start + 100_000_000,
            )
            .unwrap();
        segment
            .write(
                "diff",
                json!({"session_id":"s","frame":{"data":{"e":"depthUpdate",
            "E":(start+200_000_000)/1_000_000,"T":(start+200_000_000)/1_000_000,
            "s":"BTCUSDT","U":101,"u":101,"pu":100,"b":[["100","2"]],"a":[]}}}),
                start + 200_000_000,
            )
            .unwrap();
        segment
            .write(
                "stream_coverage",
                json!({"session_id":"s","shards":[["btcusdt@depth@100ms"]]}),
                start + 300_000_000,
            )
            .unwrap();
        segment
            .write(
                "checkpoint",
                json!({"session_id":"s","symbol":"BTCUSDT","last_update_id":101,
            "synced":true,"bridged":true,"continuity_complete":true,"stream_coverage_verified":true,
            "bids":[["100","2"]],"asks":[["101","1"]],"replay_safe":true,"reason":"scheduled"}),
                start + 400_000_000,
            )
            .unwrap();
        let artifacts = segment.close().unwrap().unwrap();
        crate::lob_archiver::write_success_marker(&artifacts.data, &artifacts.sha256).unwrap();
        let path = root.path().join("index.json");
        fs::write(&path,serde_json::to_vec(&json!({"schema":"monday.archive_continuity_index.v1",
            "start_ns":start,"end_ns":start+400_000_000,"segments":[{
            "data_path":artifacts.data,"object":format!("oss://fixture/{}",artifacts.data.file_name().unwrap().to_str().unwrap()),
            "data_sha256":artifacts.sha256,"manifest_sha256":crate::lob_archiver::sha256_file(&artifacts.manifest).unwrap()}]})).unwrap()).unwrap();
        let report = audit_archive_index(&path).unwrap();
        assert_eq!(report["native_tape_verification"], "passed", "{report}");
        assert_eq!(report["eight_hour_tape_available"], false);
        assert_eq!(report["calendar_admission"], "pending");
        fs::write(artifacts.success, b"invalid marker\n").unwrap();
        let report = audit_archive_index(&path).unwrap();
        assert_eq!(report["native_tape_verification"], "failed");
    }

    #[test]
    fn archive_coverage_crosses_hour_and_date_but_not_restart_gap() {
        let midnight = chrono::DateTime::parse_from_rfc3339("2026-09-15T00:00:00Z")
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap() as u64;
        let hour = 3_600_000_000_000;
        let mut segments: Vec<_> = (0..12)
            .map(|i| {
                segment(
                    midnight - 4 * hour + i * hour,
                    midnight - 3 * hour + i * hour,
                )
            })
            .collect();
        let report = coverage_report(&segments);
        assert_eq!(report["eight_hour_candidate_available"], true);
        assert_eq!(report["native_tape_verification"], "pending");
        segments[6].start_ns += 7_000_000_000;
        let report = coverage_report(&segments);
        assert_eq!(report["eight_hour_candidate_available"], false);
        assert_eq!(report["breaks"][0]["reason"], "recording_gap");
    }
    #[test]
    fn unsafe_segments_sessions_and_overlaps_break_coverage() {
        for mode in 0..3 {
            let mut b = segment(EIGHT_HOURS_NS / 2, EIGHT_HOURS_NS);
            if mode == 0 {
                b.replay_safe = false;
            } else if mode == 1 {
                b.capture_session_id = "new".into();
            } else {
                b.start_ns -= 1;
            }
            assert_eq!(
                coverage_report(&[segment(0, EIGHT_HOURS_NS / 2), b])
                    ["eight_hour_candidate_available"],
                false
            );
        }
    }
    #[test]
    fn retries_deduplicate_and_late_uploads_restore_order() {
        let mut records = vec![segment(10, 20)];
        merge_recent_segments(&mut records, &[segment(0, 10), segment(10, 20)]);
        assert_eq!(records.len(), 2);
        assert_eq!(
            coverage_report(&records)["longest_candidate_duration_ns"],
            20
        );
    }
}
