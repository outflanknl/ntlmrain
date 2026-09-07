//! Native WebGPU runtime for the optimized byte7 shaders.
//!
//! This module deliberately owns no CLI policy.  It exposes serializable
//! adapter/tuning descriptions and synchronous operations so both the human
//! and JSON command paths can use the same implementation.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wgpu::util::DeviceExt;

const PRECOMPUTE_COMPACT: &str = include_str!("../shaders/precompute_compact.wgsl");
const PRECOMPUTE_EXPANDED: &str = include_str!("../shaders/precompute_expanded.wgsl");
const FALSE_ALARM_COMPACT: &str = include_str!("../shaders/verify_compact.wgsl");
const FALSE_ALARM_EXPANDED: &str = include_str!("../shaders/verify_expanded.wgsl");
const DES_LUT: &[u8] = include_bytes!("../shaders/des_lut.bin");

pub const COMPLETION_MAGIC: u32 = 0x4259_3731;
pub const COMPLETION_FOUND_MAGIC: u32 = 0x4259_3732;
pub const DEFAULT_CHAIN_LEN: u32 = 881_689;
pub const DEFAULT_CHECKPOINT_STEPS: u32 = 65_536;
pub const DEFAULT_TARGET_STEPS: u64 = 192_000_000;
pub const DEFAULT_ADAPTIVE_TARGET_MS: u64 = 1_800;
pub const MAX_HOST_DISPATCH_INVOCATIONS: u32 = 65_536;
const LUT_SIZE: usize = 102_016;
const STATE_BYTES: u64 = 32;
const TUNING_SCHEMA: u32 = 3;
const TUNING_BUDGET: Duration = Duration::from_secs(15);
const TUNING_SLOW_RATE: f64 = 10_000_000.0;
const TUNING_PILOT_STEPS: u64 = 1_000_000;
const TUNING_MIN_SAMPLE_MS: f64 = 20.0;
const TUNING_FAMILY_SAMPLE_MS: f64 = 50.0;
const TUNING_WORKGROUP_SAMPLE_MS: f64 = 35.0;
const TUNING_PRODUCTION_SAMPLE_MS: f64 = 150.0;
const TUNING_SAMPLE_ROUNDS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter matched the requested backend")]
    NoAdapter,
    #[error("GPU device selection failed: {0}")]
    Selection(String),
    #[error("failed to create GPU device: {0}")]
    RequestDevice(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("shader pipeline failed: {0}")]
    Pipeline(String),
    #[error("GPU operation failed: {0}")]
    Operation(String),
    #[error("GPU dispatch did not publish all completion markers ({0})")]
    IncompleteDispatch(String),
    #[error("tuning failed: no shader/workgroup candidate completed correctly")]
    TuningFailed,
    #[error("tuning cache error: {0}")]
    Cache(String),
}

pub type Result<T> = std::result::Result<T, GpuError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendChoice {
    #[default]
    Auto,
    Dx12,
    Vulkan,
    Metal,
}

impl BackendChoice {
    pub fn backends(self) -> wgpu::Backends {
        match self {
            Self::Auto => wgpu::Backends::PRIMARY,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Metal => wgpu::Backends::METAL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShaderVariant {
    Compact,
    Expanded,
}

impl ShaderVariant {
    pub const ALL: [Self; 2] = [Self::Compact, Self::Expanded];

    pub fn id(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Expanded => "expanded",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact LUT",
            Self::Expanded => "Expanded LUT",
        }
    }

    pub fn workgroup_storage_bytes(self) -> u32 {
        match self {
            Self::Compact => 2_180,
            Self::Expanded => 31_748,
        }
    }

    fn precompute_source(self) -> &'static str {
        match self {
            Self::Compact => PRECOMPUTE_COMPACT,
            Self::Expanded => PRECOMPUTE_EXPANDED,
        }
    }

    fn false_alarm_source(self) -> &'static str {
        match self {
            Self::Compact => FALSE_ALARM_COMPACT,
            Self::Expanded => FALSE_ALARM_EXPANDED,
        }
    }
}

impl fmt::Display for ShaderVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ShaderChoice {
    #[default]
    Auto,
    Compact,
    Expanded,
}

