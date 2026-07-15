use crate::{
    AgentRunCreateRequest, AgentRunCreateResponse, AgentRunRecord, AuditLogEntry,
    ControlPlaneErrorResponse, DeploymentApplyRequest, DeploymentControlRequest, DeploymentSummary,
    DryRunPerformanceReport, OperatorEvent, PaperIntentRequest, PaperIntentResponse,
    SystemControlResponse, SystemStatus, TradingStateSnapshot,
};
use schemars::{schema::RootSchema, JsonSchema};
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct ContractSchema {
    pub file_name: &'static str,
    pub root: &'static str,
    pub schema: fn() -> RootSchema,
}

impl ContractSchema {
    fn new<T>(file_name: &'static str, root: &'static str) -> Self
    where
        T: JsonSchema + Serialize,
    {
        Self {
            file_name,
            root,
            schema: make_schema::<T>,
        }
    }
}

fn make_schema<T>() -> RootSchema
where
    T: JsonSchema,
{
    schemars::schema_for!(T)
}

pub fn contract_schemas() -> Vec<ContractSchema> {
    vec![
        ContractSchema::new::<DeploymentSummary>(
            "deployment-summary.schema.json",
            "DeploymentSummary",
        ),
        ContractSchema::new::<DeploymentApplyRequest>(
            "deployment-apply-request.schema.json",
            "DeploymentApplyRequest",
        ),
        ContractSchema::new::<DeploymentControlRequest>(
            "deployment-control-request.schema.json",
            "DeploymentControlRequest",
        ),
        ContractSchema::new::<PaperIntentRequest>(
            "paper-intent-request.schema.json",
            "PaperIntentRequest",
        ),
        ContractSchema::new::<PaperIntentResponse>(
            "paper-intent-response.schema.json",
            "PaperIntentResponse",
        ),
        ContractSchema::new::<TradingStateSnapshot>(
            "trading-state-snapshot.schema.json",
            "TradingStateSnapshot",
        ),
        ContractSchema::new::<DryRunPerformanceReport>(
            "dry-run-performance-report.schema.json",
            "DryRunPerformanceReport",
        ),
        ContractSchema::new::<SystemStatus>("system-status.schema.json", "SystemStatus"),
        ContractSchema::new::<SystemControlResponse>(
            "system-control-response.schema.json",
            "SystemControlResponse",
        ),
        ContractSchema::new::<AuditLogEntry>("audit-log-entry.schema.json", "AuditLogEntry"),
        ContractSchema::new::<ControlPlaneErrorResponse>(
            "control-plane-error-response.schema.json",
            "ControlPlaneErrorResponse",
        ),
        ContractSchema::new::<OperatorEvent>("operator-event.schema.json", "OperatorEvent"),
        ContractSchema::new::<AgentRunRecord>("agent-run-record.schema.json", "AgentRunRecord"),
        ContractSchema::new::<AgentRunCreateRequest>(
            "agent-run-create-request.schema.json",
            "AgentRunCreateRequest",
        ),
        ContractSchema::new::<AgentRunCreateResponse>(
            "agent-run-create-response.schema.json",
            "AgentRunCreateResponse",
        ),
    ]
}

pub fn serialize_schema(schema: &RootSchema) -> serde_json::Result<String> {
    let mut json = serde_json::to_string_pretty(schema)?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::{contract_schemas, serialize_schema};
    use std::path::PathBuf;

    fn schema_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("contracts")
            .join("schemas")
    }

    #[test]
    fn schema_snapshots_are_current() {
        let schema_dir = schema_dir();

        for contract in contract_schemas() {
            let path = schema_dir.join(contract.file_name);
            let expected = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read checked-in schema {}: {err}", path.display()));
            let actual = serialize_schema(&(contract.schema)())
                .unwrap_or_else(|err| panic!("serialize schema {}: {err}", contract.root));

            assert_eq!(
                expected, actual,
                "schema snapshot {} is stale; run `cargo run -p ploy-operator-contracts --example export_schemas`",
                contract.file_name
            );
        }
    }
}
