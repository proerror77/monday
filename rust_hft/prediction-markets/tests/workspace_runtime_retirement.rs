use std::fs;
use std::path::Path;
use std::process::Command;
use syn::{Attribute, Item, ItemImpl, Type};

const RETIRED_SOURCE_PATHS: &[&str] = &[
    ".dockerignore",
    ".env.production.example",
    "Dockerfile.collector",
    "Dockerfile.research",
    "Dockerfile.runner",
    "apps/ploy-runner",
    "config/production.example.toml",
    "deployment",
    "deploy-docker.sh",
    "docker-compose.prod.yml",
    "docker-compose.yml",
    "infra",
    "nginx.conf",
    "scripts/check_event_dataset_scope.sh",
    "scripts/check_event_dataset_verification_lane.sh",
    "start.sh",
    "stop.sh",
    "tools/polymarket-account-ops",
    "tools/predict-fun-account-ops",
    "tools/sdk_auth_check",
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

fn is_non_execution_surface_directory(path: &Path) -> bool {
    is_non_runtime_directory(path)
        || matches!(
            path.file_name().and_then(|value| value.to_str()),
            Some("docs" | "tasks" | "todos")
        )
}

fn is_active_execution_surface_file(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if file_name.starts_with("Dockerfile")
        || file_name.starts_with(".env")
        || matches!(
            file_name,
            ".dockerignore" | "Cargo.lock" | "Cargo.toml" | "Makefile" | "build.rs"
        )
    {
        return true;
    }

    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some(
            "bash"
                | "cjs"
                | "conf"
                | "css"
                | "html"
                | "js"
                | "json"
                | "lock"
                | "mjs"
                | "rs"
                | "service"
                | "sh"
                | "socket"
                | "timer"
                | "toml"
                | "ts"
                | "tsx"
                | "vue"
                | "yaml"
                | "yml"
                | "zsh"
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct GatewayImplementation {
    source: String,
    target: String,
    test_only: bool,
}

fn has_exact_cfg_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Ident>()
                .is_ok_and(|condition| condition == "test")
    })
}

fn gateway_target(implementation: &ItemImpl) -> Option<String> {
    let (_, trait_path, _) = implementation.trait_.as_ref()?;
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "LiveExecutionGateway")
    {
        return None;
    }

    match implementation.self_ty.as_ref() {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        _ => Some("<non-named-type>".to_string()),
    }
}

fn collect_gateway_items(
    source: &str,
    items: &[Item],
    inside_cfg_test: bool,
    implementations: &mut Vec<GatewayImplementation>,
) {
    for item in items {
        match item {
            Item::Impl(implementation) => {
                if let Some(target) = gateway_target(implementation) {
                    implementations.push(GatewayImplementation {
                        source: source.to_string(),
                        target,
                        test_only: inside_cfg_test || has_exact_cfg_test(&implementation.attrs),
                    });
                }
            }
            Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    collect_gateway_items(
                        source,
                        items,
                        inside_cfg_test || has_exact_cfg_test(&module.attrs),
                        implementations,
                    );
                }
            }
            _ => {}
        }
    }
}

fn collect_gateway_implementations(
    workspace_root: &Path,
    root: &Path,
    implementations: &mut Vec<GatewayImplementation>,
    parse_failures: &mut Vec<String>,
) {
    for entry in fs::read_dir(root).expect("read active source directory") {
        let path = entry.expect("active source entry").path();
        if path.is_dir() {
            if is_non_execution_surface_directory(&path) {
                continue;
            }
            collect_gateway_implementations(workspace_root, &path, implementations, parse_failures);
            continue;
        }

        if path == workspace_root.join("tests/workspace_runtime_retirement.rs")
            || path.extension().and_then(|value| value.to_str()) != Some("rs")
        {
            continue;
        }
        let relative = path
            .strip_prefix(workspace_root)
            .expect("source must be inside prediction-market workspace")
            .to_string_lossy()
            .replace('\\', "/");
        let body = fs::read_to_string(&path).expect("read Rust source");
        match syn::parse_file(&body) {
            Ok(file) => collect_gateway_items(&relative, &file.items, false, implementations),
            Err(error) => parse_failures.push(format!("{relative}: {error}")),
        }
    }
}

