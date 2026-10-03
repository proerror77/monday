use hft_research_platform::{
    data::{
        self, BlockRef, BlockSource, DataViewSpec, Exit, FeatureFrame, PreparationPlan,
        PublishedView, ReplayEvent, ReplayPayload, Split, TrainingFrame, TypedBlock, VerifiedCache,
        Window,
    },
    execution::{self, Acceptance, Backend, ExecutionHandle, Profile},
    identity,
    orchestrator::{Artifact, ResultReceipt, State, Task, TaskKind, TaskSpec},
    prepared, sha256,
};

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}
fn spec() -> DataViewSpec {
    DataViewSpec {
        schema: 1,
        venue: "test-venue".into(),
        instrument: "test-instrument".into(),
        market: "usdm".into(),
        depth: 3,
        sources: vec![hash('a')],
        normalizer_sha256: hash('b'),
        feature_sql_sha256: sha256(data::PREPARE_SQL.as_bytes()),
        feature_names: vec!["mid".into(), "spread".into(), "depth_imbalance".into()],
        window: Window {
            start_ns: 100,
            end_ns: 1000,
        },
        lookback_ns: 50,
        horizons_ns: vec![70, 150],
        label_tolerance_ns: 20,
        fit_cutoff_ns: 1100,
        split: Split::Train,
    }
}
fn frame(clock: i64, ordinal: u64) -> FeatureFrame {
    FeatureFrame {
        segment: "session-a-contiguous-1".into(),
        ordinal,
        event_ns: clock - 1,
        available_ns: clock,
        values: vec![1.0, 2.0, 3.0],
    }
}
fn task_spec() -> TaskSpec {
    TaskSpec {
        schema: 1,
        kind: TaskKind::Train,
        run_manifest_sha256: hash('a'),
        view_manifest_sha256: hash('a'),
        source_sha256: hash('b'),
        image: format!("registry.test/worker@sha256:{}", hash('c')),
        command: vec!["/app/worker".into()],
        profile: Profile {
            backend: Backend::AcsJob,
            cluster: "fixture-cluster".into(),
            namespace: "research".into(),
            service_account: "worker".into(),
            architecture: "amd64".into(),
            cpu_millis: 1000,
            memory_mib: 512,
            scratch_mib: 256,
            gpu: 0,
            acceptance_sha256: hash('d'),
            prepared_pvc: None,
            worker_secret: None,
        },
        timeout_ms: 5000,
        max_attempts: 2,
        output_prefix: "research/results".into(),
        fit_identity_sha256: Some(hash('e')),
    }
}
fn acceptance(task: &TaskSpec) -> Acceptance {
    Acceptance {
        profile: task.profile.clone(),
        ready: true,
        process_tree_stop: true,
        immutable_prepared_mount: false,
        command_reattach: false,
        artifact_readback: true,
    }
}
fn handle(task: &Task, lease: &hft_research_platform::orchestrator::Lease) -> ExecutionHandle {
    ExecutionHandle {
        backend: task.spec.profile.backend,
        cluster: task.spec.profile.cluster.clone(),
        namespace: task.spec.profile.namespace.clone(),
        name: execution::resource_name(lease),
        uid: "fixture-uid".into(),
        attempt: lease.attempt,
        fence: lease.fence,
        task_id: task.id.clone(),
        request_sha256: task.spec.id().unwrap(),
    }
}
fn receipt(task: &Task) -> ResultReceipt {
    ResultReceipt {
        task_id: task.id.clone(),
        attempt: task.attempt,
        fence: task.fence,
        view_manifest_sha256: task.spec.view_manifest_sha256.clone(),
        source_sha256: task.spec.source_sha256.clone(),
        image: task.spec.image.clone(),
        fit_identity_sha256: task.spec.fit_identity_sha256.clone(),
        artifacts: vec![Artifact {
            key: format!(
                "{}/{}/{}/result.bin",
                task.spec.output_prefix, task.id, task.attempt
            ),
            sha256: hash('f'),
            bytes: 1,
        }],
        checkpoint: None,
        prepared_view: None,
    }
}
struct Source {
    bytes: Vec<u8>,
    reads: usize,
}
impl BlockSource for Source {
    fn read(&mut self, _: &BlockRef) -> anyhow::Result<Vec<u8>> {
        self.reads += 1;
        Ok(self.bytes.clone())
    }
}
fn view(block: TypedBlock, exit: Exit) -> (PublishedView, Source) {
    let bytes = prepared::encode(&block).unwrap();
    let decoded = prepared::decode(&bytes).unwrap();
    let rows = match &block {
        TypedBlock::Features(r) => r.len(),
        TypedBlock::Training(r) => r.len(),
        TypedBlock::Replay(r) => r.len(),
    };
    let reference = BlockRef {
        sha256: sha256(&bytes),
        bytes: bytes.len() as u64,
        rows: rows as u64,
        decoded_bytes: data::memory_bytes(&decoded),
        exit,
    };
    (
        PublishedView {
            prepared_id: hash('a'),
            spec: spec(),
            blocks: vec![reference],
            producer_image: format!("registry.test/prepare@sha256:{}", hash('b')),
            source_receipt_sha256: hash('c'),
        },
        Source { bytes, reads: 0 },
    )
}

