//! Host-owned private TLS trust and identity. No insecure certificate override,
//! ambient proxy, redirect, token issuer or endpoint discovery.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub ca_file: Option<PathBuf>,
    /// Combined certificate chain and key PEM, mounted only into the host.
    pub identity_file: Option<PathBuf>,
}
fn pem(path: &Path, private: bool) -> Result<Vec<u8>> {
    ensure!(path.is_absolute(), "TLS material requires an absolute path");
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("TLS parent required"))?;
    ensure!(
        parent.canonicalize()? == parent,
        "TLS parent must be canonical"
    );
    let file = std::fs::File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024,
        "invalid TLS material"
    );
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0
                && parent.metadata()?.permissions().mode() & 0o077 == 0,
            "TLS key must be private"
        );
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= 64 * 1024,
        "TLS material exceeds bound"
    );
    Ok(bytes)
}
/// Host-owned or Attempt-scoped configuration; requires a private canonical
/// directory and regular file. This does not discover or issue credentials.
pub fn read_private_file(path: &Path) -> Result<Vec<u8>> {
    pem(path, true)
}
impl TlsConfig {
    pub fn client(&self, timeout: Duration, https: bool) -> Result<reqwest::Client> {
        ensure!(
            https || (self.ca_file.is_none() && self.identity_file.is_none()),
            "private TLS material requires HTTPS"
        );
        let mut client = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none());
        if let Some(path) = &self.ca_file {
            // Explicit private trust replaces public roots for this connection.
            let certificates = reqwest::Certificate::from_pem_bundle(&pem(path, false)?)?;
            ensure!(!certificates.is_empty(), "private CA missing");
            client = client.tls_built_in_root_certs(false);
            for certificate in certificates {
                client = client.add_root_certificate(certificate);
            }
        }
        if let Some(path) = &self.identity_file {
            client = client.identity(reqwest::Identity::from_pem(&pem(path, true)?)?);
        }
        Ok(client.build()?)
    }
}
/// Bounds accepted application payload. HTTP/TLS buffering can receive extra wire bytes.
pub struct BoundedBody {
    response: Option<reqwest::Response>,
    remaining: u64,
}
impl BoundedBody {
    pub fn new(response: reqwest::Response, limit: u64) -> Result<Self> {
        ensure!(
            response.content_length().is_none_or(|n| n <= limit),
            "response content length exceeds payload reservation"
        );
        Ok(Self {
            response: Some(response),
            remaining: limit,
        })
    }
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(response) = self.response.as_mut() else {
            return Ok(None);
        };
        let result = response.chunk().await;
        match result {
            Ok(Some(chunk)) if chunk.len() as u64 <= self.remaining => {
                self.remaining -= chunk.len() as u64;
                Ok(Some(chunk.to_vec()))
            }
            Ok(None) => {
                self.response.take();
                Ok(None)
            }
            Ok(Some(_)) => {
                self.response.take();
                anyhow::bail!("response payload exceeds reservation")
            }
            Err(_) => {
                self.response.take();
                anyhow::bail!("bounded response interrupted")
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tls_material_is_bounded_and_cannot_be_attached_to_cleartext() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().canonicalize()?.join("ca.pem");
        std::fs::write(&path, b"invalid CA")?;
        let tls = TlsConfig {
            ca_file: Some(path.clone()),
            identity_file: None,
        };
        assert!(tls.client(Duration::from_secs(1), false).is_err());
        assert!(tls.client(Duration::from_secs(1), true).is_err());
        std::fs::write(&path, vec![0u8; 64 * 1024 + 1])?;
        assert!(tls.client(Duration::from_secs(1), true).is_err());
        Ok(())
    }
    #[test]
    #[cfg(unix)]
    fn private_material_rejects_fifo_without_waiting_for_a_writer() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir()?;
        let directory = temporary.path().canonicalize()?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let fifo = directory.join("identity.pem");
        ensure!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()?
                .success(),
            "FIFO fixture creation failed"
        );
        std::fs::set_permissions(&fifo, std::fs::Permissions::from_mode(0o600))?;
        assert!(read_private_file(&fifo).is_err());
        assert!(TlsConfig {
            ca_file: Some(fifo),
            identity_file: None
        }
        .client(Duration::from_secs(1), true)
        .is_err());
        Ok(())
    }
    async fn response(raw: &'static [u8]) -> Result<reqwest::Response> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(raw).await.unwrap();
        });
        Ok(TlsConfig::default()
            .client(Duration::from_secs(2), false)?
            .get(format!("http://{address}/"))
            .send()
            .await?)
    }
    #[tokio::test]
    async fn accepted_payload_rejects_declared_and_chunked_overflow() -> Result<()> {
        let declared =
            response(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678")
                .await?;
        assert!(BoundedBody::new(declared, 4).is_err());
        let chunked=response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n12345678\r\n0\r\n\r\n").await?;
        let mut body = BoundedBody::new(chunked, 4)?;
        assert!(body.chunk().await.is_err());
        assert!(body.chunk().await?.is_none());
        let valid =
            response(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n1234")
                .await?;
        let mut body = BoundedBody::new(valid, 4)?;
        assert_eq!(body.chunk().await?, Some(b"1234".to_vec()));
        assert!(body.chunk().await?.is_none());
        Ok(())
    }
    #[tokio::test]
    async fn publication_client_does_not_redirect_or_retry_status_failures() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for code in ["307 Temporary Redirect", "503 Service Unavailable"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let target = format!("http://{address}/unexpected");
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(std::str::from_utf8(&buffer[..n])
                    .unwrap()
                    .starts_with("GET /initial "));
                socket.write_all(format!("HTTP/1.1 {code}\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                drop(socket);
                assert!(
                    tokio::time::timeout(Duration::from_millis(150), listener.accept())
                        .await
                        .is_err()
                );
            });
            let result = TlsConfig::default()
                .client(Duration::from_secs(2), false)?
                .get(format!("http://{address}/initial"))
                .send()
                .await?;
            assert_eq!(
                result.status().as_u16(),
                if code.starts_with("307") { 307 } else { 503 }
            );
            task.await?;
        }
        Ok(())
    }
}
