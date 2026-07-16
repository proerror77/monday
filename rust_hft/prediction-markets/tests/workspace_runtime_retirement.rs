use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use syn::visit::{self, Visit};
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

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("monday-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create temporary test directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn has_exact_cfg_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Ident>()
                .is_ok_and(|condition| condition == "test")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GatewayAliasRename {
    source: String,
    alias: String,
    test_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GatewayTraitAliases {
    production: BTreeSet<String>,
    tests: BTreeSet<String>,
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) | _ => &[],
    }
}

fn impl_item_attributes(item: &syn::ImplItem) -> &[Attribute] {
    match item {
        syn::ImplItem::Const(item) => &item.attrs,
        syn::ImplItem::Fn(item) => &item.attrs,
        syn::ImplItem::Type(item) => &item.attrs,
        syn::ImplItem::Macro(item) => &item.attrs,
        syn::ImplItem::Verbatim(_) | _ => &[],
    }
}

fn trait_item_attributes(item: &syn::TraitItem) -> &[Attribute] {
    match item {
        syn::TraitItem::Const(item) => &item.attrs,
        syn::TraitItem::Fn(item) => &item.attrs,
        syn::TraitItem::Type(item) => &item.attrs,
        syn::TraitItem::Macro(item) => &item.attrs,
        syn::TraitItem::Verbatim(_) | _ => &[],
    }
}

fn collect_gateway_alias_renames(items: &[Item], renames: &mut Vec<GatewayAliasRename>) {
    fn collect_use_tree(tree: &syn::UseTree, renames: &mut Vec<(String, String)>) {
        match tree {
            syn::UseTree::Rename(rename) => {
                renames.push((rename.ident.to_string(), rename.rename.to_string()));
            }
            syn::UseTree::Path(path) => collect_use_tree(&path.tree, renames),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    collect_use_tree(item, renames);
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
        }
    }

    struct AliasVisitor<'a> {
        inside_cfg_test: bool,
        renames: &'a mut Vec<GatewayAliasRename>,
    }

    impl<'ast> Visit<'ast> for AliasVisitor<'_> {
        fn visit_item(&mut self, item: &'ast Item) {
            let prior = self.inside_cfg_test;
            self.inside_cfg_test = prior || has_exact_cfg_test(item_attributes(item));
            visit::visit_item(self, item);
            self.inside_cfg_test = prior;
        }

        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            let prior = self.inside_cfg_test;
            self.inside_cfg_test = prior || has_exact_cfg_test(impl_item_attributes(item));
            visit::visit_impl_item(self, item);
            self.inside_cfg_test = prior;
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            let prior = self.inside_cfg_test;
            self.inside_cfg_test = prior || has_exact_cfg_test(trait_item_attributes(item));
            visit::visit_trait_item(self, item);
            self.inside_cfg_test = prior;
        }

        fn visit_item_use(&mut self, item_use: &'ast syn::ItemUse) {
            let mut renames = Vec::new();
            collect_use_tree(&item_use.tree, &mut renames);
            self.renames.extend(
                renames
                    .into_iter()
                    .map(|(source, alias)| GatewayAliasRename {
                        source,
                        alias,
                        test_only: self.inside_cfg_test,
                    }),
            );
            visit::visit_item_use(self, item_use);
        }
    }

    let mut visitor = AliasVisitor {
        inside_cfg_test: false,
        renames,
    };
    for item in items {
        visitor.visit_item(item);
    }
}

fn expand_gateway_aliases(
    renames: &[GatewayAliasRename],
    include_test_only: bool,
) -> BTreeSet<String> {
    let mut aliases = BTreeSet::from(["LiveExecutionGateway".to_string()]);
    loop {
        let before = aliases.len();
        for rename in renames {
            if (include_test_only || !rename.test_only) && aliases.contains(&rename.source) {
                aliases.insert(rename.alias.clone());
            }
        }
        if aliases.len() == before {
            return aliases;
        }
    }
}

fn gateway_trait_aliases<'a>(
    item_sets: impl IntoIterator<Item = &'a [Item]>,
) -> GatewayTraitAliases {
    let mut renames = Vec::new();
    for items in item_sets {
        collect_gateway_alias_renames(items, &mut renames);
    }
    GatewayTraitAliases {
        production: expand_gateway_aliases(&renames, false),
        tests: expand_gateway_aliases(&renames, true),
    }
}

