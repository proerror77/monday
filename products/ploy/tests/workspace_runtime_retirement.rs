use std::fs;
use std::path::Path;
use std::process::Command;

const RETIRED_SOURCE_PATHS: &[&str] = &[
    "apps/ploy-runner",
    "deployment/aws",
    "deploy-docker.sh",
    "docker-compose.prod.yml",
    "docker-compose.yml",
    "nginx.conf",
    "scripts/check_event_dataset_scope.sh",
    "scripts/check_event_dataset_verification_lane.sh",
    "start.sh",
    "stop.sh",
    "tools/polymarket-account-ops",
    "tools/predict-fun-account-ops",
    "ploy-openclaw",
    "src/CLAUDE.md",
    "src/account",
    "src/adapters",
    "src/agent_runtime.rs",
    "src/agents",
    "src/ai_clients",
    "src/analysis",
    "src/api",
    "src/cli",
    "src/collector",
    "src/config",
    "src/config.rs",
    "src/control_plane",
    "src/control_plane.rs",
    "src/coordination",
    "src/coordinator",
    "src/data_plane",
    "src/domain",
    "src/error.rs",
    "src/exchange",
    "src/main_agent_mode",
    "src/main_agent_mode.rs",
    "src/main_commands",
    "src/main_dispatch.rs",
    "src/main_modes",
    "src/main_modes.rs",
    "src/main_runtime.rs",
    "src/ml",
    "src/persistence",
    "src/platform",
    "src/plugins",
    "src/rl",
    "src/safety",
    "src/services",
    "src/signing",
    "src/strategy",
    "src/supervisor",
    "src/tui",
    "src/validation.rs",
];

const RETIRED_TEST_TARGETS: &[&str] = &[
    "examples/api_server.rs",
    "examples/backtest_gamma_scalping.rs",
    "examples/staggered_grid_backtest.rs",
    "examples/test_grok_agent.rs",
    "examples/test_winprob.rs",
    "tests/architecture_gateway_only.rs",
    "tests/engine_store_pg.rs",
    "tests/legacy_live_gate.rs",
    "tests/native_async_traits.rs",
    "tests/staging_workflow.rs",
    "tests/strategy_evaluations_and_deployment_gate.rs",
    "tests/workflow_migrations.rs",
];

fn is_non_runtime_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some(
            ".git"
                | "node_modules"
                | "target"
                | "archive"
                | "plans"
                | "reviews"
                | "superpowers"
                | "research_evidence"
                | "__pycache__"
                | ".pytest_cache"
                | ".venv"
                | "venv"
        )
    )
}

fn is_forbidden_python_artifact(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(
        extension.as_deref(),
        Some("py" | "pyw" | "ipynb" | "pyc" | "pyo" | "pyd")
    ) {
        return true;
    }

    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let lower_name = file_name.to_ascii_lowercase();
    matches!(
        lower_name.as_str(),
        "pyproject.toml" | "pipfile" | "poetry.lock" | "pytest.ini" | "tox.ini" | "setup.py"
    ) || (lower_name.starts_with("requirements") && lower_name.ends_with(".txt"))
}

fn is_python_command_name(token: &str) -> bool {
    let command = token.trim_matches(|value: char| {
        matches!(value, '"' | '\'' | '`' | '[' | ']' | '{' | '}' | ',' | ':')
    });
    let command = command.rsplit('/').next().unwrap_or(command);
    if matches!(
        command,
        "python"
            | "pip"
            | "pipx"
            | "pytest"
            | "poetry"
            | "pdm"
            | "rye"
            | "conda"
            | "mamba"
            | "hatch"
            | "tox"
            | "virtualenv"
            | "uv"
    ) {
        return true;
    }

    ["python", "pip"].iter().any(|prefix| {
        command.strip_prefix(prefix).is_some_and(|version| {
            !version.is_empty()
                && version.split('.').all(|part| {
                    !part.is_empty() && part.chars().all(|value| value.is_ascii_digit())
                })
        })
    })
}

fn line_invokes_python(line: &str) -> bool {
    line.contains("actions/setup-python@")
        || line.contains("astral-sh/setup-uv@")
        || line
            .split(|value: char| {
                value.is_whitespace() || matches!(value, ';' | '&' | '|' | '(' | ')' | '/')
            })
            .any(is_python_command_name)
}

