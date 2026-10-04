use hft_research_input::{
    data::{DataViewSpec, FeatureFrame, Split, TypedBlock, Window},
    prepared, sha256,
};

#[test]
fn input_wire_and_manifest_identity() {
    let block = TypedBlock::Features(vec![FeatureFrame {
        segment: "session-a".into(),
        ordinal: 7,
        event_ns: 199,
        available_ns: 200,
        values: vec![1.0, 2.0, 3.0],
    }]);
    let bytes = prepared::encode(&block).unwrap();
    let spec = DataViewSpec {
        schema: 1,
        venue: "test-venue".into(),
        instrument: "test-instrument".into(),
        market: "usdm".into(),
        depth: 3,
        sources: vec!["a".repeat(64)],
        normalizer_sha256: "b".repeat(64),
        feature_sql_sha256: "c".repeat(64),
        feature_names: vec!["mid".into(), "spread".into(), "depth_imbalance".into()],
        window: Window {
            start_ns: 100,
            end_ns: 1000,
        },
        lookback_ns: 50,
        horizons_ns: vec![70, 150],
        label_tolerance_ns: 20,
        fit_cutoff_ns: 1100,
        split: Split::Train,
    };
    assert_eq!(
        sha256(&bytes),
        "c73e3c0e8950e432d4d40611cd601cf40139e59c5b8d3341f7f5a78bbff05890"
    );
    assert_eq!(
        spec.id().unwrap(),
        "6c61c787ec80085574048fe0d8c8b869d21177dc8e527789c40976e1cdcb392e"
    );
    assert_eq!(prepared::decode(&bytes).unwrap(), block);
}