fn collect_authenticated_execution_surfaces(root: &Path, findings: &mut Vec<String>) {
    for entry in fs::read_dir(root).expect("read active source directory") {
        let path = entry.expect("active source entry").path();
        if path.is_dir() {
            if is_non_execution_surface_directory(&path) {
                continue;
            }
            collect_authenticated_execution_surfaces(&path, findings);
            continue;
        }

        if path.ends_with("tests/workspace_runtime_retirement.rs")
            || !is_active_execution_surface_file(&path)
        {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let forbidden = [
            ["Private", "KeySigner"].concat(),
            ["authentication", "_builder("].concat(),
            ["env::", "var(\"POLYMARKET_PRIVATE_KEY\")"].concat(),
            ["env::", "var(\"PRIVATE_KEY\")"].concat(),
            ["Polymarket", "ExecutionGateway"].concat(),
            ["new_", "authenticated("].concat(),
            ["authenticated_", "client("].concat(),
            [".", "post_order("].concat(),
            [".", "create_order("].concat(),
        ];
        for pattern in forbidden {
            if body.contains(&pattern) {
                findings.push(format!("{} contains {pattern}", path.display()));
            }
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
fn authenticated_execution_guard_covers_root_frontend_and_rust_syntax() {
    for active_path in [
        "Cargo.toml",
        "Dockerfile.runner",
        "ploy-frontend/src/services/api.ts",
    ] {
        assert!(
            is_active_execution_surface_file(Path::new(active_path)),
            "guard skipped active execution-capable path: {active_path}"
        );
    }

    let syntax = syn::parse_file(
        r#"
            impl
                ploy_connectivity::LiveExecutionGateway
                for VenueAuthenticatedClient {}

            #[cfg(test)]
            mod tests {
                impl LiveExecutionGateway for DeterministicFake {}
            }
        "#,
    )
    .expect("parse gateway fixture");
    let mut implementations = Vec::new();
    collect_gateway_items("fixture.rs", &syntax.items, false, &mut implementations);
    assert_eq!(
        implementations,
        vec![
            GatewayImplementation {
                source: "fixture.rs".to_string(),
                target: "VenueAuthenticatedClient".to_string(),
                test_only: false,
            },
            GatewayImplementation {
                source: "fixture.rs".to_string(),
                target: "DeterministicFake".to_string(),
                test_only: true,
            },
        ]
    );
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
fn prediction_market_workspace_is_a_monday_module() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let monday_root = workspace_root
        .parent()
        .and_then(Path::parent)
        .expect("prediction-market workspace must live inside Monday");
    let relative_workspace = workspace_root
        .strip_prefix(monday_root)
        .expect("prediction-market workspace must be below the Monday root");

    assert_eq!(
        relative_workspace,
        Path::new("rust_hft/prediction-markets"),
        "prediction-market code is a Monday market-family module, not a standalone product"
    );
    assert!(
        !monday_root.join("products/ploy").exists(),
        "the retired standalone-product path must not be recreated"
    );
    for adapter in [
        "rust_hft/data-pipelines/adapters/adapter-polymarket/Cargo.toml",
        "rust_hft/execution-gateway/adapters/adapter-polymarket/Cargo.toml",
    ] {
        assert!(
            monday_root.join(adapter).is_file(),
            "Monday must own the Polymarket adapter seam: {adapter}"
        );
    }
}

#[test]
fn compatibility_connectivity_has_no_concrete_polymarket_execution_adapter() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(workspace_root.join("crates/ploy-connectivity/Cargo.toml"))
        .expect("read compatibility connectivity manifest");
    let source = fs::read_to_string(workspace_root.join("crates/ploy-connectivity/src/lib.rs"))
        .expect("read compatibility connectivity source");

    for forbidden in [
        "polymarket-client-sdk",
        "polymarket_client_sdk",
        "alloy",
        "PRIVATE_KEY_VAR",
        "PolymarketExecutionGateway",
        "polymarket_execution_principal",
        "polymarket_account_readiness_from_env",
    ] {
        assert!(
            !manifest.contains(forbidden) && !source.contains(forbidden),
            "compatibility connectivity retains a second execution surface: {forbidden}"
        );
    }
    for required in [
        "pub struct DisabledLiveExecutionGateway",
        "MONDAY_LIVE_EXECUTION_DISABLED",
    ] {
        assert!(
            source.contains(required),
            "compatibility connectivity lost its production fail-closed boundary: {required}"
        );
    }
    assert!(
        !workspace_root.join("tools/sdk_auth_check").exists(),
        "prediction-market compatibility tooling must not read venue credentials directly"
    );
}

#[test]
fn active_compatibility_source_has_no_authenticated_venue_execution_surface() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut findings = Vec::new();
    collect_authenticated_execution_surfaces(workspace_root, &mut findings);
    assert!(
        findings.is_empty(),
        "authenticated venue execution escaped the canonical Monday Adapter:\n{}",
        findings.join("\n")
    );

    let mut implementations = Vec::new();
    let mut parse_failures = Vec::new();
    collect_gateway_implementations(
        workspace_root,
        workspace_root,
        &mut implementations,
        &mut parse_failures,
    );
    assert!(
        parse_failures.is_empty(),
        "gateway boundary could not parse active Rust source:\n{}",
        parse_failures.join("\n")
    );

    let production = implementations
        .iter()
        .filter(|implementation| !implementation.test_only)
        .collect::<Vec<_>>();
    assert_eq!(
        production,
        vec![&GatewayImplementation {
            source: "crates/ploy-connectivity/src/lib.rs".to_string(),
            target: "DisabledLiveExecutionGateway".to_string(),
            test_only: false,
        }],
        "the fail-closed gateway must be the only production-compiled LiveExecutionGateway; all fakes must be inside exact #[cfg(test)] modules"
    );
    assert!(
        implementations
            .iter()
            .filter(|implementation| { implementation.target != "DisabledLiveExecutionGateway" })
            .all(|implementation| implementation.test_only),
        "every LiveExecutionGateway fake must be compiled only under exact #[cfg(test)]"
    );
}

#[test]
fn monday_polymarket_data_service_is_read_only_and_fail_closed() {
    let ploy_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let monday_root = ploy_root
        .parent()
        .and_then(Path::parent)
        .expect("rust_hft/prediction-markets must live inside Monday");
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
    let forbidden = "record_market_updates_quote_depth_levels";
    assert!(
        !config.contains(forbidden),
        "full-depth collector config contains {forbidden}"
    );
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
