#![cfg(feature = "control")]
use anyhow::{ensure, Result};
use hft_research_platform::transport::TlsConfig;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

fn openssl(directory: &Path, args: &[&str]) -> Result<()> {
    ensure!(
        Command::new("openssl")
            .args(args)
            .current_dir(directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "test TLS fixture generation failed"
    );
    Ok(())
}
/// Synthetic TLS keys, a loopback-only openssl daemon and no model/cloud call.
#[tokio::test]
async fn private_tls_requires_real_ca_hostname_and_client_identity() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir()?;
    let directory = temporary.path().canonicalize()?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    openssl(
        &directory,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
            "-days",
            "1",
            "-subj",
            "/CN=fixture-private-ca",
        ],
    )?;
    for (name, extension) in [
        (
            "server",
            "subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n",
        ),
        ("client", "extendedKeyUsage=clientAuth\n"),
    ] {
        std::fs::write(directory.join(format!("{name}.ext")), extension)?;
        openssl(
            &directory,
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &format!("{name}.key"),
                "-out",
                &format!("{name}.csr"),
                "-subj",
                &format!("/CN=fixture-{name}"),
            ],
        )?;
        openssl(
            &directory,
            &[
                "x509",
                "-req",
                "-in",
                &format!("{name}.csr"),
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                &format!("{name}.crt"),
                "-days",
                "1",
                "-extfile",
                &format!("{name}.ext"),
            ],
        )?;
    }
    let identity = directory.join("identity.pem");
    let mut pem = std::fs::read(directory.join("client.crt"))?;
    pem.extend(std::fs::read(directory.join("client.key"))?);
    std::fs::write(&identity, pem)?;
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600))?;
    let body = br#"{"synthetic":"private-tls-object"}"#;
    std::fs::write(directory.join("fixture.json"), body)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    drop(listener);
    let mut server = tokio::process::Command::new("openssl")
        .args([
            "s_server",
            "-accept",
            &address.to_string(),
            "-cert",
            "server.crt",
            "-key",
            "server.key",
            "-CAfile",
            "ca.crt",
            "-Verify",
            "1",
            "-WWW",
            "-quiet",
        ])
        .current_dir(&directory)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut ready = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ensure!(ready, "loopback TLS fixture did not start");
    let tls = TlsConfig {
        ca_file: Some(directory.join("ca.crt")),
        identity_file: Some(identity.clone()),
    };
    let client = tls.client(Duration::from_secs(5), true)?;
    let endpoint = format!("https://localhost:{}/fixture.json", address.port());
    ensure!(
        client.get(&endpoint).send().await?.status().is_success(),
        "valid private TLS identity rejected"
    );
    let descriptor = hft_research_platform::block_objects::ObjectDescriptor {
        sha256: hft_research_platform::sha256(body),
        bytes: body.len() as u64,
        url: endpoint.clone(),
    };
    assert_eq!(
        hft_research_platform::block_objects::Objects::with_tls(&tls)?
            .read(&descriptor, 1024)
            .await?,
        body
    );
    assert!(
        client
            .get(format!("https://{address}/"))
            .send()
            .await
            .is_err(),
        "wrong hostname must fail"
    );
    let no_identity = TlsConfig {
        identity_file: None,
        ..tls.clone()
    }
    .client(Duration::from_secs(5), true)?;
    assert!(
        no_identity.get(&endpoint).send().await.is_err(),
        "client certificate is required"
    );
    let no_ca = TlsConfig {
        ca_file: None,
        ..tls.clone()
    }
    .client(Duration::from_secs(5), true)?;
    assert!(
        no_ca.get(&endpoint).send().await.is_err(),
        "untrusted CA must fail"
    );
    assert!(
        hft_research_platform::block_objects::Objects::new()?
            .read(&descriptor, 1024)
            .await
            .is_err(),
        "actual object client must reject the untrusted CA"
    );
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o644))?;
    assert!(
        tls.client(Duration::from_secs(5), true).is_err(),
        "private key must not be public"
    );
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600))?;
    let alias = directory.join("alias.pem");
    std::os::unix::fs::symlink(&identity, &alias)?;
    assert!(TlsConfig {
        identity_file: Some(alias),
        ..tls
    }
    .client(Duration::from_secs(5), true)
    .is_err());
    server.kill().await?;
    server.wait().await?;
    Ok(())
}
