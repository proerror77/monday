//! Loopback-only transport of the existing genuine native worker test outputs.
//! The fixture signer is synthetic. It grants no source budget or cloud launch.
use super::*;
use anyhow::ensure;
use base64::{engine::general_purpose::STANDARD, Engine};
use hft_research_platform::{
    admission::{sign, NativeAdmission, NativeAdmissionTrust},
    execution::{Backend, Profile},
    orchestrator::{worker_configuration_reference, Admission, Lease, TaskSpec},
    research::Run,
};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

fn openssl(directory: &Path, args: &[&str]) -> anyhow::Result<()> {
    ensure!(
        Command::new("openssl")
            .args(args)
            .current_dir(directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "loopback TLS fixture generation failed"
    );
    Ok(())
}

fn certificates(directory: &Path) -> anyhow::Result<()> {
    openssl(
        directory,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "1",
            "-subj",
            "/CN=native-test-ca",
        ],
    )?;
    for (name, extension) in [
        (
            "server",
            "subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n",
        ),
        ("client", "extendedKeyUsage=clientAuth\n"),
    ] {
        std::fs::write(directory.join(format!("{name}.ext")), extension)?;
        openssl(
            directory,
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
                &format!("/CN=native-test-{name}"),
            ],
        )?;
        openssl(
            directory,
            &[
                "x509",
                "-req",
                "-in",
                &format!("{name}.csr"),
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                &format!("{name}.pem"),
                "-days",
                "1",
                "-extfile",
                &format!("{name}.ext"),
            ],
        )?;
    }
    Ok(())
}

