//! Diagnostic read-only second-level orderflow import/audit.
//! Never dispatches jobs, never trains, never defaults to a Mac collector path.

use alpha_domain::sec_orderflow::{
    SecOrderflowAuditReportV1, SecOrderflowFileDigestV1, SecOrderflowInputStatusV1,
};
use alpha_engine::sec_orderflow::features::{FlowBookSnapshot, FlowTradeFragment, TradeSourceRef};
use alpha_engine::sec_orderflow::{audit_imported, merge_symbol_trades, ImportedSymbolTape};
use anyhow::{bail, Context};
use hft_research_manifest::sec_orderflow::SecOrderflowExperimentManifestV1;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_FILES_PER_SYMBOL: usize = 10_000;

#[derive(Debug, Clone)]
pub struct SecOrderflowAuditRequest {
    pub config: PathBuf,
    pub input_root: Option<PathBuf>,
}

pub fn audit(request: SecOrderflowAuditRequest) -> anyhow::Result<SecOrderflowAuditReportV1> {
    let bytes = std::fs::read(&request.config)
        .with_context(|| format!("failed to read config {}", request.config.display()))?;
    let manifest: SecOrderflowExperimentManifestV1 = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse config {}", request.config.display()))?;
    manifest.validate().map_err(anyhow::Error::msg)?;
    let Some(input_root) = request.input_root else {
        return Ok(SecOrderflowAuditReportV1::unavailable(
            &manifest,
            SecOrderflowInputStatusV1::Missing,
        )?);
    };
    if !input_root.exists() {
        return Ok(SecOrderflowAuditReportV1::unavailable(
            &manifest,
            SecOrderflowInputStatusV1::Unavailable,
        )?);
    }
    if !input_root.is_dir() {
        bail!("input root must be a directory");
    }
    let mut tapes = Vec::new();
    for symbol in &manifest.symbols {
        tapes.push(import_symbol(&input_root, symbol)?);
    }
    let empty = tapes
        .iter()
        .all(|tape| tape.trades.is_empty() && tape.books.is_empty());
    let status = if empty {
        SecOrderflowInputStatusV1::Empty
    } else {
        SecOrderflowInputStatusV1::Explicit
    };
    if empty {
        return Ok(SecOrderflowAuditReportV1::unavailable(&manifest, status)?);
    }
    audit_imported(&manifest, &tapes, status).map_err(anyhow::Error::msg)
}

fn import_symbol(root: &Path, symbol: &str) -> anyhow::Result<ImportedSymbolTape> {
    let trade_files = discover_files(root, symbol, "trades-")?;
    let book_files = discover_files(root, symbol, "book-")?;
    let mut fragments = Vec::new();
    let mut source_files = Vec::new();
    let mut parse_errors = 0;
    for path in &trade_files {
        let (rows, errors, digest) = read_jsonl_trades(path, symbol)?;
        parse_errors += errors;
        fragments.extend(rows);
        source_files.push(digest);
    }
    let mut books = Vec::new();
    for path in &book_files {
        let (rows, errors, digest) = read_jsonl_books(path, symbol)?;
        parse_errors += errors;
        books.extend(rows);
        source_files.push(digest);
    }
    let (trades, isolated) = merge_symbol_trades(symbol, fragments).map_err(anyhow::Error::msg)?;
    Ok(ImportedSymbolTape {
        symbol: symbol.to_string(),
        trades,
        books,
        source_files,
        parse_errors,
        isolated_cross_file_seconds: isolated,
    })
}

fn discover_files(root: &Path, symbol: &str, prefix: &str) -> anyhow::Result<Vec<PathBuf>> {
    let candidates = [
        root.join(symbol),
        root.join("flow").join(symbol),
        root.join("data").join("flow").join(symbol),
    ];
    for dir in candidates {
        if !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_file() && name.starts_with(prefix) && name.ends_with(".jsonl") {
                files.push(path);
            }
        }
        if !files.is_empty() {
            files.sort();
            if files.len() > MAX_FILES_PER_SYMBOL {
                bail!("too many {prefix} files under {}", dir.display());
            }
            return Ok(files);
        }
    }
    Ok(Vec::new())
}

fn read_jsonl_trades(
    path: &Path,
    symbol: &str,
) -> anyhow::Result<(Vec<FlowTradeFragment>, u64, SecOrderflowFileDigestV1)> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut rows = Vec::new();
    let mut errors = 0;
    let mut selected = 0;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.len() > MAX_LINE_BYTES {
            bail!(
                "JSONL line exceeds {MAX_LINE_BYTES} bytes in {}",
                path.display()
            );
        }
        if line.trim().is_empty() {
            continue;
        }
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
        selected += 1;
        match serde_json::from_str::<FlowTradeFragment>(&line) {
            Ok(mut row) => {
                row.source = TradeSourceRef {
                    path: path.to_string_lossy().into_owned(),
                    line: u64::try_from(index + 1).unwrap_or(u64::MAX),
                };
                if row.symbol.as_deref().is_some_and(|value| value != symbol) {
                    errors += 1;
                    continue;
                }
                row.symbol = Some(symbol.to_string());
                rows.push(row);
            }
            Err(_) => errors += 1,
        }
    }
    Ok((rows, errors, digest_for(path, selected, hasher)))
}