fn is_python_runtime_crate(name: &str) -> bool {
    matches!(
        name,
        "cpython" | "rust-cpython" | "python3-sys" | "rustpython"
    ) || name.starts_with("pyo3")
        || name.starts_with("rustpython-")
}

fn declares_python_runtime_crate(line: &str) -> bool {
    let trimmed = line.trim();
    if let Some(name) = trimmed
        .strip_prefix("name = \"")
        .and_then(|name| name.strip_suffix('"'))
    {
        return is_python_runtime_crate(name);
    }
    trimmed
        .split_once('=')
        .is_some_and(|(name, _)| is_python_runtime_crate(name.trim()))
}

fn should_scan_inline_commands(path: &Path) -> bool {
    let extension = path.extension().and_then(|value| value.to_str());
    extension.is_none()
        || matches!(
            extension,
            Some(
                "sh" | "bash"
                    | "zsh"
                    | "yml"
                    | "yaml"
                    | "toml"
                    | "service"
                    | "timer"
                    | "socket"
                    | "conf"
            )
        )
}

fn collect_forbidden_language_paths(root: &Path, paths: &mut Vec<String>) {
    for entry in fs::read_dir(root).expect("read workspace directory") {
        let path = entry.expect("workspace entry").path();
        if path.is_dir() {
            if is_non_runtime_directory(&path) {
                continue;
            }
            collect_forbidden_language_paths(&path, paths);
            continue;
        }

        if is_forbidden_python_artifact(&path) {
            paths.push(path.display().to_string());
            continue;
        }

        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        if matches!(
            path.file_name().and_then(|value| value.to_str()),
            Some("Cargo.toml" | "Cargo.lock")
        ) && body.lines().any(declares_python_runtime_crate)
        {
            paths.push(format!("{} (Python runtime Rust crate)", path.display()));
            continue;
        }
        let first_line = body.lines().next().unwrap_or_default();
        if first_line.starts_with("#!") && line_invokes_python(first_line) {
            paths.push(format!("{} (Python shebang)", path.display()));
        } else if should_scan_inline_commands(&path)
            && body
                .lines()
                .filter(|line| !line.trim_start().starts_with('#'))
                .any(line_invokes_python)
        {
            paths.push(format!("{} (inline Python command)", path.display()));
        }
    }
}

#[test]
fn python_command_detector_covers_suffixless_wrappers_and_inline_invocations() {
    let interpreter = ["py", "thon"].concat();
    assert!(line_invokes_python(&format!(
        "#!/usr/bin/env {interpreter}3"
    )));
    assert!(line_invokes_python(&format!(
        "#!/usr/bin/{interpreter}3.12 -u"
    )));
    assert!(line_invokes_python(&format!(
        "run: {interpreter} -m package"
    )));
    assert!(line_invokes_python(&format!(
        "exec /usr/bin/{interpreter}3 worker"
    )));
    assert!(line_invokes_python("uses: actions/setup-python@v5"));
    assert!(line_invokes_python("run: pip install package"));
    assert!(line_invokes_python("run: uv sync"));
    assert!(declares_python_runtime_crate("pyo3 = \"0.24\""));
    assert!(declares_python_runtime_crate("name = \"pyo3-ffi\""));
    assert!(!line_invokes_python("PYTHONPATH=/opt/rust-only"));
}

#[test]
fn workspace_is_rust_only_outside_the_frontend() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut forbidden = Vec::new();
    collect_forbidden_language_paths(repo_root, &mut forbidden);
    assert!(
        forbidden.is_empty(),
        "non-Rust research/runtime language surface remains:\n{}",
        forbidden.join("\n")
    );
    assert!(
        !repo_root.join(".github/workflows").exists(),
        "nested historical workflows must not be restored"
    );
}

#[test]
fn workspace_root_keeps_only_the_shim_surface() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut still_present = Vec::new();
    for relative_path in RETIRED_SOURCE_PATHS
        .iter()
        .chain(RETIRED_TEST_TARGETS.iter())
    {
        if repo_root.join(relative_path).exists() {
            still_present.push(relative_path.to_string());
        }
    }

    assert!(
        still_present.is_empty(),
        "legacy root runtime paths still present:\n{}",
        still_present.join("\n")
    );
}