struct Gateway {
    endpoint: String,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    corrupt_get: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    address: std::net::SocketAddr,
    prefix: Arc<Mutex<String>>,
}
impl Gateway {
    fn start(directory: &Path, prefix: String) -> anyhow::Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from_pem_file(directory.join("ca.pem"))?)?;
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()?;
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![CertificateDer::from_pem_file(directory.join("server.pem"))?],
                PrivateKeyDer::from_pem_file(directory.join("server.key"))?,
            )?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let objects = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let corrupt_get = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let prefix = Arc::new(Mutex::new(prefix));
        let server_objects = objects.clone();
        let server_corruption = corrupt_get.clone();
        let server_stop = stop.clone();
        let server_prefix = prefix.clone();
        let thread = std::thread::spawn(move || -> anyhow::Result<()> {
            let config = Arc::new(config);
            for stream in listener.incoming() {
                if server_stop.load(Ordering::SeqCst) {
                    break;
                }
                let stream = stream?;
                stream.set_read_timeout(Some(Duration::from_secs(30)))?;
                stream.set_write_timeout(Some(Duration::from_secs(30)))?;
                let connection = rustls::ServerConnection::new(config.clone())?;
                let mut reader = BufReader::new(rustls::StreamOwned::new(connection, stream));
                let mut line = String::new();
                reader.read_line(&mut line)?;
                let parts = line.split_whitespace().collect::<Vec<_>>();
                ensure!(parts.len() == 3, "invalid loopback HTTP request");
                let method = parts[0].to_owned();
                let key = parts[1].trim_start_matches('/').to_owned();
                let mut headers = BTreeMap::new();
                loop {
                    line.clear();
                    ensure!(reader.read_line(&mut line)? > 0, "truncated HTTP headers");
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.split_once(':').context("invalid HTTP header")?;
                    headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                }
                ensure!(
                    key.starts_with(server_prefix.lock().unwrap().as_str()),
                    "foreign Attempt output path"
                );
                ensure!(
                    headers.get("authorization").map(String::as_str)
                        == Some("Bearer synthetic-attempt-token"),
                    "foreign gateway token"
                );
                let (status, mut body) = match method.as_str() {
                    "PUT" => {
                        ensure!(
                            headers.get("if-none-match").map(String::as_str) == Some("*"),
                            "mutable gateway upload"
                        );
                        let length = headers
                            .get("content-length")
                            .context("missing upload length")?
                            .parse::<usize>()?;
                        ensure!(
                            length > 0 && length <= 512 * 1024 * 1024,
                            "unbounded upload"
                        );
                        let mut bytes = vec![0; length];
                        reader.read_exact(&mut bytes)?;
                        let mut objects = server_objects.lock().unwrap();
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            objects.entry(key)
                        {
                            entry.insert(bytes);
                            (201, Vec::new())
                        } else {
                            (412, Vec::new())
                        }
                    }
                    "GET" => (
                        200,
                        server_objects
                            .lock()
                            .unwrap()
                            .get(&key)
                            .context("missing immutable object")?
                            .clone(),
                    ),
                    _ => bail!("unsupported loopback method"),
                };
                if method == "GET" && server_corruption.load(Ordering::SeqCst) {
                    body[0] ^= 1;
                }
                let stream = reader.get_mut();
                write!(
                    stream,
                    "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )?;
                stream.write_all(&body)?;
                stream.flush()?;
            }
            Ok(())
        });
        Ok(Self {
            endpoint: format!("https://{address}/"),
            objects,
            corrupt_get,
            stop,
            thread: Some(thread),
            address,
            prefix,
        })
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn prepared_output(
    loaded: &LoadedRequest,
    native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
) -> anyhow::Result<(tempfile::TempDir, Gateway, BoundOutput)> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let root_path = root.path().canonicalize()?;
    certificates(&root_path)?;
    let configuration = root_path.join("config");
    let identity = root_path.join("identity");
    for path in [&configuration, &identity] {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let install = |path: &Path, bytes: &[u8]| -> anyhow::Result<()> {
        std::fs::write(path, bytes)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[37; 32]);
    let trust = NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: BTreeMap::from([(
            "synthetic".into(),
            hex::encode(key.verifying_key().as_bytes()),
        )]),
    };
    let request_bytes = serialize_request(&loaded.request)?;
    install(&configuration.join("campaign.json"), &request_bytes)?;
    install(
        &configuration.join("ca.pem"),
        &std::fs::read(root_path.join("ca.pem"))?,
    )?;
    install(
        &configuration.join("native-trust.json"),
        &serde_json::to_vec(&trust)?,
    )?;
    let mut client_pem = std::fs::read(root_path.join("client.pem"))?;
    client_pem.extend(std::fs::read(root_path.join("client.key"))?);
    install(&identity.join("tls.pem"), &client_pem)?;
    install(&identity.join("artifact.token"), b"synthetic-attempt-token")?;
    // Bind the static endpoint before computing Task.id; credentials remain late.
    let gateway = Gateway::start(&root_path, "research/native-transport/".into())?;
    install(
        &configuration.join("artifact-io.json"),
        &serde_json::to_vec(&serde_json::json!({
            "schema_version":"monday.cex_campaign_artifact_io.v1",
            "artifact_gateway":gateway.endpoint,
            "artifact_token_file":"/identity/artifact.token",
            "artifact_tls":{"ca_file":"/config/ca.pem","identity_file":"/identity/tls.pem"}
        }))?,
    )?;
    let encoded = [
        "campaign.json",
        "artifact-io.json",
        "ca.pem",
        "native-trust.json",
    ]
    .into_iter()
    .map(|name| {
        Ok((
            name.into(),
            STANDARD.encode(std::fs::read(configuration.join(name))?),
        ))
    })
    .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let image = format!("fixture/native@sha256:{}", loaded.request.image_identity);
    let command = vec![
        "/usr/local/bin/monday-cex-worker".into(),
        "mission".into(),
        "campaign-execute".into(),
        "--request".into(),
        "/config/campaign.json".into(),
        "--request-sha256".into(),
        loaded.sha256.clone(),
        "--pre-holdout".into(),
        "--campaign-id".into(),
        loaded.request.campaign_id.clone(),
        "--image-identity".into(),
        loaded.request.image_identity.clone(),
        "--work-dir".into(),
        "/work/native".into(),
    ];
    <crate::cli::WorkerCli as clap::Parser>::try_parse_from(command.iter())?;
    let h = |c: char| c.to_string().repeat(64);
    let run = Run {
        schema: 1,
        experiment_sha256: h('a'),
        kind: TaskKind::CexCampaign,
        build_artifact_sha256: h('b'),
        configuration_sha256: loaded.sha256.clone(),
        command: command.clone(),
        code_commit: loaded.request.build_source_revision.clone(),
        source_manifest_sha256: h('c'),
        image: image.clone(),
        data_manifest_sha256: native.collection_id().into(),
        seed: 7,
        evaluator_sha256: h('d'),
        evaluation_protocol_sha256: native.evaluation_protocol_sha256().into(),
        fit_identity_sha256: None,
    };
    let spec = TaskSpec {
        schema: 1,
        kind: run.kind,
        run_manifest_sha256: run.id()?,
        view_manifest_sha256: run.data_manifest_sha256.clone(),
        source_sha256: run.source_manifest_sha256.clone(),
        image,
        command,
        profile: Profile {
            backend: Backend::KubernetesJob,
            cluster: "fixture".into(),
            namespace: "research".into(),
            service_account: "native-worker".into(),
            architecture: "amd64".into(),
            cpu_millis: 1000,
            memory_mib: 512,
            scratch_mib: 128,
            gpu: 0,
            acceptance_sha256: h('e'),
            prepared_pvc: None,
            worker_secret: Some("native-config".into()),
        },
        timeout_ms: 60_000,
        max_attempts: 1,
        output_prefix: "research/native-transport".into(),
        fit_identity_sha256: None,
        worker_configuration: Some(worker_configuration_reference(
            "research",
            "native-config",
            "fixture-uid",
            &encoded,
        )?),
    };
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    let context = AttemptContext {
        lease: Lease {
            task_id: spec.id()?,
            attempt: 1,
            fence: 17,
            owner: "controlled-fixture".into(),
            expires_ms: now + 600_000,
        },
        spec: spec.clone(),
    };
    *gateway.prefix.lock().unwrap() = context.output_prefix();
    let evidence = NativeAdmission {
        schema: "monday.native_scientific_admission.v1".into(),
        tenant: "fixture".into(),
        run,
        admission: Admission {
            schema: 1,
            request_sha256: spec.id()?,
            task_spec: spec,
            resource_reservation_receipt_sha256: h('a'),
            scientific_grant_receipt_sha256: h('b'),
            release_admission_receipt_sha256: h('c'),
            max_attempts: 1,
        },
        operation_sha256: h('d'),
        native_request_sha256: loaded.sha256.clone(),
        family_id: "fixture".into(),
        root_grant_sha256: h('e'),
        approval_sha256: h('f'),
        transfer_receipt_sha256: h('a'),
        declared_trials: native.declared_trials() as u64,
        reserved_job_seconds: 60,
        reserved_llm_tokens: 0,
        issued_ms: now - 1000,
        expires_ms: now + 600_000,
    };
    let signed = sign(evidence.clone(), "synthetic".into(), &key)?;
    let signed_path = identity.join("native-admission.json");
    install(&signed_path, &serde_json::to_vec(&signed)?)?;
    let output =
        BoundOutput::from_context_at(loaded, native, context.clone(), &configuration, &identity)?;
    let mut expired = context.clone();
    expired.lease.expires_ms = now - 1;
    assert!(
        BoundOutput::from_context_at(loaded, native, expired, &configuration, &identity).is_err()
    );
    let mut changed = signed.clone();
    changed.evidence.declared_trials += 1;
    install(&signed_path, &serde_json::to_vec(&changed)?)?;
    assert!(BoundOutput::from_context_at(
        loaded,
        native,
        context.clone(),
        &configuration,
        &identity
    )
    .is_err());
    let mut changed = evidence;
    changed.run.evaluation_protocol_sha256 = h('f');
    changed.admission.task_spec.run_manifest_sha256 = changed.run.id()?;
    changed.admission.request_sha256 = changed.admission.task_spec.id()?;
    let changed_context = AttemptContext {
        spec: changed.admission.task_spec.clone(),
        lease: Lease {
            task_id: changed.admission.request_sha256.clone(),
            ..context.lease.clone()
        },
    };
    install(
        &signed_path,
        &serde_json::to_vec(&sign(changed, "synthetic".into(), &key)?)?,
    )?;
    assert!(BoundOutput::from_context_at(
        loaded,
        native,
        changed_context,
        &configuration,
        &identity
    )
    .is_err());
    install(&signed_path, &serde_json::to_vec(&signed)?)?;
    install(&configuration.join("campaign.json"), b"{}").unwrap();
    assert!(BoundOutput::from_context_at(
        loaded,
        native,
        context.clone(),
        &configuration,
        &identity
    )
    .is_err());
    install(&configuration.join("campaign.json"), &request_bytes)?;
    Ok((root, gateway, output))
}

