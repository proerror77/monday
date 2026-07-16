const DEPENDENCY_FINGERPRINT: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/prediction-policy-dependencies.txt"
));

pub(crate) fn prediction_dependency_fingerprint() -> &'static str {
    DEPENDENCY_FINGERPRINT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_profile_is_linux_db_only_and_excludes_runtime_authority() {
        let fingerprint = prediction_dependency_fingerprint();
        assert!(fingerprint.starts_with("prediction-policy-dependencies.v2\n"));
        assert!(fingerprint.contains("target=x86_64-unknown-linux-gnu\n"));
        assert!(fingerprint.contains("profile=default,db\n"));
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
    }
}