#[test]
fn horizons_use_time_not_rows_and_respect_availability() {
    let spec = spec();
    let anchor = frame(200, 0);
    let observations = vec![
        (anchor.segment.clone(), 240, 241, 1.0),
        (anchor.segment.clone(), 275, 300, 2.0),
        (anchor.segment.clone(), 355, 370, 3.0),
    ];
    let labels = data::temporal_labels(&anchor, &observations, &spec)
        .unwrap()
        .unwrap();
    assert_eq!(
        labels.iter().map(|l| l.target_event_ns).collect::<Vec<_>>(),
        [275, 355]
    );
    assert_eq!(
        labels.iter().map(|l| l.mature_ns).collect::<Vec<_>>(),
        [300, 370]
    );
    let delayed = vec![
        (anchor.segment.clone(), 275, 1200, 2.0),
        (anchor.segment.clone(), 355, 370, 3.0),
    ];
    assert!(data::temporal_labels(&anchor, &delayed, &spec)
        .unwrap()
        .is_none());
}

#[test]
fn labels_cannot_cross_gaps_sessions_or_splits() {
    let anchor = frame(900, 0);
    let observations = vec![
        (anchor.segment.clone(), 980, 981, 1.0),
        (anchor.segment.clone(), 1055, 1056, 1.0),
    ];
    assert!(data::temporal_labels(&anchor, &observations, &spec())
        .unwrap()
        .is_none());
    let anchor = frame(200, 0);
    let observations = vec![
        ("another-session".into(), 275, 276, 1.0),
        (anchor.segment.clone(), 355, 356, 1.0),
    ];
    assert!(data::temporal_labels(&anchor, &observations, &spec())
        .unwrap()
        .is_none());
    let lookback = frame(75, 0);
    assert!(data::temporal_labels(&lookback, &observations, &spec()).is_err());
}

#[test]
fn version_identity_binds_sources_features_depth_split_and_horizons() {
    let a = spec();
    let mut b = a.clone();
    b.depth += 1;
    assert_ne!(a.id().unwrap(), b.id().unwrap());
    let mut b = a.clone();
    b.horizons_ns.push(300);
    assert_ne!(a.id().unwrap(), b.id().unwrap());
    let mut b = a.clone();
    b.split = Split::Holdout;
    assert_ne!(a.id().unwrap(), b.id().unwrap());
    let mut b = a.clone();
    b.sources.push(hash('c'));
    assert_ne!(a.id().unwrap(), b.id().unwrap());
    let mut b = a.clone();
    b.horizons_ns = vec![70, 70];
    assert!(b.id().is_err());
}

#[test]
fn bounded_batch_is_loaded_once_and_reused_across_trials_and_passes() {
    let (view, mut source) = view(
        TypedBlock::Features(vec![frame(100, 0), frame(123, 1)]),
        Exit::Features,
    );
    let manifest = identity(&view).unwrap();
    let mut cache = VerifiedCache::new(64 * 1024).unwrap();
    let a = cache
        .load(&view, &manifest, Exit::Features, &mut source)
        .unwrap();
    let trial_a = a.clone();
    let trial_b = a.clone();
    assert!(std::sync::Arc::ptr_eq(
        &trial_a.blocks()[0],
        &trial_b.blocks()[0]
    ));
    let b = cache
        .load(&view, &manifest, Exit::Features, &mut source)
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&a.blocks()[0], &b.blocks()[0]));
    assert_eq!(source.reads, 1);
}