fn gateway_target(
    implementation: &ItemImpl,
    gateway_trait_aliases: &BTreeSet<String>,
) -> Option<String> {
    let (_, trait_path, _) = implementation.trait_.as_ref()?;
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| !gateway_trait_aliases.contains(&segment.ident.to_string()))
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

fn macro_mentions_gateway(syntax: &syn::Macro, gateway_trait_aliases: &BTreeSet<String>) -> bool {
    syntax.path.segments.iter().any(|segment| {
        let identifier = segment.ident.to_string();
        gateway_trait_aliases.contains(&identifier)
            || identifier.to_ascii_lowercase().contains("gateway")
    }) || syntax
        .tokens
        .to_string()
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|token| gateway_trait_aliases.contains(token))
}

fn collect_gateway_items(
    source: &str,
    items: &[Item],
    inside_cfg_test: bool,
    implementations: &mut Vec<GatewayImplementation>,
) {
    let aliases = gateway_trait_aliases([items]);
    collect_gateway_items_with_aliases(source, items, inside_cfg_test, &aliases, implementations);
}

fn collect_gateway_items_with_aliases(
    source: &str,
    items: &[Item],
    inside_cfg_test: bool,
    aliases: &GatewayTraitAliases,
    implementations: &mut Vec<GatewayImplementation>,
) {
    let mut visitor = GatewayImplementationVisitor {
        source,
        inside_cfg_test,
        gateway_trait_aliases: aliases,
        implementations,
    };
    for item in items {
        visitor.visit_item(item);
    }
}

struct GatewayImplementationVisitor<'a> {
    source: &'a str,
    inside_cfg_test: bool,
    gateway_trait_aliases: &'a GatewayTraitAliases,
    implementations: &'a mut Vec<GatewayImplementation>,
}

impl<'ast> Visit<'ast> for GatewayImplementationVisitor<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let prior = self.inside_cfg_test;
        self.inside_cfg_test = prior || has_exact_cfg_test(item_attributes(item));
        visit::visit_item(self, item);
        self.inside_cfg_test = prior;
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let prior = self.inside_cfg_test;
        self.inside_cfg_test = prior || has_exact_cfg_test(impl_item_attributes(item));
        visit::visit_impl_item(self, item);
        self.inside_cfg_test = prior;
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let prior = self.inside_cfg_test;
        self.inside_cfg_test = prior || has_exact_cfg_test(trait_item_attributes(item));
        visit::visit_trait_item(self, item);
        self.inside_cfg_test = prior;
    }

    fn visit_item_impl(&mut self, implementation: &'ast ItemImpl) {
        let aliases = if self.inside_cfg_test {
            &self.gateway_trait_aliases.tests
        } else {
            &self.gateway_trait_aliases.production
        };
        if let Some(target) = gateway_target(implementation, aliases) {
            self.implementations.push(GatewayImplementation {
                source: self.source.to_string(),
                target,
                test_only: self.inside_cfg_test,
            });
        }
        visit::visit_item_impl(self, implementation);
    }

    fn visit_macro(&mut self, syntax: &'ast syn::Macro) {
        let aliases = if self.inside_cfg_test {
            &self.gateway_trait_aliases.tests
        } else {
            &self.gateway_trait_aliases.production
        };
        if macro_mentions_gateway(syntax, aliases) {
            let finding = GatewayImplementation {
                source: self.source.to_string(),
                target: "<unexpanded LiveExecutionGateway macro>".to_string(),
                test_only: self.inside_cfg_test,
            };
            if !self.implementations.contains(&finding) {
                self.implementations.push(finding);
            }
        }
        visit::visit_macro(self, syntax);
    }
}

struct ParsedGatewaySource {
    source: String,
    syntax: syn::File,
}

fn collect_gateway_sources(
    workspace_root: &Path,
    root: &Path,
    sources: &mut Vec<ParsedGatewaySource>,
    parse_failures: &mut Vec<String>,
) {
    for entry in fs::read_dir(root).expect("read active source directory") {
        let path = entry.expect("active source entry").path();
        if path.is_dir() {
            if is_non_execution_surface_directory(&path) {
                continue;
            }
            collect_gateway_sources(workspace_root, &path, sources, parse_failures);
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
            Ok(syntax) => sources.push(ParsedGatewaySource {
                source: relative,
                syntax,
            }),
            Err(error) => parse_failures.push(format!("{relative}: {error}")),
        }
    }
}