impl ShaderChoice {
    fn selected(self) -> Option<ShaderVariant> {
        match self {
            Self::Auto => None,
            Self::Compact => Some(ShaderVariant::Compact),
            Self::Expanded => Some(ShaderVariant::Expanded),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "value")]
pub enum WorkgroupChoice {
    #[default]
    Auto,
    Fixed(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "value")]
pub enum DispatchMode {
    AdaptiveMs(u64),
    FixedSteps(u64),
}

impl Default for DispatchMode {
    fn default() -> Self {
        Self::AdaptiveMs(DEFAULT_ADAPTIVE_TARGET_MS)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuOptions {
    pub backend: BackendChoice,
    pub device: String,
    pub shader: ShaderChoice,
    pub workgroup: WorkgroupChoice,
    pub dispatch: DispatchMode,
    pub retune: bool,
    pub tuning_cache: Option<PathBuf>,
}

impl Default for GpuOptions {
    fn default() -> Self {
        Self {
            backend: BackendChoice::Auto,
            device: "auto".into(),
            shader: ShaderChoice::Auto,
            workgroup: WorkgroupChoice::Auto,
            dispatch: DispatchMode::default(),
            retune: false,
            tuning_cache: default_tuning_cache_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterLimits {
    pub max_buffer_size: u64,
    pub max_storage_buffer_binding_size: u32,
    pub max_compute_workgroup_storage_size: u32,
    pub max_compute_invocations_per_workgroup: u32,
    pub max_compute_workgroup_size_x: u32,
    pub max_compute_workgroups_per_dimension: u32,
    pub min_storage_buffer_offset_alignment: u32,
}

impl From<&wgpu::Limits> for AdapterLimits {
    fn from(value: &wgpu::Limits) -> Self {
        Self {
            max_buffer_size: value.max_buffer_size,
            max_storage_buffer_binding_size: value.max_storage_buffer_binding_size,
            max_compute_workgroup_storage_size: value.max_compute_workgroup_storage_size,
            max_compute_invocations_per_workgroup: value.max_compute_invocations_per_workgroup,
            max_compute_workgroup_size_x: value.max_compute_workgroup_size_x,
            max_compute_workgroups_per_dimension: value.max_compute_workgroups_per_dimension,
            min_storage_buffer_offset_alignment: value.min_storage_buffer_offset_alignment,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterReport {
    pub index: usize,
    pub selector_id: String,
    pub fingerprint: String,
    pub backend: String,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: String,
    pub hardware_eligible: bool,
    pub driver: String,
    pub driver_info: String,
    pub limits: AdapterLimits,
    pub supported_shaders: Vec<ShaderVariant>,
}

struct CatalogEntry {
    adapter: wgpu::Adapter,
    report: AdapterReport,
    type_rank: u8,
}

/// Enumerated adapters and stable metadata for a single backend filter.
pub struct DeviceCatalog {
    _instance: wgpu::Instance,
    entries: Vec<CatalogEntry>,
}

impl DeviceCatalog {
    pub fn enumerate(backend: BackendChoice) -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: backend.backends(),
            ..Default::default()
        });
        let adapters = instance.enumerate_adapters(backend.backends());
        let mut occurrences: HashMap<(String, u32, u32), usize> = HashMap::new();
        let mut entries = Vec::new();
        for (index, adapter) in adapters.into_iter().enumerate() {
            let info = adapter.get_info();
            let limits = adapter.limits();
            let backend_name = backend_name(info.backend).to_owned();
            let occurrence = occurrences
                .entry((backend_name.clone(), info.vendor, info.device))
                .and_modify(|n| *n += 1)
                .or_insert(1);
            let selector_id = format!(
                "{}-{:04x}-{:04x}-{}",
                backend_name, info.vendor, info.device, occurrence
            );
            let fingerprint = adapter_fingerprint(&info);
            let supported_shaders = ShaderVariant::ALL
                .into_iter()
                .filter(|v| {
                    v.workgroup_storage_bytes() <= limits.max_compute_workgroup_storage_size
                })
                .collect();
            let type_rank = device_type_rank(info.device_type);
            let hardware_eligible = is_hardware_device_type(info.device_type);
            entries.push(CatalogEntry {
                adapter,
                report: AdapterReport {
                    index,
                    selector_id,
                    fingerprint,
                    backend: backend_name,
                    name: info.name,
                    vendor_id: info.vendor,
                    device_id: info.device,
                    device_type: device_type_name(info.device_type).to_owned(),
                    hardware_eligible,
                    driver: info.driver,
                    driver_info: info.driver_info,
                    limits: AdapterLimits::from(&limits),
                    supported_shaders,
                },
                type_rank,
            });
        }
        Ok(Self {
            _instance: instance,
            entries,
        })
    }

    pub fn reports(&self) -> Vec<AdapterReport> {
        self.entries.iter().map(|e| e.report.clone()).collect()
    }

    pub fn select_report(&self, selector: &str) -> Result<AdapterReport> {
        Ok(self.select_entry(selector)?.report.clone())
    }

    /// Alias used by command frontends after displaying `reports()`.
    pub fn select(&self, selector: &str) -> Result<AdapterReport> {
        self.select_report(selector)
    }

    /// Resolve a selector for automatic compute without admitting CPU/software
    /// WebGPU adapters. An explicit CPU-class selector resolves to no report,
    /// so the caller can choose the native SIMD backend instead.
    pub fn select_hardware_report(&self, selector: &str) -> Result<Option<AdapterReport>> {
        let selector = selector.trim();
        if selector.eq_ignore_ascii_case("auto") || selector.is_empty() {
            return Ok(self
                .entries
                .iter()
                .filter(|entry| entry.report.hardware_eligible)
                .max_by_key(|entry| auto_score(entry))
                .map(|entry| entry.report.clone()));
        }
        let entry = self.select_entry(selector)?;
        Ok(entry.report.hardware_eligible.then(|| entry.report.clone()))
    }

    fn select_entry(&self, selector: &str) -> Result<&CatalogEntry> {
        let selector = selector.trim();
        if selector.eq_ignore_ascii_case("auto") || selector.is_empty() {
            return self
                .entries
                .iter()
                .max_by_key(|entry| auto_score(entry))
                .ok_or(GpuError::NoAdapter);
        }

        if let Some(exact) = self
            .entries
            .iter()
            .find(|entry| entry.report.selector_id.eq_ignore_ascii_case(selector))
        {
            return Ok(exact);
        }

        if let Ok(index) = selector.parse::<usize>() {
            return self.entries.get(index).ok_or_else(|| {
                GpuError::Selection(format!(
                    "device index {index} does not exist; available devices: {}",
                    selector_summary(&self.entries)
                ))
            });
        }

        let needle = selector.to_ascii_lowercase();
        let matches: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.report.name.to_ascii_lowercase().contains(&needle))
            .collect();
        match matches.as_slice() {
            [one] => Ok(*one),
            [] => Err(GpuError::Selection(format!(
                "no device name contains {selector:?}; available devices: {}",
                selector_summary(&self.entries)
            ))),
            _ => Err(GpuError::Selection(format!(
                "device name {selector:?} is ambiguous; matches: {}",
                matches
                    .iter()
                    .map(|e| format!("{} ({})", e.report.name, e.report.selector_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TuningMeasurement {
    pub shader: ShaderVariant,
    pub workgroup_size: u32,
    pub stage: String,
    pub elapsed_ms: f64,
    pub steps: u64,
    pub steps_per_second: f64,
    pub valid: bool,
    pub note: Option<String>,
    #[serde(default)]
    pub sample_count: usize,
    #[serde(default)]
    pub rate_cv: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TuningSelection {
    pub shader: ShaderVariant,
    pub workgroup_size: u32,
    pub minimum_workgroups: u32,
    pub estimated_steps_per_second: f64,
    pub source: String,
    #[serde(default)]
    pub selection_reason: String,
    #[serde(default)]
    pub tuning_elapsed_ms: f64,
    #[serde(default)]
    pub deadline_reached: bool,
    pub measurements: Vec<TuningMeasurement>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TuningCacheStatus {
    pub path: PathBuf,
    pub action: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningCacheEntry {
    pub schema: u32,
    pub tool_version: String,
    pub adapter_fingerprint: String,
    pub shader_bundle_hash: String,
    pub selection: TuningSelection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuningCache {
    pub entries: Vec<TuningCacheEntry>,
}

impl TuningCache {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(path).map_err(|e| GpuError::Cache(e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| GpuError::Cache(e.to_string()))
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| GpuError::Cache(e.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| GpuError::Cache(e.to_string()))?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, bytes).map_err(|e| GpuError::Cache(e.to_string()))?;
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| GpuError::Cache(e.to_string()))?;
        }
        std::fs::rename(&temporary, path).map_err(|e| GpuError::Cache(e.to_string()))
    }

    pub fn get(&self, fingerprint: &str) -> Option<&TuningSelection> {
        let bundle = shader_bundle_hash();
        self.entries
            .iter()
            .find(|entry| {
                entry.schema == TUNING_SCHEMA
                    && entry.tool_version == env!("CARGO_PKG_VERSION")
                    && entry.adapter_fingerprint == fingerprint
                    && entry.shader_bundle_hash == bundle
            })
            .map(|entry| &entry.selection)
    }

    pub fn put(&mut self, fingerprint: String, selection: TuningSelection) {
        let bundle = shader_bundle_hash();
        self.entries.retain(|entry| {
            !(entry.adapter_fingerprint == fingerprint && entry.shader_bundle_hash == bundle)
        });
        self.entries.push(TuningCacheEntry {
            schema: TUNING_SCHEMA,
            tool_version: env!("CARGO_PKG_VERSION").into(),
            adapter_fingerprint: fingerprint,
            shader_bundle_hash: bundle,
            selection,
        });
    }
}

#[derive(Debug, Clone)]
pub struct PrecomputeRequest {
    pub target: [u8; 8],
    pub chain_len: u32,
    pub table_index: u32,
    pub checkpoint_steps: u32,
    pub dispatch: DispatchMode,
}

impl PrecomputeRequest {
    pub fn new(target: [u8; 8]) -> Self {
        Self {
            target,
            chain_len: DEFAULT_CHAIN_LEN,
            table_index: 0,
            checkpoint_steps: DEFAULT_CHECKPOINT_STEPS,
            dispatch: DispatchMode::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrecomputeProgress {
    pub endpoints_done: u32,
    pub endpoints_total: u32,
    pub steps_done: u64,
    pub steps_total: u64,
    pub batch_steps: u64,
    pub batch_workgroups: u32,
    pub elapsed_ms: f64,
    pub target_steps: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FalseAlarmCandidate {
    pub start: u64,
    pub position: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FalseAlarmProgress {
    pub completed_ceiling: u32,
    pub maximum_position: u32,
    pub completed_steps: u64,
    pub total_steps: u64,
    pub batch_steps: u64,
    pub elapsed_ms: f64,
    pub step_budget: u32,
    pub active_candidates: usize,
    pub rejected_gpu_hits: usize,
    pub accepted_hits: usize,
    pub batch_workgroups: u32,
    pub dispatches: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PrecomputeParams {
    hash_lo: u32,
    hash_hi: u32,
    reduction_offset: u32,
    chain_len: u32,
    endpoint_start: u32,
    slice_start: u32,
    slice_steps: u32,
    output_len: u32,
    benchmark_steps: u32,
    mode: u32,
    padding0: u32,
    padding1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FalseParams {
    target_lo: u32,
    target_hi: u32,
    reduction_offset: u32,
    candidate_count: u32,
    step_budget: u32,
    padding0: u32,
    padding1: u32,
    padding2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CandidateState {
    index_lo: u32,
    index_hi: u32,
    result_lo: u32,
    result_hi: u32,
    target_position: u32,
    next_position: u32,
    found: u32,
    padding: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct HitProcessing {
    changed: bool,
    rejected: usize,
    stop: bool,
}

fn process_candidate_hits<F>(
    states: &mut [CandidateState],
    target: &[u8; 8],
    all: bool,
    verify_exact: &mut F,
    recovered: &mut Vec<u64>,
) -> HitProcessing
where
    F: FnMut(u64, &[u8; 8]) -> bool,
{
    let mut result = HitProcessing::default();
    for candidate in states {
        if candidate.found == 0 {
            continue;
        }
        let index = u64::from(candidate.result_lo) | (u64::from(candidate.result_hi) << 32);
        if verify_exact(index, target) {
            if !recovered.contains(&index) {
                recovered.push(index);
            }
            if !all {
                result.stop = true;
                return result;
            }
        } else {
            result.rejected += 1;
        }
        candidate.found = 0;
        candidate.result_lo = 0;
        candidate.result_hi = 0;
        result.changed = true;
    }
    result
}

fn advance_candidate_mirror_without_hit(states: &mut [CandidateState], budget: u32) {
    for candidate in states {
        candidate.next_position = candidate
            .next_position
            .saturating_add(budget)
            .min(candidate.target_position.saturating_add(1));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CandidateCompaction {
    active_count: usize,
    moved: bool,
}

fn compact_candidate_states(
    states: &mut [CandidateState],
    active_count: usize,
    finished_steps: &mut u64,
) -> CandidateCompaction {
    let mut write = 0usize;
    let mut moved = false;
    for read in 0..active_count {
        let candidate = states[read];
        if candidate.next_position <= candidate.target_position {
            if write != read {
                states[write] = candidate;
                moved = true;
            }
            write += 1;
        } else {
            *finished_steps =
                finished_steps.saturating_add(u64::from(candidate.target_position) + 1);
            moved = true;
        }
    }
    CandidateCompaction {
        active_count: write,
        moved,
    }
}

struct Pipelines {
    precompute: wgpu::ComputePipeline,
    false_alarm: wgpu::ComputePipeline,
}

/// Selected native device plus compiled matching shader pair.
pub struct GpuContext {
    pub adapter: AdapterReport,
    pub selection: TuningSelection,
    pub tuning_cache: Option<TuningCacheStatus>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    lut: wgpu::Buffer,
    pipelines: Pipelines,
}

impl GpuContext {
    /// Enumerate, select, tune (or load a validated cache choice), and compile.
    pub fn create(options: &GpuOptions) -> Result<Self> {
        let catalog = DeviceCatalog::enumerate(options.backend)?;
        Self::create_from_catalog(&catalog, &options.device, options)
    }

    pub fn create_from_catalog(
        catalog: &DeviceCatalog,
        selector: &str,
        options: &GpuOptions,
    ) -> Result<Self> {
        validate_lut()?;
        let entry = catalog.select_entry(selector)?;
        let (device, queue) = request_device(&entry.adapter)?;
        let lut = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ntlmrain DES LUT"),
            contents: DES_LUT,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let supported_workgroups = valid_workgroups(&entry.report.limits);
        if supported_workgroups.is_empty() {
            return Err(GpuError::Unsupported(
                "adapter supports no requested workgroup sizes".into(),
            ));
        }
        let supported_variants = entry.report.supported_shaders.clone();

        let manual_variant = options.shader.selected();
        if let Some(variant) = manual_variant
            && !supported_variants.contains(&variant)
        {
            return Err(GpuError::Unsupported(format!(
                "{} requires {} bytes of workgroup storage; adapter exposes {}",
                variant.label(),
                variant.workgroup_storage_bytes(),
                entry.report.limits.max_compute_workgroup_storage_size
            )));
        }
        let manual_workgroup = match options.workgroup {
            WorkgroupChoice::Auto => None,
            WorkgroupChoice::Fixed(size) => {
                if !supported_workgroups.contains(&size) {
                    return Err(GpuError::Unsupported(format!(
                        "workgroup size {size} exceeds adapter compute limits"
                    )));
                }
                Some(size)
            }
        };

        let automatic_request = manual_variant.is_none() && manual_workgroup.is_none();
        let mut cache_status = None;
        let incumbent = if automatic_request {
            options
                .tuning_cache
                .as_deref()
                .and_then(|path| match TuningCache::load(path) {
                    Ok(cache) => {
                        cache
                            .get(&entry.report.fingerprint)
                            .cloned()
                            .filter(|selection| {
                                supported_variants.contains(&selection.shader)
                                    && supported_workgroups.contains(&selection.workgroup_size)
                            })
                    }
                    Err(error) => {
                        cache_status = Some(TuningCacheStatus {
                            path: path.to_path_buf(),
                            action: "load-failed".into(),
                            warning: Some(error.to_string()),
                        });
                        None
                    }
                })
        } else {
            None
        };
        let cached = if options.retune {
            None
        } else {
            incumbent.clone()
        };
        if cached.is_some()
            && let Some(path) = options.tuning_cache.as_deref()
        {
            cache_status = Some(TuningCacheStatus {
                path: path.to_path_buf(),
                action: "loaded".into(),
                warning: None,
            });
        }

        let mut selection = if let Some(mut cached) = cached {
            // Pipeline creation is the inexpensive validity check for a cached
            // choice. A failed cached choice falls through to tuning below.
            cached.source = "cache".into();
            cached.selection_reason = "cached-winner".into();
            cached
        } else if let (Some(shader), Some(workgroup_size)) = (manual_variant, manual_workgroup) {
            TuningSelection {
                shader,
                workgroup_size,
                minimum_workgroups: 128,
                estimated_steps_per_second: 0.0,
                source: "manual".into(),
                selection_reason: "manual".into(),
                tuning_elapsed_ms: 0.0,
                deadline_reached: false,
                measurements: Vec::new(),
            }
        } else {
            auto_tune_device(
                &device,
                &queue,
                &lut,
                &entry.report,
                manual_variant,
                manual_workgroup,
                options.retune.then_some(incumbent.as_ref()).flatten(),
            )?
        };

        let pipelines = match compile_pipelines(&device, selection.shader, selection.workgroup_size)
        {
            Ok(value) => value,
            Err(error) if selection.source == "cache" => {
                selection = auto_tune_device(
                    &device,
                    &queue,
                    &lut,
                    &entry.report,
                    manual_variant,
                    manual_workgroup,
                    None,
                )?;
                compile_pipelines(&device, selection.shader, selection.workgroup_size)
                    .map_err(|_| error)?
            }
            Err(error) => return Err(error),
        };

        if selection.source != "cache"
            && selection.source != "slow-adapter-default"
            && automatic_request
            && let Some(path) = options.tuning_cache.as_deref()
        {
            let mut cache = TuningCache::load(path).unwrap_or_default();
            cache.put(entry.report.fingerprint.clone(), selection.clone());
            cache_status = Some(match cache.save_atomic(path) {
                Ok(()) => TuningCacheStatus {
                    path: path.to_path_buf(),
                    action: "saved".into(),
                    warning: None,
                },
                Err(error) => TuningCacheStatus {
                    path: path.to_path_buf(),
                    action: "save-failed".into(),
                    warning: Some(error.to_string()),
                },
            });
        }

        Ok(Self {
            adapter: entry.report.clone(),
            selection,
            tuning_cache: cache_status,
            device,
            queue,
            lut,
            pipelines,
        })
    }

    pub fn precompute(&self, request: &PrecomputeRequest) -> Result<Vec<u64>> {
        self.precompute_with_progress(request, |_| {})
    }

    pub fn precompute_with_progress<F>(
        &self,
        request: &PrecomputeRequest,
        progress: F,
    ) -> Result<Vec<u64>>
    where
        F: Fn(PrecomputeProgress),
    {
        validate_precompute_request(request)?;
        let output_len = request.chain_len - 1;
        let output_bytes = u64::from(output_len) * 8;
        ensure_storage_size(&self.adapter.limits, output_bytes, "endpoint output")?;

        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain endpoint output"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain endpoint readback"),
            size: output_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain precompute parameters"),
            size: std::mem::size_of::<PrecomputeParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let workgroup = self.selection.workgroup_size;
        let maximum_width =
            max_dispatch_invocations(&self.adapter.limits, workgroup).min(output_len);
        let maximum_groups = div_ceil_u32(maximum_width, workgroup);
        let marker_bytes = align_to(u64::from(maximum_groups.max(1)) * 4, 8);
        let markers = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain completion markers"),
            size: marker_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let marker_readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain completion marker readback"),
            size: marker_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let checkpoint_steps = align_up_u32(request.checkpoint_steps.max(1), workgroup);
        let steps_total = steps_for_range(0, output_len);
        let mut target_steps = match request.dispatch {
            DispatchMode::FixedSteps(steps) => steps.max(1),
            DispatchMode::AdaptiveMs(_) => 256_000_000,
        };
        let mut smoothed_rate = None::<f64>;
        let mut steps_done = 0u64;
        let mut finalized = 0u32;

        for slice_start in (0..output_len).step_by(checkpoint_steps as usize) {
            let mut start = slice_start;
            while start < output_len {
                let remaining = output_len - start;
                let length = sliced_dispatch_width(
                    start,
                    remaining,
                    slice_start,
                    checkpoint_steps,
                    target_steps,
                    maximum_width,
                    workgroup,
                );
                let batch_steps =
                    sliced_steps_for_range(start, length, slice_start, checkpoint_steps);
                let groups = div_ceil_u32(length, workgroup);
                debug_assert!(groups <= self.adapter.limits.max_compute_workgroups_per_dimension);
                let active_marker_bytes = align_to(u64::from(groups.max(1)) * 4, 8);

                let params = precompute_params(
                    request.target,
                    request.table_index,
                    request.chain_len,
                    start,
                    slice_start,
                    checkpoint_steps,
                    length,
                    0,
                    false,
                );
                self.queue
                    .write_buffer(&uniform, 0, bytemuck::bytes_of(&params));
                let binding_size = NonZeroU64::new(u64::from(length) * 8).unwrap();
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("ntlmrain precompute bindings"),
                    layout: &self.pipelines.precompute.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &output,
                                offset: u64::from(start) * 8,
                                size: Some(binding_size),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.lut.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: markers.as_entire_binding(),
                        },
                    ],
                });
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("ntlmrain precompute dispatch"),
                        });
                encoder.clear_buffer(&markers, 0, Some(active_marker_bytes));
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("ntlmrain precompute pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.pipelines.precompute);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.dispatch_workgroups(groups, 1, 1);
                }
                encoder.copy_buffer_to_buffer(
                    &markers,
                    0,
                    &marker_readback,
                    0,
                    active_marker_bytes,
                );
                let started = Instant::now();
                self.queue.submit(Some(encoder.finish()));
                let marker_data = map_read(&self.device, &marker_readback, active_marker_bytes)?;
                let elapsed = started.elapsed();
                validate_markers(&marker_data, groups, false)?;

                if let DispatchMode::AdaptiveMs(target_ms) = request.dispatch {
                    let elapsed_seconds = elapsed.as_secs_f64().max(0.001);
                    let measured = batch_steps as f64 / elapsed_seconds;
                    let rate = smoothed_rate
                        .map(|old| old * 0.75 + measured * 0.25)
                        .unwrap_or(measured);
                    smoothed_rate = Some(rate);
                    let desired = (rate * target_ms as f64 / 1_000.0) as u64;
                    target_steps = desired.clamp(64_000_000, 4_000_000_000);
                }

                start += length;
                steps_done = steps_done.saturating_add(batch_steps);
                finalized = finalized.max(
                    start.min(
                        slice_start
                            .saturating_add(checkpoint_steps)
                            .saturating_add(1),
                    ),
                );
                progress(PrecomputeProgress {
                    endpoints_done: finalized,
                    endpoints_total: output_len,
                    steps_done: steps_done.min(steps_total),
                    steps_total,
                    batch_steps,
                    batch_workgroups: groups,
                    elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
                    target_steps,
                });
            }
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ntlmrain endpoint copy"),
            });
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, output_bytes);
        self.queue.submit(Some(encoder.finish()));
        let bytes = map_read(&self.device, &staging, output_bytes)?;
        let words: &[u32] = bytemuck::cast_slice(&bytes);
        let mut endpoints = Vec::with_capacity(output_len as usize);
        // The shader emits the table's reverse order. The file format expects
        // monotonically increasing endpoint ordinals.
        for source in (0..output_len as usize).rev() {
            let lo = u64::from(words[source * 2]);
            let hi = u64::from(words[source * 2 + 1]);
            endpoints.push(lo | (hi << 32));
        }
        Ok(endpoints)
    }

    /// Check candidate starts on the GPU. Every reported hit is passed to
    /// `verify_exact`; rejected hits are cleared and the remaining chain walks
    /// continue rather than terminating the search.
    pub fn check_candidates<F>(
        &self,
        candidates: &[FalseAlarmCandidate],
        target: [u8; 8],
        table_index: u32,
        all: bool,
        verify_exact: F,
    ) -> Result<Vec<u64>>
    where
        F: FnMut(u64, &[u8; 8]) -> bool,
    {
        self.check_candidates_with_progress(
            candidates,
            target,
            table_index,
            all,
            verify_exact,
            |_| {},
        )
    }

    pub fn check_candidates_with_progress<F, P>(
        &self,
        candidates: &[FalseAlarmCandidate],
        target: [u8; 8],
        table_index: u32,
        all: bool,
        mut verify_exact: F,
        progress: P,
    ) -> Result<Vec<u64>>
    where
        F: FnMut(u64, &[u8; 8]) -> bool,
        P: Fn(FalseAlarmProgress),
    {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut ordered = candidates.to_vec();
        ordered.sort_by_key(|candidate| std::cmp::Reverse(candidate.position));
        let mut states: Vec<CandidateState> = ordered
            .iter()
            .map(|candidate| CandidateState {
                index_lo: candidate.start as u32,
                index_hi: (candidate.start >> 32) as u32,
                result_lo: 0,
                result_hi: 0,
                target_position: candidate.position,
                next_position: 0,
                found: 0,
                padding: 0,
            })
            .collect();
        let state_bytes = states.len() as u64 * STATE_BYTES;
        ensure_storage_size(&self.adapter.limits, state_bytes, "candidate state")?;

        let state = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ntlmrain candidate state"),
                contents: bytemuck::cast_slice(&states),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            });
        let state_readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain candidate readback"),
            size: state_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain false-alarm parameters"),
            size: std::mem::size_of::<FalseParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let workgroup = self.selection.workgroup_size;
        let capacity = false_alarm_batch_capacity(&self.adapter.limits, workgroup)?;
        let maximum_groups = div_ceil_u32(capacity.min(states.len() as u32), workgroup);
        let marker_bytes = align_to(u64::from(maximum_groups.max(1)) * 4, 8);
        let markers = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain false-alarm markers"),
            size: marker_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let marker_readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ntlmrain false-alarm marker readback"),
            size: marker_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let maximum_position = ordered.iter().map(|c| c.position).max().unwrap_or(0);
        let maximum_steps = maximum_position.saturating_add(1);
        let total_steps = ordered
            .iter()
            .map(|candidate| u64::from(candidate.position) + 1)
            .sum::<u64>();
        // Start conservatively. Precompute tuning rates can substantially
        // overestimate the branchier checker kernel on some Windows drivers.
        let mut budget = maximum_steps.clamp(1, 64);
        let mut active_count = states.len();
        let mut completed_ceiling = 0u32;
        let mut finished_steps = 0u64;
        let mut completed_steps;
        let mut reported_steps = 0u64;
        let mut irregular_schedule = false;
        let mut recovered = Vec::new();
        let mut rejected = 0usize;
        while active_count > 0 {
            let maximum_remaining = if irregular_schedule {
                states[..active_count]
                    .iter()
                    .map(|state| {
                        state
                            .target_position
                            .saturating_add(1)
                            .saturating_sub(state.next_position)
                    })
                    .max()
                    .unwrap_or(0)
            } else {
                maximum_steps.saturating_sub(completed_ceiling)
            };
            if maximum_remaining == 0 {
                break;
            }
            budget = budget.min(maximum_remaining).max(1);
            let mut found_in_round = false;
            let mut round_groups = 0u32;
            let mut dispatches = 0u32;
            let mut offset = 0usize;
            let started = Instant::now();
            while offset < active_count {
                let count = (active_count - offset).min(capacity as usize);
                let groups = div_ceil_u32(count as u32, workgroup);
                let active_marker_bytes = align_to(u64::from(groups.max(1)) * 4, 8);
                let params = false_params(target, table_index, count as u32, budget);
                self.queue
                    .write_buffer(&uniform, 0, bytemuck::bytes_of(&params));
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("ntlmrain false-alarm bindings"),
                    layout: &self.pipelines.false_alarm.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &state,
                                offset: offset as u64 * STATE_BYTES,
                                size: Some(NonZeroU64::new(count as u64 * STATE_BYTES).unwrap()),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.lut.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: markers.as_entire_binding(),
                        },
                    ],
                });
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("ntlmrain false-alarm dispatch"),
                        });
                encoder.clear_buffer(&markers, 0, Some(active_marker_bytes));
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("ntlmrain false-alarm pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.pipelines.false_alarm);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.dispatch_workgroups(groups, 1, 1);
                }
                encoder.copy_buffer_to_buffer(
                    &markers,
                    0,
                    &marker_readback,
                    0,
                    active_marker_bytes,
                );
                self.queue.submit(Some(encoder.finish()));
                let marker_data = map_read(&self.device, &marker_readback, active_marker_bytes)?;
                found_in_round = validate_markers(&marker_data, groups, true)?;
                round_groups += groups;
                dispatches += 1;
                offset += count;
                if found_in_round {
                    break;
                }
            }
            let elapsed = started.elapsed();

            if found_in_round {
                let active_bytes = active_count as u64 * STATE_BYTES;
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("ntlmrain candidate state copy"),
                        });
                encoder.copy_buffer_to_buffer(&state, 0, &state_readback, 0, active_bytes);
                self.queue.submit(Some(encoder.finish()));
                let bytes = map_read(&self.device, &state_readback, active_bytes)?;
                states[..active_count].copy_from_slice(bytemuck::cast_slice(&bytes));
                let processed = process_candidate_hits(
                    &mut states[..active_count],
                    &target,
                    all,
                    &mut verify_exact,
                    &mut recovered,
                );
                rejected += processed.rejected;
                if processed.stop {
                    return Ok(recovered);
                }
                // A rejected hit stopped its lane early, while later batches
                // may not have run at all. Switch to the general per-lane
                // schedule only for this rare continuation path.
                irregular_schedule = true;
                let compacted =
                    compact_candidate_states(&mut states, active_count, &mut finished_steps);
                active_count = compacted.active_count;
                if active_count > 0 {
                    self.queue.write_buffer(
                        &state,
                        0,
                        bytemuck::cast_slice(&states[..active_count]),
                    );
                }
            } else if irregular_schedule {
                advance_candidate_mirror_without_hit(&mut states[..active_count], budget);
                let compacted =
                    compact_candidate_states(&mut states, active_count, &mut finished_steps);
                active_count = compacted.active_count;
                if compacted.moved && active_count > 0 {
                    self.queue.write_buffer(
                        &state,
                        0,
                        bytemuck::cast_slice(&states[..active_count]),
                    );
                }
            } else {
                completed_ceiling = completed_ceiling.saturating_add(budget);
                while active_count > 0
                    && states[active_count - 1].target_position.saturating_add(1)
                        <= completed_ceiling
                {
                    active_count -= 1;
                    finished_steps = finished_steps
                        .saturating_add(u64::from(states[active_count].target_position) + 1);
                }
            }

            if irregular_schedule {
                completed_ceiling = states[..active_count]
                    .iter()
                    .map(|state| state.next_position)
                    .min()
                    .unwrap_or(maximum_steps);
                completed_steps = finished_steps.saturating_add(
                    states[..active_count]
                        .iter()
                        .map(|state| u64::from(state.next_position))
                        .sum::<u64>(),
                );
            } else {
                completed_steps = finished_steps.saturating_add(
                    (active_count as u64).saturating_mul(u64::from(completed_ceiling)),
                );
            }
            completed_steps = completed_steps.min(total_steps);
            let batch_steps = completed_steps.saturating_sub(reported_steps);
            reported_steps = completed_steps;
            progress(FalseAlarmProgress {
                completed_ceiling,
                maximum_position,
                completed_steps,
                total_steps,
                batch_steps,
                elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
                step_budget: budget,
                active_candidates: active_count,
                rejected_gpu_hits: rejected,
                accepted_hits: recovered.len(),
                batch_workgroups: round_groups,
                dispatches,
            });

            let desired =
                (budget as f64 * 800.0 / (elapsed.as_secs_f64() * 1_000.0).max(1.0)).round() as u32;
            budget = if elapsed > Duration::from_millis(1_200) {
                desired
            } else {
                ((u64::from(budget) + u64::from(desired)) / 2) as u32
            }
            .clamp(1, 65_536);
        }
        Ok(recovered)
    }
}

fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue)> {
    let adapter_limits = adapter.limits();
    let mut required = wgpu::Limits::downlevel_defaults();
    required.max_buffer_size = adapter_limits.max_buffer_size;
    required.max_storage_buffer_binding_size = adapter_limits.max_storage_buffer_binding_size;
    required.max_compute_workgroup_storage_size = adapter_limits.max_compute_workgroup_storage_size;
    required.max_compute_invocations_per_workgroup = adapter_limits
        .max_compute_invocations_per_workgroup
        .min(1_024);
    required.max_compute_workgroup_size_x = adapter_limits.max_compute_workgroup_size_x.min(1_024);
    required.max_compute_workgroups_per_dimension =
        adapter_limits.max_compute_workgroups_per_dimension;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ntlmrain GPU"),
        required_features: wgpu::Features::empty(),
        required_limits: required,
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .map_err(|e| GpuError::RequestDevice(e.to_string()))
}

fn compile_pipelines(
    device: &wgpu::Device,
    variant: ShaderVariant,
    workgroup: u32,
) -> Result<Pipelines> {
    Ok(Pipelines {
        precompute: compile_pipeline(
            device,
            variant.precompute_source(),
            &format!("{} precompute WG {workgroup}", variant.label()),
            workgroup,
        )?,
        false_alarm: compile_pipeline(
            device,
            variant.false_alarm_source(),
            &format!("{} false-alarm WG {workgroup}", variant.label()),
            workgroup,
        )?,
    })
}

fn compile_pipeline(
    device: &wgpu::Device,
    source: &'static str,
    label: &str,
    workgroup: u32,
) -> Result<wgpu::ComputePipeline> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions {
            constants: &[("WORKGROUP_SIZE", workgroup as f64)],
            ..Default::default()
        },
        cache: None,
    });
    if let Some(error) = pollster::block_on(device.pop_error_scope()) {
        return Err(GpuError::Pipeline(error.to_string()));
    }
    Ok(pipeline)
}

type TuningKey = (ShaderVariant, u32);

struct TuningDeadline {
    started: Instant,
    limit: Duration,
}

impl TuningDeadline {
    fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn remaining(&self) -> Duration {
        self.limit.saturating_sub(self.elapsed())
    }

