use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueKind {
    Polymarket,
    PredictFun,
    Kalshi,
    Sportsbook,
}

#[cfg(test)]
mod tests {
    use super::VenueKind;

    #[test]
    fn predict_fun_uses_stable_wire_name() {
        let encoded = serde_json::to_string(&VenueKind::PredictFun).unwrap();
        assert_eq!(encoded, "\"predict_fun\"");
        assert_eq!(
            serde_json::from_str::<VenueKind>(&encoded).unwrap(),
            VenueKind::PredictFun
        );
    }
}

impl Default for VenueKind {
    fn default() -> Self {
        Self::Polymarket
    }
}