#[test]
fn corruption_wrong_manifest_and_decoded_memory_budget_fail_closed() {
    let (view, mut source) = view(TypedBlock::Features(vec![frame(100, 0)]), Exit::Features);
    let manifest = identity(&view).unwrap();
    let mut cache = VerifiedCache::new(64 * 1024).unwrap();
    assert!(cache
        .load(&view, &hash('f'), Exit::Features, &mut source)
        .is_err());
    source.bytes[0] ^= 1;
    assert!(cache
        .load(&view, &manifest, Exit::Features, &mut source)
        .is_err());
    let mut altered = view.clone();
    altered.blocks[0].decoded_bytes = 1;
    assert!(cache
        .load(
            &altered,
            &identity(&altered).unwrap(),
            Exit::Features,
            &mut source
        )
        .is_err());
}

#[test]
fn cached_bytes_do_not_bypass_split_or_clock_admission() {
    let labels = data::temporal_labels(
        &frame(200, 0),
        &[
            (frame(200, 0).segment.clone(), 275, 276, 1.0),
            (frame(200, 0).segment.clone(), 355, 356, 2.0),
        ],
        &spec(),
    )
    .unwrap()
    .unwrap();
    let (view, mut source) = view(
        TypedBlock::Training(vec![TrainingFrame {
            feature: frame(200, 0),
            labels,
        }]),
        Exit::Training,
    );
    let mut cache = VerifiedCache::new(64 * 1024).unwrap();
    cache
        .load(
            &view,
            &identity(&view).unwrap(),
            Exit::Training,
            &mut source,
        )
        .unwrap();
    let mut sealed = view.clone();
    sealed.spec.split = Split::Holdout;
    assert!(cache
        .load(
            &sealed,
            &identity(&sealed).unwrap(),
            Exit::Training,
            &mut source
        )
        .is_err());
    assert_eq!(source.reads, 1);
}

#[test]
fn replay_requires_seed_and_contiguous_order() {
    let event = |ordinal, payload| ReplayEvent {
        segment: "s".into(),
        ordinal,
        event_ns: 99 + ordinal as i64,
        available_ns: 100 + ordinal as i64,
        payload,
    };
    let delta = ReplayPayload::Delta {
        bids: vec![],
        asks: vec![],
    };
    let (unseeded, mut source) = view(
        TypedBlock::Replay(vec![event(0, delta.clone())]),
        Exit::Replay,
    );
    assert!(VerifiedCache::new(64 * 1024)
        .unwrap()
        .load(
            &unseeded,
            &identity(&unseeded).unwrap(),
            Exit::Replay,
            &mut source
        )
        .is_err());
    let seed = ReplayPayload::Snapshot {
        bids: vec![[1.0, 1.0]],
        asks: vec![[2.0, 1.0]],
    };
    let (gapped, mut source) = view(
        TypedBlock::Replay(vec![event(0, seed), event(2, delta)]),
        Exit::Replay,
    );
    assert!(VerifiedCache::new(64 * 1024)
        .unwrap()
        .load(
            &gapped,
            &identity(&gapped).unwrap(),
            Exit::Replay,
            &mut source
        )
        .is_err());
}

#[test]
fn retry_waits_for_stop_and_fences_old_results() {
    let mut task = Task::new(task_spec()).unwrap();
    let first = task.claim("owner-a", 1000, 1000).unwrap();
    task.launched(&first, 1001, handle(&task, &first)).unwrap();
    let stale = receipt(&task);
    assert!(task.expire(2000).unwrap());
    assert_eq!(task.state, State::Stopping);
    assert!(task.claim("owner-b", 2001, 1000).is_err());
    assert!(task.stage_result(&first, 2001, stale.clone()).is_err());
    task.stopped(first.attempt, first.fence).unwrap();
    let second = task.claim("owner-b", 2002, 1000).unwrap();
    assert!(second.fence > first.fence);
    task.launched(&second, 2003, handle(&task, &second))
        .unwrap();
    assert!(task.stage_result(&second, 2004, stale).is_err());
}

#[test]
fn cancellation_supersedes_retry_and_rejects_late_results() {
    let mut task = Task::new(task_spec()).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    task.launched(&lease, 1001, handle(&task, &lease)).unwrap();
    let receipt = receipt(&task);
    task.stop(State::Failed, true).unwrap();
    task.stop(State::Cancelled, false).unwrap();
    assert!(task.stage_result(&lease, 1002, receipt).is_err());
    task.stopped(lease.attempt, lease.fence).unwrap();
    assert_eq!(task.state, State::Cancelled);
    assert!(task.claim("owner", 1003, 1000).is_err());
}