fn collect_gateway_implementations(
    workspace_root: &Path,
    root: &Path,
    implementations: &mut Vec<GatewayImplementation>,
    parse_failures: &mut Vec<String>,
) {
    let mut sources = Vec::new();
    collect_gateway_sources(workspace_root, root, &mut sources, parse_failures);
    let aliases =
        gateway_trait_aliases(sources.iter().map(|source| source.syntax.items.as_slice()));
    for source in sources {
        collect_gateway_items_with_aliases(
            &source.source,
            &source.syntax.items,
            false,
            &aliases,
            implementations,
        );
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
            use ploy_connectivity::LiveExecutionGateway as VenueGateway;

            impl
                ploy_connectivity::LiveExecutionGateway
                for VenueAuthenticatedClient {}

            impl VenueGateway for AliasedVenueAuthenticatedClient {}

            const _: () = {
                impl LiveExecutionGateway for BlockScopedVenueAuthenticatedClient {}
            };

            macro_rules! implement_gateway {
                ($target:ty) => {
                    impl LiveExecutionGateway for $target {}
                };
            }
            implement_gateway!(MacroGeneratedVenueAuthenticatedClient);

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
                target: "AliasedVenueAuthenticatedClient".to_string(),
                test_only: false,
            },
            GatewayImplementation {
                source: "fixture.rs".to_string(),
                target: "BlockScopedVenueAuthenticatedClient".to_string(),
                test_only: false,
            },
            GatewayImplementation {
                source: "fixture.rs".to_string(),
                target: "<unexpanded LiveExecutionGateway macro>".to_string(),
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
fn gateway_guard_fails_closed_on_unexpanded_production_macro() {
    let syntax = syn::parse_file(
        r#"
            install_live_execution_gateway!(MacroGeneratedVenueAuthenticatedClient);
        "#,
    )
    .expect("parse unexpanded production gateway macro fixture");
    let mut implementations = Vec::new();
    collect_gateway_items(
        "macro-fixture.rs",
        &syntax.items,
        false,
        &mut implementations,
    );

    assert_eq!(
        implementations,
        vec![GatewayImplementation {
            source: "macro-fixture.rs".to_string(),
            target: "<unexpanded LiveExecutionGateway macro>".to_string(),
            test_only: false,
        }]
    );
}

#[test]
fn gateway_guard_resolves_cross_file_alias_and_reexport_chains() {
    let fixture = TemporaryDirectory::new("cross-file-gateway-fixture");
    fs::write(
        fixture.path().join("aliases.rs"),
        "pub use ploy_connectivity::LiveExecutionGateway as VenueGateway;",
    )
    .expect("write gateway alias fixture");
    fs::write(
        fixture.path().join("client.rs"),
        r#"
            use crate::aliases::VenueGateway as ExecGateway;
            impl ExecGateway for CrossFileVenueClient {}
        "#,
    )
    .expect("write gateway client fixture");

    let mut implementations = Vec::new();
    let mut parse_failures = Vec::new();
    collect_gateway_implementations(
        fixture.path(),
        fixture.path(),
        &mut implementations,
        &mut parse_failures,
    );

    assert!(parse_failures.is_empty(), "{parse_failures:?}");
    assert_eq!(
        implementations,
        vec![GatewayImplementation {
            source: "client.rs".to_string(),
            target: "CrossFileVenueClient".to_string(),
            test_only: false,
        }]
    );
}

#[test]
fn gateway_guard_propagates_cfg_test_through_every_item_container() {
    let syntax = syn::parse_file(
        r#"
            #[cfg(test)]
            use ploy_connectivity::LiveExecutionGateway as TestOnlyGateway;

            #[cfg(test)]
            const TEST_GATEWAY: () = {
                impl LiveExecutionGateway for ConstFake {}
            };

            #[cfg(test)]
            fn install_function_fake() {
                impl LiveExecutionGateway for FunctionFake {}
            }

            struct FixtureContainer;
            impl FixtureContainer {
                #[cfg(test)]
                fn install_method_fake() {
                    impl LiveExecutionGateway for MethodFake {}
                }
            }

            trait FixtureTrait {
                #[cfg(test)]
                fn install_default_method_fake() {
                    impl LiveExecutionGateway for TraitMethodFake {}
                }
            }

            #[cfg(test)]
            install_live_execution_gateway!(MacroFake);

            trait TestOnlyGateway {}
            impl TestOnlyGateway for UnrelatedProductionType {}
        "#,
    )
    .expect("parse cfg(test) gateway fixture");
    let mut implementations = Vec::new();
    collect_gateway_items(
        "cfg-test-fixture.rs",
        &syntax.items,
        false,
        &mut implementations,
    );

    assert_eq!(
        implementations,
        vec![
            GatewayImplementation {
                source: "cfg-test-fixture.rs".to_string(),
                target: "ConstFake".to_string(),
                test_only: true,
            },
            GatewayImplementation {
                source: "cfg-test-fixture.rs".to_string(),
                target: "FunctionFake".to_string(),
                test_only: true,
            },
            GatewayImplementation {
                source: "cfg-test-fixture.rs".to_string(),
                target: "MethodFake".to_string(),
                test_only: true,
            },
            GatewayImplementation {
                source: "cfg-test-fixture.rs".to_string(),
                target: "TraitMethodFake".to_string(),
                test_only: true,
            },
            GatewayImplementation {
                source: "cfg-test-fixture.rs".to_string(),
                target: "<unexpanded LiveExecutionGateway macro>".to_string(),
                test_only: true,
            },
        ]
    );
}

#[test]
fn compiler_seal_rejects_cross_file_external_and_attribute_macro_gateway_impls() {
    let fixture = TemporaryDirectory::new("compiler-seal-fixture");
    let macro_provider = fixture.path().join("gateway-macros");
    let application = fixture.path().join("application");
    fs::create_dir_all(macro_provider.join("src")).expect("create macro provider source");
    fs::create_dir_all(application.join("src")).expect("create fixture application source");

    fs::write(
        fixture.path().join("Cargo.toml"),
        r#"
            [workspace]
            members = ["gateway-macros", "application"]
            resolver = "2"
        "#,
    )
    .expect("write fixture workspace manifest");
    fs::write(
        macro_provider.join("Cargo.toml"),
        r#"
            [package]
            name = "gateway-macros"
            version = "0.0.0"
            edition = "2021"

            [lib]
            proc-macro = true
        "#,
    )
    .expect("write macro provider manifest");
    fs::write(
        macro_provider.join("src/lib.rs"),
        r#"
            use proc_macro::TokenStream;

            #[proc_macro]
            pub fn install_external_gateway(_input: TokenStream) -> TokenStream {
                "impl ploy_connectivity::LiveExecutionGateway for ExternalMacroClient {}"
                    .parse()
                    .expect("valid external macro expansion")
            }

            #[proc_macro_attribute]
            pub fn install_attribute_gateway(
                _attribute: TokenStream,
                item: TokenStream,
            ) -> TokenStream {
                format!(
                    "{item} impl ploy_connectivity::LiveExecutionGateway for AttributeMacroClient {{}}"
                )
                .parse()
                .expect("valid attribute macro expansion")
            }
        "#,
    )
    .expect("write macro provider source");

    let connectivity = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/ploy-connectivity")
        .canonicalize()
        .expect("canonical connectivity path");
    fs::write(
        application.join("Cargo.toml"),
        format!(
            r#"
                [package]
                name = "gateway-seal-fixture"
                version = "0.0.0"
                edition = "2021"

                [dependencies]
                gateway-macros = {{ path = "../gateway-macros" }}
                ploy-connectivity = {{ path = "{}", default-features = false }}
            "#,
            connectivity.display()
        ),
    )
    .expect("write fixture application manifest");
    fs::write(
        application.join("src/lib.rs"),
        r#"
            use gateway_macros::{install_attribute_gateway, install_external_gateway};

            mod aliased;
            mod aliases;

            #[derive(Debug)]
            struct ExternalMacroClient;
            install_external_gateway!();

            #[install_attribute_gateway]
            #[derive(Debug)]
            struct AttributeMacroClient;
        "#,
    )
    .expect("write fixture application source");
    fs::write(
        application.join("src/aliases.rs"),
        "pub use ploy_connectivity::LiveExecutionGateway as VenueGateway;",
    )
    .expect("write compiler-seal alias fixture");
    fs::write(
        application.join("src/aliased.rs"),
        r#"
            use crate::aliases::VenueGateway as ExecGateway;

            #[derive(Debug)]
            pub struct CrossFileAliasedClient;

            impl ExecGateway for CrossFileAliasedClient {}
        "#,
    )
    .expect("write compiler-seal aliased implementation fixture");

    let output = Command::new(env!("CARGO"))
        .args(["check", "--offline", "-p", "gateway-seal-fixture"])
        .current_dir(fixture.path())
        .env("CARGO_TARGET_DIR", fixture.path().join("target"))
        .output()
        .expect("run compiler-seal fixture");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "macro gateway fixture compiled");
    assert!(stderr.contains("ExternalMacroClient"), "{stderr}");
    assert!(stderr.contains("AttributeMacroClient"), "{stderr}");
    assert!(stderr.contains("CrossFileAliasedClient"), "{stderr}");
    assert!(stderr.contains("ProductionExecutionSeal"), "{stderr}");
}

#[test]
fn gateway_test_support_feature_is_dev_only_and_registry_pinned() {
    fn collect_registry_entries(root: &Path, workspace_root: &Path, entries: &mut Vec<String>) {
        for entry in fs::read_dir(root).expect("read manifest registry directory") {
            let path = entry.expect("manifest registry entry").path();
            if path.is_dir() {
                if is_non_runtime_directory(&path) {
                    continue;
                }
                collect_registry_entries(&path, workspace_root, entries);
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) != Some("Cargo.toml") {
                continue;
            }

            let relative = path
                .strip_prefix(workspace_root)
                .expect("manifest must be inside workspace")
                .to_string_lossy()
                .replace('\\', "/");
            let mut section = String::new();
            for line in fs::read_to_string(&path)
                .expect("read manifest registry source")
                .lines()
            {
                let trimmed = line.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    section = trimmed.to_string();
                }
                if trimmed.contains("test-support") {
                    entries.push(format!("{relative}|{section}|{trimmed}"));
                }
            }
        }
    }

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut entries = Vec::new();
    collect_registry_entries(workspace_root, workspace_root, &mut entries);
    entries.sort();

    assert_eq!(
        entries,
        vec![
            "crates/ploy-connectivity/Cargo.toml|[features]|test-support = []",
            "crates/ploy-daemon-host/Cargo.toml|[dev-dependencies]|ploy-connectivity = { workspace = true, features = [\"test-support\"] }",
            "crates/ploy-platform-runtime/Cargo.toml|[dev-dependencies]|ploy-connectivity = { workspace = true, features = [\"test-support\"] }",
        ],
        "the compiler seal may be relaxed only for the two pinned unit-test crates and never by a production dependency"
    );
}

