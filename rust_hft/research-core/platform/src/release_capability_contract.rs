//! Shared wire contract. Requests are selectors until independently authorized.
use hft_research_platform::{build::BuildSpec, release::SourceArchive};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextBinding {
    pub(crate) repository: String,
    pub(crate) source_sha: String,
    pub(crate) product: String,
    pub(crate) image_repository: String,
    pub(crate) software_run_id: u64,
    pub(crate) publisher_run_id: u64,
    pub(crate) publisher_run_attempt: u32,
    pub(crate) publisher_job_id: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Source,
    Publish,
    Read,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) schema: u32,
    pub(crate) context: ContextBinding,
    pub(crate) phase: Phase,
    pub(crate) publisher_prefixes: Vec<String>,
    pub(crate) image: Option<String>,
    pub(crate) plan_sha256: Option<String>,
    pub(crate) expires_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub(crate) schema: u32,
    pub(crate) image: String,
    pub(crate) source: SourceArchive,
    pub(crate) builds: Vec<BuildSpec>,
    pub(crate) publisher_prefixes: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(crate) schema: u32,
    pub(crate) request_sha256: String,
    pub(crate) expires_ms: u64,
    pub(crate) role: String,
    pub(crate) prefixes: Vec<String>,
    pub(crate) token: String,
}
