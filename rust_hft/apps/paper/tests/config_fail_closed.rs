use std::{fs, process::Command};

#[test]
fn retired_venue_config_fails_startup() {
    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/dev/system.yaml"
    ))
    .expect("read known-good paper config");
    let retired = source.replace("venue_type: BINANCE", "venue_type: HYPERLIQUID");
    assert_ne!(retired, source, "fixture must contain the supported venue");

    let path = std::env::temp_dir().join(format!(
        "hft-paper-retired-venue-{}.yaml",
        std::process::id()
    ));
    fs::write(&path, retired).expect("write retired venue config");

    let output = Command::new(env!("CARGO_BIN_EXE_hft-paper"))
        .args(["--config", path.to_str().expect("UTF-8 temp path")])
        .output()
        .expect("run hft-paper");
    let _ = fs::remove_file(path);

    assert!(!output.status.success(), "retired venue must fail startup");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("retired runtime adapter"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
