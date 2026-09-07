use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

const POLICY_TARGET: &str = "x86_64-unknown-linux-gnu";
const POLICY_GRAPH_SCHEMA: &str = "prediction-policy-dependencies.v5";
const POLICY_GRAPH_FILE: &str = "prediction-policy-dependencies.linux.txt";
const CANONICAL_POLICY_DEPENDENCY_HASH_FILE: &str = "prediction-policy-dependencies.linux.sha256";
const POLICY_INPUTS: [(&str, &str); 11] = [
    ("Cargo.lock", "Cargo.lock"),
    ("Cargo.toml", "Cargo.toml"),
    (
        "crates/ploy-research/Cargo.toml",
        "crates/ploy-research/Cargo.toml",
    ),
    (
        "crates/ploy-feed-loaders/Cargo.toml",
        "crates/ploy-feed-loaders/Cargo.toml",
    ),
    (
        "crates/ploy-market-contracts/Cargo.toml",
        "crates/ploy-market-contracts/Cargo.toml",
    ),
    (
        "crates/ploy-market-data/Cargo.toml",
        "crates/ploy-market-data/Cargo.toml",
    ),
    (
        "../data-pipelines/core/Cargo.toml",
        "../data-pipelines/core/Cargo.toml",
    ),
    (
        "../market-core/core/Cargo.toml",
        "../market-core/core/Cargo.toml",
    ),
    (
        "../market-core/integration/Cargo.toml",
        "../market-core/integration/Cargo.toml",
    ),
    (
        "../market-core/ports/Cargo.toml",
        "../market-core/ports/Cargo.toml",
    ),
    (
        "../market-core/snapshot/Cargo.toml",
        "../market-core/snapshot/Cargo.toml",
    ),
];
const FORBIDDEN_RUNTIME_PACKAGES: [&str; 3] = [
    "ploy-operator-contracts",
    "ploy-strategy-bundles",
    "ploy-trading",
];
const EXCLUDED_HOST_OR_PROC_MACRO_PACKAGES: [&str; 3] =
    ["core-foundation-sys", "security-framework", "sqlx-macros"];

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_dir = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("ploy-research must remain under <workspace>/crates");
    let graph_path = manifest_dir.join(POLICY_GRAPH_FILE);
    let canonical_hash_path = manifest_dir.join(CANONICAL_POLICY_DEPENDENCY_HASH_FILE);
    println!("cargo:rerun-if-changed={}", graph_path.display());
    println!("cargo:rerun-if-changed={}", canonical_hash_path.display());
    for (_, relative_path) in POLICY_INPUTS {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_dir.join(relative_path).display()
        );
    }

    let graph = fs::read_to_string(&graph_path).unwrap_or_else(|error| {
        panic!(
            "read checked-in Linux policy dependency graph {}: {error}",
            graph_path.display()
        )
    });
    validate_checked_in_graph(&graph, workspace_dir);

    let canonical_hash = read_canonical_dependency_hash(&canonical_hash_path);
    let graph_hash = dependency_fingerprint_hash(&graph);
    if canonical_hash != graph_hash {
        panic!(
            "checked-in Linux prediction-policy dependency graph hash {graph_hash} does not match {}; regenerate both reviewed policy graph artifacts together",
            canonical_hash_path.display()
        );
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(
        out_dir.join("prediction-policy-dependencies.txt"),
        format!("{canonical_hash}\n"),
    )
    .expect("write canonical governed prediction policy dependency hash");
}

pub(crate) fn validate_checked_in_graph(graph: &str, workspace_dir: &Path) {
    let mut lines = graph.lines();
    let headers = [
        POLICY_GRAPH_SCHEMA.to_owned(),
        format!("target={POLICY_TARGET}"),
        "profile=default,db".to_owned(),
    ];
    for expected in headers {
        let actual = lines.next().unwrap_or_else(|| {
            panic!("checked-in Linux policy dependency graph is missing {expected}")
        });
        if actual != expected {
            panic!(
                "checked-in Linux policy dependency graph expected {expected:?}, found {actual:?}"
            );
        }
    }
    for (label, relative_path) in POLICY_INPUTS {
        let expected = format!(
            "input:{label}={}",
            sha256_file(&workspace_dir.join(relative_path))
        );
        let actual = lines.next().unwrap_or_else(|| {
            panic!("checked-in Linux policy dependency graph is missing {expected}")
        });
        if actual != expected {
            panic!(
                "checked-in Linux policy dependency graph input {label} is stale: expected {expected:?}, found {actual:?}; regenerate the reviewed graph"
            );
        }
    }

    let mut package_count = 0usize;
    let mut has_sqlx_postgres = false;
    let mut sqlx_has_postgres_feature = false;
    for line in lines {
        let package = line.strip_prefix("package:").unwrap_or_else(|| {
            panic!("checked-in Linux policy dependency graph has invalid line {line:?}")
        });
        let (package_id, fields) = package.split_once('|').unwrap_or_else(|| {
            panic!("checked-in Linux policy dependency graph has invalid package {line:?}")
        });
        let (name, version) = package_id.split_once('@').unwrap_or_else(|| {
            panic!("checked-in Linux policy dependency graph has invalid package id {package_id:?}")
        });
        if name.is_empty()
            || version.is_empty()
            || !fields.starts_with("source=")
            || !fields.contains("|checksum=")
            || !fields.contains("|features=")
        {
            panic!("checked-in Linux policy dependency graph has invalid package {line:?}");
        }
        if FORBIDDEN_RUNTIME_PACKAGES.contains(&name) {
            panic!("checked-in Linux policy dependency graph includes runtime authority {name}");
        }
        if EXCLUDED_HOST_OR_PROC_MACRO_PACKAGES.contains(&name) {
            panic!(
                "checked-in Linux policy dependency graph includes host or proc-macro dependency {name}"
            );
        }
        if name == "sqlx-sqlite" {
            panic!("checked-in Linux policy dependency graph includes sqlx-sqlite");
        }
        has_sqlx_postgres |= name == "sqlx-postgres";
        if name == "sqlx" {
            let features = fields
                .split_once("|features=")
                .map(|(_, features)| features)
                .expect("validated sqlx package must include features");
            sqlx_has_postgres_feature |= features.split(',').any(|feature| feature == "postgres");
        }
        package_count += 1;
    }
    if package_count == 0 {
        panic!("checked-in Linux policy dependency graph has no packages");
    }
    if !has_sqlx_postgres || !sqlx_has_postgres_feature {
        panic!(
            "checked-in Linux policy dependency graph must retain the PostgreSQL sqlx runtime profile"
        );
    }
}

fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("read policy dependency input {}: {error}", path.display()));
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn read_canonical_dependency_hash(path: &Path) -> String {
    let value = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "read canonical Linux policy dependency hash {}: {error}",
            path.display()
        )
    });
    let value = value.trim();
    if !is_sha256_id(value) {
        panic!(
            "canonical Linux policy dependency hash {} must contain one sha256:<64 lowercase hexadecimal characters> value",
            path.display()
        );
    }
    value.to_owned()
}

fn dependency_fingerprint_hash(fingerprint: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(fingerprint))
}

fn is_sha256_id(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