    fn can_schedule(&self, expected_ms: f64) -> bool {
        self.remaining().as_secs_f64() * 1_000.0 > expected_ms.max(0.0) + 10.0
    }

    fn expired(&self) -> bool {
        self.elapsed() >= self.limit
    }
}

#[derive(Debug)]
struct StableTuningOutcome {
    key: TuningKey,
    reason: &'static str,
    tied: Vec<TuningKey>,
}

fn median_f64(values: &[f64]) -> f64 {
    let mut sorted = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn coefficient_of_variation(values: &[f64]) -> f64 {
    let valid = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if valid.len() < 2 {
        return 0.0;
    }
    let mean = valid.iter().sum::<f64>() / valid.len() as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let variance = valid
        .iter()
        .map(|value| {
            let difference = value - mean;
            difference * difference
        })
        .sum::<f64>()
        / valid.len() as f64;
    variance.sqrt() / mean
}

fn aggregate_tuning_samples(
    key: TuningKey,
    stage: &str,
    samples: &[TuningMeasurement],
) -> Option<TuningMeasurement> {
    let valid = samples
        .iter()
        .filter(|sample| sample.valid)
        .collect::<Vec<_>>();
    if valid.is_empty() {
        return None;
    }
    let rates = valid
        .iter()
        .map(|sample| sample.steps_per_second)
        .collect::<Vec<_>>();
    Some(TuningMeasurement {
        shader: key.0,
        workgroup_size: key.1,
        stage: stage.into(),
        elapsed_ms: valid.iter().map(|sample| sample.elapsed_ms).sum(),
        steps: valid.iter().map(|sample| sample.steps).sum(),
        steps_per_second: median_f64(&rates),
        valid: true,
        note: None,
        sample_count: valid.len(),
        rate_cv: coefficient_of_variation(&rates),
    })
}

fn tuning_uncertainty(measurement: &TuningMeasurement) -> f64 {
    (2.0 * measurement.rate_cv).clamp(0.03, 0.10)
}

fn confirmed_slow_tuning_rates(rates: &[f64]) -> bool {
    let valid = rates
        .iter()
        .copied()
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .collect::<Vec<_>>();
    valid.len() >= 2 && median_f64(&valid) < TUNING_SLOW_RATE
}

fn alternating_tuning_order(keys: &[TuningKey], round: usize) -> Vec<TuningKey> {
    let mut order = keys.to_vec();
    if round % 2 == 1 {
        order.reverse();
    }
    order
}

fn stable_tuning_winner(
    measurements: &[TuningMeasurement],
    incumbent: Option<TuningKey>,
    deadline_reached: bool,
) -> Option<StableTuningOutcome> {
    let mut ranked = measurements
        .iter()
        .filter(|measurement| measurement.valid && measurement.steps_per_second > 0.0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.steps_per_second.total_cmp(&left.steps_per_second));
    let leader = *ranked.first()?;
    let tied = ranked
        .iter()
        .copied()
        .filter(|candidate| {
            let gap = (leader.steps_per_second - candidate.steps_per_second).max(0.0)
                / leader.steps_per_second.max(1.0);
            gap <= tuning_uncertainty(leader).max(tuning_uncertainty(candidate))
        })
        .collect::<Vec<_>>();
    let tied_keys = tied
        .iter()
        .map(|measurement| (measurement.shader, measurement.workgroup_size))
        .collect::<Vec<_>>();

    if let Some(key) = incumbent
        && tied_keys.contains(&key)
    {
        return Some(StableTuningOutcome {
            key,
            reason: "cached-winner-retained",
            tied: tied_keys,
        });
    }

    let compact_wg64 = (ShaderVariant::Compact, 64);
    if tied_keys.contains(&compact_wg64) {
        return Some(StableTuningOutcome {
            key: compact_wg64,
            reason: if tied_keys.len() == 1 {
                "clear-winner"
            } else if deadline_reached {
                "deadline-tie-break"
            } else {
                "deterministic-tie-break"
            },
            tied: tied_keys,
        });
    }

    let selected = tied
        .iter()
        .min_by_key(|measurement| {
            (
                measurement.shader.workgroup_storage_bytes(),
                measurement.workgroup_size.abs_diff(64),
                measurement.shader.id(),
                measurement.workgroup_size,
            )
        })
        .copied()?;
    Some(StableTuningOutcome {
        key: (selected.shader, selected.workgroup_size),
        reason: if tied_keys.len() == 1 {
            "clear-winner"
        } else if deadline_reached {
            "deadline-tie-break"
        } else {
            "deterministic-tie-break"
        },
        tied: tied_keys,
    })
}

fn calibrated_synthetic_steps(rate: f64, target_ms: f64, invocations: u32) -> u32 {
    if !rate.is_finite() || rate <= 0.0 || invocations == 0 {
        return 1;
    }
    ((rate * target_ms / 1_000.0 / f64::from(invocations)).ceil() as u64)
        .clamp(1, u64::from(u32::MAX)) as u32
}

fn production_tuning_shape(rate: f64, fraction: f64, target_ms: f64) -> BenchmarkShape {
    let endpoint_start = ((DEFAULT_CHAIN_LEN - 1) as f64 * fraction) as u32;
    let invocations = MAX_HOST_DISPATCH_INVOCATIONS.min(DEFAULT_CHAIN_LEN - 1 - endpoint_start);
    let slice_steps =
        calibrated_synthetic_steps(rate, target_ms, invocations).clamp(1, DEFAULT_CHECKPOINT_STEPS);
    BenchmarkShape::Production {
        endpoint_start,
        invocations,
        slice_steps,
    }
}

fn production_tuning_measurements(
    finalists: &[TuningKey],
    samples: &HashMap<(TuningKey, usize), Vec<TuningMeasurement>>,
) -> Vec<TuningMeasurement> {
    const POINTS: [(f64, f64); 3] = [(0.20, 0.16), (0.60, 0.40), (0.88, 0.44)];
    let mut results = Vec::new();
    for key in finalists {
        let mut weighted_inverse = 0.0;
        let mut elapsed_ms = 0.0;
        let mut steps = 0u64;
        let mut sample_count = 0usize;
        let mut maximum_cv = 0.0f64;
        let mut complete = true;
        for (point_index, (_, weight)) in POINTS.iter().enumerate() {
            let point_samples = samples
                .get(&(*key, point_index))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let Some(point) = aggregate_tuning_samples(*key, "production-point", point_samples)
            else {
                complete = false;
                break;
            };
            weighted_inverse += weight / point.steps_per_second.max(1.0);
            elapsed_ms += point.elapsed_ms;
            steps = steps.saturating_add(point.steps);
            sample_count += point.sample_count;
            maximum_cv = maximum_cv.max(point.rate_cv);
        }
        if complete {
            results.push(TuningMeasurement {
                shader: key.0,
                workgroup_size: key.1,
                stage: "production".into(),
                elapsed_ms,
                steps,
                steps_per_second: 1.0 / weighted_inverse.max(f64::MIN_POSITIVE),
                valid: true,
                note: None,
                sample_count,
                rate_cv: maximum_cv,
            });
        }
    }
    results
}

fn tuning_pipeline(
    device: &wgpu::Device,
    key: TuningKey,
    pipelines: &mut HashMap<TuningKey, Arc<wgpu::ComputePipeline>>,
) -> Result<Arc<wgpu::ComputePipeline>> {
    if let Some(pipeline) = pipelines.get(&key) {
        return Ok(Arc::clone(pipeline));
    }
    let pipeline = Arc::new(compile_pipeline(
        device,
        key.0.precompute_source(),
        "tuning",
        key.1,
    )?);
    pipelines.insert(key, Arc::clone(&pipeline));
    Ok(pipeline)
}

#[allow(clippy::too_many_arguments)]
fn auto_tune_device(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    lut: &wgpu::Buffer,
    adapter: &AdapterReport,
    forced_variant: Option<ShaderVariant>,
    forced_workgroup: Option<u32>,
    incumbent: Option<&TuningSelection>,
) -> Result<TuningSelection> {
    const POINTS: [f64; 3] = [0.20, 0.60, 0.88];
    let deadline = TuningDeadline::new(TUNING_BUDGET);
    let variants = forced_variant
        .map(|variant| vec![variant])
        .unwrap_or_else(|| adapter.supported_shaders.clone());
    let supported_workgroups = valid_workgroups(&adapter.limits);
    let candidate_workgroups = forced_workgroup
        .map(|value| vec![value])
        .unwrap_or_else(|| supported_workgroups.clone());
    let baseline_workgroup = if supported_workgroups.contains(&64) {
        64
    } else {
        *supported_workgroups.first().ok_or(GpuError::TuningFailed)?
    };
    let baseline_key = (ShaderVariant::Compact, baseline_workgroup);
    let automatic_request = forced_variant.is_none() && forced_workgroup.is_none();
    let incumbent_key = incumbent.map(|selection| (selection.shader, selection.workgroup_size));
    let mut pipelines = HashMap::<TuningKey, Arc<wgpu::ComputePipeline>>::new();
    let mut measurements = Vec::new();

    // Compile and validate the portable Compact baseline before optional work.
    let baseline_pipeline = tuning_pipeline(device, baseline_key, &mut pipelines)?;
    let validation_shape = BenchmarkShape::Synthetic {
        invocations: 4_096,
        steps: 64,
    };
    let (baseline_validation, reference_words) = benchmark_compiled_candidate(
        device,
        queue,
        lut,
        &baseline_pipeline,
        baseline_key.0,
        baseline_key.1,
        "validation",
        validation_shape,
    )?;
    measurements.push(baseline_validation.clone());

    // A progressive Compact pilot obtains a safe calibration. At least two
    // samples are required before a genuinely slow forced-WebGPU adapter is
    // classified below the 10M steps/s threshold.
    let pilot_invocations = 4_096;
    let mut pilot_steps = TUNING_PILOT_STEPS.div_ceil(u64::from(pilot_invocations)) as u32;
    let mut pilot_samples = Vec::new();
    for _ in 0..3 {
        let expected_ms = pilot_samples
            .last()
            .map(|sample: &TuningMeasurement| {
                (f64::from(pilot_invocations) * f64::from(pilot_steps) * 1_000.0
                    / sample.steps_per_second.max(1.0))
                .clamp(TUNING_MIN_SAMPLE_MS, 2_000.0)
            })
            .unwrap_or(TUNING_MIN_SAMPLE_MS);
        if !deadline.can_schedule(expected_ms) {
            break;
        }
        let (sample, _) = benchmark_compiled_candidate(
            device,
            queue,
            lut,
            &baseline_pipeline,
            baseline_key.0,
            baseline_key.1,
            "pilot",
            BenchmarkShape::Synthetic {
                invocations: pilot_invocations,
                steps: pilot_steps,
            },
        )?;
        let long_enough = sample.elapsed_ms >= TUNING_MIN_SAMPLE_MS;
        pilot_steps = pilot_steps.max(calibrated_synthetic_steps(
            sample.steps_per_second,
            TUNING_MIN_SAMPLE_MS,
            pilot_invocations,
        ));
        pilot_samples.push(sample);
        if pilot_samples.len() >= 2 && long_enough {
            break;
        }
    }
    if pilot_samples.is_empty() {
        pilot_samples.push(baseline_validation.clone());
    }
    let pilot = aggregate_tuning_samples(baseline_key, "pilot", &pilot_samples)
        .ok_or(GpuError::TuningFailed)?;
    let pilot_rate = pilot.steps_per_second;
    measurements.push(pilot.clone());
    if automatic_request
        && confirmed_slow_tuning_rates(
            &pilot_samples
                .iter()
                .map(|sample| sample.steps_per_second)
                .collect::<Vec<_>>(),
        )
    {
        return Ok(TuningSelection {
            shader: ShaderVariant::Compact,
            workgroup_size: baseline_workgroup,
            minimum_workgroups: 128,
            estimated_steps_per_second: pilot_rate,
            source: "slow-adapter-default".into(),
            selection_reason: "slow-adapter-default".into(),
            tuning_elapsed_ms: deadline.elapsed().as_secs_f64() * 1_000.0,
            deadline_reached: deadline.expired(),
            measurements,
        });
    }

    // Validate each family at the common baseline workgroup, then measure three
    // alternating rounds so clock ramp and thermal drift affect both equally.
    let mut calibration_rates = HashMap::<TuningKey, f64>::new();
    calibration_rates.insert(baseline_key, pilot_rate);
    let mut family_keys = Vec::new();
    for variant in &variants {
        let key = (*variant, baseline_workgroup);
        if !deadline.can_schedule(TUNING_MIN_SAMPLE_MS) {
            break;
        }
        let pipeline = match tuning_pipeline(device, key, &mut pipelines) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                measurements.push(failed_measurement(
                    key.0,
                    key.1,
                    "family-validation",
                    error.to_string(),
                ));
                continue;
            }
        };
        if key != baseline_key && !deadline.can_schedule(TUNING_MIN_SAMPLE_MS) {
            break;
        }
        let calibration = if key == baseline_key {
            baseline_validation.clone()
        } else {
            match benchmark_compiled_candidate(
                device,
                queue,
                lut,
                &pipeline,
                key.0,
                key.1,
                "family-validation",
                validation_shape,
            ) {
                Ok((mut row, words)) => {
                    row.valid = words == reference_words;
                    if !row.valid {
                        row.note = Some("output differed from Compact WG64 reference".into());
                    }
                    row
                }
                Err(error) => {
                    measurements.push(failed_measurement(
                        key.0,
                        key.1,
                        "family-validation",
                        error.to_string(),
                    ));
                    continue;
                }
            }
        };
        measurements.push(calibration.clone());
        if calibration.valid {
            calibration_rates.insert(key, calibration.steps_per_second);
            family_keys.push(key);
        }
    }
    if family_keys.is_empty() {
        return Err(GpuError::TuningFailed);
    }
    let mut family_samples = HashMap::<TuningKey, Vec<TuningMeasurement>>::new();
    for round in 0..TUNING_SAMPLE_ROUNDS {
        let order = alternating_tuning_order(&family_keys, round);
        for key in order {
            if !deadline.can_schedule(TUNING_FAMILY_SAMPLE_MS) {
                break;
            }
            let pipeline = Arc::clone(pipelines.get(&key).ok_or(GpuError::TuningFailed)?);
            let seed = calibration_rates.get(&key).copied().unwrap_or(pilot_rate);
            match benchmark_compiled_candidate(
                device,
                queue,
                lut,
                &pipeline,
                key.0,
                key.1,
                "family-sample",
                BenchmarkShape::Synthetic {
                    invocations: MAX_HOST_DISPATCH_INVOCATIONS,
                    steps: calibrated_synthetic_steps(
                        seed,
                        TUNING_FAMILY_SAMPLE_MS,
                        MAX_HOST_DISPATCH_INVOCATIONS,
                    ),
                },
            ) {
                Ok((row, _)) => {
                    calibration_rates.insert(key, row.steps_per_second);
                    family_samples.entry(key).or_default().push(row);
                }
                Err(error) => measurements.push(failed_measurement(
                    key.0,
                    key.1,
                    "family-sample",
                    error.to_string(),
                )),
            }
        }
    }
    let family_measurements = family_keys
        .iter()
        .filter_map(|key| {
            aggregate_tuning_samples(
                *key,
                "family",
                family_samples
                    .get(key)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    measurements.extend(family_measurements.clone());

    // Sweep every supported workgroup for every surviving family. Validation
    // doubles as the warm-up/calibration pass; timing rounds alternate order.
    let surviving_variants = family_measurements
        .iter()
        .map(|measurement| measurement.shader)
        .collect::<Vec<_>>();
    let mut workgroup_keys = Vec::new();
    for variant in surviving_variants {
        for workgroup in &candidate_workgroups {
            let key = (variant, *workgroup);
            if !deadline.can_schedule(TUNING_MIN_SAMPLE_MS) {
                break;
            }
            let pipeline = match tuning_pipeline(device, key, &mut pipelines) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    measurements.push(failed_measurement(
                        key.0,
                        key.1,
                        "workgroup-validation",
                        error.to_string(),
                    ));
                    continue;
                }
            };
            if !calibration_rates.contains_key(&key) && !deadline.can_schedule(TUNING_MIN_SAMPLE_MS)
            {
                break;
            }
            let calibration = if let Some(rate) = calibration_rates.get(&key).copied() {
                TuningMeasurement {
                    shader: key.0,
                    workgroup_size: key.1,
                    stage: "workgroup-validation".into(),
                    elapsed_ms: 0.0,
                    steps: 0,
                    steps_per_second: rate,
                    valid: true,
                    note: Some("reused family validation".into()),
                    sample_count: 1,
                    rate_cv: 0.0,
                }
            } else {
                match benchmark_compiled_candidate(
                    device,
                    queue,
                    lut,
                    &pipeline,
                    key.0,
                    key.1,
                    "workgroup-validation",
                    validation_shape,
                ) {
                    Ok((mut row, words)) => {
                        row.valid = words == reference_words;
                        if !row.valid {
                            row.note = Some("output differed from Compact WG64 reference".into());
                        }
                        row
                    }
                    Err(error) => {
                        measurements.push(failed_measurement(
                            key.0,
                            key.1,
                            "workgroup-validation",
                            error.to_string(),
                        ));
                        continue;
                    }
                }
            };
            measurements.push(calibration.clone());
            if calibration.valid {
                calibration_rates.insert(key, calibration.steps_per_second);
                workgroup_keys.push(key);
            }
        }
    }
    let mut workgroup_samples = HashMap::<TuningKey, Vec<TuningMeasurement>>::new();
    for round in 0..TUNING_SAMPLE_ROUNDS {
        let order = alternating_tuning_order(&workgroup_keys, round);
        for key in order {
            if !deadline.can_schedule(TUNING_WORKGROUP_SAMPLE_MS) {
                break;
            }
            let pipeline = Arc::clone(pipelines.get(&key).ok_or(GpuError::TuningFailed)?);
            let seed = calibration_rates.get(&key).copied().unwrap_or(pilot_rate);
            match benchmark_compiled_candidate(
                device,
                queue,
                lut,
                &pipeline,
                key.0,
                key.1,
                "workgroup-sample",
                BenchmarkShape::Synthetic {
                    invocations: MAX_HOST_DISPATCH_INVOCATIONS,
                    steps: calibrated_synthetic_steps(
                        seed,
                        TUNING_WORKGROUP_SAMPLE_MS,
                        MAX_HOST_DISPATCH_INVOCATIONS,
                    ),
                },
            ) {
                Ok((row, _)) => {
                    calibration_rates.insert(key, row.steps_per_second);
                    workgroup_samples.entry(key).or_default().push(row);
                }
                Err(error) => measurements.push(failed_measurement(
                    key.0,
                    key.1,
                    "workgroup-sample",
                    error.to_string(),
                )),
            }
        }
    }
    let workgroup_measurements = workgroup_keys
        .iter()
        .filter_map(|key| {
            aggregate_tuning_samples(
                *key,
                "workgroup",
                workgroup_samples
                    .get(key)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    measurements.extend(workgroup_measurements.clone());

    // Keep the best workgroup from every family and Compact WG64. Preliminary
    // samples choose finalists only; the final score is production-shaped.
    let mut finalists = Vec::<TuningKey>::new();
    for variant in &variants {
        if let Some(best) = workgroup_measurements
            .iter()
            .filter(|measurement| measurement.shader == *variant)
            .max_by(|left, right| left.steps_per_second.total_cmp(&right.steps_per_second))
        {
            finalists.push((best.shader, best.workgroup_size));
        }
    }
    if automatic_request && pipelines.contains_key(&baseline_key) {
        finalists.push(baseline_key);
    }
    finalists.sort_by_key(|key| (key.0.id(), key.1));
    finalists.dedup();
    if finalists.is_empty() {
        if automatic_request {
            finalists.push(baseline_key);
        } else {
            return Err(GpuError::TuningFailed);
        }
    }
    finalists.sort_by(|left, right| {
        if *left == baseline_key {
            return std::cmp::Ordering::Less;
        }
        if *right == baseline_key {
            return std::cmp::Ordering::Greater;
        }
        let left_rate = workgroup_measurements
            .iter()
            .find(|measurement| (measurement.shader, measurement.workgroup_size) == *left)
            .map(|measurement| measurement.steps_per_second)
            .unwrap_or(0.0);
        let right_rate = workgroup_measurements
            .iter()
            .find(|measurement| (measurement.shader, measurement.workgroup_size) == *right)
            .map(|measurement| measurement.steps_per_second)
            .unwrap_or(0.0);
        right_rate.total_cmp(&left_rate)
    });

    let mut production_samples = HashMap::<(TuningKey, usize), Vec<TuningMeasurement>>::new();
    for key in &finalists {
        let Some(pipeline) = pipelines.get(key).cloned() else {
            continue;
        };
        let seed_rate = workgroup_measurements
            .iter()
            .find(|measurement| (measurement.shader, measurement.workgroup_size) == *key)
            .map(|measurement| measurement.steps_per_second)
            .unwrap_or(pilot_rate);
        for (point_index, fraction) in POINTS.iter().enumerate() {
            if !deadline.can_schedule(TUNING_PRODUCTION_SAMPLE_MS) {
                break;
            }
            let shape = production_tuning_shape(seed_rate, *fraction, TUNING_PRODUCTION_SAMPLE_MS);
            // One production-shaped warm-up precedes the recorded samples.
            let _ = benchmark_compiled_candidate(
                device,
                queue,
                lut,
                &pipeline,
                key.0,
                key.1,
                "production-warmup",
                shape,
            );
            for _ in 0..TUNING_SAMPLE_ROUNDS {
                if !deadline.can_schedule(TUNING_PRODUCTION_SAMPLE_MS) {
                    break;
                }
                match benchmark_compiled_candidate(
                    device,
                    queue,
                    lut,
                    &pipeline,
                    key.0,
                    key.1,
                    "production-sample",
                    shape,
                ) {
                    Ok((row, _)) => production_samples
                        .entry((*key, point_index))
                        .or_default()
                        .push(row),
                    Err(error) => {
                        measurements.push(failed_measurement(
                            key.0,
                            key.1,
                            "production-sample",
                            error.to_string(),
                        ));
                        break;
                    }
                }
            }
        }
    }
    let mut production_measurements =
        production_tuning_measurements(&finalists, &production_samples);
    let mut outcome =
        stable_tuning_winner(&production_measurements, incumbent_key, deadline.expired());

    // If the leading pair remains statistically tied, spend at most two more
    // samples per production point while budget remains.
    if let Some(current) = &outcome
        && current.tied.len() > 1
    {
        let extended = current.tied.iter().take(2).copied().collect::<Vec<_>>();
        for round in 0..2 {
            let mut order = extended.clone();
            if round % 2 == 1 {
                order.reverse();
            }
            for key in order {
                let Some(pipeline) = pipelines.get(&key).cloned() else {
                    continue;
                };
                let seed_rate = production_measurements
                    .iter()
                    .find(|measurement| (measurement.shader, measurement.workgroup_size) == key)
                    .map(|measurement| measurement.steps_per_second)
                    .unwrap_or(pilot_rate);
                for (point_index, fraction) in POINTS.iter().enumerate() {
                    if !deadline.can_schedule(TUNING_PRODUCTION_SAMPLE_MS) {
                        break;
                    }
                    let shape =
                        production_tuning_shape(seed_rate, *fraction, TUNING_PRODUCTION_SAMPLE_MS);
                    if let Ok((row, _)) = benchmark_compiled_candidate(
                        device,
                        queue,
                        lut,
                        &pipeline,
                        key.0,
                        key.1,
                        "production-extra",
                        shape,
                    ) {
                        production_samples
                            .entry((key, point_index))
                            .or_default()
                            .push(row);
                    }
                }
            }
        }
        production_measurements = production_tuning_measurements(&finalists, &production_samples);
        outcome = stable_tuning_winner(&production_measurements, incumbent_key, deadline.expired());
    }
    measurements.extend(production_measurements.clone());

    let (selected_key, estimated_steps_per_second, selection_reason) =
        if let Some(outcome) = outcome {
            let rate = production_measurements
                .iter()
                .find(|measurement| (measurement.shader, measurement.workgroup_size) == outcome.key)
                .map(|measurement| measurement.steps_per_second)
                .unwrap_or(pilot_rate);
            (outcome.key, rate, outcome.reason)
        } else if automatic_request {
            (baseline_key, pilot_rate, "deadline-tie-break")
        } else {
            let fallback = workgroup_measurements
                .iter()
                .chain(family_measurements.iter())
                .filter(|measurement| {
                    forced_variant
                        .map(|variant| measurement.shader == variant)
                        .unwrap_or(true)
                        && forced_workgroup
                            .map(|workgroup| measurement.workgroup_size == workgroup)
                            .unwrap_or(true)
                })
                .max_by(|left, right| left.steps_per_second.total_cmp(&right.steps_per_second))
                .map(|measurement| {
                    (
                        (measurement.shader, measurement.workgroup_size),
                        measurement.steps_per_second,
                    )
                })
                .ok_or(GpuError::TuningFailed)?;
            (fallback.0, fallback.1, "deadline-tie-break")
        };

    Ok(TuningSelection {
        shader: selected_key.0,
        workgroup_size: selected_key.1,
        minimum_workgroups: 128,
        estimated_steps_per_second,
        source: if forced_variant.is_some() || forced_workgroup.is_some() {
            "partial-override-tune".into()
        } else {
            "auto-tune".into()
        },
        selection_reason: selection_reason.into(),
        tuning_elapsed_ms: deadline.elapsed().as_secs_f64() * 1_000.0,
        deadline_reached: deadline.expired(),
        measurements,
    })
}
#[derive(Clone, Copy)]
enum BenchmarkShape {
    Synthetic {
        invocations: u32,
        steps: u32,
    },
    Production {
        endpoint_start: u32,
        invocations: u32,
        slice_steps: u32,
    },
}

#[allow(clippy::too_many_arguments)]
fn benchmark_compiled_candidate(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    lut: &wgpu::Buffer,
    pipeline: &wgpu::ComputePipeline,
    variant: ShaderVariant,
    workgroup: u32,
    stage: &str,
    shape: BenchmarkShape,
) -> Result<(TuningMeasurement, Vec<u32>)> {
    let (invocations, params, steps) = match shape {
        BenchmarkShape::Synthetic { invocations, steps } => (
            invocations,
            precompute_params(
                [0x25, 0x77, 0x89, 0x87, 0x04, 0x01, 0xc9, 0x65],
                0,
                1_025,
                0,
                0,
                u32::MAX,
                invocations,
                steps,
                true,
            ),
            u64::from(invocations) * u64::from(steps),
        ),
        BenchmarkShape::Production {
            endpoint_start,
            invocations,
            slice_steps,
        } => (
            invocations,
            precompute_params(
                [0x25, 0x77, 0x89, 0x87, 0x04, 0x01, 0xc9, 0x65],
                0,
                DEFAULT_CHAIN_LEN,
                endpoint_start,
                0,
                slice_steps,
                invocations,
                0,
                false,
            ),
            sliced_steps_for_range(endpoint_start, invocations, 0, slice_steps),
        ),
    };
    let groups = div_ceil_u32(invocations, workgroup);
    let output_bytes = u64::from(invocations) * 8;
    let marker_bytes = align_to(u64::from(groups.max(1)) * 4, 8);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ntlmrain tune output"),
        size: output_bytes,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ntlmrain tune readback"),
        size: output_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ntlmrain tune params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let markers = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ntlmrain tune markers"),
        size: marker_bytes,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let marker_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ntlmrain tune marker readback"),
        size: marker_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ntlmrain tune bindings"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: lut.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: markers.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("ntlmrain tune dispatch"),
    });
    encoder.clear_buffer(&markers, 0, Some(marker_bytes));
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("ntlmrain tune pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(groups, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&markers, 0, &marker_readback, 0, marker_bytes);
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output_bytes);
    let started = Instant::now();
    queue.submit(Some(encoder.finish()));
    let marker_data = map_read(device, &marker_readback, marker_bytes)?;
    let elapsed = started.elapsed();
    validate_markers(&marker_data, groups, false)?;
    let output_data = map_read(device, &readback, output_bytes)?;
    let words = bytemuck::cast_slice(&output_data).to_vec();
    let elapsed_seconds = elapsed.as_secs_f64().max(0.000_001);
    Ok((
        TuningMeasurement {
            shader: variant,
            workgroup_size: workgroup,
            stage: stage.into(),
            elapsed_ms: elapsed_seconds * 1_000.0,
            steps,
            steps_per_second: steps as f64 / elapsed_seconds,
            valid: true,
            note: None,
            sample_count: 1,
            rate_cv: 0.0,
        },
        words,
    ))
}

fn failed_measurement(
    shader: ShaderVariant,
    workgroup_size: u32,
    stage: &str,
    note: String,
) -> TuningMeasurement {
    TuningMeasurement {
        shader,
        workgroup_size,
        stage: stage.into(),
        elapsed_ms: 0.0,
        steps: 0,
        steps_per_second: 0.0,
        valid: false,
        note: Some(note),
        sample_count: 0,
        rate_cv: 0.0,
    }
}

#[allow(clippy::too_many_arguments)]
fn precompute_params(
    target: [u8; 8],
    table_index: u32,
    chain_len: u32,
    endpoint_start: u32,
    slice_start: u32,
    slice_steps: u32,
    output_len: u32,
    benchmark_steps: u32,
    benchmark: bool,
) -> PrecomputeParams {
    PrecomputeParams {
        hash_lo: u32::from_le_bytes(target[0..4].try_into().unwrap()),
        hash_hi: u32::from_le_bytes(target[4..8].try_into().unwrap()),
        reduction_offset: table_index.wrapping_mul(65_536),
        chain_len,
        endpoint_start,
        slice_start,
        slice_steps,
        output_len,
        benchmark_steps,
        mode: u32::from(benchmark),
        padding0: 0,
        padding1: 0,
    }
}

fn false_params(target: [u8; 8], table_index: u32, count: u32, budget: u32) -> FalseParams {
    FalseParams {
        target_lo: u32::from_le_bytes(target[0..4].try_into().unwrap()),
        target_hi: u32::from_le_bytes(target[4..8].try_into().unwrap()),
        reduction_offset: table_index.wrapping_mul(65_536),
        candidate_count: count,
        step_budget: budget,
        padding0: 0,
        padding1: 0,
        padding2: 0,
    }
}

fn validate_precompute_request(request: &PrecomputeRequest) -> Result<()> {
    if !(2..=0x7fff_ffff).contains(&request.chain_len) {
        return Err(GpuError::Unsupported(
            "chain length must be between 2 and 2^31-1".into(),
        ));
    }
    if request.checkpoint_steps == 0 {
        return Err(GpuError::Unsupported(
            "checkpoint steps must be non-zero".into(),
        ));
    }
    if matches!(
        request.dispatch,
        DispatchMode::AdaptiveMs(0) | DispatchMode::FixedSteps(0)
    ) {
        return Err(GpuError::Unsupported(
            "dispatch target must be non-zero".into(),
        ));
    }
    Ok(())
}

fn ensure_storage_size(limits: &AdapterLimits, bytes: u64, label: &str) -> Result<()> {
    if bytes > limits.max_buffer_size {
        return Err(GpuError::Unsupported(format!(
            "{label} needs {bytes} bytes; adapter max buffer size is {}",
            limits.max_buffer_size
        )));
    }
    if bytes > u64::from(limits.max_storage_buffer_binding_size) {
        return Err(GpuError::Unsupported(format!(
            "{label} needs {bytes} bytes; adapter max storage binding is {}",
            limits.max_storage_buffer_binding_size
        )));
    }
    Ok(())
}

fn map_read(device: &wgpu::Device, buffer: &wgpu::Buffer, bytes: u64) -> Result<Vec<u8>> {
    let slice = buffer.slice(0..bytes);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    poll_wait_forever(device)?;
    receiver
        .recv()
        .map_err(|e| GpuError::Operation(e.to_string()))?
        .map_err(|e| GpuError::Operation(format!("GPU readback mapping failed: {e}")))?;
    let result = slice.get_mapped_range().to_vec();
    let _ = slice;
    buffer.unmap();
    Ok(result)
}

fn poll_wait_forever(device: &wgpu::Device) -> Result<()> {
    loop {
        match device.poll(wgpu::PollType::wait()) {
            Ok(_) => return Ok(()),
            Err(wgpu::PollError::Timeout) => continue,
        }
    }
}

/// Validate all expected completion markers. Returns true if any workgroup
/// reported a found candidate.
pub fn validate_markers(bytes: &[u8], groups: u32, allow_found: bool) -> Result<bool> {
    let words: &[u32] = bytemuck::try_cast_slice(bytes)
        .map_err(|e| GpuError::Operation(format!("invalid marker readback: {e}")))?;
    let too_short = words.len().checked_sub(groups as usize).is_none();
    if too_short {
        return Err(GpuError::IncompleteDispatch(groups.to_string()));
    }
    let mut found = false;
    for (index, marker) in words.iter().take(groups as usize).enumerate() {
        match *marker {
            COMPLETION_MAGIC => {}
            COMPLETION_FOUND_MAGIC if allow_found => found = true,
            value => {
                return Err(GpuError::IncompleteDispatch(format!(
                    "workgroup {index} returned 0x{value:08x}"
                )));
            }
        }
    }
    Ok(found)
}

pub fn valid_workgroups(limits: &AdapterLimits) -> Vec<u32> {
    [32, 64, 128, 256, 512, 1_024]
        .into_iter()
        .filter(|size| {
            *size <= limits.max_compute_invocations_per_workgroup
                && *size <= limits.max_compute_workgroup_size_x
        })
        .collect()
}

pub fn max_dispatch_invocations(limits: &AdapterLimits, workgroup: u32) -> u32 {
    limits
        .max_compute_workgroups_per_dimension
        .saturating_mul(workgroup)
        .min(MAX_HOST_DISPATCH_INVOCATIONS)
        .max(workgroup)
}

pub fn false_alarm_batch_capacity(limits: &AdapterLimits, workgroup: u32) -> Result<u32> {
    let offset_candidate_alignment = limits.min_storage_buffer_offset_alignment.max(1)
        / gcd(
            limits.min_storage_buffer_offset_alignment.max(1),
            STATE_BYTES as u32,
        );
    let alignment = lcm(workgroup, offset_candidate_alignment);
    let raw = limits
        .max_compute_workgroups_per_dimension
        .saturating_mul(workgroup)
        .min(limits.max_storage_buffer_binding_size / STATE_BYTES as u32);
    let capacity = raw / alignment * alignment;
    if capacity == 0 {
        Err(GpuError::Unsupported(
            "storage binding is too small for one false-alarm batch".into(),
        ))
    } else {
        Ok(capacity)
    }
}

pub fn sliced_steps_for_range(start: u32, length: u32, slice_start: u32, slice_steps: u32) -> u64 {
    if length == 0 || slice_steps == 0 {
        return 0;
    }
    let end = u64::from(start) + u64::from(length) - 1;
    let active_start = u64::from(start).max(u64::from(slice_start) + 1);
    if active_start > end {
        return 0;
    }
    let ramp_end = end.min(u64::from(slice_start) + u64::from(slice_steps) - 1);
    let mut steps = 0u64;
    if active_start <= ramp_end {
        let count = ramp_end - active_start + 1;
        let first = active_start - u64::from(slice_start);
        let last = ramp_end - u64::from(slice_start);
        steps = steps.saturating_add(count.saturating_mul(first + last) / 2);
    }
    let full_start = active_start.max(u64::from(slice_start) + u64::from(slice_steps));
    if full_start <= end {
        steps = steps.saturating_add((end - full_start + 1) * u64::from(slice_steps));
    }
    steps
}

pub fn sliced_dispatch_width(
    start: u32,
    remaining: u32,
    slice_start: u32,
    slice_steps: u32,
    target_steps: u64,
    maximum_width: u32,
    alignment: u32,
) -> u32 {
    let maximum = remaining.min(maximum_width).max(1);
    let mut low = 1u32;
    let mut high = maximum;
    let mut best = 1u32;
    while low <= high {
        let middle = low + (high - low) / 2;
        if sliced_steps_for_range(start, middle, slice_start, slice_steps) <= target_steps {
            best = middle;
            low = middle.saturating_add(1);
        } else {
            high = middle.saturating_sub(1);
        }
    }
    if maximum >= alignment {
        best = (best / alignment).max(1) * alignment;
    }
    best.min(maximum).max(1)
}

pub fn steps_for_range(start: u32, length: u32) -> u64 {
    if length == 0 {
        return 0;
    }
    let start = u64::from(start);
    let length = u64::from(length);
    length * start + length * (length - 1) / 2
}

pub fn shader_bundle_hash() -> String {
    let mut digest = Sha256::new();
    for source in [
        PRECOMPUTE_COMPACT,
        PRECOMPUTE_EXPANDED,
        FALSE_ALARM_COMPACT,
        FALSE_ALARM_EXPANDED,
    ] {
        digest.update(source.as_bytes());
    }
    digest.update(DES_LUT);
    hex::encode(digest.finalize())
}

pub fn default_tuning_cache_path() -> Option<PathBuf> {
    crate::platform::project_dirs().map(|dirs| dirs.cache_dir().join("gpu-tuning.json"))
}

fn adapter_fingerprint(info: &wgpu::AdapterInfo) -> String {
    let mut digest = Sha256::new();
    digest.update(backend_name(info.backend));
    digest.update(info.vendor.to_le_bytes());
    digest.update(info.device.to_le_bytes());
    digest.update(info.name.as_bytes());
    digest.update(info.driver.as_bytes());
    digest.update(info.driver_info.as_bytes());
    hex::encode(&digest.finalize()[..16])
}

fn auto_score(entry: &CatalogEntry) -> (u8, u32, u32, u64) {
    (
        entry.type_rank,
        entry.report.limits.max_compute_invocations_per_workgroup,
        entry.report.limits.max_compute_workgroup_storage_size,
        entry.report.limits.max_buffer_size,
    )
}

fn device_type_rank(device_type: wgpu::DeviceType) -> u8 {
    match device_type {
        wgpu::DeviceType::DiscreteGpu => 4,
        wgpu::DeviceType::IntegratedGpu => 3,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Cpu => 1,
        _ => 0,
    }
}

fn is_hardware_device_type(device_type: wgpu::DeviceType) -> bool {
    matches!(
        device_type,
        wgpu::DeviceType::DiscreteGpu
            | wgpu::DeviceType::IntegratedGpu
            | wgpu::DeviceType::VirtualGpu
    )
}

fn device_type_name(device_type: wgpu::DeviceType) -> &'static str {
    match device_type {
        wgpu::DeviceType::DiscreteGpu => "discrete",
        wgpu::DeviceType::IntegratedGpu => "integrated",
        wgpu::DeviceType::VirtualGpu => "virtual",
        wgpu::DeviceType::Cpu => "cpu",
        _ => "other",
    }
}

fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Metal => "metal",
        wgpu::Backend::Dx12 => "dx12",
        wgpu::Backend::Gl => "gl",
        wgpu::Backend::BrowserWebGpu => "browser",
        wgpu::Backend::Noop => "noop",
    }
}

