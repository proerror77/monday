# ACK research accelerator

Agents do not decide GPU from wall-clock or from “training should be faster.”
ACK CEX compute is a typed accelerator bound into the Campaign grant and the
rendered Job. Read that contract; do not provision a GPU pool because a run is
slow.

## How you know whether ACK needs GPU

1. Read `CURRENT_RESEARCH_TRAINER_BACKEND` and
   `admit_research_accelerator` in
   `rust_hft/alpha-harness/domain/src/research_accelerator.rs`.
2. Inspect the rendered Campaign Job: CPU Jobs select `workload=backtest` and
   must not request `nvidia.com/gpu`. Dispatch admission binds
   `CampaignExecutionBindingV1.accelerator` from that Job and rejects a GPU
   shape while the trainer is Burn `ndarray` on CPU.
3. If the resolver returns `cpu` (the current and default result), keep the
   existing Spot CPU pool `ecs.u1-c1m4.xlarge` labeled `workload=backtest`.
   Do not create GPU nodes, CUDA images, or `nvidia.com/gpu` resources.

GPU is needed only when a later merged change sets the trainer backend to
`BurnCuda` **and** admits `ResearchAcceleratorV1::CudaGpu`. That change must
land together:

- Burn `cuda` in the runner image (new digest, bound `image.revision`)
- Job `nvidia.com/gpu: 1` plus
  `nodeSelector.research.monday/accelerator=cuda-gpu`
- A **separate** GPU node pool with that accelerator label
- GPU nodes must **not** carry `workload=backtest` (CPU Jobs and
  `monday-remote-build` stay on the CPU pool)
- A new grant; do not rewrite an existing CPU grant as GPU

Ridge-only and the current hidden-dim-8 ndarray MLP stay on CPU even after a
CUDA backend exists, until a non-Ridge campaign explicitly binds `cuda_gpu`.

## Current admitted shape

| Item | Value |
| --- | --- |
| Trainer | Burn `ndarray` + `NdArrayDevice::Cpu` |
| Accelerator | `cpu` (omitted on the grant wire) |
| Node pool | Spot `ecs.u1-c1m4.xlarge`, `workload=backtest` |
| Job CPU / memory | 3500m / 8Gi request, 12Gi limit |
| `nvidia.com/gpu` | forbidden |

Opening GPU is a separate Research contract, not an H1 speed switch.
