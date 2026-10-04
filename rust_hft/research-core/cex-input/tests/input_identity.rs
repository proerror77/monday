use hft_cex_research_input::{
    data::{
        BlockRef, BlockSource, DataViewSpec, Exit, FeatureFrame, PublishedView, Split, TypedBlock,
        VerifiedCache, Window,
    },
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

#[test]
fn cached_manifest_reuse_still_rejects_changed_metadata_and_clock() -> anyhow::Result<()> {
    let typed = TypedBlock::Features(vec![FeatureFrame {
        segment: "session-a".into(),
        ordinal: 7,
        event_ns: 199,
        available_ns: 200,
        values: vec![1.0],
    }]);
    let bytes = prepared::encode(&typed)?;
    struct Source {
        bytes: Vec<u8>,
        reads: usize,
    }
    impl BlockSource for Source {
        fn read(&mut self, _: &BlockRef) -> anyhow::Result<Vec<u8>> {
            self.reads += 1;
            Ok(self.bytes.clone())
        }
    }
    let view = PublishedView {
        prepared_id: "a".repeat(64),
        spec: DataViewSpec {
            schema: 1,
            venue: "fixture".into(),
            instrument: "fixture".into(),
            market: "usdm".into(),
            depth: 1,
            sources: vec!["a".repeat(64)],
            normalizer_sha256: "b".repeat(64),
            feature_sql_sha256: "c".repeat(64),
            feature_names: vec!["x".into()],
            window: Window {
                start_ns: 100,
                end_ns: 1000,
            },
            lookback_ns: 50,
            horizons_ns: vec![100],
            label_tolerance_ns: 0,
            fit_cutoff_ns: 1000,
            split: Split::Train,
        },
        blocks: vec![BlockRef {
            sha256: sha256(&bytes),
            bytes: bytes.len() as u64,
            rows: 1,
            decoded_bytes: hft_cex_research_input::data::memory_bytes(&typed),
            exit: Exit::Features,
        }],
        producer_image: format!("fixture@sha256:{}", "d".repeat(64)),
        source_receipt_sha256: "e".repeat(64),
    };
    let id = hft_cex_research_input::identity(&view)?;
    let mut source = Source { bytes, reads: 0 };
    let mut cache = VerifiedCache::new(1024 * 1024)?;
    cache.load(&view, &id, Exit::Features, &mut source)?;
    cache.load(&view, &id, Exit::Features, &mut source)?;
    let mut changed = view.clone();
    changed.spec.feature_names[0] = "different".into();
    assert!(cache
        .load(&changed, &id, Exit::Features, &mut source)
        .is_err());
    let mut changed = view.clone();
    changed.spec.window.start_ns = 300;
    let changed_id = hft_cex_research_input::identity(&changed)?;
    assert!(cache
        .load(&changed, &changed_id, Exit::Features, &mut source)
        .is_err());
    assert_eq!(source.reads, 1);
    Ok(())
}
