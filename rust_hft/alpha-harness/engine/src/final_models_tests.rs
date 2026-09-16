use super::*;
use alpha_domain::{
    CexBaselineFoldV1, CexBaselineModelV1, CexBaselineRangeV1, CexGpPolicyV1, SearchBudget,
};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};

const FLOW: &str = alpha_domain::CEX_RESEARCH_AGGREGATE_TRADE_FLOW_IMBALANCE_FIELD;
const FIELDS: [&str; 9] = [
    FLOW,
    "book_imbalance",
    "book_imbalance_top5",
    "weighted_book_imbalance_top5",
    "near_depth_concentration_skew_top5",
    "vwap_center_deviation_top5_bps",
    "spread_bps",
    "bid_depth_top5",
    "ask_depth_top5",
];

fn policy() -> CexGpPolicyV1 {
    CexGpPolicyV1::controlled_dynamic_v4(
        "h1-research-test",
        FIELDS.iter().map(|s| (*s).into()).collect(),
        7,
        &SearchBudget {
            max_candidates: FIELDS.len() * 2 + 4,
            max_expansions: 1,
            max_tokens: 0,
            max_seconds: 0,
        },
    )
    .unwrap()
}

fn field(name: &str) -> FactorAst {
    FactorAst::Terminal(FactorTerminal::Field(name.into()))
}

fn rows() -> Vec<ResearchRow> {
    let start = chrono::DateTime::parse_from_rfc3339("2026-09-12T05:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    (0..64)
        .map(|i| ResearchRow {
            series_id: 1,
            available_time: start + chrono::TimeDelta::seconds(i),
            label_available_time: start + chrono::TimeDelta::seconds(i + 5),
            signal: 999.0,
            label: -999.0,
            fee_bps: 2.0,
            funding_bps: 0.0,
            pit_funding: false,
            latency_bps: 0.5,
            features: FIELDS
                .iter()
                .enumerate()
                .map(|(j, s)| ((*s).into(), (i as f64 + j as f64 + 1.0) / 100.0))
                .collect(),
        })
        .collect()
}

fn fold(width: usize) -> CexBaselineFoldV1 {
    CexBaselineFoldV1::new(
        1,
        CexBaselineRangeV1 { start: 0, end: 5 },
        CexBaselineRangeV1 { start: 5, end: 10 },
        CexBaselineRangeV1 { start: 10, end: 20 },
        CexBaselineRangeV1 { start: 20, end: 21 },
        vec![0.0; 10],
        CexBaselineModelV1::Ridge {
            intercept: 0.125,
            means: (0..width).map(|i| 0.25 * (i + 1) as f64).collect(),
            scales: (0..width).map(|i| 2.0 + i as f64).collect(),
            coefficients: (0..width).map(|i| 0.5 / (i + 1) as f64).collect(),
        },
    )
    .unwrap()
}

#[test]
fn research_model_uses_all_nine_fields_and_preserves_live_rejection() {
    let fold = fold(FIELDS.len());
    let fitted = FittedInputs {
        fold: &fold,
        protocol_hash: "a".repeat(64),
        factors: FIELDS
            .iter()
            .enumerate()
            .map(|(i, s)| FrozenModelFactorV1 {
                ast: field(s),
                negative: i == 0,
            })
            .collect(),
    };
    let rows = rows();
    let predictions = research_predictions(&rows, &fitted, &policy()).unwrap();
    for (row, prediction) in rows.iter().zip(&predictions) {
        let features = FIELDS
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if i == 0 {
                    -row.features[*s]
                } else {
                    row.features[*s]
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            prediction.to_bits(),
            fold.model.predict(&features).unwrap().to_bits()
        );
    }
    assert_eq!(policy().candidate_history_rows(&field(FLOW)).unwrap(), 1);
    assert!(hft_factor_dsl::validate_live_formula(&field(FLOW))
        .unwrap_err()
        .to_string()
        .contains("unsupported live field"));
    let mut changed = rows.clone();
    for row in &mut changed {
        row.label = 123.0;
        row.signal = -456.0;
    }
    assert_eq!(
        predictions,
        research_predictions(&changed, &fitted, &policy()).unwrap()
    );
    changed[40].features.insert(FLOW.into(), -0.99);
    let perturbed = research_predictions(&changed, &fitted, &policy()).unwrap();
    assert_eq!(&predictions[..40], &perturbed[..40]);
    assert_ne!(predictions[40], perturbed[40]);
}

#[test]
fn research_model_rejects_missing_nonfinite_and_unregistered_features() {
    let fold = fold(1);
    let mut fitted = FittedInputs {
        fold: &fold,
        protocol_hash: "a".repeat(64),
        factors: vec![FrozenModelFactorV1 {
            ast: field(FLOW),
            negative: false,
        }],
    };
    let mut missing = rows();
    missing[12].features.remove(FLOW);
    assert!(research_predictions(&missing, &fitted, &policy())
        .unwrap_err()
        .contains("not registered"));
    let mut invalid = rows();
    invalid[12].features.insert(FLOW.into(), f64::NAN);
    assert!(research_predictions(&invalid, &fitted, &policy())
        .unwrap_err()
        .contains("not finite"));
    fitted.factors[0].ast = field("signal");
    assert!(research_predictions(&rows(), &fitted, &policy()).is_err());
}

#[test]
fn research_rolling_history_restarts_at_series_boundary_without_future_data() {
    let fold = fold(1);
    let ast = FactorAst::call(
        FactorOperator::Delta,
        vec![
            field(FLOW),
            FactorAst::Terminal(FactorTerminal::Constant("5".into())),
        ],
    )
    .unwrap();
    assert_eq!(policy().candidate_history_rows(&ast).unwrap(), 6);
    let fitted = FittedInputs {
        fold: &fold,
        protocol_hash: "a".repeat(64),
        factors: vec![FrozenModelFactorV1 {
            ast,
            negative: true,
        }],
    };
    let mut rows = rows();
    for row in &mut rows[32..] {
        row.series_id = 2;
    }
    let predictions = research_predictions(&rows, &fitted, &policy()).unwrap();
    assert!(predictions[..5].iter().all(|p| *p == 0.0));
    assert!(predictions[32..37].iter().all(|p| *p == 0.0));
    let expected = fold
        .model
        .predict(&[-(rows[37].features[FLOW] - rows[32].features[FLOW])])
        .unwrap();
    assert_eq!(predictions[37].to_bits(), expected.to_bits());
    assert_eq!(
        &predictions[..45],
        &research_predictions(&rows[..45], &fitted, &policy()).unwrap()
    );
}