#[test]
fn standalone_platform_installer_is_a_fail_closed_tombstone() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let installer = repo_root.join("scripts/install-platform-service.sh");
    let body = fs::read_to_string(&installer).expect("read retired platform installer");

    let output = Command::new("bash")
        .arg(&installer)
        .output()
        .expect("run retired platform installer");

    assert_eq!(output.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("standalone PLOY platform installer is retired and disabled in Monday"));
    for forbidden in ["sudo", "systemctl", "useradd", "/opt/ploy"] {
        assert!(
            !body.contains(forbidden),
            "retired platform installer still contains host mutation command: {forbidden}"
        );
    }
}

#[test]
fn openclaw_compatibility_example_rejects_remote_mutations() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rpc = repo_root.join("examples/openclaw/skill-ploy-rpc/bin/ployrpc");
    let ctl = repo_root.join("examples/openclaw/skill-ploy-rpc/bin/ployctl");

    let rpc_output = Command::new("bash")
        .arg(&rpc)
        .arg("pm.submit_limit")
        .arg("{}")
        .env_remove("PLOY_TRADING_HOST")
        .env_remove("PLOY_TRADING_SSH_OPTS")
        .output()
        .expect("run read-only RPC wrapper");
    assert!(!rpc_output.status.success());
    assert!(String::from_utf8_lossy(&rpc_output.stderr).contains("disabled in Monday"));

    let ctl_output = Command::new("bash")
        .arg(&ctl)
        .arg("stop")
        .env_remove("PLOY_TRADING_HOST")
        .env_remove("PLOY_TRADING_SSH_OPTS")
        .output()
        .expect("run read-only control wrapper");
    assert!(!ctl_output.status.success());
    assert!(String::from_utf8_lossy(&ctl_output.stderr).contains("disabled in Monday"));

    for relative in [
        "examples/openclaw/README.md",
        "examples/openclaw/skill-ploy-rpc/SKILL.md",
        "examples/openclaw/skill-ploy-rpc/prompts/autonomous_event_trader.md",
        "examples/openclaw/skill-ploy-rpc/prompts/autonomous_multi_source_trader.md",
        "docs/OPENCLAW_INTEGRATION.md",
    ] {
        let body = fs::read_to_string(repo_root.join(relative)).expect("read OpenClaw document");
        for forbidden in [
            "PLOY_RPC_WRITE_ENABLED",
            "pm.submit_limit",
            "pm.cancel_order",
        ] {
            assert!(
                !body.contains(forbidden),
                "{relative} still advertises retired write surface {forbidden}"
            );
        }
    }
}

fn collect_markdown_files(root: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(root).expect("read documentation directory") {
        let path = entry.expect("documentation entry").path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|value| value.to_str()),
                Some("archive" | "dist" | "node_modules" | "target")
            ) {
                continue;
            }
            collect_markdown_files(&path, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            files.push(path);
        }
    }
}

#[test]
fn standalone_operational_docs_are_explicitly_marked_historical() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for relative_root in [
        "docs",
        "config",
        "examples",
        "ploy-frontend",
        "apps/ploy-agent-sidecar",
        "tasks",
    ] {
        collect_markdown_files(&repo_root.join(relative_root), &mut files);
    }

    let markers = [
        ".github/workflows/deploy-",
        ".github/workflows/approve-",
        ".github/workflows/release-platform",
        "/opt/ploy",
        "tango-1-1",
        "ploy-trade-1",
        "systemctl",
        "PLOY_RPC_WRITE_ENABLED",
        "pm.submit_limit",
        "pm.cancel_order",
        "vercel --prod",
        "git remote add origin",
    ];
    let current_exceptions = ["docs/operations/data-jobs-inventory.md"];
    let mut unmarked = Vec::new();

    for path in files {
        let relative = path
            .strip_prefix(repo_root)
            .expect("documentation path under repository root")
            .to_string_lossy()
            .replace('\\', "/");
        if relative.starts_with("docs/archive/")
            || relative.starts_with("docs/plans/")
            || relative.starts_with("docs/reviews/")
            || relative.starts_with("docs/superpowers/")
            || relative.starts_with("tasks/research_evidence/")
            || (relative.starts_with("tasks/") && relative.contains("_audit_"))
            || current_exceptions.contains(&relative.as_str())
        {
            continue;
        }

        let body = fs::read_to_string(&path).expect("read operational document");
        if markers.iter().any(|marker| body.contains(marker))
            && !body.contains("Historical standalone PLOY")
        {
            unmarked.push(relative);
        }
    }

    assert!(
        unmarked.is_empty(),
        "standalone operational docs lack the historical marker:\n{}",
        unmarked.join("\n")
    );
}

