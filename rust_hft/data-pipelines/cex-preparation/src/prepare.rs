//! Data-owner execution of one bounded immutable preparation request.
use crate::cli::print_json;
#[cfg(test)]
use crate::cli::BUILD_SOURCE_REVISION;
use anyhow::{bail, Context};
#[cfg(test)]
use hft_cex_research_input::inventory::{
    selection_fingerprint_from_inputs, FRESH_WINDOW_SELECTION_SCHEMA,
};
use hft_cex_research_input::{
    inventory::{FreshWindowMode, FreshWindowRequest, FreshWindowSelection},
    preparation::*,
};
use hft_collector::research_inventory::select_fresh_window;
use hft_research_artifacts::sha256_file;
#[cfg(test)]
use hft_research_manifest::binance::Market;
#[cfg(test)]
use std::{collections::BTreeMap, fs, path::PathBuf};
use std::{
    io::{self, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
/// Freeze the selected archive window once, run the existing materializer, and
/// verify the resulting immutable local Campaign input set. A completed run is
/// read back and reused; it is never rematerialized on restart.
pub fn prepare(args: PreparationArgs) -> anyhow::Result<()> {
    validate_args(&args)?;
    let raw_root = read_only_root(&args.raw_root, "raw archive")?;
    let reference_root = read_only_root(&args.reference_root, "reference archive")?;
    hft_research_artifacts::ensure_real_directory(&args.output_root, "fresh input output")?;
    hft_research_artifacts::ensure_real_directory(
        &args.materializer_work_dir,
        "materializer work",
    )?;
    ensure_output_parent(&args.inventory_out, "frozen inventory")?;
    ensure_output_parent(&args.request_out, "fresh preparation request")?;
    if let Some(report) = &args.report_out {
        ensure_output_parent(report, "fresh input preparation report")?;
    }
    ensure_regular_executable(&args.materializer, "materializer")?;
    if let Some(path) = args.binary_dir.as_deref() {
        read_only_root(path, "materializer binary directory")?;
    }

    let output_root = args.output_root.canonicalize()?;
    let run_root = output_root.join(&args.output_prefix);
    let expected_campaign_inputs = run_root.join("receipts/campaign-inputs.json");
    if normalize_for_compare(&args.campaign_inputs_out)?
        != normalize_for_compare(&expected_campaign_inputs)?
    {
        bail!(
            "campaign inputs output must be the materializer receipt under the selected output prefix: {}",
            expected_campaign_inputs.display()
        );
    }
    let expected_request = expected_request_path(&output_root, &args.output_prefix);
    if normalize_for_compare(&args.request_out)? != normalize_for_compare(&expected_request)? {
        bail!(
            "fresh preparation request output must be the fixed identity path: {}",
            expected_request.display()
        );
    }
    let expected_selection = expected_selection_path(&output_root, &args.output_prefix);
    let existing_request = if args.request_out.is_file() {
        Some(read_json_bounded::<PreparationRequest>(&args.request_out)?)
    } else {
        None
    };
    if args.inventory_out.try_exists()? && existing_request.is_none() {
        bail!("fresh preparation request identity is required before an existing inventory can be reused");
    }
    if args.inventory_out.try_exists()? && !expected_selection.is_file() {
        bail!("fresh window selection identity is required before an existing inventory can be reused");
    }
    let mode = resolve_selection_mode(&args, existing_request.as_ref())?;
    let request = build_preparation_request(
        &args,
        &raw_root,
        &reference_root,
        &output_root,
        mode,
        &expected_selection,
    )?;

    let run_exists = run_root.try_exists()?;
    if run_exists && !args.request_out.is_file() {
        bail!("fresh preparation request identity is missing for an existing output prefix");
    }
    if run_exists && !expected_selection.is_file() {
        bail!("fresh window selection identity is missing for an existing output prefix");
    }
    let request_sha256 = ensure_preparation_request(&args.request_out, &request, !run_exists)?;
    let mut partial_owner_verified = false;
    if args.campaign_inputs_out.is_file() {
        let complete_result = (|| -> anyhow::Result<PreparationOutput> {
            let (selection, selection_sha256) =
                read_selection(&expected_selection, &request, &args)?;
            let inventory = inventory_from_selection(&args, &selection)?;
            verify_inventory_matches(&args.inventory_out, &inventory, metadata_limit(&args))?;
            let verified = verify_materialized_outputs(&args, &run_root, &args.inventory_out)?;
            let fingerprint =
                input_fingerprint(&args, &request_sha256, &verified.inventory_sha256)?;
            if fingerprint != selection.input_fingerprint_sha256 {
                bail!("selected fresh window fingerprint differs from frozen inventory");
            }
            let report = preparation_report(
                &args,
                &run_root,
                &verified,
                &fingerprint,
                &request_sha256,
                &selection,
                &selection_sha256,
            )?;
            write_report_create_once(args.report_out.as_deref(), &report)?;
            Ok(PreparationOutput {
                report,
                reused_existing: true,
            })
        })();
        match complete_result {
            Ok(output) => return print_json(&output),
            Err(full_error) => {
                verify_partial_materialized_outputs(
                    &args,
                    &run_root,
                    &args.campaign_inputs_out,
                )
                .with_context(|| {
                    format!(
                        "complete fresh output failed verification and cannot be safely resumed: {full_error}"
                    )
                })?;
                partial_owner_verified = true;
            }
        }
    }
    if run_exists && !partial_owner_verified {
        let staged_owner = args
            .materializer_work_dir
            .join("staged-output/receipts/campaign-inputs.json");
        if args.campaign_inputs_out.is_file() {
            verify_partial_materialized_outputs(&args, &run_root, &args.campaign_inputs_out)?;
        } else if staged_owner.is_file() {
            verify_partial_materialized_outputs(
                &args,
                &args.materializer_work_dir.join("staged-output"),
                &staged_owner,
            )?;
            verify_partial_materialized_outputs(&args, &run_root, &staged_owner)?;
        } else {
            bail!(
                "existing fresh materializer output has no verifiable preparation owner: {}",
                run_root.display()
            );
        }
    }

    let (selection, selection_sha256) = if expected_selection.is_file() {
        read_selection(&expected_selection, &request, &args)?
    } else {
        let window_request =
            fresh_window_request(&args, &raw_root, &reference_root, &request.selection);
        let selection = select_fresh_window(&window_request)?;
        write_selection_create_once(&expected_selection, &selection)?;
        read_selection(&expected_selection, &request, &args)?
    };
    let inventory = inventory_from_selection(&args, &selection)?;
    if args.inventory_out.try_exists()? {
        validate_existing_inventory(&args, &args.inventory_out)?;
        verify_inventory_matches(&args.inventory_out, &inventory, metadata_limit(&args))?;
    } else {
        write_bytes_create_once(
            &args.inventory_out,
            inventory.inventory_env.as_bytes(),
            "frozen inventory",
        )?;
    }
    let inventory_sha256 = sha256_file(&args.inventory_out)?;
    if inventory_sha256 != inventory.inventory_sha256 {
        bail!("frozen inventory SHA256 readback differs from collector freezer");
    }
    validate_inventory_env(&args, &inventory.inventory_env)?;

    run_materializer(&args, &args.inventory_out)?;
    let verified = verify_materialized_outputs(&args, &run_root, &args.inventory_out)?;
    let derived_fingerprint =
        input_fingerprint(&args, &request_sha256, &verified.inventory_sha256)?;
    let fingerprint = if inventory.input_fingerprint_sha256.is_empty() {
        derived_fingerprint
    } else {
        if inventory.input_fingerprint_sha256 != derived_fingerprint {
            bail!("collector input fingerprint differs from frozen inventory contents");
        }
        inventory.input_fingerprint_sha256.clone()
    };
    if fingerprint != selection.input_fingerprint_sha256 {
        bail!("selected fresh window fingerprint differs from frozen inventory");
    }
    let report = preparation_report(
        &args,
        &run_root,
        &verified,
        &fingerprint,
        &request_sha256,
        &selection,
        &selection_sha256,
    )?;
    write_report_create_once(args.report_out.as_deref(), &report)?;
    print_json(&PreparationOutput {
        report,
        reused_existing: false,
    })
}

fn fresh_window_request(
    args: &PreparationArgs,
    raw_root: &Path,
    reference_root: &Path,
    mode: &FreshWindowMode,
) -> FreshWindowRequest {
    FreshWindowRequest {
        raw_root: raw_root.to_path_buf(),
        reference_root: reference_root.to_path_buf(),
        mode: mode.clone(),
        market: parse_market(&args.market).expect("validated fresh input market"),
        symbol: args.symbol.clone(),
        source_revision: args.source_revision.clone(),
        image_ref: args.image_ref.clone(),
        mission_id: args.mission_id.clone(),
        output_prefix: args.output_prefix.clone(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        max_scan_entries: args.max_scan_entries,
        max_inputs: args.max_inputs,
        max_input_bytes: args.max_input_bytes,
    }
}

fn ensure_output_parent(path: &Path, label: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    hft_research_artifacts::ensure_real_directory(parent, label)
}

fn write_selection_create_once(
    path: &Path,
    selection: &FreshWindowSelection,
) -> anyhow::Result<()> {
    write_bytes_create_once(
        path,
        &serde_json::to_vec_pretty(selection)?,
        "fresh window selection",
    )
}

fn ensure_preparation_request(
    path: &Path,
    request: &PreparationRequest,
    create_if_missing: bool,
) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec_pretty(request)?;
    if path.try_exists()? {
        let existing: PreparationRequest = read_json_bounded(path)?;
        if existing != *request {
            bail!("fresh preparation request identity differs from the existing request");
        }
    } else if create_if_missing {
        write_bytes_create_once(path, &bytes, "fresh preparation request")?;
    } else {
        bail!("fresh preparation request identity is missing for existing outputs");
    }
    let actual: PreparationRequest = read_json_bounded(path)?;
    if actual != *request {
        bail!("fresh preparation request changed during readback");
    }
    sha256_file(path)
}

struct MaterializerProcess {
    child: Option<Child>,
}

impl MaterializerProcess {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("materializer process guard still owns its child")
    }

    fn finish(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_materializer(&mut child);
        }
    }
}

