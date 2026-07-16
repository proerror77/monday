use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const POLICY_TARGET: &str = "x86_64-unknown-linux-gnu";
const FORBIDDEN_RUNTIME_PACKAGES: [&str; 3] = [
    "ploy-operator-contracts",
    "ploy-strategy-bundles",
    "ploy-trading",
];

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: PathBuf,
}

#[derive(Deserialize)]
struct Lockfile {
    package: Vec<LockPackage>,
}

#[derive(Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_dir = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("ploy-research must remain under <workspace>/crates");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_dir.join("Cargo.lock").display()
    );

    let output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(&manifest_dir)
        .args([
            "metadata",
            "--manifest-path",
            "Cargo.toml",
            "--format-version",
            "1",
            "--locked",
            "--features",
            "db",
        ])
        .output()
        .expect("run cargo metadata for the governed prediction policy profile");
    if !output.status.success() {
        panic!(
            "cargo metadata failed for the governed prediction policy profile: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .expect("parse cargo metadata for the governed prediction policy profile");
    let tree_output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(&manifest_dir)
        .args([
            "tree",
            "--manifest-path",
            "Cargo.toml",
            "--locked",
            "--features",
            "db",
            "--target",
            POLICY_TARGET,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}\t{f}",
        ])
        .output()
        .expect("run cargo tree for the governed prediction policy profile");
    if !tree_output.status.success() {
        panic!(
            "cargo tree failed for the governed prediction policy profile: {}",
            String::from_utf8_lossy(&tree_output.stderr)
        );
    }
    let lockfile: Lockfile = toml::from_str(
        &fs::read_to_string(workspace_dir.join("Cargo.lock"))
            .expect("read Cargo.lock for the governed prediction policy profile"),
    )
    .expect("parse Cargo.lock for the governed prediction policy profile");
    let fingerprint = normalized_active_graph(
        &metadata,
        &lockfile,
        &String::from_utf8(tree_output.stdout).expect("cargo tree output must be UTF-8"),
    )
    .expect("normalize governed prediction policy dependency graph");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("prediction-policy-dependencies.txt"),
        fingerprint,
    )
    .expect("write governed prediction policy dependency fingerprint");

    for package in &metadata.packages {
        if package.source.is_none() && package.manifest_path.starts_with(workspace_dir) {
            println!("cargo:rerun-if-changed={}", package.manifest_path.display());
        }
    }
}

fn normalized_active_graph(
    metadata: &Metadata,
    lockfile: &Lockfile,
    tree: &str,
) -> Result<String, String> {
    let mut packages = BTreeMap::<(&str, &str), Vec<&Package>>::new();
    for package in &metadata.packages {
        packages
            .entry((package.name.as_str(), package.version.as_str()))
            .or_default()
            .push(package);
    }
    let checksums = lockfile
        .package
        .iter()
        .map(|package| {
            (
                (
                    package.name.as_str(),
                    package.version.as_str(),
                    package.source.as_deref().unwrap_or("path"),
                ),
                package.checksum.as_deref().unwrap_or("none"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut normalized_packages = BTreeSet::new();
    let mut selected_names = BTreeSet::new();
    for line in tree.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.strip_suffix(" (*)").unwrap_or(line);
        let (display, features) = line
            .split_once('\t')
            .ok_or_else(|| format!("cargo tree line has no feature delimiter: {line}"))?;
        let mut display_parts = display.split_whitespace();
        let name = display_parts
            .next()
            .ok_or_else(|| format!("cargo tree line has no package name: {line}"))?;
        let version = display_parts
            .next()
            .and_then(|value| value.strip_prefix('v'))
            .ok_or_else(|| format!("cargo tree line has no package version: {line}"))?;
        let candidates = packages
            .get(&(name, version))
            .ok_or_else(|| format!("cargo metadata has no package for {name}@{version}"))?;
        let package = match candidates.as_slice() {
            [package] => *package,
            _ => {
                return Err(format!(
                    "cargo metadata package identity is ambiguous for {name}@{version}"
                ))
            }
        };
        let source = package.source.as_deref().unwrap_or("path");
        let checksum = checksums
            .get(&(package.name.as_str(), package.version.as_str(), source))
            .copied()
            .ok_or_else(|| {
                format!(
                    "Cargo.lock is missing {}@{} from {source}",
                    package.name, package.version
                )
            })?;
        let mut features = features
            .split(',')
            .filter(|feature| !feature.is_empty())
            .collect::<Vec<_>>();
        features.sort();
        selected_names.insert(package.name.as_str());
        normalized_packages.insert(format!(
            "{}@{}|source={}|checksum={}|features={}",
            package.name,
            package.version,
            source,
            checksum,
            features.join(",")
        ));
    }
    for forbidden in FORBIDDEN_RUNTIME_PACKAGES {
        if selected_names.contains(forbidden) {
            return Err(format!(
                "governed prediction policy graph includes runtime authority {forbidden}"
            ));
        }
    }

    let mut output =
        format!("prediction-policy-dependencies.v2\ntarget={POLICY_TARGET}\nprofile=default,db\n");
    for package in normalized_packages {
        output.push_str("package:");
        output.push_str(&package);
        output.push('\n');
    }
    Ok(output)
}