#[test]
fn timeout_preserves_original_deadline_and_has_no_retry() {
    let mut task = Task::new(task_spec()).unwrap();
    let lease = task.claim("owner", 1000, 10000).unwrap();
    task.launched(&lease, 1001, handle(&task, &lease)).unwrap();
    let deadline = task.deadline_ms;
    task.heartbeat(&lease, 2000, 10000).unwrap();
    assert_eq!(task.deadline_ms, deadline);
    assert!(task.expire(6000).unwrap());
    task.stopped(lease.attempt, lease.fence).unwrap();
    assert_eq!(task.state, State::TimedOut);
}

#[test]
fn accepted_result_becomes_terminal_only_after_resource_stop() {
    let mut task = Task::new(task_spec()).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    task.launched(&lease, 1001, handle(&task, &lease)).unwrap();
    let result = receipt(&task);
    task.stage_result(&lease, 1002, result).unwrap();
    assert_eq!(task.state, State::Stopping);
    let restored: Task = serde_json::from_slice(&serde_json::to_vec(&task).unwrap()).unwrap();
    assert_eq!(restored, task);
    task.stopped(lease.attempt, lease.fence).unwrap();
    assert_eq!(task.state, State::Succeeded);
    assert!(task.receipt.is_some());
}

#[test]
fn job_admission_is_pinned_and_resources_are_bounded() {
    let mut task = Task::new(task_spec()).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    let accepted = acceptance(&task.spec);
    let job = execution::render(&task.spec, &lease, &accepted).unwrap();
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert_eq!(
        job["spec"]["template"]["spec"]["automountServiceAccountToken"],
        false
    );
    assert_eq!(job["metadata"]["name"], execution::resource_name(&lease));
    let mut unknown = accepted;
    unknown.ready = false;
    assert!(execution::render(&task.spec, &lease, &unknown).is_err());
    let mut gpu = task.spec.clone();
    gpu.profile.gpu = 1;
    assert!(gpu.validate().is_err());
}

#[test]
fn sandbox_is_interactive_and_cannot_assume_command_reconnect() {
    let mut spec = task_spec();
    spec.profile.backend = Backend::AgentSandbox;
    assert!(spec.validate().is_err());
    spec.kind = TaskKind::Explore;
    let mut task = Task::new(spec).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    let mut accepted = acceptance(&task.spec);
    assert!(execution::render(&task.spec, &lease, &accepted).is_err());
    accepted.command_reattach = true;
    let resource = execution::render(&task.spec, &lease, &accepted).unwrap();
    assert_eq!(resource["apiVersion"], "agents.kruise.io/v1alpha1");
}

#[test]
fn shared_mount_requires_explicit_compatibility_acceptance() {
    let mut spec = task_spec();
    spec.profile.prepared_pvc = Some("prepared".into());
    let mut task = Task::new(spec).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    let mut accepted = acceptance(&task.spec);
    assert!(execution::render(&task.spec, &lease, &accepted).is_err());
    accepted.immutable_prepared_mount = true;
    let job = execution::render(&task.spec, &lease, &accepted).unwrap();
    assert_eq!(
        job["spec"]["template"]["spec"]["volumes"][1]["persistentVolumeClaim"]["readOnly"],
        true
    );
}

#[test]
fn result_manifest_binds_trial_fit_attempt_and_artifact_prefix() {
    let mut task = Task::new(task_spec()).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    let mut result = receipt(&task);
    result.fit_identity_sha256 = Some(hash('a'));
    assert!(result.validate(&task.spec, &lease).is_err());
    let mut result = receipt(&task);
    result.artifacts[0].key = "research/another-task/result.bin".into();
    assert!(result.validate(&task.spec, &lease).is_err());
    let plan = PreparationPlan {
        recipe_sql: None,
        spec: spec(),
        source_receipt_sha256: hash('b'),
        producer_image: task.spec.image.clone(),
    };
    assert!(plan.id().is_ok());
}