impl Drop for MaterializerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_materializer(&mut child);
        }
    }
}

fn run_materializer(args: &PreparationArgs, inventory: &Path) -> anyhow::Result<()> {
    let output_root = args.output_root.canonicalize()?;
    let mut command = Command::new(&args.materializer);
    command
        .arg("--inventory")
        .arg(inventory)
        .arg("--raw-root")
        .arg(&args.raw_root)
        .arg("--reference-root")
        .arg(&args.reference_root)
        .arg("--output-root")
        .arg(output_root)
        .arg("--work-dir")
        .arg(&args.materializer_work_dir)
        .arg("--role")
        .arg("all");
    if args.sequence_output {
        command.arg("--sequence-output");
    }
    if args.market_encoder_output {
        command.arg("--market-encoder-output");
        if let Some(start) = args.market_feature_start_received_at_ns {
            command
                .arg("--market-feature-start-received-at-ns")
                .arg(start.to_string());
        }
        if let Some(end) = args.market_feature_end_received_at_ns {
            command
                .arg("--market-feature-end-received-at-ns")
                .arg(end.to_string());
        }
    }
    if let Some(binary_dir) = &args.binary_dir {
        command.arg("--binary-dir").arg(binary_dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start fresh materializer {}", args.materializer.display()))?;
    let mut process = MaterializerProcess::new(child);
    let stdout = process
        .child_mut()
        .stdout
        .take()
        .context("materializer stdout unavailable")?;
    let stderr = process
        .child_mut()
        .stderr
        .take()
        .context("materializer stderr unavailable")?;
    let max = args.max_materializer_output_bytes;
    let total_bytes = Arc::new(AtomicU64::new(0));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let stdout_total = Arc::clone(&total_bytes);
    let stdout_exceeded = Arc::clone(&output_exceeded);
    let stdout_sender = sender.clone();
    thread::spawn(move || {
        let result = read_stream_bounded(stdout, max, &stdout_total, &stdout_exceeded);
        let _ = stdout_sender.send((true, result));
    });
    let stderr_total = Arc::clone(&total_bytes);
    let stderr_exceeded = Arc::clone(&output_exceeded);
    thread::spawn(move || {
        let result = read_stream_bounded(stderr, max, &stderr_total, &stderr_exceeded);
        let _ = sender.send((false, result));
    });
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(args.materializer_timeout_seconds))
        .context("materializer timeout overflow")?;
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut failure = None;
    loop {
        while let Ok((is_stdout, result)) = receiver.try_recv() {
            if is_stdout {
                stdout = Some(result);
            } else {
                stderr = Some(result);
            }
        }
        if output_exceeded.load(Ordering::SeqCst) {
            terminate_materializer(process.child_mut());
            failure = Some("materializer stdout and stderr exceed the bounded output budget");
            break;
        }
        if status.is_none() {
            status = process
                .child_mut()
                .try_wait()
                .context("poll fresh materializer")?;
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            terminate_materializer(process.child_mut());
            failure = Some("fresh materializer exceeded its bounded timeout");
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let cleanup_deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .context("materializer cleanup timeout overflow")?;
    while stdout.is_none() || stderr.is_none() {
        if Instant::now() >= cleanup_deadline {
            bail!("fresh materializer pipe cleanup exceeded its bounded timeout");
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok((is_stdout, result)) => {
                if is_stdout {
                    stdout = Some(result);
                } else {
                    stderr = Some(result);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("fresh materializer pipe reader exited without a result");
            }
        }
    }
    if let Some(reason) = failure {
        bail!("{reason}");
    }
    let status = status
        .or_else(|| process.child_mut().try_wait().ok().flatten())
        .context("fresh materializer exit status is unavailable")?;
    let _stdout = stdout
        .expect("bounded materializer stdout result was collected")
        .context("read materializer stdout")?;
    let stderr = stderr
        .expect("bounded materializer stderr result was collected")
        .context("read materializer stderr")?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        bail!(
            "fresh materializer exited unsuccessfully with {:?}: {}",
            status.code(),
            stderr.trim()
        );
    }
    process.finish();
    Ok(())
}

fn read_stream_bounded(
    mut reader: impl Read,
    max_bytes: u64,
    total_bytes: &AtomicU64,
    output_exceeded: &AtomicBool,
) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return if total > max_bytes {
                bail!("materializer output exceeds the bounded output budget")
            } else {
                Ok(output)
            };
        }
        total = total.saturating_add(read as u64);
        let global_total = total_bytes.fetch_add(read as u64, Ordering::SeqCst) + read as u64;
        if global_total > max_bytes {
            output_exceeded.store(true, Ordering::SeqCst);
            return Ok(output);
        }
        if total <= max_bytes {
            output.extend_from_slice(&buffer[..read]);
        }
    }
}

