//! Declared ACK research compute. Agents do not infer GPU from wall-clock or
//! "training should be faster."
//!
//! The current CEX trainer is Burn `ndarray` on the host CPU. Campaign Jobs
//! therefore bind the CPU Spot pool (`workload=backtest`) and must not request
//! `nvidia.com/gpu`. `CudaGpu` is a real later contract: CUDA trainer backend,
//! CUDA runner image, a separate GPU node pool, and `nvidia.com/gpu` together.
//! The current backend rejects that grant and those Jobs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const RESEARCH_GPU_RESOURCE: &str = "nvidia.com/gpu";
pub const RESEARCH_ACCELERATOR_NODE_LABEL: &str = "research.monday/accelerator";
pub const RESEARCH_ACCELERATOR_CUDA_GPU: &str = "cuda-gpu";
pub const ADMITTED_CAMPAIGN_JOB_CPU_MILLIS: u32 = 3500;
pub const ADMITTED_CAMPAIGN_JOB_MEMORY_REQUEST: &str = "8Gi";
pub const ADMITTED_CAMPAIGN_JOB_MEMORY_LIMIT: &str = "12Gi";

/// Compiled CEX trainer device. Flip only in the same change that compiles
/// Burn `cuda` into the runner image and renders GPU Jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchTrainerBackendV1 {
    NdArrayCpu,
    BurnCuda,
}

pub const CURRENT_RESEARCH_TRAINER_BACKEND: ResearchTrainerBackendV1 =
    ResearchTrainerBackendV1::NdArrayCpu;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchAcceleratorV1 {
    #[default]
    Cpu,
    CudaGpu,
}

