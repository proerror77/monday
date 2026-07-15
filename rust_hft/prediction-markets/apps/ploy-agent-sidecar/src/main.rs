fn main() {
    if let Err(err) = ploy_agent_sidecar::run() {
        eprintln!("ploy-agent-sidecar failed: {err}");
        std::process::exit(1);
    }
}