fn terminate_materializer(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &process_group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn write_report_create_once(path: Option<&Path>, report: &PreparationReport) -> anyhow::Result<()> {
    let Some(path) = path else { return Ok(()) };
    let bytes = serde_json::to_vec_pretty(report)?;
    if path.try_exists()? {
        ensure_regular_file(path, "fresh preparation report")?;
        if read_file_bounded(path, bytes.len() as u64, "fresh preparation report")? != bytes {
            bail!("fresh preparation report identity differs from the existing report");
        }
        return Ok(());
    }
    write_bytes_create_once(path, &bytes, "fresh preparation report")
}

fn write_bytes_create_once(path: &Path, bytes: &[u8], label: &str) -> anyhow::Result<()> {
    ensure_output_parent(path, label)?;
    hft_research_artifacts::ensure_output_path_is_not_symlink(path, label)?;
    let mut temporary = hft_research_artifacts::temporary_output_file(path, ".fresh-inputs-")?;
    std::io::Write::write_all(temporary.as_file_mut(), bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            ensure_regular_file(path, label)?;
            if read_file_bounded(path, bytes.len() as u64, label)? == bytes {
                Ok(())
            } else {
                bail!("{label} identity differs from the existing create-once file")
            }
        }
        Err(error) => {
            Err(error.error).with_context(|| format!("publish {label} {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> PreparationArgs {
        PreparationArgs {
            source_revision: BUILD_SOURCE_REVISION.into(),
            raw_root: PathBuf::from("/raw"),
            reference_root: PathBuf::from("/reference"),
            start_received_at_ns: Some(10),
            end_received_at_ns: Some(20),
            duration_ns: None,
            cutoff_received_at_ns: None,
            max_candidates: None,
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            image_ref: format!("registry/research@sha256:{}", "a".repeat(64)),
            mission_id: "fresh-window".into(),
            output_prefix: "campaigns/fresh".into(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
            max_scan_entries: 100,
            max_inputs: 10,
            max_input_bytes: 1_000_000,
            inventory_out: PathBuf::from("inventory.env"),
            request_out: PathBuf::from("fresh-request.json"),
            campaign_inputs_out: PathBuf::from("out/campaigns/fresh/receipts/campaign-inputs.json"),
            output_root: PathBuf::from("out"),
            materializer: PathBuf::from("materializer.sh"),
            materializer_work_dir: PathBuf::from("materializer-work"),
            binary_dir: None,
            sequence_output: false,
            market_encoder_output: false,
            market_feature_start_received_at_ns: None,
            market_feature_end_received_at_ns: None,
            materializer_timeout_seconds: 10,
            max_materializer_output_bytes: 1024,
            report_out: None,
        }
    }

    #[test]
    fn market_preparation_binds_explicit_window_and_label_only_lookahead() {
        let mut request = args();
        request.market_encoder_output = true;
        request.symbol = "SOLUSDT".into();
        request.label_horizon_buckets = 30;
        request.start_received_at_ns = Some(0);
        request.end_received_at_ns = Some(120_000_000_000);
        request.market_feature_end_received_at_ns = Some(90_000_000_000);
        validate_args(&request).unwrap();
        request.market_feature_start_received_at_ns = Some(30_000_000_000);
        validate_args(&request).unwrap();
        resolve_selection_mode(&request, None).unwrap();
        request.market_feature_start_received_at_ns = Some(61_000_000_000);
        assert!(validate_args(&request).is_err());
        assert!(resolve_selection_mode(&request, None).is_err());
        request.market_feature_start_received_at_ns = Some(30_000_000_001);
        assert!(validate_args(&request).is_err());
        request.market_feature_start_received_at_ns = Some(30_000_000_000);
        request.market_feature_end_received_at_ns = Some(121_000_000_000);
        assert!(validate_args(&request).is_err());
        request.market_feature_end_received_at_ns = Some(90_000_000_001);
        assert!(validate_args(&request).is_err());
        request.market_feature_end_received_at_ns = Some(90_000_000_000);
        request.market_encoder_output = false;
        assert!(validate_args(&request).is_err());
        request.market_encoder_output = true;
        request.start_received_at_ns = None;
        assert!(validate_args(&request).is_err());
    }

    #[test]
    fn market_export_verifier_preserves_legacy_reports_and_rejects_mode_drift() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("materialization.json");
        std::fs::write(&path, b"legacy non-JSON materialization report").unwrap();
        let item = CampaignInputItem {
            relative_path: "materialization.json".into(),
            sha256: "a".repeat(64),
            object_url: "https://example.invalid/materialization.json".into(),
        };
        let mut request = args();
        verify_market_encoder_output(&request, root.path(), &item).unwrap();
        request.market_encoder_output = true;
        assert!(verify_market_encoder_output(&request, root.path(), &item).is_err());
        request.market_encoder_output = false;
        std::fs::write(&path, br#"{"market_encoder":{}}"#).unwrap();
        assert!(verify_market_encoder_output(&request, root.path(), &item).is_err());
    }

    #[test]
    fn market_metadata_limit_accepts_daily_evidence_but_keeps_a_hard_bound() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("materialization.json");
        let report = serde_json::json!({"market_encoder": {"schema_version": "monday.market_encoder_export.v1"},
            "padding": "x".repeat(MAX_PREPARATION_BYTES as usize)});
        std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
        assert!(read_json_bounded::<serde_json::Value>(&path).is_err());
        let item = CampaignInputItem {
            relative_path: "materialization.json".into(),
            sha256: "a".repeat(64),
            object_url: "https://example.invalid/report".into(),
        };
        let mut request = args();
        request.market_encoder_output = true;
        let error = verify_market_encoder_output(&request, root.path(), &item).unwrap_err();
        // The larger document passed bounded decoding but still cannot bypass
        // the actual manifest admission checks.
        assert!(error.to_string().contains("missing market manifest digest"));
        std::fs::write(&path, vec![b'x'; MAX_MARKET_METADATA_BYTES as usize + 1]).unwrap();
        assert!(read_file_bounded(&path, metadata_limit(&request), "market evidence").is_err());
    }

    #[test]
    fn rejects_unbounded_preparation_limits() {
        let mut request = args();
        request.max_inputs = 0;
        assert!(validate_args(&request).is_err());
        request = args();
        request.max_input_bytes = MAX_INPUT_BYTES + 1;
        assert!(validate_args(&request).is_err());
        request = args();
        request.materializer_timeout_seconds = MAX_TIMEOUT_SECONDS + 1;
        assert!(validate_args(&request).is_err());

        let root = tempfile::tempdir().unwrap();
        let oversized = root.path().join("oversized");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(MAX_PREPARATION_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert!(read_file_bounded(&oversized, MAX_PREPARATION_BYTES, "test").is_err());

        let raw_count = usize::MAX.to_string();
        let counts = BTreeMap::from([
            ("RAW_SEGMENT_COUNT", raw_count.as_str()),
            ("REFERENCE_COUNT", "1"),
        ]);
        assert!(inventory_counts(&counts, MAX_INPUTS).is_err());
        let counts = BTreeMap::from([("RAW_SEGMENT_COUNT", "8192"), ("REFERENCE_COUNT", "1")]);
        assert!(inventory_counts(&counts, MAX_INPUTS).is_err());
    }

    #[test]
    fn rejects_archive_window_reversal_and_unsafe_prefix() {
        let mut request = args();
        request.end_received_at_ns = request.start_received_at_ns;
        assert!(validate_args(&request).is_err());
        request = args();
        request.output_prefix = "../fresh".into();
        assert!(validate_args(&request).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_runs_materializer_once_and_reuses_verified_receipt() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let raw_root = root.path().join("raw");
        let reference_root = root.path().join("reference");
        let output_root = root.path().join("output");
        let work_dir = root.path().join("materializer-work");
        fs::create_dir_all(&raw_root).unwrap();
        fs::create_dir_all(&reference_root).unwrap();
        fs::create_dir_all(&output_root).unwrap();

        let inventory_out = root.path().join("frozen.env");
        let image_ref = format!("registry/research@sha256:{}", "a".repeat(64));

        let materializer = root.path().join("materializer.sh");
        fs::write(
            &materializer,
            r#"#!/bin/sh
set -eu
inventory=
output_root=
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --inventory) inventory=$2; shift 2 ;;
    --output-root) output_root=$2; shift 2 ;;
    --work-dir) work_dir=$2; shift 2 ;;
    --raw-root|--reference-root|--role) shift 2 ;;
    *) shift ;;
  esac
done
count=0
[ ! -e "$work_dir/count" ] || count=$(cat "$work_dir/count")
printf '%s\n' "$((count + 1))" >"$work_dir/count"
if [ -e "$work_dir/fail-next" ]; then
  rm -f "$work_dir/fail-next"
  exit 75
fi
run_root="$output_root/run"
mkdir -p "$run_root/artifacts" "$run_root/receipts"
cp "$inventory" "$run_root/receipts/frozen-inventory.env"
printf 'feature\n' >"$run_root/artifacts/feature.jsonl"
printf 'materialization\n' >"$run_root/artifacts/materialization.json"
printf 'replay\n' >"$run_root/artifacts/replay.parquet"
printf 'manifest\n' >"$run_root/artifacts/replay-manifest.json"
sha() { shasum -a 256 "$1" | awk '{print $1}'; }
inventory_sha=$(sha "$inventory")
feature_sha=$(sha "$run_root/artifacts/feature.jsonl")
materialization_sha=$(sha "$run_root/artifacts/materialization.json")
replay_sha=$(sha "$run_root/artifacts/replay.parquet")
manifest_sha=$(sha "$run_root/artifacts/replay-manifest.json")
cat >"$run_root/receipts/campaign-inputs.json" <<EOF
{
  "schema_version":"monday.cex_campaign_inputs.v1",
  "run_id":"inventory-fresh",
  "source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "image_ref":"registry/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "mission_id":"fresh-window","market":"usdm","symbol":"BTCUSDT","output_prefix":"run",
  "output_object_base_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization",
  "readback_scope":"same-mounted-ossfs-prefix",
  "feature":{"relative_path":"artifacts/feature.jsonl","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/feature.jsonl","sha256":"$feature_sha"},
  "materialization":{"relative_path":"artifacts/materialization.json","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/materialization.json","sha256":"$materialization_sha"},
  "replay_artifact":{"relative_path":"artifacts/replay.parquet","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/replay.parquet","sha256":"$replay_sha"},
  "replay_manifest":{"relative_path":"artifacts/replay-manifest.json","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/replay-manifest.json","sha256":"$manifest_sha"}
}
EOF
campaign_sha=$(sha "$run_root/receipts/campaign-inputs.json")
cat >"$run_root/receipts/materialization-receipt.json" <<EOF
{
  "schema_version":"monday.cex_materialization_receipt.v1",
  "run_id":"inventory-fresh",
  "source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "image_ref":"registry/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "inventory_sha256":"$inventory_sha",
  "campaign_inputs_sha256":"$campaign_sha",
  "feature_sha256":"$feature_sha",
  "materialization_sha256":"$materialization_sha",
  "replay_artifact_sha256":"$replay_sha",
  "replay_manifest_sha256":"$manifest_sha"
}
EOF
"#,
        )
        .unwrap();
        fs::set_permissions(&materializer, fs::Permissions::from_mode(0o700)).unwrap();

        let args = PreparationArgs {
            source_revision: BUILD_SOURCE_REVISION.into(),
            raw_root,
            reference_root,
            start_received_at_ns: Some(1),
            end_received_at_ns: Some(2),
            duration_ns: None,
            cutoff_received_at_ns: None,
            max_candidates: None,
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            image_ref,
            mission_id: "fresh-window".into(),
            output_prefix: "run".into(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
            max_scan_entries: 100,
            max_inputs: 4,
            max_input_bytes: 1_000_000,
            inventory_out,
            request_out: root.path().join("output/.fresh-inputs/run/request.json"),
            campaign_inputs_out: output_root.join("run/receipts/campaign-inputs.json"),
            output_root,
            materializer,
            materializer_work_dir: work_dir.clone(),
            binary_dir: None,
            sequence_output: false,
            market_encoder_output: false,
            market_feature_start_received_at_ns: None,
            market_feature_end_received_at_ns: None,
            materializer_timeout_seconds: 10,
            max_materializer_output_bytes: 16 * 1024,
            report_out: Some(root.path().join("preparation.json")),
        };

        let raw_input = hft_cex_research_input::inventory::FrozenInput {
            relative_path: "raw.jsonl.zst".into(),
            content_sha256: "1".repeat(64),
            manifest_sha256: "2".repeat(64),
            bytes: 1,
            start_received_at_ns: 1,
            end_received_at_ns: 2,
        };
        let reference_input = hft_cex_research_input::inventory::FrozenInput {
            relative_path: "reference.json".into(),
            content_sha256: "3".repeat(64),
            manifest_sha256: "4".repeat(64),
            bytes: 1,
            start_received_at_ns: 1,
            end_received_at_ns: 1,
        };
        let selection = FreshWindowSelection {
            schema_version: FRESH_WINDOW_SELECTION_SCHEMA.to_string(),
            mode: FreshWindowMode::Explicit {
                start_received_at_ns: 1,
                end_received_at_ns: 2,
            },
            market: Market::Usdm,
            selected_start_received_at_ns: 1,
            selected_end_received_at_ns: 2,
            raw: vec![raw_input],
            references: vec![reference_input],
            verified_bytes: 2,
            // The latest candidate may have consumed one additional reference
            // before falling back; prepare must preserve that cumulative
            // budget evidence while accepting the selected input bytes.
            verification_bytes: 3,
            input_fingerprint_sha256: selection_fingerprint_from_inputs(
                &[hft_cex_research_input::inventory::FrozenInput {
                    relative_path: "raw.jsonl.zst".into(),
                    content_sha256: "1".repeat(64),
                    manifest_sha256: "2".repeat(64),
                    bytes: 1,
                    start_received_at_ns: 1,
                    end_received_at_ns: 2,
                }],
                &[hft_cex_research_input::inventory::FrozenInput {
                    relative_path: "reference.json".into(),
                    content_sha256: "3".repeat(64),
                    manifest_sha256: "4".repeat(64),
                    bytes: 1,
                    start_received_at_ns: 1,
                    end_received_at_ns: 1,
                }],
            )
            .unwrap(),
            inventory_eligible: true,
            materialized_pit_admitted: false,
        };
        let selection_path = expected_selection_path(
            &args.output_root.canonicalize().unwrap(),
            &args.output_prefix,
        );
        fs::create_dir_all(&args.materializer_work_dir).unwrap();
        let request = build_preparation_request(
            &args,
            &args.raw_root.canonicalize().unwrap(),
            &args.reference_root.canonicalize().unwrap(),
            &args.output_root.canonicalize().unwrap(),
            selection.mode.clone(),
            &selection_path,
        )
        .unwrap();
        write_bytes_create_once(
            &args.request_out,
            &serde_json::to_vec_pretty(&request).unwrap(),
            "fresh preparation request",
        )
        .unwrap();
        write_selection_create_once(&selection_path, &selection).unwrap();
        let expected_inventory = inventory_from_selection(&args, &selection).unwrap();
        fs::write(&args.inventory_out, expected_inventory.inventory_env).unwrap();
        prepare(args.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "1"
        );
        fs::write(args.raw_root.join("new-after-freeze"), b"ignored").unwrap();
        prepare(args.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "1"
        );
        fs::remove_dir_all(args.output_root.join("run")).unwrap();
        fs::write(work_dir.join("fail-next"), b"").unwrap();
        assert!(prepare(args.clone()).is_err());
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "2"
        );
        prepare(args.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "3"
        );
        let mut changed_request = args.clone();
        changed_request.request_out = root.path().join("another-request.json");
        assert!(prepare(changed_request).is_err());
        let mut changed_window = args.clone();
        changed_window.start_received_at_ns = Some(3);
        assert!(prepare(changed_window).is_err());
        let mut changed_limits = args.clone();
        changed_limits.max_inputs = 3;
        assert!(prepare(changed_limits).is_err());
        let changed_root = root.path().join("other-raw");
        fs::create_dir_all(&changed_root).unwrap();
        let mut changed_archive = args.clone();
        changed_archive.raw_root = changed_root;
        assert!(prepare(changed_archive).is_err());

        // The controller reads this completed evidence without archive mounts or an executable.
        let report_path = args.report_out.as_ref().unwrap();
        let report_before = fs::read(report_path).unwrap();
        let count_before = fs::read(work_dir.join("count")).unwrap();
        assert!(verify_prepared_inputs(&args).is_ok());
        fs::remove_file(&args.materializer).unwrap();
        fs::remove_dir_all(&args.raw_root).unwrap();
        fs::remove_dir_all(&args.reference_root).unwrap();
        assert!(verify_prepared_inputs(&args).is_ok());
        assert_eq!(fs::read(work_dir.join("count")).unwrap(), count_before);

        let feature = args.output_root.join("run/artifacts/feature.jsonl");
        let original_feature = fs::read(&feature).unwrap();
        fs::write(&feature, b"tampered").unwrap();
        assert!(verify_prepared_inputs(&args).is_err());
        fs::write(&feature, original_feature).unwrap();
        let mut wrong_budget = args.clone();
        wrong_budget.max_inputs -= 1;
        assert!(verify_prepared_inputs(&wrong_budget).is_err());
        let mut wrong_source = args.clone();
        wrong_source.source_revision = "b".repeat(40);
        assert!(verify_prepared_inputs(&wrong_source).is_err());
        let selection_bytes = fs::read(&selection_path).unwrap();
        let mut changed_selection: serde_json::Value =
            serde_json::from_slice(&selection_bytes).unwrap();
        changed_selection["verification_bytes"] = serde_json::json!(args.max_input_bytes + 1);
        fs::write(
            &selection_path,
            serde_json::to_vec(&changed_selection).unwrap(),
        )
        .unwrap();
        assert!(verify_prepared_inputs(&args).is_err());
        fs::write(&selection_path, selection_bytes).unwrap();
        let published_inventory = args.output_root.join("run/receipts/frozen-inventory.env");
        fs::remove_file(&published_inventory).unwrap();
        assert!(verify_prepared_inputs(&args).is_err());
        assert!(!published_inventory.exists());
        fs::copy(&args.inventory_out, &published_inventory).unwrap();
        fs::remove_file(&args.request_out).unwrap();
        assert!(verify_prepared_inputs(&args).is_err());
        assert!(!args.request_out.exists());
        assert_eq!(fs::read(report_path).unwrap(), report_before);
        assert_eq!(fs::read(work_dir.join("count")).unwrap(), count_before);
    }

    #[cfg(unix)]
    #[test]
    fn materializer_timeout_and_output_bomb_kill_the_owned_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let output_root = root.path().join("output");
        let work_dir = root.path().join("work");
        fs::create_dir_all(&output_root).unwrap();
        fs::create_dir_all(&work_dir).unwrap();

        let mut request = args();
        request.output_root = output_root;
        request.materializer_work_dir = work_dir.clone();
        // Allow normal process startup while concurrent workspace checks are running.
        request.materializer_timeout_seconds = 3;
        request.max_materializer_output_bytes = 1024;

        let timeout_script = root.path().join("timeout.sh");
        fs::write(
            &timeout_script,
            r#"#!/bin/sh
set -eu
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --work-dir) work_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "$$" >"$work_dir/timeout-pid"
(sleep 5; printf late >"$work_dir/late-marker") &
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(&timeout_script, fs::Permissions::from_mode(0o700)).unwrap();
        request.materializer = timeout_script;
        let timeout_error = run_materializer(&request, Path::new("/tmp/frozen.env"))
            .expect_err("a descendant-held pipe must hit the preparation timeout");
        assert!(timeout_error.to_string().contains("bounded timeout"));
        thread::sleep(Duration::from_millis(100));
        assert!(!work_dir.join("late-marker").exists());
        let timeout_pid = fs::read_to_string(work_dir.join("timeout-pid")).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", timeout_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());

        let bomb_script = root.path().join("bomb.sh");
        fs::write(
            &bomb_script,
            r#"#!/bin/sh
set -eu
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --work-dir) work_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "$$" >"$work_dir/bomb-pid"
(sleep 5; printf late >"$work_dir/bomb-late-marker") &
dd if=/dev/zero bs=4096 count=8 2>/dev/null
"#,
        )
        .unwrap();
        fs::set_permissions(&bomb_script, fs::Permissions::from_mode(0o700)).unwrap();
        request.materializer_timeout_seconds = 10;
        request.materializer = bomb_script;
        let bomb_error = run_materializer(&request, Path::new("/tmp/frozen.env"))
            .expect_err("output overrun must terminate the preparation process group");
        assert!(bomb_error.to_string().contains("output budget"));
        thread::sleep(Duration::from_millis(100));
        assert!(!work_dir.join("bomb-late-marker").exists());
        let bomb_pid = fs::read_to_string(work_dir.join("bomb-pid")).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", bomb_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());

        let success_script = root.path().join("success.sh");
        fs::write(
            &success_script,
            r#"#!/bin/sh
set -eu
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --work-dir) work_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
(sleep 3; printf late >"$work_dir/success-late-marker") >/dev/null 2>&1 &
printf '%s\n' "$!" >"$work_dir/success-pid"
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(&success_script, fs::Permissions::from_mode(0o700)).unwrap();
        request.materializer = success_script;
        run_materializer(&request, Path::new("/tmp/frozen.env"))
            .expect("successful materializer must still clean descendants");
        thread::sleep(Duration::from_millis(100));
        assert!(!work_dir.join("success-late-marker").exists());
        let success_pid = fs::read_to_string(work_dir.join("success-pid")).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", success_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn legacy_materialization_report_is_not_parsed_as_sequence_json() {
        let root = tempfile::tempdir().unwrap();
        let report = root.path().join("materialization.json");
        fs::write(&report, b"materialization\n").unwrap();
        let item = CampaignInputItem {
            relative_path: PathBuf::from("materialization.json"),
            object_url: "https://bucket.example/materialization.json".into(),
            sha256: "a".repeat(64),
        };
        let mut request = args();
        assert!(verify_sequence_output(&request, root.path(), &item).is_ok());
        request.sequence_output = true;
        assert!(verify_sequence_output(&request, root.path(), &item).is_err());
        fs::write(
            &report,
            format!("{{\"sequence_manifest_sha256\":\"{}\"}}\n", "a".repeat(64)),
        )
        .unwrap();
        request.sequence_output = false;
        assert!(verify_sequence_output(&request, root.path(), &item).is_err());
        request.symbol = "SOLUSDT".into();
        request.market = "usdm".into();
        request.bucket_ms = 1000;
        request.top_depth = 5;
        request.label_horizon_buckets = 30;
        request.sequence_output = true;
        assert!(validate_args(&request).is_ok());
        request.symbol = "BTCUSDT".into();
        assert!(validate_args(&request).is_err());
    }
}
