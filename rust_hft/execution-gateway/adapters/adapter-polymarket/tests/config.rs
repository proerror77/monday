use execution::ExecutionMode;
use execution_adapter_polymarket::{PolymarketExecutionConfig, WalletSignatureType};
use secrecy::SecretString;

fn private_key() -> SecretString {
    SecretString::new(
        "0x59c6995e998f97a5a0044976f7d7bb6c4df7f1a4a144c6f7d838d5f2f7f6a1f8".into(),
    )
}

#[test]
fn eoa_configuration_rejects_a_funder() {
    let config = PolymarketExecutionConfig {
        private_key: Some(private_key()),
        funder: Some("0x1111111111111111111111111111111111111111".into()),
        signature_type: WalletSignatureType::Eoa,
        mode: ExecutionMode::Live,
        ..PolymarketExecutionConfig::default()
    };

    let error = config.validate().expect_err("EOA must not use a funder");

    assert!(error.to_string().contains("must not be set for eoa"));
}
