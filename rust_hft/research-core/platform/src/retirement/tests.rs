use super::*;
use crate::{
    admission::{NativeAdmission, NativeAdmissionTrust},
    execution::{AttemptIdentityRef, Profile},
    orchestrator::{Admission, Task, TaskSpec, WorkerConfigurationRef},
    research::{Run, TerminalLedgerEvent},
    terminal_audit::{
        sign_terminal_audit, NativeTerminalAuditWitness, NATIVE_TERMINAL_AUDIT_SCHEMA,
    },
};
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tokio::sync::Mutex;
fn h(c: char) -> String {
    c.to_string().repeat(64)
}

// Synthetic mechanical receiver fixtures. No real Source grant, dataset,
// publication, scientific result, provider or key is represented by these bytes.
fn collection() -> Result<Value> {
    use hft_cex_research_input::data::{
        BlockRef, DataViewSpec, Exit, PublishedView, Split, Window,
    };
    let view = |exit: Exit, end: i64| -> Result<Value> {
        let manifest = PublishedView {
            prepared_id: h('a'),
            spec: DataViewSpec {
                schema: 1,
                venue: "binance".into(),
                instrument: "BTCUSDT".into(),
                market: "spot".into(),
                depth: 5,
                sources: vec![h('b')],
                normalizer_sha256: h('c'),
                feature_sql_sha256: h('d'),
                feature_names: vec!["mid_price".into()],
                window: Window {
                    start_ns: 100,
                    end_ns: end,
                },
                lookback_ns: 0,
                horizons_ns: vec![100],
                label_tolerance_ns: 0,
                fit_cutoff_ns: end,
                split: Split::Validation,
            },
            blocks: vec![BlockRef {
                sha256: h('e'),
                bytes: 16,
                rows: 1,
                decoded_bytes: 128,
                exit,
            }],
            producer_image: format!("fixture@sha256:{}", h('f')),
            source_receipt_sha256: h('c'),
        };
        Ok(json!({"manifest_sha256":identity(&manifest)?,"manifest":manifest}))
    };
    let value = json!({"schema_version":"monday.cex_campaign_prepared_inputs.v1","source":{"build":{"source_revision":"a".repeat(40),"image_identity":format!("fixture@sha256:{}",h('f'))},"preparation_run_id":"fixture","preparation_receipt_sha256":h('c'),"feature_sha256":h('b'),"materialization_sha256":h('d'),"replay_artifact_sha256":h('e'),"replay_manifest_sha256":h('f')},"original":{"total_rows":3,"original_window":{"start_ns":100,"end_ns":500},"original_rows_sha256":h('a'),"protocol_json":"{}","protocol_sha256":sha256(b"{}"),"search_rows":{"start":0,"end":1},"visible_rows":{"start":0,"end":1},"development_window":{"start_ns":100,"end_ns":200},"authorized_context_end_ns":300,"selection":null,"holdout":{"original_rows":{"start":1,"end":3},"window":{"start_ns":300,"end_ns":500},"source_content_sha256":h('b')}},"label_recipe":hft_cex_research_input::campaign::NATIVE_LABEL_RECIPE,"anchors":[{"ordinal":0,"series_id":1,"segment":"fixture","observed_at_ns":100,"future_at_ns":200,"mature_at_ns":200,"signal":0.0,"fee_bps":0.0,"funding_bps":0.0,"pit_funding":false,"latency_bps":0.0}],"development_rows_sha256":h('c'),"replay_rows_sha256":h('d'),"features":view(Exit::Features,200)?,"future_marks":view(Exit::Features,300)?,"replay":view(Exit::Replay,300)?});
    let typed: hft_cex_research_input::campaign::CampaignPreparedInputsV1 =
        serde_json::from_value(value)?;
    typed.id()?;
    Ok(serde_json::to_value(typed)?)
}
struct Fixture {
    snapshot: NativeTerminalSnapshot,
    collection: Value,
    acceptance: Acceptance,
    job: Value,
    pod: Value,
    objects: BTreeMap<String, Vec<u8>>,
    witness: SignedNativeTerminalAuditWitness,
}
impl Fixture {
    fn scope(&self) -> Result<Scope> {
        Scope::new(
            self.snapshot.clone(),
            self.collection.clone(),
            self.acceptance.clone(),
        )
    }
}
fn fixture() -> Result<Fixture> {
    let collection = collection()?;
    let typed: hft_cex_research_input::campaign::CampaignPreparedInputsV1 =
        serde_json::from_value(collection.clone())?;
    let command = vec![
        "/app/worker".into(),
        "mission".into(),
        "campaign-execute".into(),
        "--pre-holdout".into(),
        "--request-sha256".into(),
        h('1'),
    ];
    let profile = Profile {
        backend: Backend::KubernetesJob,
        cluster: "fixture".into(),
        namespace: "research".into(),
        service_account: "worker".into(),
        architecture: "amd64".into(),
        cpu_millis: 1000,
        memory_mib: 128,
        scratch_mib: 64,
        gpu: 0,
        acceptance_sha256: h('e'),
        prepared_pvc: None,
        worker_secret: Some("configuration".into()),
    };
    let run = Run {
        schema: 1,
        experiment_sha256: h('a'),
        kind: TaskKind::CexCampaign,
        build_artifact_sha256: h('b'),
        configuration_sha256: h('1'),
        command: command.clone(),
        code_commit: "a".repeat(40),
        source_manifest_sha256: h('c'),
        image: format!("fixture@sha256:{}", h('f')),
        data_manifest_sha256: typed.id()?,
        seed: 1,
        evaluator_sha256: h('d'),
        evaluation_protocol_sha256: typed.original.protocol_sha256.clone(),
        fit_identity_sha256: None,
    };
    let spec = TaskSpec {
        schema: 1,
        kind: TaskKind::CexCampaign,
        run_manifest_sha256: run.id()?,
        view_manifest_sha256: typed.id()?,
        source_sha256: run.source_manifest_sha256.clone(),
        image: run.image.clone(),
        command,
        profile: profile.clone(),
        timeout_ms: 10000,
        max_attempts: 1,
        output_prefix: "research/results".into(),
        fit_identity_sha256: None,
        worker_configuration: Some(WorkerConfigurationRef {
            schema: "monday.worker_configuration.v1".into(),
            secret_name: "configuration".into(),
            secret_uid: "static-uid".into(),
            configuration_sha256: h('2'),
        }),
    };
    let id = spec.id()?;
    let transfer = json!({"operation_id":"fixture:original-operation","tenant":"fixture","run_sha256":run.id()?,"request_sha256":id});
    let source_transfer = json!({"receipt":{"schema_version":"monday.campaign_ledger_receipt.v1","family_id":"fixture.family:one","sequence":1,"recorded_at":"2026-10-05T00:00:00Z","event":{"kind":"platform_transferred","transfer":transfer}},"content_sha256":h('a'),"auth_tag":h('b')});
    let transfer_bytes = serde_json::to_vec(&source_transfer)?;
    let native = NativeAdmission {
        schema: "monday.native_scientific_admission.v1".into(),
        tenant: "fixture".into(),
        run: run.clone(),
        admission: Admission {
            schema: 1,
            request_sha256: id.clone(),
            task_spec: spec.clone(),
            resource_reservation_receipt_sha256: h('a'),
            scientific_grant_receipt_sha256: h('b'),
            release_admission_receipt_sha256: h('c'),
            max_attempts: 1,
        },
        operation_sha256: identity(&"fixture:original-operation")?,
        native_request_sha256: h('1'),
        family_id: "fixture.family:one".into(),
        root_grant_sha256: h('d'),
        approval_sha256: h('e'),
        transfer_receipt_sha256: sha256(&transfer_bytes),
        declared_trials: 1,
        reserved_job_seconds: 10,
        reserved_llm_tokens: 0,
        issued_ms: 1,
        expires_ms: 20000,
    };
    let key = SigningKey::from_bytes(&[71; 32]);
    let signed = crate::admission::sign(native, "fixture".into(), &key)?;
    let trust = NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: [(
            "fixture".into(),
            key.verifying_key()
                .to_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        )]
        .into(),
    };
    trust.verify(&signed)?;
    let mut task = Task::new(spec.clone())?;
    let lease = task.claim("owner", 1000, 10000)?;
    let handle = ExecutionHandle {
        backend: profile.backend,
        cluster: profile.cluster.clone(),
        namespace: profile.namespace.clone(),
        name: crate::execution::resource_name(&lease),
        uid: "original-job".into(),
        attempt: lease.attempt,
        fence: lease.fence,
        task_id: id.clone(),
        request_sha256: id.clone(),
    };
    let reference = AttemptIdentityRef {
        secret_name: format!("{}-identity", handle.name),
        secret_uid: "original-secret".into(),
        scope_sha256: h('a'),
        native_evidence_sha256: signed.evidence_sha256.clone(),
        data_sha256: h('b'),
        attempt: lease.attempt,
        fence: lease.fence,
        deadline_ms: 11000,
        launch_lease: lease.clone(),
    };
    task.attempt_identity = Some(reference.clone());
    task.launched(&lease, 1000, handle.clone())?;
    let executed = task.clone();
    task.stop(State::Cancelled, false)?;
    task.stopped(lease.attempt, lease.fence)?;
    let snapshot = NativeTerminalSnapshot {
        schema: "monday.native_platform_terminal_snapshot.v1".into(),
        tenant: "fixture".into(),
        task: task.clone(),
        run,
        native_admission: signed,
        native_trust: trust,
        terminal_revision: 3,
        terminal_event: TerminalLedgerEvent {
            revision: 3,
            event: "stop_reconciled".into(),
            document: task,
        },
        execution_event: Some(TerminalLedgerEvent {
            revision: 1,
            event: "launched".into(),
            document: executed,
        }),
        result: None,
    };
    let acceptance = Acceptance {
        profile,
        ready: true,
        process_tree_stop: true,
        immutable_prepared_mount: false,
        command_reattach: false,
        artifact_readback: true,
    };
    let mut job = crate::execution::render(&spec, &lease, &acceptance)?;
    // Same exact late identity projection as the real launcher, no secret bytes.
    for pointer in [
        "/metadata/annotations",
        "/spec/template/metadata/annotations",
    ] {
        let v = job.pointer_mut(pointer).unwrap();
        v["monday.io/identity-secret-uid"] = json!(reference.secret_uid);
        v["monday.io/identity-scope-sha256"] = json!(reference.scope_sha256);
    }
    job["spec"]["template"]["spec"]["volumes"].as_array_mut().unwrap().push(json!({"name":"identity-inputs","secret":{"secretName":reference.secret_name,"defaultMode":288}}));
    job["spec"]["template"]["spec"]["initContainers"][0]["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"identity-inputs","mountPath":"/identity-inputs","readOnly":true}));
    job["metadata"]["uid"] = json!(handle.uid);
    job["status"] = json!({"active":0,"failed":1,"conditions":[{"type":"Failed","status":"True"}]});
    // Representative API defaults/quantity normalization. The archive and
    // fresh loopback readback use these actual objects, not untouched render.
    let pod_spec = &mut job["spec"]["template"]["spec"];
    pod_spec["securityContext"]["fsGroupChangePolicy"] = json!("Always");
    pod_spec["securityContext"]["supplementalGroupsPolicy"] = json!("Merge");
    for volume in pod_spec["volumes"].as_array_mut().unwrap() {
        if let Some(secret) = volume.get_mut("secret") {
            secret["optional"] = json!(false);
        }
        if let Some(empty) = volume.get_mut("emptyDir") {
            let mib = empty["sizeLimit"]
                .as_str()
                .unwrap()
                .strip_suffix("Mi")
                .unwrap()
                .parse::<u64>()?;
            empty["sizeLimit"] = json!((mib * 1024 * 1024).to_string());
            empty["medium"] = json!("");
        }
    }
    for section in ["containers", "initContainers"] {
        for container in pod_spec[section].as_array_mut().unwrap() {
            container["imagePullPolicy"] = json!("IfNotPresent");
            container["terminationMessagePath"] = json!("/dev/termination-log");
            container["terminationMessagePolicy"] = json!("File");
            container["securityContext"]["privileged"] = json!(false);
            container["securityContext"]["procMount"] = json!("Default");
            container["securityContext"]["capabilities"]["add"] = json!([]);
            for resources in ["requests", "limits"] {
                container["resources"][resources]["cpu"] = json!("1");
                container["resources"][resources]["memory"] = json!("134217728");
            }
            for mount in container["volumeMounts"].as_array_mut().unwrap() {
                if mount.get("readOnly").is_none() {
                    mount["readOnly"] = json!(false);
                }
                mount["mountPropagation"] = json!("None");
            }
        }
    }
    let mut metadata = job["spec"]["template"]["metadata"].clone();
    metadata["uid"] = json!("original-pod");
    metadata["name"] = json!("original-pod-name");
    metadata["namespace"] = json!("research");
    metadata["ownerReferences"] =
        json!([{"kind":"Job","name":handle.name,"uid":handle.uid,"controller":true}]);
    let pod = json!({"kind":"Pod","apiVersion":"v1","metadata":metadata,"spec":job["spec"]["template"]["spec"],"status":{"phase":"Failed","containerStatuses":[{"name":"worker","restartCount":0,"state":{"terminated":{"exitCode":1}}}],"initContainerStatuses":[{"name":"stage-configuration","restartCount":0,"state":{"terminated":{"exitCode":0}}}]}});
    let files = BTreeMap::from([
        (
            "platform-snapshot.json".to_owned(),
            serde_json::to_vec_pretty(&snapshot)?,
        ),
        ("job.json".into(), serde_json::to_vec_pretty(&job)?),
        ("pod.json".into(), serde_json::to_vec_pretty(&pod)?),
        (
            "prepared-inputs.json".into(),
            serde_json::to_vec_pretty(&collection)?,
        ),
        ("source-transfer.json".into(), transfer_bytes),
    ]);
    let identities: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name, json!({"sha256":sha256(bytes),"bytes":bytes.len()})))
        .collect();
    let manifest = serde_json::to_vec_pretty(
        &json!({"schema":"monday.native_terminal_observation_files.v1","files":identities}),
    )?;
    let audit = json!({"schema_version":"monday.campaign_platform_terminal_audit.v1","transfer":source_transfer["receipt"]["event"]["transfer"],"platform_state":"cancelled","scientific_status":"unknown","charging_trials":1,"known_scientific_consumption":null,"retained_manifest_sha256":sha256(&manifest),"platform_snapshot_sha256":sha256(&files["platform-snapshot.json"]),"observer_release_sha256":h('a'),"native_admission_sha256":identity(&snapshot.native_admission)?,"native_trust_sha256":identity(&snapshot.native_trust)?,"collection_sha256":spec.view_manifest_sha256,"task_id":id,"attempt":lease.attempt,"fence":lease.fence,"terminal_revision":3,"terminal_event_sha256":identity(&snapshot.terminal_event)?,"execution_event_sha256":identity(&snapshot.execution_event)?,"job_uid":handle.uid,"pod_uid":"original-pod","job_sha256":identity(&job)?,"pod_sha256":identity(&pod)?,"native_result_sha256":null,"observed_at":"2026-10-05T00:00:00Z"});
    let receipt = serde_json::to_vec(
        &json!({"receipt":{"schema_version":"monday.campaign_ledger_receipt.v1","family_id":"fixture.family:one","sequence":2,"recorded_at":"2026-10-05T00:00:00Z","event":{"kind":"platform_settled","audit":audit}},"content_sha256":h('a'),"auth_tag":h('b')}),
    )?;
    let witness = sign_terminal_audit(
        NativeTerminalAuditWitness {
            schema: NATIVE_TERMINAL_AUDIT_SCHEMA.into(),
            tenant: "fixture".into(),
            operation_sha256: snapshot.native_admission.evidence.operation_sha256.clone(),
            request_sha256: id.clone(),
            run_sha256: snapshot.run.id()?,
            native_evidence_sha256: snapshot.native_admission.evidence_sha256.clone(),
            audit_receipt_sha256: sha256(&receipt),
            retained_manifest_sha256: sha256(&manifest),
            task_id: id,
            attempt: lease.attempt,
            fence: lease.fence,
            job_uid: handle.uid,
            pod_uid: "original-pod".into(),
            terminal_revision: 3,
            issued_ms: 1791158400000,
        },
        "fixture".into(),
        &key,
    )?;
    let prefix = format!(
        "research/native-terminal-audits/{}/{}",
        witness.evidence.operation_sha256,
        identity(&audit)?
    );
    let mut objects: BTreeMap<_, _> = files
        .into_iter()
        .map(|(n, b)| (format!("{prefix}/{n}"), b))
        .collect();
    objects.insert(format!("{prefix}/retained-manifest.json"), manifest);
    objects.insert(
        format!("{prefix}/terminal-audit.json"),
        serde_json::to_vec_pretty(&audit)?,
    );
    objects.insert(source_receipt_key("fixture.family:one", 2)?, receipt);
    objects.insert(
        format!(
            "research/native-terminal-audits/{}/signed-{}.json",
            witness.evidence.operation_sha256, witness.evidence_sha256
        ),
        serde_json::to_vec_pretty(&witness)?,
    );
    Ok(Fixture {
        snapshot,
        collection,
        acceptance,
        job,
        pod,
        objects,
        witness,
    })
}

