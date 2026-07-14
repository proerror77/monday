use std::fs;
use std::path::Path;

fn repo_file(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn release_platform_workflow_builds_new_workspace_binaries() {
    let content = repo_file(".github/workflows/release-platform.yml");
    let mut offenders = Vec::new();

    for needle in [
        "cargo build --release --locked",
        "new-ployd",
        "new-ploy-runner",
        "-p ploy-agent-sidecar",
        "-p ployctl",
        "-p ploytui",
        "bin/ployd",
        "bin/ploy-agent-sidecar",
        "bin/ployctl",
        "bin/ploytui",
        "deployment/ployd.service",
        "deployment/ploy-agent-sidecar.service",
        "deployment/ploy-maintenance.service",
        "deployment/ploy-maintenance.timer",
        "deployment/ploy-platform-watchdog.service",
        "deployment/ploy-platform-watchdog.timer",
        "scripts/install-platform-service.sh",
        "scripts/ploy_maintenance.sh",
        "scripts/ploy_platform_watchdog.sh",
        "ploytui",
    ] {
        if !content.contains(needle) {
            offenders.push(format!(
                "release-platform.yml: missing `{needle}` in the new platform release path"
            ));
        }
    }

    if content.contains("target/release/ploy") {
        offenders
            .push("release-platform.yml: still references legacy target/release/ploy".to_string());
    }
    if content.contains("uses: appleboy/") || content.contains("environment: production") {
        offenders.push(
            "release-platform.yml must remain build-only; named host workflows own deployment"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "platform release workflow guard failed:\n{}",
        offenders.join("\n")
    );
}