#[test]
fn verified_checkpoint_is_trial_local_monotonic_and_survives_retry() {
    use hft_research_platform::orchestrator::Checkpoint;
    let mut task = Task::new(task_spec()).unwrap();
    let lease = task.claim("owner", 1000, 1000).unwrap();
    task.launched(&lease, 1001, handle(&task, &lease)).unwrap();
    let checkpoint = Checkpoint {
        task_id: task.id.clone(),
        attempt: lease.attempt,
        fence: lease.fence,
        step: 7,
        view_manifest_sha256: task.spec.view_manifest_sha256.clone(),
        source_sha256: task.spec.source_sha256.clone(),
        image: task.spec.image.clone(),
        fit_identity_sha256: task.spec.fit_identity_sha256.clone(),
        artifact: receipt(&task).artifacts.remove(0),
    };
    task.stage_checkpoint(&lease, 1002, checkpoint.clone())
        .unwrap();
    task.stage_checkpoint(&lease, 1003, checkpoint.clone())
        .unwrap();
    let mut wrong = checkpoint.clone();
    wrong.fit_identity_sha256 = Some(hash('a'));
    assert!(task.stage_checkpoint(&lease, 1004, wrong).is_err());
    task.stop(State::Failed, true).unwrap();
    task.stopped(lease.attempt, lease.fence).unwrap();
    let next = task.claim("owner", 1100, 1000).unwrap();
    task.launched(&next, 1101, handle(&task, &next)).unwrap();
    assert_eq!(task.checkpoint, Some(checkpoint.clone()));
    assert!(task
        .stage_checkpoint(&next, 1102, checkpoint.clone())
        .is_err());
    let mut progress = checkpoint;
    progress.attempt = next.attempt;
    progress.fence = next.fence;
    progress.artifact.key = format!(
        "{}/{}/{}/checkpoint.bin",
        task.spec.output_prefix, task.id, next.attempt
    );
    assert!(task
        .stage_checkpoint(&next, 1103, progress.clone())
        .is_err());
    progress.step += 1;
    task.stage_checkpoint(&next, 1104, progress).unwrap();
    let other = Task::new(task_spec()).unwrap();
    assert!(other.checkpoint.is_none());
}

#[test]
fn streaming_order_checks_block_boundaries_and_never_reopens_segments() {
    let mut order = data::BlockOrder::default();
    order
        .observe(&TypedBlock::Features(vec![frame(200, 0)]))
        .unwrap();
    assert!(order
        .observe(&TypedBlock::Features(vec![frame(190, 1)]))
        .is_err());
    let mut next = frame(300, 0);
    next.segment = "b".into();
    order.observe(&TypedBlock::Features(vec![next])).unwrap();
    assert!(order
        .observe(&TypedBlock::Features(vec![frame(400, 1)]))
        .is_err());
}

#[test]
fn task_admission_preserves_exact_native_grant_and_attempt_budget() {
    use hft_research_platform::orchestrator::Admission;
    let spec = task_spec();
    let mut admission = Admission {
        schema: 1,
        request_sha256: spec.id().unwrap(),
        task_spec: spec.clone(),
        resource_reservation_receipt_sha256: hash('a'),
        scientific_grant_receipt_sha256: hash('b'),
        release_admission_receipt_sha256: hash('c'),
        max_attempts: spec.max_attempts,
    };
    admission.validate(&spec).unwrap();
    admission.max_attempts = 1;
    assert!(admission.validate(&spec).is_err());
    admission.max_attempts = spec.max_attempts;
    admission.scientific_grant_receipt_sha256.clear();
    assert!(admission.validate(&spec).is_err());
    admission.scientific_grant_receipt_sha256 = hash('b');
    let mut changed = spec;
    changed.command.push("changed".into());
    assert!(admission.validate(&changed).is_err());
}

#[test]
fn reviewed_sql_changes_data_identity_without_rebuilding_the_worker() {
    let image = format!("fixture@sha256:{}", hash('a'));
    let first = PreparationPlan {
        spec: spec(),
        source_receipt_sha256: hash('b'),
        producer_image: image.clone(),
        recipe_sql: None,
    };
    let mut second = first.clone();
    let sql = data::PREPARE_SQL.replace(
        "asks_price[1]-bids_price[1]",
        "(asks_price[1]-bids_price[1])*2",
    );
    second.spec.feature_sql_sha256 = sha256(sql.as_bytes());
    second.recipe_sql = Some(sql);
    assert_ne!(first.id().unwrap(), second.id().unwrap());
    assert_eq!(first.producer_image, second.producer_image);
    let mut mismatched = second.clone();
    mismatched.spec.feature_sql_sha256 = first.spec.feature_sql_sha256.clone();
    assert!(mismatched.id().is_err());
    second.recipe_sql = Some("DROP TABLE research.normalized_books;".into());
    second.spec.feature_sql_sha256 = sha256(second.recipe_sql.as_ref().unwrap().as_bytes());
    assert!(second.id().is_err());
}
