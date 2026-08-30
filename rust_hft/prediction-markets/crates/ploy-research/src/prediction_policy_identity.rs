const DEPENDENCY_FINGERPRINT: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/prediction-policy-dependencies.txt"
));

#[cfg(test)]
const CHECKED_IN_DEPENDENCY_GRAPH: &str =
    include_str!("../prediction-policy-dependencies.linux.txt");

#[cfg(test)]
#[allow(dead_code)]
#[path = "../build.rs"]
mod policy_graph_build;

pub(crate) fn prediction_dependency_fingerprint() -> &'static str {
    DEPENDENCY_FINGERPRINT
}

#[cfg(test)]
fn checked_in_prediction_dependency_graph() -> &'static str {
    CHECKED_IN_DEPENDENCY_GRAPH
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        any::Any,
        panic::{catch_unwind, AssertUnwindSafe},
        path::Path,
    };

    #[test]
    fn runtime_identity_uses_a_canonical_checked_in_linux_graph_with_features() {
        let canonical = prediction_dependency_fingerprint().trim();
        assert!(
            canonical.strip_prefix("sha256:").is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            }),
            "the runtime policy dependency identity must be a canonical SHA-256"
        );

        let fingerprint = checked_in_prediction_dependency_graph();
        assert_eq!(
            canonical,
            format!("sha256:{:x}", Sha256::digest(fingerprint)),
            "the runtime identity must hash the exact checked-in Linux graph"
        );
        assert!(fingerprint.starts_with("prediction-policy-dependencies.v5\n"));
        assert!(fingerprint.contains("target=x86_64-unknown-linux-gnu\n"));
        assert!(fingerprint.contains("profile=default,db\n"));
        for input in [
            "Cargo.lock",
            "Cargo.toml",
            "crates/ploy-research/Cargo.toml",
            "crates/ploy-feed-loaders/Cargo.toml",
            "crates/ploy-market-contracts/Cargo.toml",
            "crates/ploy-market-data/Cargo.toml",
            "../data-pipelines/core/Cargo.toml",
            "../market-core/core/Cargo.toml",
            "../market-core/integration/Cargo.toml",
            "../market-core/ports/Cargo.toml",
            "../market-core/snapshot/Cargo.toml",
        ] {
            assert!(fingerprint.contains(&format!("input:{input}=sha256:")));
        }
        assert!(fingerprint.contains("package:sqlx-postgres@0.8.6|"));
        let sqlx = fingerprint
            .lines()
            .find(|line| line.starts_with("package:sqlx@0.8.6|"))
            .expect("resolved policy graph must include sqlx");
        assert!(sqlx
            .split("|features=")
            .nth(1)
            .is_some_and(|features| features.split(',').any(|feature| feature == "postgres")));
        assert!(!fingerprint.contains("package:sqlx-sqlite@"));
        for forbidden in [
            "ploy-operator-contracts",
            "ploy-strategy-bundles",
            "ploy-trading",
        ] {
            assert!(!fingerprint.contains(&format!("package:{forbidden}@")));
        }
        for excluded_package in ["core-foundation-sys", "security-framework", "sqlx-macros"] {
            assert!(
                !fingerprint.contains(&format!("package:{excluded_package}@")),
                "the Linux policy fingerprint must not include host or proc-macro dependency {excluded_package}"
            );
        }
    }

    #[test]
    fn checked_in_graph_validation_rejects_stale_and_forbidden_counterexamples() {
        let workspace_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("ploy-research must remain under <workspace>/crates");
        let graph = checked_in_prediction_dependency_graph();

        let lockfile_input = graph
            .lines()
            .find(|line| line.starts_with("input:Cargo.lock="))
            .expect("checked-in graph must pin Cargo.lock");
        let stale_lockfile_graph = graph.replacen(
            lockfile_input,
            "input:Cargo.lock=sha256:0000000000000000000000000000000000000000000000000000000000000000",
            1,
        );
        assert_graph_is_rejected(
            &stale_lockfile_graph,
            workspace_dir,
            "input Cargo.lock is stale",
        );

        for (package, expected) in [
            ("ploy-trading", "includes runtime authority ploy-trading"),
            ("sqlx-sqlite", "includes sqlx-sqlite"),
            (
                "core-foundation-sys",
                "includes host or proc-macro dependency core-foundation-sys",
            ),
            (
                "security-framework",
                "includes host or proc-macro dependency security-framework",
            ),
            (
                "sqlx-macros",
                "includes host or proc-macro dependency sqlx-macros",
            ),
        ] {
            let forbidden_package_graph = graph_with_package(graph, package);
            assert_graph_is_rejected(&forbidden_package_graph, workspace_dir, expected);
        }
    }

    fn graph_with_package(graph: &str, package: &str) -> String {
        let original = graph
            .lines()
            .find(|line| line.starts_with("package:"))
            .expect("checked-in graph must contain a package");
        let (_, fields) = original
            .strip_prefix("package:")
            .and_then(|package| package.split_once('|'))
            .expect("checked-in package must retain fields");
        graph.replacen(original, &format!("package:{package}@0.0.0|{fields}"), 1)
    }

    fn assert_graph_is_rejected(graph: &str, workspace_dir: &Path, expected: &str) {
        let panic = catch_unwind(AssertUnwindSafe(|| {
            policy_graph_build::validate_checked_in_graph(graph, workspace_dir);
        }))
        .expect_err("counterexample graph must fail closed");
        let message = panic_message(panic);
        assert!(
            message.contains(expected),
            "expected rejection {expected:?}, got {message:?}"
        );
    }

    fn panic_message(panic: Box<dyn Any + Send>) -> String {
        if let Some(message) = panic.downcast_ref::<String>() {
            return message.clone();
        }
        if let Some(message) = panic.downcast_ref::<&str>() {
            return (*message).to_owned();
        }
        "non-string panic payload".to_owned()
    }
}