#[test]
fn gateway_defining_crate_has_a_pinned_production_macro_surface() {
    fn path_name(path: &syn::Path) -> String {
        path.segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::")
    }

    struct MacroSurfaceVisitor {
        inside_cfg_test: bool,
        attributes: BTreeMap<String, usize>,
        macros: BTreeMap<String, usize>,
        reviewed_macro_imports: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for MacroSurfaceVisitor {
        fn visit_item(&mut self, item: &'ast Item) {
            let prior = self.inside_cfg_test;
            self.inside_cfg_test = prior || has_exact_cfg_test(item_attributes(item));
            visit::visit_item(self, item);
            self.inside_cfg_test = prior;
        }

        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            let prior = self.inside_cfg_test;
            self.inside_cfg_test = prior || has_exact_cfg_test(impl_item_attributes(item));
            visit::visit_impl_item(self, item);
            self.inside_cfg_test = prior;
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            let prior = self.inside_cfg_test;
            self.inside_cfg_test = prior || has_exact_cfg_test(trait_item_attributes(item));
            visit::visit_trait_item(self, item);
            self.inside_cfg_test = prior;
        }

        fn visit_attribute(&mut self, attribute: &'ast Attribute) {
            if self.inside_cfg_test || attribute.path().is_ident("doc") {
                return;
            }
            if attribute.path().is_ident("derive") {
                let derives = attribute
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                    )
                    .expect("parse production derive registry");
                for derive in derives {
                    *self
                        .attributes
                        .entry(format!("derive:{}", path_name(&derive)))
                        .or_default() += 1;
                }
            } else {
                *self
                    .attributes
                    .entry(format!("attribute:{}", path_name(attribute.path())))
                    .or_default() += 1;
            }
            visit::visit_attribute(self, attribute);
        }

        fn visit_macro(&mut self, syntax: &'ast syn::Macro) {
            if !self.inside_cfg_test {
                *self.macros.entry(path_name(&syntax.path)).or_default() += 1;
            }
            visit::visit_macro(self, syntax);
        }

        fn visit_item_use(&mut self, item_use: &'ast syn::ItemUse) {
            fn collect_bindings(
                tree: &syn::UseTree,
                prefix: &mut Vec<String>,
                bindings: &mut Vec<(String, String)>,
            ) {
                match tree {
                    syn::UseTree::Path(path) => {
                        prefix.push(path.ident.to_string());
                        collect_bindings(&path.tree, prefix, bindings);
                        prefix.pop();
                    }
                    syn::UseTree::Name(name) => {
                        let mut full_path = prefix.clone();
                        full_path.push(name.ident.to_string());
                        bindings.push((name.ident.to_string(), full_path.join("::")));
                    }
                    syn::UseTree::Rename(rename) => {
                        let mut full_path = prefix.clone();
                        full_path.push(rename.ident.to_string());
                        bindings.push((rename.rename.to_string(), full_path.join("::")));
                    }
                    syn::UseTree::Group(group) => {
                        for tree in &group.items {
                            collect_bindings(tree, prefix, bindings);
                        }
                    }
                    syn::UseTree::Glob(_) => {}
                }
            }

            if !self.inside_cfg_test {
                let mut bindings = Vec::new();
                collect_bindings(&item_use.tree, &mut Vec::new(), &mut bindings);
                let reviewed_bindings = [
                    "Clone",
                    "Copy",
                    "Debug",
                    "Default",
                    "Eq",
                    "Error",
                    "Hash",
                    "PartialEq",
                    "cfg",
                    "default",
                    "error",
                    "must_use",
                ];
                self.reviewed_macro_imports
                    .extend(bindings.into_iter().filter_map(|(binding, path)| {
                        reviewed_bindings
                            .contains(&binding.as_str())
                            .then_some(path)
                    }));
            }
            visit::visit_item_use(self, item_use);
        }
    }

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(workspace_root.join("crates/ploy-connectivity/src/lib.rs"))
        .expect("read gateway-defining source");
    let syntax = syn::parse_file(&source).expect("parse gateway-defining source");
    let mut visitor = MacroSurfaceVisitor {
        inside_cfg_test: false,
        attributes: BTreeMap::new(),
        macros: BTreeMap::new(),
        reviewed_macro_imports: BTreeSet::new(),
    };
    visitor.visit_file(&syntax);

    assert_eq!(
        visitor.attributes,
        BTreeMap::from([
            ("attribute:cfg".to_string(), 2),
            ("attribute:default".to_string(), 1),
            ("attribute:error".to_string(), 3),
            ("attribute:must_use".to_string(), 2),
            ("derive:Clone".to_string(), 12),
            ("derive:Copy".to_string(), 2),
            ("derive:Debug".to_string(), 12),
            ("derive:Default".to_string(), 3),
            ("derive:Eq".to_string(), 5),
            ("derive:Error".to_string(), 1),
            ("derive:Hash".to_string(), 1),
            ("derive:PartialEq".to_string(), 11),
        ]),
        "new production attributes or derive macros in the seal-defining crate require explicit security review"
    );
    assert!(
        visitor.macros.is_empty(),
        "the seal-defining crate must not contain production function-like macro expansion sites: {:?}",
        visitor.macros
    );
    assert_eq!(
        visitor.reviewed_macro_imports,
        BTreeSet::from(["thiserror::Error".to_string()]),
        "reviewed derive and attribute names may only import the lockfile-pinned thiserror macro"
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