#[test]
fn default_config_does_not_advertise_live_enablement() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let body = fs::read_to_string(repo_root.join("config/default.toml"))
        .expect("read default configuration");

    for retired_instruction in [
        "To enable any live order path",
        "complete the explicit deployment checklist",
    ] {
        assert!(
            !body.contains(retired_instruction),
            "default config still advertises retired live enablement: {retired_instruction}"
        );
    }
    assert!(
        body.contains("separate reviewed Monday change"),
        "default config must name the reviewed Monday authority gate"
    );
}

#[test]
fn monday_polymarket_data_service_is_read_only_and_fail_closed() {
    let ploy_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let monday_root = ploy_root
        .parent()
        .and_then(Path::parent)
        .expect("products/ploy must live inside Monday");
    let config =
        fs::read_to_string(monday_root.join("deployment/aliyun/polymarket-market-tape.toml"))
            .expect("read Polymarket market-data config");
    let service =
        fs::read_to_string(monday_root.join("deployment/aliyun/polymarket-market-tape.service"))
            .expect("read Polymarket market-data service");
    let reference_service = fs::read_to_string(
        monday_root.join("deployment/aliyun/polymarket-reference-collector.service"),
    )
    .expect("read Polymarket reference service");
    let market_upload_service = fs::read_to_string(
        monday_root.join("deployment/aliyun/polymarket-market-tape-upload.service"),
    )
    .expect("read Polymarket market tape upload service");
    let reference_upload_service = fs::read_to_string(
        monday_root.join("deployment/aliyun/polymarket-reference-upload.service"),
    )
    .expect("read Polymarket reference upload service");

    for required in [
        "mode = \"dryrun\"",
        "strategy_variant = \"noop\"",
        "market_data_source = \"external_direct\"",
        "record_market_updates_to = \"/data/monday/spool/polymarket/market-updates.ndjson\"",
        "record_market_updates_include_kinds = [\"quote\", \"event_discovered\", \"event_expired\", \"reference_price\"]",
        "record_market_updates_quote_sample_ms = 1000",
        "record_market_updates_event_scoped_quotes = true",
        "symbols = [\"BTCUSDT\", \"ETHUSDT\", \"SOLUSDT\", \"XRPUSDT\", \"DOGEUSDT\", \"HYPEUSDT\", \"BNBUSDT\"]",
    ] {
        assert!(config.contains(required), "config missing {required}");
    }
    for required in [
        "User=hftcollector",
        "--dry-run",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ReadWritePaths=/data/monday/spool/polymarket",
    ] {
        assert!(service.contains(required), "service missing {required}");
    }
    for forbidden in ["live-execution", "PRIVATE_KEY", "EnvironmentFile="] {
        assert!(!service.contains(forbidden), "service contains {forbidden}");
        assert!(
            !reference_service.contains(forbidden),
            "reference service contains {forbidden}"
        );
    }
    for forbidden in ["record_market_updates_quote_depth_levels"] {
        assert!(
            !config.contains(forbidden),
            "full-depth collector config contains {forbidden}"
        );
    }
    for required in [
        "User=hftcollector",
        "/opt/monday/bin/polymarket-raw-ops collect-reference",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ReadWritePaths=/data/monday/spool/polymarket-reference",
    ] {
        assert!(
            reference_service.contains(required),
            "reference service missing {required}"
        );
    }
    for (name, upload_service) in [
        ("market", &market_upload_service),
        ("reference", &reference_upload_service),
    ] {
        assert!(
            upload_service.contains("/opt/monday/bin/polymarket-raw-ops upload"),
            "{name} uploader must use the Rust raw-ops binary"
        );
        for forbidden in ["python3", ".py", "PRIVATE_KEY", "live-execution"] {
            assert!(
                !upload_service.contains(forbidden),
                "{name} uploader contains forbidden runtime surface {forbidden}"
            );
        }
    }
    for retired in [
        "polymarket_reference_collector.py",
        "polymarket_market_tape_upload.py",
        "polymarket_reference_canonicalize.py",
    ] {
        assert!(
            !monday_root.join("deployment/aliyun").join(retired).exists(),
            "retired Python runtime still exists: {retired}"
        );
    }
}
