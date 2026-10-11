//! Shared cluster IO. Pure object identity belongs to hft-research-artifacts.
//! This crate has no venue research or order execution authority.
use anyhow::{bail, Context};
use hft_research_artifacts::identity::{validate_dns_label, validate_identifier};
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub fn validate_cluster_target(context: &str, namespace: &str) -> anyhow::Result<()> {
    validate_identifier("Kubernetes context", context)?;
    validate_dns_label("namespace", namespace)
}

pub fn kubectl_binary() -> PathBuf {
    std::env::var_os("MONDAY_KUBECTL_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("kubectl"))
}

pub fn kubectl_json<const N: usize>(
    kubectl: &Path,
    context: &str,
    namespace: &str,
    args: [&str; N],
    action: &str,
) -> anyhow::Result<Value> {
    let output = Command::new(kubectl)
        .arg("--context")
        .arg(context)
        .arg("--namespace")
        .arg(namespace)
        .args(args)
        .output()
        .with_context(|| format!("start kubectl to {action}"))?;
    let stdout = ensure_kubectl_success(output, action)?;
    serde_json::from_slice(&stdout).with_context(|| format!("parse kubectl output for {action}"))
}

pub fn kubectl_with_input<const N: usize>(
    kubectl: &Path,
    context: &str,
    namespace: &str,
    args: [&str; N],
    input: &[u8],
) -> anyhow::Result<Output> {
    let mut child = Command::new(kubectl)
        .arg("--context")
        .arg(context)
        .arg("--namespace")
        .arg(namespace)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start kubectl Job creation")?;
    child
        .stdin
        .take()
        .context("kubectl stdin is unavailable")?
        .write_all(input)?;
    child.wait_with_output().context("wait for kubectl")
}

pub fn ensure_kubectl_success(output: Output, action: &str) -> anyhow::Result<Vec<u8>> {
    if !output.status.success() {
        bail!(
            "kubectl failed to {action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