#[derive(Default)]
struct Api {
    objects: BTreeMap<String, Vec<u8>>,
    job: Option<Value>,
    pod: Option<Value>,
    deletes: u32,
    unknown: bool,
}
async fn api(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<Api>>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut s = state.lock().await;
    if method == axum::http::Method::DELETE {
        let options: Value = serde_json::from_slice(&body).unwrap();
        if s.job
            .as_ref()
            .is_none_or(|j| j["metadata"]["uid"] != options["preconditions"]["uid"])
        {
            return axum::http::StatusCode::CONFLICT.into_response();
        }
        assert_eq!(options["propagationPolicy"], "Foreground");
        s.deletes += 1;
        if s.unknown {
            s.unknown = false;
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        s.job = None;
        s.pod = None;
        return axum::http::StatusCode::OK.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    if let Some(v) = s.objects.get(path) {
        return v.clone().into_response();
    }
    if path.starts_with("apis/batch/v1/") {
        return s
            .job
            .as_ref()
            .map(|v| axum::Json(v.clone()).into_response())
            .unwrap_or_else(|| axum::http::StatusCode::NOT_FOUND.into_response());
    }
    if path.ends_with("/pods") {
        return axum::Json(json!({"items":s.pod.iter().collect::<Vec<_>>() })).into_response();
    }
    if path.contains("/pods/") {
        return s
            .pod
            .as_ref()
            .map(|v| axum::Json(v.clone()).into_response())
            .unwrap_or_else(|| axum::http::StatusCode::NOT_FOUND.into_response());
    }
    axum::http::StatusCode::NOT_FOUND.into_response()
}
async fn server(
    f: &Fixture,
) -> Result<(
    Arc<Mutex<Api>>,
    ArtifactGateway,
    Kubernetes,
    tokio::task::JoinHandle<()>,
)> {
    let state = Arc::new(Mutex::new(Api {
        objects: f.objects.clone(),
        job: Some(f.job.clone()),
        pod: Some(f.pod.clone()),
        deletes: 0,
        unknown: false,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new()
        .fallback(axum::routing::any(api))
        .with_state(state.clone());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Ok((
        state,
        ArtifactGateway::retirement_fixture(&format!("http://{address}/"))?,
        Kubernetes::retirement_fixture(&format!("http://{address}/"), "fixture")?,
        handle,
    ))
}
#[test]
fn exact_source_receipt_key_rejects_path_query_and_encoding_variants() -> Result<()> {
    let good = source_receipt_key("fixture.family:one", 2)?;
    assert!(source_receipt_key_valid(&good));
    for bad in [
        good.replace("sequence=00000000000000000002", "sequence=2"),
        format!("{good}?x=1"),
        good.replace("family-id=fixture.family:one", "family-id=a/b"),
        good.replace("family-id=fixture.family:one", "family-id=%2e%2e"),
        good.replace("receipt.json", "other.json"),
        good.replace("00000000000000000002", "00000000000000000000"),
    ] {
        assert!(!source_receipt_key_valid(&bad), "{bad}");
    }
    Ok(())
}
#[tokio::test]
async fn loopback_terminal_archive_rejects_scope_and_actual_bytes_before_permit() -> Result<()> {
    let f = fixture()?;
    let (state, gateway, _, handle) = server(&f).await?;
    readback(&gateway, f.scope()?, &f.witness.evidence_sha256, 2).await?;
    let mut foreign = f.scope()?;
    foreign.snapshot.tenant = "foreign".into();
    assert!(readback(&gateway, foreign, &f.witness.evidence_sha256, 2)
        .await
        .is_err());
    let key = state
        .lock()
        .await
        .objects
        .keys()
        .find(|k| k.ends_with("/pod.json"))
        .unwrap()
        .clone();
    let original = state.lock().await.objects.remove(&key).unwrap();
    assert!(
        readback(&gateway, f.scope()?, &f.witness.evidence_sha256, 2)
            .await
            .is_err()
    );
    state
        .lock()
        .await
        .objects
        .insert(key, [original, b"changed".to_vec()].concat());
    assert!(
        readback(&gateway, f.scope()?, &f.witness.evidence_sha256, 2)
            .await
            .is_err()
    );
    assert_eq!(state.lock().await.deletes, 0);
    handle.abort();
    Ok(())
}
#[test]
fn terminal_provider_rejects_uid_context_resources_and_live_process() -> Result<()> {
    let f = fixture()?;
    let scope = f.scope()?;
    let verify = |job: &Value, pod: &Value| {
        crate::execution::verify_retirement_observation(
            &scope.snapshot.task.spec,
            &scope.lease,
            &scope.handle,
            &scope.acceptance,
            scope.snapshot.task.attempt_identity.as_ref().unwrap(),
            job,
            pod,
        )
    };
    verify(&f.job, &f.pod)?;
    let mut j = f.job.clone();
    j["metadata"]["uid"] = json!("foreign");
    assert!(verify(&j, &f.pod).is_err());
    let mut j = f.job.clone();
    j["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["cpu"] = json!("2000m");
    assert!(verify(&j, &f.pod).is_err());
    let mut j = f.job.clone();
    j["spec"]["template"]["spec"]["containers"][0]["args"] = json!(["--foreign"]);
    assert!(verify(&j, &f.pod).is_err());
    let mut j = f.job.clone();
    j["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["nvidia.com/gpu"] =
        json!("1");
    assert!(verify(&j, &f.pod).is_err());
    let mut j = f.job.clone();
    j["metadata"]["annotations"]["monday.io/identity-secret-uid"] = json!("foreign-secret");
    assert!(verify(&j, &f.pod).is_err());
    let mut j = f.job.clone();
    j["spec"]["template"]["spec"]["initContainers"][0]["command"] = json!(["/app/other"]);
    assert!(verify(&j, &f.pod).is_err());
    let mut j = f.job.clone();
    j["spec"]["template"]["spec"]["containers"][0]["securityContext"]["privileged"] = json!(true);
    assert!(verify(&j, &f.pod).is_err());
    let mut p = f.pod.clone();
    p["metadata"]["ownerReferences"][0]["uid"] = json!("foreign");
    assert!(verify(&f.job, &p).is_err());
    let mut p = f.pod.clone();
    p["status"]["containerStatuses"][0]["state"] = json!({"running":{}});
    assert!(verify(&f.job, &p).is_err());
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable MONDAY_TEST_DATABASE_URL ending /monday_foundation_retirement_test"]
async fn native_retirement_real_pg_archive_delete_unknown_and_recovery() -> Result<()> {
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    ensure!(
        url.ends_with("/monday_foundation_test")
            || url.ends_with("/monday_foundation_retirement_test"),
        "test database identity mismatch"
    );
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    for sql in [
        crate::postgres::MIGRATION,
        crate::postgres::NATIVE_ADMISSION_MIGRATION,
        crate::postgres::NATIVE_CAMPAIGN_INPUTS_MIGRATION,
        MIGRATION,
    ] {
        sqlx_core::raw_sql::raw_sql(sql).execute(&pool).await?;
    }
    let f = fixture()?;
    let snapshot = &f.snapshot;
    let native = &snapshot.native_admission;
    let task = &snapshot.task;
    query("INSERT INTO research.experiments VALUES($1,'fixture','{}')")
        .bind(&snapshot.run.experiment_sha256)
        .execute(&pool)
        .await?;
    query("INSERT INTO research.build_artifacts VALUES($1,$2,'{}')")
        .bind(&snapshot.run.build_artifact_sha256)
        .bind(h('a'))
        .execute(&pool)
        .await?;
    query("INSERT INTO research.runs VALUES($1,$2,$3,'fixture',$4)")
        .bind(snapshot.run.id()?)
        .bind(&snapshot.run.experiment_sha256)
        .bind(&snapshot.run.build_artifact_sha256)
        .bind(serde_json::to_value(&snapshot.run)?)
        .execute(&pool)
        .await?;
    query("INSERT INTO research.inputs VALUES($1,'cex_campaign',$2)")
        .bind(&task.spec.view_manifest_sha256)
        .bind(&f.collection)
        .execute(&pool)
        .await?;
    query("INSERT INTO research.backends VALUES($1,$2,false)")
        .bind(&task.spec.profile.acceptance_sha256)
        .bind(serde_json::to_value(&f.acceptance)?)
        .execute(&pool)
        .await?;
    query("INSERT INTO research.admissions VALUES($1,$2)")
        .bind(&task.id)
        .bind(serde_json::to_value(&native.evidence.admission)?)
        .execute(&pool)
        .await?;
    query("INSERT INTO research.native_admission_imports(request_sha256,tenant,operation_sha256,evidence_sha256,trust_sha256,expires_ms,document,trust_document) VALUES($1,'fixture',$2,$3,$4,$5,$6,$7)")
        .bind(&task.id).bind(&native.evidence.operation_sha256).bind(&native.evidence_sha256).bind(identity(&snapshot.native_trust)?).bind(native.evidence.expires_ms).bind(serde_json::to_value(native)?).bind(serde_json::to_value(&snapshot.native_trust)?).execute(&pool).await?;
    query("INSERT INTO research.native_campaign_inputs(request_sha256,manifest_sha256,tenant,verification_sha256) VALUES($1,$2,'fixture',$3)").bind(&task.id).bind(&task.spec.view_manifest_sha256).bind(h('a')).execute(&pool).await?;
    query("INSERT INTO research.tasks(task_id,tenant,idempotency_key,request_sha256,run_manifest_sha256,view_manifest_sha256,state,document,revision) VALUES($1,'fixture','mechanical-fixture',$1,$2,$3,'cancelled',$4,3)").bind(&task.id).bind(snapshot.run.id()?).bind(&task.spec.view_manifest_sha256).bind(serde_json::to_value(task)?).execute(&pool).await?;
    for event in [
        snapshot.execution_event.as_ref().unwrap(),
        &snapshot.terminal_event,
    ] {
        query("INSERT INTO research.events(task_id,revision,event,document) VALUES($1,$2,$3,$4)")
            .bind(&task.id)
            .bind(event.revision)
            .bind(&event.event)
            .bind(serde_json::to_value(&event.document)?)
            .execute(&pool)
            .await?;
    }
    sqlx_core::raw_sql::raw_sql("CREATE ROLE terminal_retirement_host NOLOGIN; GRANT USAGE ON SCHEMA research TO terminal_retirement_host; GRANT SELECT ON ALL TABLES IN SCHEMA research TO terminal_retirement_host; GRANT INSERT ON research.native_terminal_retirement_audits,research.native_terminal_retirement_events TO terminal_retirement_host; GRANT UPDATE(revision) ON research.tasks TO terminal_retirement_host;").execute(&pool).await?;
    let host_pool = sqlx_postgres::PgPoolOptions::new()
        .after_connect(|connection, _| {
            Box::pin(async move {
                query("SET ROLE terminal_retirement_host")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await?;
    let ledger = Ledger { pool: host_pool };
    let request = RetirementRequest {
        tenant: "fixture".into(),
        task_id: task.id.clone(),
        witness_sha256: f.witness.evidence_sha256.clone(),
        receipt_sequence: 2,
    };
    let (state, gateway, kube, handle) = server(&f).await?;
    assert!(retire(
        &RetirementConfig::default(),
        &ledger,
        &kube,
        &gateway,
        &request
    )
    .await
    .is_err());
    assert_eq!(state.lock().await.deletes, 0);
    let enabled = RetirementConfig { enabled: true };
    let raw_key = state
        .lock()
        .await
        .objects
        .keys()
        .find(|k| k.ends_with("/pod.json"))
        .unwrap()
        .clone();
    let raw = state.lock().await.objects.remove(&raw_key).unwrap();
    assert!(retire(&enabled, &ledger, &kube, &gateway, &request)
        .await
        .is_err());
    let count: i64 =
        query_scalar("SELECT count(*) FROM research.native_terminal_retirement_audits")
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    state.lock().await.objects.insert(raw_key, raw);
    state.lock().await.job.as_mut().unwrap()["metadata"]["uid"] = json!("foreign");
    assert!(retire(&enabled, &ledger, &kube, &gateway, &request)
        .await
        .is_err());
    assert_eq!(state.lock().await.deletes, 0);
    state.lock().await.job = Some(f.job.clone());
    state.lock().await.unknown = true;
    assert_eq!(
        retire(&enabled, &ledger, &kube, &gateway, &request).await?,
        RetirementOutcome::Pending
    );
    let rows: Vec<String> =
        query_scalar("SELECT event FROM research.native_terminal_retirement_events")
            .fetch_all(&pool)
            .await?;
    assert_eq!(rows, vec!["delete_requested"]);
    // Simulate the original DELETE finishing after its reply was lost. The next
    // process keeps the durable original UID; it does not submit a new target.
    state.lock().await.job = None;
    state.lock().await.pod = None;
    assert_eq!(
        retire(&enabled, &ledger, &kube, &gateway, &request).await?,
        RetirementOutcome::Retired
    );
    assert_eq!(
        retire(&enabled, &ledger, &kube, &gateway, &request).await?,
        RetirementOutcome::Retired
    );
    assert_eq!(state.lock().await.deletes, 1);
    let current = ledger.native_terminal_snapshot("fixture", &task.id).await?;
    assert_eq!(current.terminal_revision, 3);
    assert_eq!(current.task, *task);
    let rows: i64 = query_scalar("SELECT count(*) FROM research.native_terminal_retirement_events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rows, 2);
    assert!(
        query("UPDATE research.native_terminal_retirement_audits SET tenant='other'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        query("DELETE FROM research.native_terminal_retirement_events")
            .execute(&pool)
            .await
            .is_err()
    );
    // A replacement with the same name cannot inherit prior retirement.
    let mut replacement = f.job.clone();
    replacement["metadata"]["uid"] = json!("replacement");
    state.lock().await.job = Some(replacement);
    assert!(retire(&enabled, &ledger, &kube, &gateway, &request)
        .await
        .is_err());
    assert_eq!(state.lock().await.deletes, 1);
    sqlx_core::raw_sql::raw_sql("CREATE ROLE terminal_retirement_reader NOLOGIN; GRANT USAGE ON SCHEMA research TO terminal_retirement_reader; GRANT SELECT ON ALL TABLES IN SCHEMA research TO terminal_retirement_reader;").execute(&pool).await?;
    let mut tx = pool.begin().await?;
    query("SET LOCAL ROLE terminal_retirement_reader")
        .execute(&mut *tx)
        .await?;
    assert!(query("INSERT INTO research.native_terminal_retirement_events SELECT evidence_sha256,'retired',document,clock_timestamp() FROM research.native_terminal_retirement_events WHERE event='delete_requested'").execute(&mut *tx).await.is_err());
    tx.rollback().await?;
    handle.abort();
    Ok(())
}