fn selector_summary(entries: &[CatalogEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{}:{} ({})",
                entry.report.index, entry.report.name, entry.report.selector_id
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn validate_lut() -> Result<()> {
    if DES_LUT.len() != LUT_SIZE {
        Err(GpuError::Operation(format!(
            "embedded LUT is {} bytes; expected {LUT_SIZE}",
            DES_LUT.len()
        )))
    } else {
        Ok(())
    }
}

fn div_ceil_u32(value: u32, divisor: u32) -> u32 {
    value.div_ceil(divisor)
}

fn align_to(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

fn align_up_u32(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn lcm(left: u32, right: u32) -> u32 {
    left / gcd(left, right) * right
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> AdapterLimits {
        AdapterLimits {
            max_buffer_size: 1 << 30,
            max_storage_buffer_binding_size: 1 << 30,
            max_compute_workgroup_storage_size: 32_768,
            max_compute_invocations_per_workgroup: 1_024,
            max_compute_workgroup_size_x: 1_024,
            max_compute_workgroups_per_dimension: 65_535,
            min_storage_buffer_offset_alignment: 256,
        }
    }

    fn tuning_row(
        shader: ShaderVariant,
        workgroup_size: u32,
        rate: f64,
        rate_cv: f64,
    ) -> TuningMeasurement {
        TuningMeasurement {
            shader,
            workgroup_size,
            stage: "test".into(),
            elapsed_ms: 50.0,
            steps: rate as u64 / 20,
            steps_per_second: rate,
            valid: true,
            note: None,
            sample_count: 3,
            rate_cv,
        }
    }

    #[test]
    fn automatic_hardware_classification_excludes_software_adapters() {
        assert!(is_hardware_device_type(wgpu::DeviceType::DiscreteGpu));
        assert!(is_hardware_device_type(wgpu::DeviceType::IntegratedGpu));
        assert!(is_hardware_device_type(wgpu::DeviceType::VirtualGpu));
        assert!(!is_hardware_device_type(wgpu::DeviceType::Cpu));
        assert!(!is_hardware_device_type(wgpu::DeviceType::Other));
    }

    #[test]
    fn shader_assets_and_abis_are_embedded() {
        validate_lut().unwrap();
        for variant in ShaderVariant::ALL {
            assert!(variant.precompute_source().contains("struct Params"));
            assert!(variant.precompute_source().contains("completion_buf"));
            assert!(variant.false_alarm_source().contains("struct FalseParams"));
            assert!(variant.false_alarm_source().contains("CandidateState"));
        }
        assert_eq!(shader_bundle_hash().len(), 64);
    }

    #[test]
    fn dispatch_is_bounded_below_adapter_dimension_limit() {
        let limits = limits();
        let width = max_dispatch_invocations(&limits, 64);
        assert_eq!(width, 65_536);
        let groups = div_ceil_u32(width, 64);
        assert_eq!(groups, 1_024);
        assert!(groups <= limits.max_compute_workgroups_per_dimension);
    }

    #[test]
    fn low_dimension_limit_is_honoured() {
        let mut limits = limits();
        limits.max_compute_workgroups_per_dimension = 10;
        assert_eq!(max_dispatch_invocations(&limits, 64), 640);
    }

    #[test]
    fn false_alarm_capacity_is_binding_and_offset_aligned() {
        let mut limits = limits();
        limits.max_compute_workgroups_per_dimension = 100;
        limits.max_storage_buffer_binding_size = 100_000;
        let capacity = false_alarm_batch_capacity(&limits, 64).unwrap();
        assert!(capacity <= 100 * 64);
        assert_eq!((capacity * STATE_BYTES as u32) % 256, 0);
        assert_eq!(capacity % 64, 0);
    }

    #[test]
    fn marker_validation_accepts_found_only_when_requested() {
        let words = [COMPLETION_MAGIC, COMPLETION_FOUND_MAGIC];
        let bytes = bytemuck::cast_slice(&words);
        assert!(validate_markers(bytes, 2, true).unwrap());
        assert!(validate_markers(bytes, 2, false).is_err());
        assert!(validate_markers(bytemuck::cast_slice(&[0u32]), 1, true).is_err());
    }

    #[test]
    fn sliced_step_count_matches_simple_reference() {
        fn slow(start: u32, length: u32, slice_start: u32, slice_steps: u32) -> u64 {
            (start..start + length)
                .map(|position| {
                    if position <= slice_start {
                        0
                    } else {
                        (position - slice_start).min(slice_steps) as u64
                    }
                })
                .sum()
        }
        for start in [0, 1, 10, 100] {
            for length in [1, 2, 31, 100] {
                assert_eq!(
                    sliced_steps_for_range(start, length, 7, 33),
                    slow(start, length, 7, 33)
                );
            }
        }
    }

    #[test]
    fn sliced_width_respects_budget_and_alignment() {
        let width = sliced_dispatch_width(100, 10_000, 0, 65_536, 1_000_000, 65_536, 64);
        assert_eq!(width % 64, 0);
        assert!(sliced_steps_for_range(100, width, 0, 65_536) <= 1_000_000);
    }

    #[test]
    fn params_match_wgsl_layout_and_little_endian_hash() {
        let params = precompute_params(
            [1, 2, 3, 4, 5, 6, 7, 8],
            2,
            881_689,
            10,
            20,
            30,
            40,
            50,
            false,
        );
        assert_eq!(std::mem::size_of::<PrecomputeParams>(), 48);
        assert_eq!(params.hash_lo, 0x0403_0201);
        assert_eq!(params.hash_hi, 0x0807_0605);
        assert_eq!(params.reduction_offset, 131_072);
        assert_eq!(std::mem::size_of::<FalseParams>(), 32);
        assert_eq!(std::mem::size_of::<CandidateState>(), 32);
    }

    #[test]
    fn cache_ignores_wrong_bundle_or_schema() {
        let selection = TuningSelection {
            shader: ShaderVariant::Compact,
            workgroup_size: 64,
            minimum_workgroups: 128,
            estimated_steps_per_second: 1.0,
            source: "test".into(),
            selection_reason: "test".into(),
            tuning_elapsed_ms: 0.0,
            deadline_reached: false,
            measurements: vec![],
        };
        let mut cache = TuningCache::default();
        cache.put("adapter".into(), selection.clone());
        assert_eq!(cache.get("adapter"), Some(&selection));
        cache.entries[0].schema += 1;
        assert!(cache.get("adapter").is_none());
    }

    #[test]
    fn tuning_cache_round_trips_through_atomic_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("gpu-tuning.json");
        let selection = TuningSelection {
            shader: ShaderVariant::Compact,
            workgroup_size: 64,
            minimum_workgroups: 128,
            estimated_steps_per_second: 1.0,
            source: "test".into(),
            selection_reason: "test".into(),
            tuning_elapsed_ms: 1.0,
            deadline_reached: false,
            measurements: vec![],
        };
        let mut cache = TuningCache::default();
        cache.put("adapter".into(), selection.clone());
        cache.save_atomic(&path).unwrap();
        assert_eq!(
            TuningCache::load(&path).unwrap().get("adapter"),
            Some(&selection)
        );
    }

    #[test]
    fn cache_identity_uses_one_directory_except_for_macos_bundle_id() {
        let path = default_tuning_cache_path().unwrap();
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(
            rendered.ends_with("ntlmrain/cache/gpu-tuning.json")
                || rendered.ends_with("ntlmrain/gpu-tuning.json")
        );
        if cfg!(target_os = "windows") {
            assert!(!rendered.contains("ntlmrain/ntlmrain"));
        }
    }

    #[test]
    fn slow_pilot_requires_two_samples_strictly_below_ten_million() {
        assert!(!confirmed_slow_tuning_rates(&[5_000_000.0]));
        assert!(!confirmed_slow_tuning_rates(&[9_000_000.0, 11_000_000.0]));
        assert!(confirmed_slow_tuning_rates(&[8_000_000.0, 9_000_000.0]));
    }

    #[test]
    fn alternating_rounds_reverse_candidate_order() {
        let keys = [
            (ShaderVariant::Compact, 32),
            (ShaderVariant::Compact, 64),
            (ShaderVariant::Expanded, 64),
        ];
        assert_eq!(alternating_tuning_order(&keys, 0), keys);
        assert_eq!(
            alternating_tuning_order(&keys, 1),
            keys.into_iter().rev().collect::<Vec<_>>()
        );
    }

    #[test]
    fn stable_winner_prefers_compact_or_cached_incumbent_inside_tie_band() {
        let rows = [
            tuning_row(ShaderVariant::Compact, 64, 100.0, 0.01),
            tuning_row(ShaderVariant::Expanded, 128, 102.0, 0.01),
        ];
        let compact = stable_tuning_winner(&rows, None, false).unwrap();
        assert_eq!(compact.key, (ShaderVariant::Compact, 64));
        assert_eq!(compact.reason, "deterministic-tie-break");

        let incumbent =
            stable_tuning_winner(&rows, Some((ShaderVariant::Expanded, 128)), false).unwrap();
        assert_eq!(incumbent.key, (ShaderVariant::Expanded, 128));
        assert_eq!(incumbent.reason, "cached-winner-retained");

        let clear_rows = [
            tuning_row(ShaderVariant::Compact, 64, 100.0, 0.01),
            tuning_row(ShaderVariant::Expanded, 128, 120.0, 0.01),
        ];
        let clear =
            stable_tuning_winner(&clear_rows, Some((ShaderVariant::Compact, 64)), false).unwrap();
        assert_eq!(clear.key, (ShaderVariant::Expanded, 128));
        assert_eq!(clear.reason, "clear-winner");
    }

    #[test]
    fn production_score_is_weighted_harmonic_rate() {
        let key = (ShaderVariant::Compact, 64);
        let mut samples = HashMap::new();
        for (index, rate) in [100.0, 50.0, 25.0].into_iter().enumerate() {
            samples.insert((key, index), vec![tuning_row(key.0, key.1, rate, 0.0)]);
        }
        let score = production_tuning_measurements(&[key], &samples)[0].steps_per_second;
        let expected = 1.0 / (0.16 / 100.0 + 0.40 / 50.0 + 0.44 / 25.0);
        assert!((score - expected).abs() < 1e-9);
    }

    #[test]
    fn zero_budget_deadline_schedules_no_work() {
        let deadline = TuningDeadline::new(Duration::ZERO);
        assert!(deadline.expired());
        assert!(!deadline.can_schedule(0.0));
    }

    #[test]
    fn manual_request_validation_rejects_zero_targets() {
        let mut request = PrecomputeRequest::new([0; 8]);
        request.dispatch = DispatchMode::FixedSteps(0);
        assert!(validate_precompute_request(&request).is_err());
        request.dispatch = DispatchMode::AdaptiveMs(0);
        assert!(validate_precompute_request(&request).is_err());
    }

    #[test]
    fn rejected_gpu_hit_is_cleared_and_can_continue() {
        let mut states = [CandidateState {
            result_lo: 7,
            result_hi: 0,
            target_position: 100,
            next_position: 11,
            found: 1,
            ..CandidateState::zeroed()
        }];
        let mut recovered = Vec::new();
        let mut reject = |_: u64, _: &[u8; 8]| false;
        let result =
            process_candidate_hits(&mut states, &[0; 8], false, &mut reject, &mut recovered);
        assert_eq!(result.rejected, 1);
        assert!(result.changed);
        assert!(!result.stop);
        assert_eq!(states[0].found, 0);
        assert_eq!(states[0].next_position, 11);
        advance_candidate_mirror_without_hit(&mut states, 20);
        assert_eq!(states[0].next_position, 31);
    }

    #[test]
    fn false_alarm_compaction_removes_finished_lanes_and_preserves_work() {
        let mut states = [
            CandidateState {
                index_lo: 10,
                target_position: 100,
                next_position: 64,
                ..CandidateState::zeroed()
            },
            CandidateState {
                index_lo: 20,
                target_position: 3,
                next_position: 4,
                ..CandidateState::zeroed()
            },
            CandidateState {
                index_lo: 30,
                target_position: 80,
                next_position: 12,
                ..CandidateState::zeroed()
            },
        ];
        let mut finished_steps = 7;
        let result = compact_candidate_states(&mut states, 3, &mut finished_steps);
        assert_eq!(result.active_count, 2);
        assert!(result.moved);
        assert_eq!(finished_steps, 11);
        assert_eq!(states[0].index_lo, 10);
        assert_eq!(states[1].index_lo, 30);
    }

    #[test]
    fn all_mode_clears_accepted_hit_and_keeps_searching() {
        let mut states = [CandidateState {
            result_lo: 9,
            target_position: 10,
            next_position: 4,
            found: 1,
            ..CandidateState::zeroed()
        }];
        let mut recovered = Vec::new();
        let mut accept = |index: u64, _: &[u8; 8]| index == 9;
        let result =
            process_candidate_hits(&mut states, &[0; 8], true, &mut accept, &mut recovered);
        assert_eq!(recovered, vec![9]);
        assert!(result.changed);
        assert!(!result.stop);
        assert_eq!(states[0].found, 0);
    }
}
