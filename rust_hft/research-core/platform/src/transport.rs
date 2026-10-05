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
}