#[test]
fn native_attempt_transport_requires_exact_private_admitted_binding() -> anyhow::Result<()> {
    let fixture = crate::mission_campaign::tests::native_prepared_fixture_for_tests();
    let loaded = LoadedRequest {
        request: fixture.request.clone(),
        sha256: fixture.inputs.request_sha256().into(),
    };
    let (_root, gateway, output) = prepared_output(&loaded, &fixture.inputs)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let artifact = runtime.block_on(output.writer.put(
        "transport-probe.json",
        b"actual private transport bytes".to_vec(),
    ))?;
    assert_eq!(
        artifact.key,
        format!("{}transport-probe.json", output.context.output_prefix())
    );
    assert_eq!(
        gateway.objects.lock().unwrap().get(&artifact.key).unwrap(),
        b"actual private transport bytes"
    );
    assert_eq!(
        runtime.block_on(output.writer.put(
            "transport-probe.json",
            b"actual private transport bytes".to_vec(),
        ))?,
        artifact
    );
    gateway.corrupt_get.store(true, Ordering::SeqCst);
    assert!(runtime.block_on(output.writer.readback(&artifact)).is_err());
    Ok(())
}

pub(crate) fn assert_publication(
    loaded: &LoadedRequest,
    native: &prepared_inputs::VerifiedNativeCampaignPreparedInputs,
    result: &CampaignResultV1,
    sha: &str,
    directory: &Path,
) -> anyhow::Result<()> {
    let (_root, gateway, output) = prepared_output(loaded, native)?;
    let context = &output.context;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let receipt = runtime.block_on(publish(
        context,
        &output.writer,
        loaded,
        native,
        result,
        sha,
        directory,
    ))?;
    // An ambiguous immutable PUT must read back the existing original bytes.
    let repeated = runtime.block_on(output.writer.put_file(
        "native-campaign-result.json",
        &directory.join("campaign-result-readback.json"),
        sha,
    ))?;
    assert_eq!(repeated, receipt.artifacts[0]);
    let objects = gateway.objects.lock().unwrap();
    let published: ResultReceipt = serde_json::from_slice(
        objects
            .get(&format!("{}receipt.json", context.output_prefix()))
            .unwrap(),
    )?;
    assert_eq!(published, receipt);
    published.validate(&context.spec, &context.lease)?;
    for artifact in &published.artifacts {
        let bytes = objects.get(&artifact.key).unwrap();
        assert_eq!(artifact.sha256, hft_cex_research_input::sha256(bytes));
        assert_eq!(artifact.bytes, bytes.len() as u64);
    }
    let scientific: CexCampaignResultReceipt = serde_json::from_slice(
        objects
            .get(&format!("{}cex-campaign.json", context.output_prefix()))
            .unwrap(),
    )?;
    scientific.validate(&context.spec, &published.artifacts)?;
    assert_eq!(
        scientific.scientific_status,
        ScientificStatus::InsufficientEvidence
    );
    assert_eq!(scientific.native_request_sha256, loaded.sha256);
    assert_eq!(scientific.collection_sha256, native.collection_id());
    assert_eq!(scientific.rounds.len(), result.rounds.len());
    for round in &scientific.rounds {
        let readback = _root
            .path()
            .join(format!("readback-{}.zip", round.round_id));
        std::fs::write(
            &readback,
            objects.get(&round.result_zip.artifact.key).unwrap(),
        )?;
        assert_eq!(actual_archive_entries(&readback)?, round.entries);
    }
    drop(objects);
    gateway.corrupt_get.store(true, Ordering::SeqCst);
    assert!(runtime
        .block_on(output.writer.readback(&published.artifacts[0]))
        .is_err());
    Ok(())
}
