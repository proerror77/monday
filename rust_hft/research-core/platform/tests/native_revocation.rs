#![cfg(feature = "native-admission")]
use ed25519_dalek::SigningKey;
use hft_research_platform::{
    admission::NativeAdmissionTrust,
    identity,
    revocation::{sign_revocation, NativeRequestRevocation, NATIVE_REQUEST_REVOCATION_SCHEMA},
};
use std::collections::BTreeMap;

fn fixture() -> NativeRequestRevocation {
    NativeRequestRevocation {
        schema: NATIVE_REQUEST_REVOCATION_SCHEMA.into(),
        tenant: "fixture".into(),
        request_sha256: "a".repeat(64),
        operation_sha256: identity(&"original-structured-operation-id").unwrap(),
        family_id: "family".into(),
        root_grant_sha256: "b".repeat(64),
        reason_receipt_sha256: "c".repeat(64),
        effective_ms: 50_000,
        issued_ms: 1_000,
    }
}
fn trust(key: &SigningKey) -> NativeAdmissionTrust {
    NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: BTreeMap::from([(
            "host".into(),
            key.verifying_key()
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )]),
    }
}

#[test]
fn native_revocation_preserves_future_time_and_binds_every_source_identity() {
    let key = SigningKey::from_bytes(&[31; 32]);
    let mut trusted = trust(&key);
    trusted.native_reservation_keys.insert(
        "publisher".into(),
        trusted.native_reservation_keys["host"].clone(),
    );
    let signed = sign_revocation(fixture(), "host".into(), &key).unwrap();
    let verified = trusted.verify_revocation(&signed).unwrap();
    assert_eq!(verified.evidence(), &fixture());
    assert_eq!(verified.signed(), &signed);
    assert_eq!(verified.trust_sha256(), identity(&trusted).unwrap());
    for field in 0..9 {
        let mut changed = signed.clone();
        match field {
            0 => changed.evidence.tenant = "foreign".into(),
            1 => changed.evidence.request_sha256 = "d".repeat(64),
            2 => changed.evidence.operation_sha256 = "d".repeat(64),
            3 => changed.evidence.family_id = "foreign".into(),
            4 => changed.evidence.root_grant_sha256 = "d".repeat(64),
            5 => changed.evidence.reason_receipt_sha256 = "d".repeat(64),
            6 => changed.evidence.effective_ms += 1,
            7 => changed.evidence.issued_ms += 1,
            _ => changed.key_id = "publisher".into(),
        }
        changed.evidence_sha256 = changed.evidence.id().unwrap();
        assert!(trusted.verify_revocation(&changed).is_err());
    }
}

#[test]
fn native_revocation_rejects_weak_keys_and_another_signature_domain() {
    let key = SigningKey::from_bytes(&[31; 32]);
    let trusted = trust(&key);
    let mut signed = sign_revocation(fixture(), "host".into(), &key).unwrap();
    let mut weak = trusted.clone();
    weak.native_reservation_keys
        .insert("host".into(), format!("01{}", "00".repeat(31)));
    signed.signature_hex = format!("01{}", "00".repeat(63));
    assert!(weak.verify_revocation(&signed).is_err());
    use ed25519_dalek::Signer;
    signed.signature_hex = key
        .sign(
            format!(
                "monday.native_scientific_admission.v1:{}",
                signed.evidence_sha256
            )
            .as_bytes(),
        )
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(trusted.verify_revocation(&signed).is_err());
    let mut invalid = fixture();
    invalid.effective_ms = 0;
    assert!(invalid.validate().is_err());
    invalid = fixture();
    invalid.issued_ms = 0;
    assert!(invalid.validate().is_err());
}
