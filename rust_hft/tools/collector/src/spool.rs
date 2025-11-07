use anyhow::{Context, Result};
use lz4::{Decoder, EncoderBuilder};
use rand::{distributions::Alphanumeric, Rng};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::db::DbSink;
use std::sync::Arc;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn spool_dir() -> PathBuf {
    PathBuf::from(env_string(
        "COLLECTOR_SPOOL_DIR",
        "/tmp/hft-collector-spool",
    ))
}

fn max_bytes() -> u64 {
    // 預設 64MiB，避免佔用過多本地磁碟
    env_u64("COLLECTOR_SPOOL_MAX_BYTES", 64 * 1024 * 1024)
}

fn drain_limit() -> usize {
    env_u64("COLLECTOR_SPOOL_DRAIN_LIMIT", 5) as usize
}

fn ttl_secs() -> u64 {
    // 預設 6 小時
    env_u64("COLLECTOR_SPOOL_TTL_SECS", 6 * 3600)
}

fn ensure_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("create dir: {}", dir.display()))?;
    }
    Ok(())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis()
}

fn random_tag(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn total_dir_size(dir: &Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    size = size.saturating_add(md.len());
                }
            }
        }
    }
    size
}

fn list_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    let mt = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    files.push((mt, p));
                }
            }
        }
    }
    files.sort_by_key(|(mt, _)| *mt);
    files.into_iter().map(|(_, p)| p).collect()
}

fn compress_lz4(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = EncoderBuilder::new().level(1).build(Vec::new())?;
    enc.write_all(data)?;
    let (buf, res) = enc.finish();
    res?;
    Ok(buf)
}

fn decompress_lz4_to_vec(mut rdr: Decoder<File>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    rdr.read_to_end(&mut buf)?;
    Ok(buf)
}

fn sanitize_table_name(table: &str) -> String {
    table
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 將失敗批次寫入本地簡易 spool（LZ4），避免短暫故障丟數。
/// 檔案內容：第一行為 JSON header {"table":"..."}，其後為 JSONEachRow 行。
pub fn save(table: &str, rows: &[String]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let dir = spool_dir();
    ensure_dir(&dir)?;

    let header = serde_json::json!({"table": table});
    let mut body = String::new();
    body.push_str(&header.to_string());
    body.push('\n');
    for line in rows {
        body.push_str(line);
        body.push('\n');
    }
    let data = compress_lz4(body.as_bytes())?;

    let fname = format!(
        "{}-{}-{}.lz4",
        now_ms(),
        sanitize_table_name(table),
        random_tag(6)
    );
    let path = dir.join(fname);
    fs::write(&path, data)?;

    // 超出容量時，刪除最舊檔案直到回到上限
    let mut size = total_dir_size(&dir);
    let maxb = max_bytes();
    if size > maxb {
        let files = list_files_sorted(&dir);
        for p in files {
            if size <= maxb {
                break;
            }
            if let Ok(md) = fs::metadata(&p) {
                let len = md.len();
                let _ = fs::remove_file(&p);
                size = size.saturating_sub(len);
            }
        }
    }
    Ok(rows.len())
}

/// 嘗試回放本地 spool 的失敗批次；成功入庫後刪檔，否則保留等待下次。
pub async fn drain(sink: &Arc<dyn DbSink>) -> Result<usize> {
    let dir = spool_dir();
    ensure_dir(&dir)?;
    let files = list_files_sorted(&dir);
    let mut done_rows = 0usize;
    let limit = drain_limit();
    let ttl = Duration::from_secs(ttl_secs());
    let now = SystemTime::now();

    for (processed, path) in files.into_iter().enumerate() {
        if processed >= limit {
            break;
        }
        // TTL: 過舊直接刪除
        if let Ok(md) = fs::metadata(&path) {
            if let Ok(mt) = md.modified() {
                if now.duration_since(mt).unwrap_or(Duration::from_secs(0)) > ttl {
                    let _ = fs::remove_file(&path);
                    continue;
                }
            }
        }

        let f = match File::open(&path) {
            Ok(x) => x,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let rdr = match Decoder::new(f) {
            Ok(r) => r,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let buf = match decompress_lz4_to_vec(rdr) {
            Ok(b) => b,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let s = String::from_utf8_lossy(&buf);
        let mut lines = s.lines();
        let header = match lines.next() {
            Some(h) => h,
            None => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let table = match serde_json::from_str::<serde_json::Value>(header)
            .ok()
            .and_then(|v| {
                v.get("table")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
            }) {
            Some(t) => t,
            None => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let rows: Vec<String> = lines
            .map(|ln| ln.to_string())
            .filter(|ln| !ln.is_empty())
            .collect();
        if rows.is_empty() {
            let _ = fs::remove_file(&path);
            continue;
        }

        match sink.insert_json_rows(&table, &rows).await {
            Ok(n) if n == rows.len() => {
                done_rows += n;
                let _ = fs::remove_file(&path);
            }
            Ok(_) => {
                // 未完全成功，保留等待下次
                break;
            }
            Err(_) => {
                // 連線錯誤，保留等待下次
                break;
            }
        }
    }
    Ok(done_rows)
}
