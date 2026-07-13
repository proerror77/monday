use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ControlPlaneErrorResponse {
    pub error: String,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::ControlPlaneErrorResponse;
    use serde_json::json;

    #[test]
    fn control_plane_error_response_uses_stable_wire_keys() {
        let value = serde_json::to_value(ControlPlaneErrorResponse {
            error: "deployment_not_found".to_string(),
            message: Some("deployment `missing.paper` was not found".to_string()),
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "error": "deployment_not_found",
                "message": "deployment `missing.paper` was not found",
            })
        );
    }
}