impl ResearchAcceleratorV1 {
    pub fn is_cpu(&self) -> bool {
        matches!(self, Self::Cpu)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ResearchAcceleratorError {
    #[error("ACK research GPU is not admitted: trainer backend is ndarray CPU")]
    GpuRequiresCudaTrainer,
    #[error("cuda_gpu Job must request nvidia.com/gpu")]
    GpuJobMissingGpu,
    #[error("cuda_gpu Job must select {RESEARCH_ACCELERATOR_NODE_LABEL}={RESEARCH_ACCELERATOR_CUDA_GPU}")]
    GpuJobMissingSelector,
    #[error("Campaign Job GPU resource is invalid")]
    InvalidGpuResource,
    #[error("Campaign Job pod spec is missing containers")]
    InvalidPodSpec,
    #[error("Campaign Job GPU request and limit must match")]
    GpuResourceMismatch,
}

pub fn admit_research_accelerator(
    requested: ResearchAcceleratorV1,
) -> Result<ResearchAcceleratorV1, ResearchAcceleratorError> {
    match (requested, CURRENT_RESEARCH_TRAINER_BACKEND) {
        (ResearchAcceleratorV1::Cpu, _) => Ok(ResearchAcceleratorV1::Cpu),
        (ResearchAcceleratorV1::CudaGpu, ResearchTrainerBackendV1::BurnCuda) => {
            Ok(ResearchAcceleratorV1::CudaGpu)
        }
        (ResearchAcceleratorV1::CudaGpu, ResearchTrainerBackendV1::NdArrayCpu) => {
            Err(ResearchAcceleratorError::GpuRequiresCudaTrainer)
        }
    }
}

pub fn cpu_research_node_selector() -> Value {
    serde_json::json!({
        "kubernetes.io/arch": "amd64",
        "workload": "backtest",
    })
}

pub fn cpu_research_container_resources() -> Value {
    serde_json::json!({
        "requests": {
            "cpu": format!("{ADMITTED_CAMPAIGN_JOB_CPU_MILLIS}m"),
            "memory": ADMITTED_CAMPAIGN_JOB_MEMORY_REQUEST,
        },
        "limits": {
            "cpu": format!("{ADMITTED_CAMPAIGN_JOB_CPU_MILLIS}m"),
            "memory": ADMITTED_CAMPAIGN_JOB_MEMORY_LIMIT,
        },
    })
}

pub fn inspect_pod_spec_accelerator(
    pod_spec: &Value,
) -> Result<ResearchAcceleratorV1, ResearchAcceleratorError> {
    let selector = pod_spec
        .get("nodeSelector")
        .and_then(|selector| selector.get(RESEARCH_ACCELERATOR_NODE_LABEL))
        .and_then(Value::as_str);
    let containers = pod_spec
        .get("containers")
        .and_then(Value::as_array)
        .ok_or(ResearchAcceleratorError::InvalidPodSpec)?;
    if containers.is_empty() {
        return Err(ResearchAcceleratorError::InvalidPodSpec);
    }
    let mut gpu_units = None;
    for container in containers {
        let requests = gpu_count(&container["resources"]["requests"])?;
        let limits = gpu_count(&container["resources"]["limits"])?;
        match (requests, limits) {
            (None, None) => {}
            (Some(request), Some(limit)) if request == limit => {
                gpu_units = Some(gpu_units.unwrap_or(request));
                if gpu_units != Some(request) {
                    return Err(ResearchAcceleratorError::GpuResourceMismatch);
                }
            }
            _ => return Err(ResearchAcceleratorError::GpuResourceMismatch),
        }
    }
    match (selector, gpu_units) {
        (None, None) => Ok(ResearchAcceleratorV1::Cpu),
        (Some(RESEARCH_ACCELERATOR_CUDA_GPU), Some(1)) => Ok(ResearchAcceleratorV1::CudaGpu),
        (None, Some(_)) => Err(ResearchAcceleratorError::GpuJobMissingSelector),
        (Some(RESEARCH_ACCELERATOR_CUDA_GPU), None) => {
            Err(ResearchAcceleratorError::GpuJobMissingGpu)
        }
        _ => Err(ResearchAcceleratorError::InvalidGpuResource),
    }
}

pub fn bind_pod_spec_accelerator(
    pod_spec: &Value,
) -> Result<ResearchAcceleratorV1, ResearchAcceleratorError> {
    admit_research_accelerator(inspect_pod_spec_accelerator(pod_spec)?)
}

fn gpu_count(resources: &Value) -> Result<Option<u32>, ResearchAcceleratorError> {
    let Some(value) = resources.get(RESEARCH_GPU_RESOURCE) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let parsed = match value {
        Value::String(text) => text.parse::<u32>().ok(),
        Value::Number(number) => number.as_u64().and_then(|count| u32::try_from(count).ok()),
        _ => None,
    };
    match parsed {
        Some(0) | None => Err(ResearchAcceleratorError::InvalidGpuResource),
        Some(count) => Ok(Some(count)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cpu_pod() -> Value {
        json!({
            "nodeSelector": cpu_research_node_selector(),
            "containers": [{ "resources": cpu_research_container_resources() }],
        })
    }

    #[test]
    fn current_trainer_admits_cpu_and_rejects_gpu() {
        assert_eq!(
            CURRENT_RESEARCH_TRAINER_BACKEND,
            ResearchTrainerBackendV1::NdArrayCpu
        );
        assert_eq!(
            admit_research_accelerator(ResearchAcceleratorV1::Cpu).unwrap(),
            ResearchAcceleratorV1::Cpu
        );
        assert_eq!(
            admit_research_accelerator(ResearchAcceleratorV1::CudaGpu).unwrap_err(),
            ResearchAcceleratorError::GpuRequiresCudaTrainer
        );
    }

    #[test]
    fn cpu_campaign_pod_has_no_gpu_resource() {
        let resources = cpu_research_container_resources();
        assert!(resources["requests"].get(RESEARCH_GPU_RESOURCE).is_none());
        assert!(resources["limits"].get(RESEARCH_GPU_RESOURCE).is_none());
        assert_eq!(
            bind_pod_spec_accelerator(&cpu_pod()).unwrap(),
            ResearchAcceleratorV1::Cpu
        );
    }

    #[test]
    fn gpu_job_shape_is_recognized_and_still_rejected() {
        let pod = json!({
            "nodeSelector": {
                "kubernetes.io/arch": "amd64",
                "research.monday/accelerator": RESEARCH_ACCELERATOR_CUDA_GPU,
            },
            "containers": [{
                "resources": {
                    "requests": { "nvidia.com/gpu": "1" },
                    "limits": { "nvidia.com/gpu": "1" },
                }
            }],
        });
        assert_eq!(
            inspect_pod_spec_accelerator(&pod).unwrap(),
            ResearchAcceleratorV1::CudaGpu
        );
        assert_eq!(
            bind_pod_spec_accelerator(&pod).unwrap_err(),
            ResearchAcceleratorError::GpuRequiresCudaTrainer
        );
    }

    #[test]
    fn cpu_job_cannot_smuggle_a_gpu_request() {
        let mut pod = cpu_pod();
        pod["containers"][0]["resources"]["limits"][RESEARCH_GPU_RESOURCE] = json!("1");
        pod["containers"][0]["resources"]["requests"][RESEARCH_GPU_RESOURCE] = json!("1");
        assert_eq!(
            inspect_pod_spec_accelerator(&pod).unwrap_err(),
            ResearchAcceleratorError::GpuJobMissingSelector
        );
        pod["nodeSelector"][RESEARCH_ACCELERATOR_NODE_LABEL] = json!(RESEARCH_ACCELERATOR_CUDA_GPU);
        assert_eq!(
            bind_pod_spec_accelerator(&pod).unwrap_err(),
            ResearchAcceleratorError::GpuRequiresCudaTrainer
        );
    }

    #[test]
    fn cpu_job_cannot_select_the_gpu_pool() {
        let mut pod = cpu_pod();
        pod["nodeSelector"][RESEARCH_ACCELERATOR_NODE_LABEL] = json!(RESEARCH_ACCELERATOR_CUDA_GPU);
        assert_eq!(
            inspect_pod_spec_accelerator(&pod).unwrap_err(),
            ResearchAcceleratorError::GpuJobMissingGpu
        );
    }
}
