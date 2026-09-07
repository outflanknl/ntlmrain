//! Unified WebGPU and native CPU compute execution.

use rayon::{ThreadPool, ThreadPoolBuilder};
use serde::Serialize;
use thiserror::Error;

use crate::{
    cpu,
    cpu_verify::{CpuCandidate, verify_candidates_with_progress},
    gpu::{FalseAlarmCandidate, GpuContext, GpuError, PrecomputeRequest, TuningSelection},
    params::{GPU_TARGET_STEPS_PER_DISPATCH, PrecomputeConfig, WORKGROUP_SIZE},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputeCandidate {
    pub start: u64,
    pub position: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComputePrecomputeProgress {
    pub endpoints_done: u32,
    pub endpoints_total: u32,
    pub steps_done: u64,
    pub steps_total: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComputeVerifyProgress {
    pub completed_steps: u64,
    pub total_steps: u64,
    pub active_candidates: usize,
    pub step_budget: Option<u32>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CpuComputeMetadata {
    pub kind: &'static str,
    pub implementation: &'static str,
    pub architecture: &'static str,
    pub threads: usize,
    pub selection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored_webgpu_adapter: Option<String>,
}

pub struct CpuContext {
    pool: ThreadPool,
    pub metadata: CpuComputeMetadata,
}

impl CpuContext {
    pub fn create(
        threads: Option<usize>,
        selection: impl Into<String>,
        ignored_webgpu_adapter: Option<String>,
    ) -> Result<Self, ComputeError> {
        let threads = threads.unwrap_or_else(available_cpu_threads);
        if threads == 0 {
            return Err(ComputeError::InvalidCpuThreads);
        }
        let pool = ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("ntlmrain-cpu-{index}"))
            .build()?;
        Ok(Self {
            pool,
            metadata: CpuComputeMetadata {
                kind: "cpu",
                implementation: native_cpu_implementation(),
                architecture: std::env::consts::ARCH,
                threads,
                selection: selection.into(),
                ignored_webgpu_adapter,
            },
        })
    }

    fn precompute_with_progress<F>(&self, request: &PrecomputeRequest, progress: F) -> Vec<u64>
    where
        F: Fn(ComputePrecomputeProgress) + Sync,
    {
        let config = PrecomputeConfig {
            hash: request.target,
            table_index: request.table_index,
            chain_len: u64::from(request.chain_len),
            device_num: 0,
            total_devices: 1,
            workgroup_size: WORKGROUP_SIZE,
            gpu_target_steps_per_dispatch: GPU_TARGET_STEPS_PER_DISPATCH,
        };
        let expected_endpoints = request.chain_len.saturating_sub(1);
        let mut endpoints = self.pool.install(|| {
            cpu::precompute_with_progress(&config, |state| {
                progress(ComputePrecomputeProgress {
                    endpoints_done: state.indices_done.min(expected_endpoints),
                    endpoints_total: expected_endpoints,
                    steps_done: state.steps_done,
                    steps_total: state.steps_total,
                });
            })
        });
        endpoints.truncate(expected_endpoints as usize);
        // Native CPU precompute uses the shader/OpenCL work-item layout:
        // GID zero represents the latest target position. Public endpoint
        // artifacts use ascending target-position ordinals, matching the GPU
        // path and RainbowCrackalack, so normalize at the compute boundary.
        endpoints.reverse();
        endpoints
    }

    fn verify_with_progress<F>(
        &self,
        candidates: &[ComputeCandidate],
        target: [u8; 8],
        table_index: u32,
        all: bool,
        progress: F,
    ) -> Vec<u64>
    where
        F: Fn(ComputeVerifyProgress) + Sync,
    {
        let cpu_candidates = candidates
            .iter()
            .map(|candidate| CpuCandidate {
                start: candidate.start,
                position: candidate.position,
            })
            .collect::<Vec<_>>();
        self.pool.install(|| {
            verify_candidates_with_progress(&cpu_candidates, target, table_index, all, |state| {
                progress(ComputeVerifyProgress {
                    completed_steps: state.steps_done,
                    total_steps: state.steps_total,
                    active_candidates: state.candidates_total.saturating_sub(state.candidates_done)
                        as usize,
                    step_budget: None,
                });
            })
            .keys
        })
    }
}

pub enum ComputeContext {
    WebGpu(Box<GpuContext>),
    Cpu(CpuContext),
}

impl ComputeContext {
    pub fn precompute_with_progress<F>(
        &self,
        request: &PrecomputeRequest,
        progress: F,
    ) -> Result<Vec<u64>, ComputeError>
    where
        F: Fn(ComputePrecomputeProgress) + Sync,
    {
        match self {
            Self::WebGpu(context) => context
                .precompute_with_progress(request, |state| {
                    progress(ComputePrecomputeProgress {
                        endpoints_done: state.endpoints_done,
                        endpoints_total: state.endpoints_total,
                        steps_done: state.steps_done,
                        steps_total: state.steps_total,
                    });
                })
                .map_err(ComputeError::Gpu),
            Self::Cpu(context) => Ok(context.precompute_with_progress(request, progress)),
        }
    }

    pub fn verify_with_progress<F>(
        &self,
        candidates: &[ComputeCandidate],
        target: [u8; 8],
        table_index: u32,
        all: bool,
        progress: F,
    ) -> Result<Vec<u64>, ComputeError>
    where
        F: Fn(ComputeVerifyProgress) + Sync,
    {
        match self {
            Self::WebGpu(context) => {
                let gpu_candidates = candidates
                    .iter()
                    .map(|candidate| FalseAlarmCandidate {
                        start: candidate.start,
                        position: candidate.position,
                    })
                    .collect::<Vec<_>>();
                context
                    .check_candidates_with_progress(
                        &gpu_candidates,
                        target,
                        table_index,
                        all,
                        cpu::is_exact_des_key_match,
                        |state| {
                            progress(ComputeVerifyProgress {
                                completed_steps: state.completed_steps,
                                total_steps: state.total_steps,
                                active_candidates: state.active_candidates,
                                step_budget: Some(state.step_budget),
                            });
                        },
                    )
                    .map_err(ComputeError::Gpu)
            }
            Self::Cpu(context) => {
                Ok(context.verify_with_progress(candidates, target, table_index, all, progress))
            }
        }
    }

    pub fn gpu(&self) -> Option<&GpuContext> {
        match self {
            Self::WebGpu(context) => Some(context),
            Self::Cpu(_) => None,
        }
    }

    pub fn cpu(&self) -> Option<&CpuContext> {
        match self {
            Self::WebGpu(_) => None,
            Self::Cpu(context) => Some(context),
        }
    }

    pub fn tuning(&self) -> Option<&TuningSelection> {
        self.gpu().map(|context| &context.selection)
    }
}

#[derive(Debug, Error)]
pub enum ComputeError {
    #[error(transparent)]
    Gpu(#[from] GpuError),
    #[error("--cpu-threads must be at least 1")]
    InvalidCpuThreads,
    #[error("could not create native CPU worker pool: {0}")]
    ThreadPool(#[from] rayon::ThreadPoolBuildError),
}

pub fn available_cpu_threads() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

#[cfg(target_arch = "x86_64")]
pub const fn native_cpu_implementation() -> &'static str {
    "fast-des SIMD, 512-key bitslice"
}

#[cfg(not(target_arch = "x86_64"))]
pub const fn native_cpu_implementation() -> &'static str {
    "portable u64, 64-lane bitslice"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{byte7_index_to_plaintext, netntlmv1_hash, precompute_one};
    use crate::gpu::DispatchMode;

    #[test]
    fn cpu_precompute_matches_gpu_endpoint_order() {
        let context = CpuContext::create(Some(2), "test", None).unwrap();
        let request = PrecomputeRequest {
            target: [0x25, 0x77, 0x89, 0x87, 0x04, 0x01, 0xc9, 0x65],
            chain_len: 128,
            table_index: 0,
            checkpoint_steps: 32,
            dispatch: DispatchMode::FixedSteps(64_000_000),
        };
        let actual = context.precompute_with_progress(&request, |_| {});
        let expected = (0..127)
            .rev()
            .map(|gid| precompute_one(&request.target, 0, 128, 0, 1, 0, gid))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn cpu_context_recovers_exact_key() {
        let index = 0x00e2_2e04_519a_a757;
        let target = netntlmv1_hash(&byte7_index_to_plaintext(index));
        let context = CpuContext::create(Some(2), "test", None).unwrap();
        let hits = context.verify_with_progress(
            &[ComputeCandidate {
                start: index,
                position: 0,
            }],
            target,
            0,
            false,
            |_| {},
        );
        assert_eq!(hits, vec![index]);
    }
}