fn read_jsonl_books(
    path: &Path,
    symbol: &str,
) -> anyhow::Result<(Vec<FlowBookSnapshot>, u64, SecOrderflowFileDigestV1)> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut rows = Vec::new();
    let mut errors = 0;
    let mut selected = 0;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.len() > MAX_LINE_BYTES {
            bail!(
                "JSONL line exceeds {MAX_LINE_BYTES} bytes in {}",
                path.display()
            );
        }
        if line.trim().is_empty() {
            continue;
        }
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
        selected += 1;
        match serde_json::from_str::<FlowBookSnapshot>(&line) {
            Ok(mut row) => {
                row.source = TradeSourceRef {
                    path: path.to_string_lossy().into_owned(),
                    line: u64::try_from(index + 1).unwrap_or(u64::MAX),
                };
                if row.symbol != symbol {
                    errors += 1;
                    continue;
                }
                rows.push(row);
            }
            Err(_) => errors += 1,
        }
    }
    Ok((rows, errors, digest_for(path, selected, hasher)))
}

fn digest_for(path: &Path, rows: u64, hasher: Sha256) -> SecOrderflowFileDigestV1 {
    SecOrderflowFileDigestV1 {
        path: path.to_string_lossy().into_owned(),
        rows,
        sha256: format!("{:x}", hasher.finalize()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn missing_input_root_is_ineligible_without_scanning_mac_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&SecOrderflowExperimentManifestV1::canonical_audit()).unwrap(),
        )
        .unwrap();
        let report = audit(SecOrderflowAuditRequest {
            config,
            input_root: None,
        })
        .unwrap();
        assert!(!report.eligibility.research_eligible);
        assert_eq!(report.input_status, SecOrderflowInputStatusV1::Missing);
        assert!(report.identities.input_list_sha256.is_none());
        assert_eq!(report.eligibility.jobs_dispatched, 0);
    }

    #[test]
    fn unavailable_path_exits_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&SecOrderflowExperimentManifestV1::canonical_audit()).unwrap(),
        )
        .unwrap();
        let report = audit(SecOrderflowAuditRequest {
            config,
            input_root: Some(dir.path().join("does-not-exist")),
        })
        .unwrap();
        assert_eq!(report.input_status, SecOrderflowInputStatusV1::Unavailable);
        assert!(!report.eligibility.research_eligible);
    }

    #[test]
    fn tiny_fixture_reports_identities_and_invalid_target_masks() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&SecOrderflowExperimentManifestV1::canonical_audit()).unwrap(),
        )
        .unwrap();
        for symbol in ["MARSCOINUSDT", "NIULAIUSDT", "HAJIMIUSDT"] {
            let symbol_dir = dir.path().join(symbol);
            std::fs::create_dir_all(&symbol_dir).unwrap();
            let mut trades =
                std::fs::File::create(symbol_dir.join("trades-fixture.jsonl")).unwrap();
            writeln!(
                trades,
                r#"{{"ts":1000000,"sec":1000,"o":100.0,"h":101.0,"l":99.0,"c":100.5,"buyVol":1,"sellVol":2,"buyTo":100.5,"sellTo":201.0,"buyN":1,"sellN":1,"tradesN":2,"tradesTo":301.5,"vwap":100.5,"cvd":9}}"#
            )
            .unwrap();
            writeln!(
                trades,
                r#"{{"ts":1000000,"sec":1000,"o":100.5,"h":102.0,"l":98.0,"c":101.0,"buyVol":3,"sellVol":0,"buyTo":303.0,"sellTo":0.0,"buyN":1,"sellN":0,"tradesN":1,"tradesTo":303.0,"vwap":101.0}}"#
            )
            .unwrap();
            writeln!(
                trades,
                r#"{{"ts":1001000,"sec":1001,"o":101.0,"h":103.0,"l":100.0,"c":102.0,"buyVol":1,"sellVol":1,"buyTo":102.0,"sellTo":102.0,"buyN":1,"sellN":1,"tradesN":2,"tradesTo":204.0,"vwap":102.0}}"#
            )
            .unwrap();
            let mut books = std::fs::File::create(symbol_dir.join("book-fixture.jsonl")).unwrap();
            writeln!(
                books,
                r#"{{"ts":1015000,"symbol":"{symbol}","mid":101.0,"bid1":100.9,"ask1":101.1,"spreadBp":19.8,"bidNotional5":10,"askNotional5":10,"imb5":1.0,"imb20":1.0,"bids":[[100.9,1]],"asks":[[101.1,1]]}}"#
            )
            .unwrap();
        }
        let report = audit(SecOrderflowAuditRequest {
            config,
            input_root: Some(dir.path().to_path_buf()),
        })
        .unwrap();
        assert_eq!(report.input_status, SecOrderflowInputStatusV1::Explicit);
        assert!(!report.eligibility.research_eligible);
        assert!(report.identities.input_list_sha256.is_some());
        assert!(!report.target_counts.is_empty());
        assert!(report.target_counts.iter().any(|row| row.target
            == hft_research_manifest::sec_orderflow::SecOrderflowTargetKindV1::MarkReturn
            && row.valid == 0
            && row
                .rejection_reasons
                .keys()
                .any(|reason| reason.contains("mark"))));
        assert_eq!(report.eligibility.jobs_dispatched, 0);
        assert!(!report.eligibility.live_admitted);
    }
}
