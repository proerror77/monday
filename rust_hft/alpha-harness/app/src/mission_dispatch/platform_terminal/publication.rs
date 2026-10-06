//! Finite immutable publication derived from the authenticated audit package.
//! Every PUT is followed by independent bounded GET and exact content readback.
use anyhow::{ensure, Context};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Publication {
    put_url: String,
    readback_url: String,
}

pub(super) fn publish(
    client: &Client,
    retained: &Path,
    operation: &str,
    audit: &alpha_store::campaign_ledger::CampaignPlatformTerminalAuditV1,
    origin: &str,
    publications: &BTreeMap<String, Publication>,
) -> anyhow::Result<()> {
    super::retained_files::verify(retained, &audit.retained_manifest_sha256)?;
    let prefix = format!(
        "research/native-terminal-audits/{operation}/{}",
        super::audit_package_id(audit)?
    );
    let objects = super::retained_files::objects(retained)?;
    ensure!(
        publications.len() == objects.len(),
        "terminal publication requires exact observation coverage"
    );
    for (name, sha256, bytes) in objects {
        let key = format!("{prefix}/{name}");
        let access = publications
            .get(&key)
            .context("retained terminal object lacks exact publication access")?;
        let expected = format!("{origin}/{key}");
        for url in [&access.put_url, &access.readback_url] {
            ensure!(
                hft_research_dispatch_io::canonical_tokyo_oss_internal_object(
                    "terminal observation",
                    url
                )? == expected,
                "terminal observation publication changed original operation or audit bytes"
            );
        }
        publish_file(client, &retained.join(name), &sha256, bytes, access)?;
    }
    Ok(())
}

pub(super) fn validate_witness_access(expected: &str, access: &Publication) -> anyhow::Result<()> {
    for url in [&access.put_url, &access.readback_url] {
        ensure!(
            hft_research_dispatch_io::canonical_tokyo_oss_internal_object("terminal witness", url)?
                == expected,
            "terminal witness publication changed original operation or signed evidence"
        );
    }
    Ok(())
}

pub(super) fn publish_witness(
    client: &Client,
    path: &Path,
    bytes: &[u8],
    expected: &str,
    access: &Publication,
) -> anyhow::Result<()> {
    for url in [&access.put_url, &access.readback_url] {
        ensure!(
            hft_research_dispatch_io::canonical_tokyo_oss_internal_object("terminal witness", url)?
                == expected,
            "terminal witness publication changed original operation or signed evidence"
        );
    }
    publish_file(
        client,
        path,
        &hft_research_platform::sha256(bytes),
        bytes.len() as u64,
        access,
    )
}

pub(super) fn publish_file(
    client: &Client,
    path: &Path,
    sha256: &str,
    bytes: u64,
    access: &Publication,
) -> anyhow::Result<()> {
    use rustix::fs::{open, Mode, OFlags};
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    ensure!(
        file.metadata()?.is_file()
            && file.metadata()?.len() == bytes
            && bytes > 0
            && bytes <= 512 * 1024 * 1024,
        "terminal publication file changed its bound"
    );
    let response = client
        .put(&access.put_url)
        .header("x-oss-forbid-overwrite", "true")
        .header(reqwest::header::CONTENT_LENGTH, bytes)
        .body(reqwest::blocking::Body::new(file.take(bytes + 1)))
        .send()
        .map_err(reqwest::Error::without_url)?;
    ensure!(
        response.status().is_success() || response.status() == reqwest::StatusCode::CONFLICT,
        "immutable terminal observation publication rejected"
    );
    let mut response = client
        .get(&access.readback_url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(reqwest::Error::without_url)?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .context("terminal readback size overflow")?;
        ensure!(
            count <= bytes,
            "terminal observation readback exceeds its bound"
        );
        digest.update(&buffer[..n]);
    }
    ensure!(
        count == bytes && hex::encode(digest.finalize()) == sha256,
        "independent terminal observation readback changed immutable bytes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_immutable_put_requires_exact_independent_get_bytes() -> anyhow::Result<()> {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
        };
        for corrupt in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let address = listener.local_addr()?;
            let bytes = b"synthetic terminal observation bytes".to_vec();
            let expected = bytes.clone();
            let server = std::thread::spawn(move || -> anyhow::Result<()> {
                for index in 0..2 {
                    let (stream, _) = listener.accept()?;
                    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
                    let mut reader = BufReader::new(stream);
                    let mut first = String::new();
                    reader.read_line(&mut first)?;
                    let mut size = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line)?;
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                size = value.trim().parse()?;
                            }
                        }
                    }
                    let body = if index == 0 {
                        assert!(first.starts_with("PUT /object "));
                        let mut observed = vec![0; size];
                        reader.read_exact(&mut observed)?;
                        assert_eq!(observed, expected);
                        Vec::new()
                    } else {
                        assert!(first.starts_with("GET /object "));
                        let mut result = expected.clone();
                        if corrupt {
                            result[0] ^= 1;
                        }
                        result
                    };
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )?;
                    reader.get_mut().write_all(&body)?;
                }
                Ok(())
            });
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("synthetic-observation.json");
            std::fs::write(&path, &bytes)?;
            let access = Publication {
                put_url: format!("http://{address}/object"),
                readback_url: format!("http://{address}/object"),
            };
            let client = Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .build()?;
            let result = publish_file(
                &client,
                &path,
                &hft_research_platform::sha256(&bytes),
                bytes.len() as u64,
                &access,
            );
            assert_eq!(result.is_err(), corrupt);
            server.join().unwrap()?;
        }
        Ok(())
    }
}
