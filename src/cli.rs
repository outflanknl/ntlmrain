//! Command-line orchestration for ntlmrain.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, SecondsFormat};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    CHAIN_LEN, FIXED_CHALLENGE_HEX, TABLE_INDEX,
    artifacts::{RunArtifacts, RunManifest, atomic_write},
    compute::{
        ComputeCandidate, ComputeContext, CpuContext, available_cpu_threads,
        native_cpu_implementation,
    },
    config::{Config, ConfigOverrides},
    cpu::{assemble_nt_hash, byte7_index_to_plaintext, expand_des_key, recover_pt3},
    formats::{CandidateFile, EndpointFile, Role, order_paths_by_role, run_id_from_filename},
    gpu::{
        BackendChoice, DeviceCatalog, DispatchMode, GpuContext, GpuOptions, PrecomputeRequest,
        ShaderChoice, WorkgroupChoice,
    },
    input::{ParsedTarget, Target, parse_target},
    local_lookup::{LocalLookupOptions, LocalTable},
    remote_lookup::{RemoteLookupClient, RemoteLookupConfig, RemoteProgress},
};

pub const EXIT_NO_MATCH: i32 = 2;
pub const EXIT_INPUT_ARTIFACT: i32 = 3;
pub const EXIT_GPU: i32 = 4;
pub const EXIT_LOOKUP: i32 = 5;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("no verified plaintext was found")]
    NoMatch,
    #[error("{0}")]
    InputArtifact(String),
    #[error("{0}")]
    Gpu(String),
    #[error("{0}")]
    Lookup(String),
}

impl CliError {
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::NoMatch => EXIT_NO_MATCH,
            Self::InputArtifact(_) => EXIT_INPUT_ARTIFACT,
            Self::Gpu(_) => EXIT_GPU,
            Self::Lookup(_) => EXIT_LOOKUP,
        }
    }
}

pub fn error_exit_code(error: &anyhow::Error) -> i32 {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<CliError>())
        .map(CliError::exit_code)
        .unwrap_or(1)
}

#[derive(Debug, Parser)]
#[command(
    name = "ntlmrain",
    version,
    about = "Native NetNTLMv1 rainbow-table recovery"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    quiet: bool,
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long, global = true, value_name = "URL")]
    remote_url: Option<String>,
    #[arg(long, global = true)]
    remote_username: Option<String>,
    #[arg(long, global = true)]
    remote_password: Option<String>,
    #[arg(long, global = true)]
    no_remote_auth: bool,
    #[arg(long, global = true, default_value = "artifacts", value_name = "DIR")]
    artifacts_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run precompute, lookup, verification, and local DES3 recovery when applicable.
    Crack(CrackArgs),
    /// Generate Web2-compatible NTLMEND1 endpoint files.
    Precompute(PrecomputeArgs),
    /// Resolve one or two endpoint files using the remote service or local table.
    Lookup(LookupCommandArgs),
    /// Verify one or two NTLMCAN1 candidate files on the selected compute engine.
    Verify(VerifyArgs),
    /// Benchmark and cache a GPU shader/workgroup selection.
    Tune(TuneArgs),
    /// Enumerate WebGPU adapters and native CPU fallback capability.
    Devices(DevicesArgs),
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct TargetArgs {
    #[arg(long, value_name = "CAPTURE_OR_HEX")]
    netntlmv1: Option<String>,
    #[arg(long, value_name = "HEX")]
    des: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BackendArg {
    Auto,
    Dx12,
    Vulkan,
    Metal,
}
impl From<BackendArg> for BackendChoice {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::Auto => Self::Auto,
            BackendArg::Dx12 => Self::Dx12,
            BackendArg::Vulkan => Self::Vulkan,
            BackendArg::Metal => Self::Metal,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ShaderArg {
    Auto,
    Compact,
    Expanded,
}
impl From<ShaderArg> for ShaderChoice {
    fn from(value: ShaderArg) -> Self {
        match value {
            ShaderArg::Auto => Self::Auto,
            ShaderArg::Compact => Self::Compact,
            ShaderArg::Expanded => Self::Expanded,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DispatchArg {
    Adaptive,
    Fixed,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum, PartialEq, Eq)]
enum ComputeArg {
    #[default]
    Auto,
    Webgpu,
    Cpu,
}

#[derive(Clone, Debug, Args)]
struct GpuArgs {
    #[arg(long, default_value = "auto")]
    device: String,
    #[arg(long, value_enum, default_value_t = BackendArg::Auto)]
    backend: BackendArg,
    #[arg(long, value_enum, default_value_t = ShaderArg::Auto)]
    shader: ShaderArg,
    #[arg(long, default_value = "auto")]
    workgroup: String,
    #[arg(long, value_enum, default_value_t = DispatchArg::Adaptive)]
    dispatch_mode: DispatchArg,
    #[arg(long, default_value_t = 1.8)]
    adaptive_target: f64,
    #[arg(long, default_value = "256M")]
    dispatch_steps: String,
    #[arg(long)]
    retune: bool,
    #[arg(long, value_name = "PATH")]
    tuning_cache: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
struct ComputeArgs {
    /// Compute engine: hardware WebGPU with native CPU fallback, forced WebGPU, or forced CPU.
    #[arg(long, value_enum, default_value_t = ComputeArg::Auto)]
    compute: ComputeArg,
    /// Native CPU worker count. Defaults to all available logical CPUs.
    #[arg(long, value_parser = clap::value_parser!(usize))]
    cpu_threads: Option<usize>,
    #[command(flatten)]
    gpu: GpuArgs,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LookupBackendArg {
    Remote,
    Local,
}

#[derive(Clone, Debug, Args)]
struct LookupArgs {
    #[arg(long = "lookup", value_enum, default_value_t = LookupBackendArg::Remote)]
    lookup_backend: LookupBackendArg,
    #[arg(long)]
    data_base: Option<PathBuf>,
    #[arg(long)]
    index: Option<PathBuf>,
    #[arg(long)]
    read_workers: Option<usize>,
    #[arg(long)]
    preload_index: bool,
    #[arg(long)]
    lock_index: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RoleArg {
    Des1,
    Des2,
}
impl From<RoleArg> for Role {
    fn from(value: RoleArg) -> Self {
        match value {
            RoleArg::Des1 => Self::Des1,
            RoleArg::Des2 => Self::Des2,
        }
    }
}

#[derive(Debug, Args)]
struct CrackArgs {
    #[command(flatten)]
    target: TargetArgs,
    #[command(flatten)]
    compute: ComputeArgs,
    #[command(flatten)]
    lookup: LookupArgs,
    #[arg(long)]
    all: bool,
}
#[derive(Debug, Args)]
struct PrecomputeArgs {
    #[command(flatten)]
    target: TargetArgs,
    #[command(flatten)]
    compute: ComputeArgs,
    #[arg(long, value_enum)]
    role: Option<RoleArg>,
}
#[derive(Debug, Args)]
struct LookupCommandArgs {
    #[arg(required = true, num_args = 1..=2, value_name = "ENDPOINTS")]
    endpoints: Vec<PathBuf>,
    #[command(flatten)]
    lookup: LookupArgs,
    #[arg(long, value_enum)]
    role: Option<RoleArg>,
}
#[derive(Debug, Args)]
struct VerifyArgs {
    #[arg(required = true, num_args = 1..=2, value_name = "CANDIDATES")]
    candidates: Vec<PathBuf>,
    #[command(flatten)]
    target: TargetArgs,
    #[command(flatten)]
    compute: ComputeArgs,
    #[arg(long, value_enum)]
    role: Option<RoleArg>,
    #[arg(long)]
    all: bool,
}
#[derive(Debug, Args)]
struct TuneArgs {
    #[command(flatten)]
    gpu: GpuArgs,
}
#[derive(Debug, Args)]
struct DevicesArgs {
    #[arg(long, value_enum, default_value_t = BackendArg::Auto)]
    backend: BackendArg,
}

struct ProgressOutput {
    quiet: bool,
    interactive: bool,
    state: Mutex<ProgressState>,
}

#[derive(Debug)]
struct ProgressState {
    active: bool,
    last_width: usize,
    last_noninteractive: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipelineStage {
    Precompute,
    Lookup,
    Verify,
}

impl PipelineStage {
    fn heading(self) -> &'static str {
        match self {
            Self::Precompute => "==> Stage 1: precompute endpoints",
            Self::Lookup => "==> Stage 2: table lookup",
            Self::Verify => "==> Stage 3: verify candidate chains",
        }
    }
}
impl ProgressOutput {
    fn new(quiet: bool) -> Self {
        Self {
            quiet,
            interactive: io::stderr().is_terminal(),
            state: Mutex::new(ProgressState {
                active: false,
                last_width: 0,
                last_noninteractive: None,
            }),
        }
    }

    fn message(&self, message: impl AsRef<str>) {
        if self.quiet {
            return;
        }
        let mut state = self.state.lock().expect("progress lock");
        end_active_line(&mut state);
        eprintln!("{}", message.as_ref());
    }

    fn stage(&self, stage: PipelineStage) {
        self.message(stage.heading());
    }

    fn saved(&self, kind: &str, path: &Path) {
        self.message(format!("    saved {kind}: {}", path.display()));
    }

    fn status(&self, message: impl AsRef<str>) {
        if self.quiet {
            return;
        }
        let message = message.as_ref();
        let mut state = self.state.lock().expect("progress lock");
        if self.interactive {
            render_active_line(&mut state, message);
        } else {
            let now = Instant::now();
            let should_emit = state
                .last_noninteractive
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(2));
            if should_emit {
                eprintln!("{message}");
                state.last_noninteractive = Some(now);
            }
        }
    }

    fn finish(&self, message: impl AsRef<str>) {
        if self.quiet {
            return;
        }
        let message = message.as_ref();
        let mut state = self.state.lock().expect("progress lock");
        if self.interactive {
            render_active_line(&mut state, message);
            eprintln!();
        } else {
            eprintln!("{message}");
        }
        state.active = false;
        state.last_width = 0;
        state.last_noninteractive = None;
    }

    fn end(&self) {
        if self.quiet {
            return;
        }
        let mut state = self.state.lock().expect("progress lock");
        end_active_line(&mut state);
    }
}

fn render_active_line(state: &mut ProgressState, message: &str) {
    let padding = state.last_width.saturating_sub(message.chars().count());
    eprint!("\r{message}{}", " ".repeat(padding));
    let _ = io::stderr().flush();
    state.active = true;
    state.last_width = message.chars().count();
}

fn end_active_line(state: &mut ProgressState) {
    if state.active {
        eprintln!();
        state.active = false;
        state.last_width = 0;
    }
    state.last_noninteractive = None;
}

fn progress_line(label: &str, fraction: f64, detail: &str) -> String {
    let fraction = fraction.clamp(0.0, 1.0);
    let filled = (fraction * 20.0).round() as usize;
    format!(
        "{label} [{}{}] {:5.1}% | {detail}",
        "#".repeat(filled),
        "-".repeat(20 - filled),
        fraction * 100.0
    )
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn human_duration(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "--".into();
    }
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}h{:02}m{:02}s",
            seconds / 3_600,
            (seconds % 3_600) / 60,
            seconds % 60
        )
    }
}

fn rate_and_eta(done: u64, total: u64, elapsed: Duration, unit: &str) -> String {
    if total > 0 && done >= total {
        return rate_and_elapsed(done, elapsed, unit);
    }
    let seconds = elapsed.as_secs_f64().max(0.001);
    let rate = done as f64 / seconds;
    let eta = if done == 0 {
        "--".into()
    } else {
        human_duration(total.saturating_sub(done) as f64 / rate.max(f64::MIN_POSITIVE))
    };
    format!(
        "{} {unit}/s | elapsed {} | ETA {eta}",
        human_rate(rate),
        human_duration(seconds)
    )
}

fn rate_and_elapsed(done: u64, elapsed: Duration, unit: &str) -> String {
    let seconds = elapsed.as_secs_f64().max(0.001);
    format!(
        "{} {unit}/s | elapsed {}",
        human_rate(done as f64 / seconds),
        human_duration(seconds)
    )
}

fn human_rate(rate: f64) -> String {
    for (scale, suffix) in [(1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "K")] {
        if rate >= scale {
            return format!("{:.2}{suffix}", rate / scale);
        }
    }
    format!("{rate:.2}")
}

fn parameter_label(role: &str) -> &'static str {
    match role {
        "des1" => "DES1 (CT1)",
        "des2" => "DES2 (CT2)",
        _ => "DES",
    }
}

fn show_parameters(
    progress: &ProgressOutput,
    targets: &[(String, [u8; 8])],
    k3_ciphertext: Option<[u8; 8]>,
    started_at: &DateTime<Local>,
) {
    progress.message("Parameters:");
    progress.message(format!(
        "    started: {}",
        started_at.to_rfc3339_opts(SecondsFormat::Secs, true)
    ));
    for (role, target) in targets {
        let label = parameter_label(role);
        progress.message(format!("    {label}: {}", hex::encode_upper(target)));
    }
    if let Some(target) = k3_ciphertext {
        progress.message(format!("    DES3 (CT3): {}", hex::encode_upper(target)));
    }
    progress.message(format!("    challenge: {FIXED_CHALLENGE_HEX}"));
    progress.message("");
}

fn announce_gpu(progress: &ProgressOutput, context: &GpuContext, args: &GpuArgs) {
    let device_selection = if args.device.eq_ignore_ascii_case("auto") {
        "selected automatically"
    } else {
        "selected by --device"
    };
    let tuning = match context.selection.source.as_str() {
        "auto-tune" => format!("auto-tuned ({})", context.selection.selection_reason),
        "cache" => "cached auto-tune".into(),
        "manual" => "manual shader/workgroup".into(),
        "partial-override-tune" => {
            format!(
                "tuned with manual override ({})",
                context.selection.selection_reason
            )
        }
        "slow-adapter-default" => "slow-adapter default".into(),
        other => other.to_owned(),
    };
    progress.message(format!(
        "[GPU] {device_selection}: {} ({}) \u{00b7} {} \u{00b7} {} \u{00b7} WG {} \u{00b7} {tuning}",
        context.adapter.name,
        context.adapter.selector_id,
        context.adapter.backend,
        context.selection.shader,
        context.selection.workgroup_size
    ));
    if let Some(cache) = &context.tuning_cache {
        match cache.action.as_str() {
            "loaded" => progress.message(format!(
                "      tuning cache loaded: {}",
                cache.path.display()
            )),
            "saved" => progress.message(format!(
                "      tuning cache saved: {}",
                cache.path.display()
            )),
            "load-failed" | "save-failed" => progress.message(format!(
                "      warning: tuning cache {}: {}",
                cache.action,
                cache.warning.as_deref().unwrap_or("unknown cache error")
            )),
            _ => {}
        }
    }
}
fn announce_compute(progress: &ProgressOutput, context: &ComputeContext, args: &ComputeArgs) {
    match context {
        ComputeContext::WebGpu(gpu) => announce_gpu(progress, gpu, &args.gpu),
        ComputeContext::Cpu(cpu) => {
            let selection = if cpu.metadata.selection == "forced" {
                "selected by --compute cpu".to_owned()
            } else if let Some(adapter) = &cpu.metadata.ignored_webgpu_adapter {
                format!("selected automatically: {adapter} is a CPU/software WebGPU adapter")
            } else {
                "selected automatically: no hardware GPU detected".to_owned()
            };
            progress.message(format!(
                "[CPU] {selection} \u{00b7} {} \u{00b7} {} threads",
                cpu.metadata.implementation, cpu.metadata.threads
            ));
            if cpu.metadata.ignored_webgpu_adapter.is_some() {
                progress.message(
                    "      use --compute webgpu to force the selected software WebGPU adapter",
                );
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct RecoveredPart {
    role: Option<String>,
    ciphertext: String,
    index: String,
    plaintext: String,
    des_key: String,
}
#[derive(Debug, Serialize)]
struct RecoveryResult {
    status: &'static str,
    started_at: String,
    elapsed_seconds: f64,
    recovered: Vec<RecoveredPart>,
    nt_hashes: Vec<String>,
    artifact_dir: Option<String>,
}
struct StagedFile {
    path: PathBuf,
    role: Option<Role>,
}

fn pt3_recovered_part(ciphertext: [u8; 8], pt3: [u8; 2]) -> RecoveredPart {
    let plaintext7 = [pt3[0], pt3[1], 0, 0, 0, 0, 0];
    RecoveredPart {
        role: Some("des3".into()),
        ciphertext: hex::encode_upper(ciphertext),
        index: u16::from_be_bytes(pt3).to_string(),
        plaintext: hex::encode_upper(pt3),
        des_key: hex::encode_upper(expand_des_key(&plaintext7)),
    }
}

fn result_role(role: Role) -> String {
    role.to_string()
}
enum LookupEngine {
    Remote(Box<RemoteLookupClient>),
    Local(Box<LocalTable>),
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let json_output = cli.json;
    match run_cli(cli) {
        Ok(()) => Ok(()),
        Err(error) => {
            if json_output && !matches!(error, CliError::NoMatch) {
                let _ = emit_json(&json!({
                    "status": "error",
                    "exit_code": error.exit_code(),
                    "error": error.to_string(),
                }));
            }
            Err(anyhow::Error::new(error))
        }
    }
}

fn run_cli(cli: Cli) -> Result<(), CliError> {
    let config = Config::load(
        cli.config.as_deref(),
        &ConfigOverrides {
            remote_url: cli.remote_url.clone(),
            remote_username: cli.remote_username.clone(),
            remote_password: cli.remote_password.clone(),
            remote_auth: cli.no_remote_auth.then_some(false),
        },
    )
    .map_err(input_error)?;
    let progress = ProgressOutput::new(cli.quiet);
    let result = match cli.command {
        Command::Devices(args) => command_devices(args, cli.json),
        Command::Tune(args) => command_tune(args, cli.json, &progress),
        Command::Precompute(args) => {
            command_precompute(args, cli.json, &cli.artifacts_dir, &progress)
        }
        Command::Lookup(args) => {
            command_lookup(args, cli.json, &cli.artifacts_dir, &config, &progress)
        }
        Command::Verify(args) => command_verify(args, cli.json, &cli.artifacts_dir, &progress),
        Command::Crack(args) => {
            command_crack(args, cli.json, &cli.artifacts_dir, &config, &progress)
        }
    };
    progress.end();
    result
}

fn command_devices(args: DevicesArgs, json_output: bool) -> Result<(), CliError> {
    let reports = DeviceCatalog::enumerate(args.backend.into())
        .map_err(gpu_error)?
        .reports();
    if json_output {
        emit_json(&json!({
            "devices": reports,
            "native_cpu": {
                "available": true,
                "implementation": native_cpu_implementation(),
                "architecture": std::env::consts::ARCH,
                "logical_threads": available_cpu_threads()
            }
        }))?;
    } else {
        for report in reports {
            println!(
                "[{}] {} · {} · {} · {} (vendor {:04x}, device {:04x})",
                report.index,
                report.selector_id,
                report.backend,
                report.name,
                report.device_type,
                report.vendor_id,
                report.device_id
            );
            println!(
                "    driver: {} {} · shaders: {} · max WG X: {} · max groups/dim: {}",
                report.driver,
                report.driver_info,
                report
                    .supported_shaders
                    .iter()
                    .map(|shader| shader.id())
                    .collect::<Vec<_>>()
                    .join(","),
                report.limits.max_compute_workgroup_size_x,
                report.limits.max_compute_workgroups_per_dimension
            );
            println!(
                "    automatic compute: {}",
                if report.hardware_eligible {
                    "eligible"
                } else {
                    "excluded (CPU/software WebGPU adapter)"
                }
            );
        }
        println!(
            "[CPU] native \u{00b7} {} \u{00b7} {} logical threads \u{00b7} automatic fallback",
            native_cpu_implementation(),
            available_cpu_threads()
        );
    }
    Ok(())
}

fn command_tune(
    mut args: TuneArgs,
    json_output: bool,
    progress: &ProgressOutput,
) -> Result<(), CliError> {
    progress.message("selecting and tuning GPU...");
    args.gpu.retune = true;
    let context = create_gpu(&args.gpu)?;
    announce_gpu(progress, &context, &args.gpu);
    if json_output {
        emit_json(
            &json!({ "device": context.adapter, "selection": context.selection, "cache": context.tuning_cache }),
        )?;
    } else {
        println!(
            "{} ({}) · {} · WG {} · {:.3}G steps/s · {}",
            context.adapter.name,
            context.adapter.selector_id,
            context.selection.shader,
            context.selection.workgroup_size,
            context.selection.estimated_steps_per_second / 1_000_000_000.0,
            context.selection.selection_reason
        );
    }
    Ok(())
}

fn command_precompute(
    args: PrecomputeArgs,
    json_output: bool,
    artifact_root: &Path,
    progress: &ProgressOutput,
) -> Result<(), CliError> {
    let started_at = Local::now();
    let total_started = Instant::now();
    let parsed = parse_target_args(&args.target)?;
    let targets = parsed.target.des_targets();
    if targets.len() > 1 && args.role.is_some() {
        return Err(input_error("--role is valid only for a single DES target"));
    }
    let roles = if targets.len() == 1 {
        vec![args.role.map(Role::from).unwrap_or(Role::Des1)]
    } else {
        vec![Role::Des1, Role::Des2]
    };
    let parameters = targets
        .iter()
        .copied()
        .zip(roles.iter().copied())
        .map(|(target, role)| (role.to_string(), target))
        .collect::<Vec<_>>();
    show_parameters(
        progress,
        &parameters,
        parsed.target.k3_ciphertext(),
        &started_at,
    );
    let run = RunArtifacts::create(artifact_root).map_err(input_error)?;
    progress.message("selecting compute engine...");
    let context = create_compute(&args.compute, progress)?;
    announce_compute(progress, &context, &args.compute);
    let dispatch = compute_dispatch(&context, &args.compute.gpu)?;
    progress.message("");
    progress.stage(PipelineStage::Precompute);
    let mut outputs = BTreeMap::new();
    for (target, role) in targets.into_iter().zip(roles) {
        progress.message(format!("precomputing {}...", role.human_label()));
        let started = Instant::now();
        let request = precompute_request(target, dispatch);
        let endpoints = context
            .precompute_with_progress(&request, |state| {
                let fraction = state.steps_done as f64 / state.steps_total.max(1) as f64;
                progress.status(progress_line(
                    role.human_label(),
                    fraction,
                    &format!(
                        "{}/{} endpoints | {}",
                        state.endpoints_done,
                        state.endpoints_total,
                        rate_and_eta(
                            state.steps_done,
                            state.steps_total,
                            started.elapsed(),
                            "steps"
                        )
                    ),
                ))
            })
            .map_err(gpu_error)?;
        progress.finish(progress_line(
            role.human_label(),
            1.0,
            &format!(
                "{} endpoints | elapsed {}",
                endpoints.len(),
                human_duration(started.elapsed().as_secs_f64())
            ),
        ));
        let path = run.endpoint_path(role);
        EndpointFile::new(endpoints)
            .write_atomic(&path)
            .map_err(input_error)?;
        outputs.insert(role.to_string(), display_path(&path));
        progress.saved("endpoints", &path);
    }
    let elapsed_seconds = total_started.elapsed().as_secs_f64();
    let result = json!({
        "status": "ok",
        "started_at": started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "elapsed_seconds": elapsed_seconds,
        "artifact_dir": display_path(&run.directory),
        "endpoints": outputs
    });
    write_manifest(
        &run,
        "precompute",
        Some(&parsed),
        Some(&context),
        &outputs,
        &result,
    )?;
    emit_value(
        &result,
        json_output,
        format!(
            "endpoints written to {}\nTotal elapsed: {}",
            run.directory.display(),
            human_duration(elapsed_seconds)
        ),
    )
}

impl LookupEngine {
    fn lookup(&self, endpoints: &[u8], progress: &ProgressOutput) -> Result<Vec<u8>, CliError> {
        match self {
            Self::Remote(client) => {
                let cancelled = AtomicBool::new(false);
                let upload_started = Instant::now();
                let mut processing_started = None;
                let mut download_started = None;
                client
                    .lookup(endpoints, Some(&cancelled), |event| match event {
                        RemoteProgress::Upload {
                            loaded,
                            total,
                            done,
                        } => {
                            let line = progress_line(
                                "    lookup upload",
                                if done {
                                    1.0
                                } else {
                                    loaded as f64 / total.max(1) as f64
                                },
                                &format!(
                                    "{} / {} | elapsed {}{}",
                                    human_bytes(loaded),
                                    human_bytes(total),
                                    human_duration(upload_started.elapsed().as_secs_f64()),
                                    if done { "" } else { " | connecting/sending" }
                                ),
                            );
                            if done {
                                progress.finish(line);
                            } else {
                                progress.status(line);
                            }
                        }
                        RemoteProgress::Status(status) => {
                            let started = *processing_started.get_or_insert_with(Instant::now);
                            let elapsed = started.elapsed();
                            let detail = if status.state == "queued" {
                                format!(
                                    "{} / {} records | elapsed {} | queue position {}",
                                    status.processed_records,
                                    status.record_count,
                                    human_duration(elapsed.as_secs_f64()),
                                    status
                                        .queue_position
                                        .map(|position| position.to_string())
                                        .unwrap_or_else(|| "pending".into())
                                )
                            } else if status.state == "ready" {
                                format!(
                                    "{} / {} records | {}",
                                    status.processed_records,
                                    status.record_count,
                                    rate_and_elapsed(status.processed_records, elapsed, "records")
                                )
                            } else {
                                format!(
                                    "{} / {} records | {}",
                                    status.processed_records,
                                    status.record_count,
                                    rate_and_eta(
                                        status.processed_records,
                                        status.record_count,
                                        elapsed,
                                        "records"
                                    )
                                )
                            };
                            let line = progress_line(
                                &format!("    lookup {}", status.state),
                                status.progress,
                                &detail,
                            );
                            if status.state == "ready" {
                                progress.finish(line);
                            } else {
                                progress.status(line);
                            }
                        }
                        RemoteProgress::Download {
                            loaded,
                            total,
                            done,
                        } => {
                            let started = *download_started.get_or_insert_with(Instant::now);
                            let elapsed = started.elapsed();
                            let (fraction, detail) = match total {
                                Some(total) => (
                                    if done {
                                        1.0
                                    } else {
                                        loaded as f64 / total.max(1) as f64
                                    },
                                    format!(
                                        "{} / {} | {}",
                                        human_bytes(loaded),
                                        human_bytes(total),
                                        if done {
                                            rate_and_elapsed(loaded, elapsed, "bytes")
                                        } else {
                                            rate_and_eta(loaded, total, elapsed, "bytes")
                                        }
                                    ),
                                ),
                                None => (
                                    if done { 1.0 } else { 0.0 },
                                    format!(
                                        "{} | {:.2} MiB/s | elapsed {}",
                                        human_bytes(loaded),
                                        loaded as f64
                                            / elapsed.as_secs_f64().max(0.001)
                                            / (1024.0 * 1024.0),
                                        human_duration(elapsed.as_secs_f64())
                                    ),
                                ),
                            };
                            let line = progress_line("    lookup download", fraction, &detail);
                            if done {
                                progress.finish(line);
                            } else {
                                progress.status(line);
                            }
                        }
                    })
                    .map_err(lookup_error)
            }
            Self::Local(table) => table
                .lookup_endpoint_file(endpoints, None)
                .map_err(lookup_error),
        }
    }
}

fn command_lookup(
    args: LookupCommandArgs,
    json_output: bool,
    artifact_root: &Path,
    config: &Config,
    progress: &ProgressOutput,
) -> Result<(), CliError> {
    let files = resolve_stage_files(&args.endpoints, args.role)?;
    let input_suffix = shared_endpoint_suffix(&files)?;
    let engine = create_lookup_engine(&args.lookup, config)?;
    let run = RunArtifacts::create(artifact_root).map_err(input_error)?;
    progress.stage(PipelineStage::Lookup);
    let mut outputs = BTreeMap::new();
    for (index, staged) in files.iter().enumerate() {
        progress.message(match staged.role {
            Some(role) => format!("looking up {}...", role.human_label()),
            None => format!("looking up {}...", staged.path.display()),
        });
        let bytes = fs::read(&staged.path).map_err(input_error)?;
        let endpoint_file = EndpointFile::decode(&bytes).map_err(input_error)?;
        require_endpoint_count(endpoint_file.endpoints.len())?;
        let candidates = engine.lookup(&bytes, progress)?;
        CandidateFile::decode(&candidates).map_err(input_error)?;
        let suffix = input_suffix.as_deref().unwrap_or(&run.run_id);
        let path = match staged.role {
            Some(role) => run
                .directory
                .join(format!("{}-{suffix}.candidates", role.filename_prefix())),
            None => run.directory.join(format!("lookup-{suffix}.candidates")),
        };
        atomic_write(&path, &candidates).map_err(input_error)?;
        progress.saved("candidates", &path);
        let label = staged
            .role
            .map(|role| role.to_string())
            .unwrap_or_else(|| format!("input-{}", index + 1));
        outputs.insert(label, display_path(&path));
    }
    let result = json!({ "status": "ok", "artifact_dir": display_path(&run.directory), "candidates": outputs });
    write_manifest(&run, "lookup", None, None, &outputs, &result)?;
    emit_value(
        &result,
        json_output,
        format!("candidates written to {}", run.directory.display()),
    )
}

fn command_verify(
    args: VerifyArgs,
    json_output: bool,
    artifact_root: &Path,
    progress: &ProgressOutput,
) -> Result<(), CliError> {
    let started_at = Local::now();
    let total_started = Instant::now();
    let parsed = parse_target_args(&args.target)?;
    let files = resolve_stage_files(&args.candidates, args.role)?;
    validate_verification_shape(&files, &parsed.target)?;
    let parameters = files
        .iter()
        .map(|staged| {
            target_for_role(&parsed.target, staged.role, files.len()).map(|target| {
                (
                    staged
                        .role
                        .map(|role| role.to_string())
                        .unwrap_or_else(|| "des".into()),
                    target,
                )
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    show_parameters(
        progress,
        &parameters,
        parsed.target.k3_ciphertext(),
        &started_at,
    );
    let run = RunArtifacts::create(artifact_root).map_err(input_error)?;
    progress.message("selecting compute engine...");
    let context = create_compute(&args.compute, progress)?;
    announce_compute(progress, &context, &args.compute);
    progress.message("");
    progress.stage(PipelineStage::Verify);
    let recovery = verify_files(&context, &files, &parsed, args.all, progress)?;
    finish_recovery(
        "verify",
        json_output,
        &run,
        &parsed,
        &context,
        BTreeMap::new(),
        files.len(),
        recovery,
        &started_at,
        total_started,
    )
}

fn command_crack(
    args: CrackArgs,
    json_output: bool,
    artifact_root: &Path,
    config: &Config,
    progress: &ProgressOutput,
) -> Result<(), CliError> {
    let started_at = Local::now();
    let total_started = Instant::now();
    let parsed = parse_target_args(&args.target)?;
    let parameters = parsed
        .target
        .des_targets()
        .into_iter()
        .enumerate()
        .map(|(index, target)| ([Role::Des1, Role::Des2][index].to_string(), target))
        .collect::<Vec<_>>();
    show_parameters(
        progress,
        &parameters,
        parsed.target.k3_ciphertext(),
        &started_at,
    );
    let run = RunArtifacts::create(artifact_root).map_err(input_error)?;
    progress.message("selecting compute engine...");
    let context = create_compute(&args.compute, progress)?;
    announce_compute(progress, &context, &args.compute);
    let engine = create_lookup_engine(&args.lookup, config)?;
    let dispatch = compute_dispatch(&context, &args.compute.gpu)?;
    let mut outputs = BTreeMap::new();

    progress.message("");
    progress.stage(PipelineStage::Precompute);
    let mut endpoint_files = Vec::new();
    for (index, target) in parsed.target.des_targets().into_iter().enumerate() {
        let role = [Role::Des1, Role::Des2][index];
        progress.message(format!("    precomputing {}...", role.human_label()));
        let started = Instant::now();
        let endpoints = context
            .precompute_with_progress(&precompute_request(target, dispatch), |state| {
                let fraction = state.steps_done as f64 / state.steps_total.max(1) as f64;
                progress.status(progress_line(
                    &format!("    {}", role.human_label()),
                    fraction,
                    &format!(
                        "{}/{} endpoints | {}",
                        state.endpoints_done,
                        state.endpoints_total,
                        rate_and_eta(
                            state.steps_done,
                            state.steps_total,
                            started.elapsed(),
                            "steps"
                        )
                    ),
                ))
            })
            .map_err(gpu_error)?;
        progress.finish(progress_line(
            &format!("    {}", role.human_label()),
            1.0,
            &format!(
                "{} endpoints | elapsed {}",
                endpoints.len(),
                human_duration(started.elapsed().as_secs_f64())
            ),
        ));
        let endpoint_file = EndpointFile::new(endpoints);
        let endpoint_path = run.endpoint_path(role);
        endpoint_file
            .write_atomic(&endpoint_path)
            .map_err(input_error)?;
        outputs.insert(format!("{role}-endpoints"), display_path(&endpoint_path));
        progress.saved("endpoints", &endpoint_path);
        endpoint_files.push((role, endpoint_file));
    }

    progress.message("");
    progress.stage(PipelineStage::Lookup);
    let mut candidate_files = Vec::new();
    for (role, endpoint_file) in endpoint_files {
        progress.message(format!("    looking up {}...", role.human_label()));
        let candidate_bytes = engine.lookup(&endpoint_file.encode(), progress)?;
        CandidateFile::decode(&candidate_bytes).map_err(input_error)?;
        let candidate_path = run.candidate_path(role);
        atomic_write(&candidate_path, &candidate_bytes).map_err(input_error)?;
        outputs.insert(format!("{role}-candidates"), display_path(&candidate_path));
        progress.saved("candidates", &candidate_path);
        candidate_files.push(StagedFile {
            path: candidate_path,
            role: Some(role),
        });
    }

    progress.message("");
    progress.stage(PipelineStage::Verify);
    let recovery = verify_files(&context, &candidate_files, &parsed, args.all, progress)?;
    finish_recovery(
        "crack",
        json_output,
        &run,
        &parsed,
        &context,
        outputs,
        candidate_files.len(),
        recovery,
        &started_at,
        total_started,
    )
}

struct VerificationOutput {
    recovered: Vec<RecoveredPart>,
    nt_hashes: Vec<String>,
}

fn verify_files(
    context: &ComputeContext,
    files: &[StagedFile],
    parsed: &ParsedTarget,
    all: bool,
    progress: &ProgressOutput,
) -> Result<VerificationOutput, CliError> {
    let mut recovered = Vec::new();
    let mut recovered_indexes: BTreeMap<Role, Vec<u64>> = BTreeMap::new();
    for staged in files {
        let candidate_file = CandidateFile::read(&staged.path).map_err(input_error)?;
        require_endpoint_count(
            usize::try_from(candidate_file.query_count)
                .map_err(|_| input_error("candidate query count does not fit this platform"))?,
        )?;
        let target = target_for_role(&parsed.target, staged.role, files.len())?;
        let candidates = candidate_file
            .records
            .into_iter()
            .map(|record| {
                Ok(ComputeCandidate {
                    start: record.start,
                    position: u32::try_from(record.ordinal).map_err(|_| {
                        input_error("candidate ordinal does not fit the GPU format")
                    })?,
                })
            })
            .collect::<Result<Vec<_>, CliError>>()?;
        progress.message(format!(
            "    verifying {} candidate chains{}...",
            candidates.len(),
            staged
                .role
                .map(|role| format!(" for {}", role.human_label()))
                .unwrap_or_default()
        ));
        let verify_started = Instant::now();
        let hits = context
            .verify_with_progress(&candidates, target, TABLE_INDEX, all, |state| {
                let budget = state
                    .step_budget
                    .map(|budget| format!(" | budget {budget}"))
                    .unwrap_or_default();
                progress.status(progress_line(
                    "    verify",
                    state.completed_steps as f64 / state.total_steps.max(1) as f64,
                    &format!(
                        "{} / {} steps | {} active{} | {}",
                        state.completed_steps,
                        state.total_steps,
                        state.active_candidates,
                        budget,
                        rate_and_eta(
                            state.completed_steps,
                            state.total_steps,
                            verify_started.elapsed(),
                            "steps"
                        )
                    ),
                ))
            })
            .map_err(gpu_error)?;
        progress.finish(progress_line(
            "    verify",
            1.0,
            &format!(
                "{} candidates | {} verified plaintext chunks | elapsed {}",
                candidates.len(),
                hits.len(),
                human_duration(verify_started.elapsed().as_secs_f64())
            ),
        ));
        for index in &hits {
            let plaintext = byte7_index_to_plaintext(*index);
            recovered.push(RecoveredPart {
                role: staged.role.map(result_role),
                ciphertext: hex::encode_upper(target),
                index: index.to_string(),
                plaintext: hex::encode_upper(plaintext),
                des_key: hex::encode_upper(expand_des_key(&plaintext)),
            });
        }
        if let Some(role) = staged.role {
            recovered_indexes.entry(role).or_default().extend(hits);
        }
    }
    let mut nt_hashes = Vec::new();
    if let Target::FullResponse(response) = &parsed.target {
        let ct3: [u8; 8] = response[16..24].try_into().expect("eight bytes");
        if let Some(pt3) = recover_pt3(&ct3) {
            recovered.push(pt3_recovered_part(ct3, pt3));
            if let (Some(pt1), Some(pt2)) = (
                recovered_indexes.get(&Role::Des1),
                recovered_indexes.get(&Role::Des2),
            ) {
                'outer: for left in pt1 {
                    for right in pt2 {
                        nt_hashes.push(hex::encode_upper(assemble_nt_hash(*left, *right, pt3)));
                        if !all {
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    Ok(VerificationOutput {
        recovered,
        nt_hashes,
    })
}

#[allow(clippy::too_many_arguments)]
fn finish_recovery(
    command: &str,
    json_output: bool,
    run: &RunArtifacts,
    parsed: &ParsedTarget,
    context: &ComputeContext,
    outputs: BTreeMap<String, String>,
    file_count: usize,
    recovery: VerificationOutput,
    started_at: &DateTime<Local>,
    total_started: Instant,
) -> Result<(), CliError> {
    let complete = recovery_complete(
        &parsed.target,
        file_count,
        &recovery.recovered,
        &recovery.nt_hashes,
    );
    let elapsed_seconds = (total_started.elapsed().as_secs_f64() * 1_000.0).round() / 1_000.0;
    let result = RecoveryResult {
        status: if complete { "ok" } else { "no_match" },
        started_at: started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        elapsed_seconds,
        recovered: recovery.recovered,
        nt_hashes: recovery.nt_hashes,
        artifact_dir: Some(display_path(&run.directory)),
    };
    let value = serde_json::to_value(&result).map_err(input_error)?;
    write_manifest(run, command, Some(parsed), Some(context), &outputs, &value)?;
    emit_recovery(&result, json_output)?;
    if complete {
        Ok(())
    } else {
        Err(CliError::NoMatch)
    }
}

fn validate_verification_shape(files: &[StagedFile], target: &Target) -> Result<(), CliError> {
    match (files.len(), target) {
        (1, Target::Des(_)) => Ok(()),
        (1, Target::TwoDes(_) | Target::FullResponse(_)) if files[0].role.is_some() => Ok(()),
        (1, _) => Err(input_error(
            "a single candidate file with a multi-part response requires a DES1 or DES2 role",
        )),
        (2, Target::TwoDes(_) | Target::FullResponse(_)) => Ok(()),
        (2, Target::Des(_)) => Err(input_error(
            "two candidate files require a 32- or 48-character NetNTLMv1 response",
        )),
        _ => Err(input_error(
            "verification requires one or two candidate files",
        )),
    }
}

fn recovery_complete(
    target: &Target,
    file_count: usize,
    recovered: &[RecoveredPart],
    nt_hashes: &[String],
) -> bool {
    if file_count == 1 {
        return recovered
            .iter()
            .any(|part| part.role.as_deref() != Some("des3"));
    }
    match target {
        Target::Des(_) => false,
        Target::TwoDes(_) => {
            recovered
                .iter()
                .any(|part| part.role.as_deref() == Some("des1"))
                && recovered
                    .iter()
                    .any(|part| part.role.as_deref() == Some("des2"))
        }
        Target::FullResponse(_) => {
            !nt_hashes.is_empty()
                && recovered
                    .iter()
                    .any(|part| part.role.as_deref() == Some("des1"))
                && recovered
                    .iter()
                    .any(|part| part.role.as_deref() == Some("des2"))
        }
    }
}

fn target_for_role(
    target: &Target,
    role: Option<Role>,
    file_count: usize,
) -> Result<[u8; 8], CliError> {
    match target {
        Target::Des(bytes) if file_count == 1 => Ok(*bytes),
        Target::TwoDes(bytes) => select_response_part(bytes, role),
        Target::FullResponse(bytes) => select_response_part(&bytes[..16], role),
        _ => Err(input_error("target does not match the candidate file set")),
    }
}

fn select_response_part(bytes: &[u8], role: Option<Role>) -> Result<[u8; 8], CliError> {
    let role = role.ok_or_else(|| input_error("candidate role is required"))?;
    let offset = role.key_index() * 8;
    Ok(bytes[offset..offset + 8]
        .try_into()
        .expect("validated response"))
}

fn parse_target_args(args: &TargetArgs) -> Result<ParsedTarget, CliError> {
    match (&args.netntlmv1, &args.des) {
        (Some(value), None) => {
            let parsed = parse_target(value).map_err(input_error)?;
            if matches!(parsed.target, Target::Des(_)) {
                return Err(input_error(
                    "--netntlmv1 requires a 32/48-character response or full capture",
                ));
            }
            Ok(parsed)
        }
        (None, Some(value)) => {
            let parsed = parse_target(value).map_err(input_error)?;
            if !matches!(parsed.target, Target::Des(_)) {
                return Err(input_error(
                    "--des requires exactly 16 hexadecimal characters",
                ));
            }
            Ok(parsed)
        }
        _ => Err(input_error("provide exactly one of --netntlmv1 or --des")),
    }
}

fn resolve_stage_files(
    paths: &[PathBuf],
    explicit_role: Option<RoleArg>,
) -> Result<Vec<StagedFile>, CliError> {
    if paths.len() == 2 && explicit_role.is_some() {
        return Err(input_error("--role is valid only with one file"));
    }
    order_paths_by_role(paths)
        .map_err(input_error)?
        .into_iter()
        .map(|(path, inferred)| {
            let explicit = explicit_role.map(Role::from);
            if inferred.is_some() && explicit.is_some() && inferred != explicit {
                return Err(input_error(format!(
                    "--role conflicts with filename role for {}",
                    path.display()
                )));
            }
            Ok(StagedFile {
                path,
                role: explicit.or(inferred),
            })
        })
        .collect()
}

fn shared_endpoint_suffix(files: &[StagedFile]) -> Result<Option<String>, CliError> {
    let suffixes = files
        .iter()
        .map(|file| run_id_from_filename(&file.path))
        .collect::<Vec<_>>();
    if suffixes.len() == 2 && suffixes[0] != suffixes[1] {
        return Err(input_error(
            "two endpoint files must have the same 12-hex run ID suffix",
        ));
    }
    Ok(suffixes.into_iter().next().flatten())
}

fn require_endpoint_count(count: usize) -> Result<(), CliError> {
    let expected = CHAIN_LEN as usize - 1;
    if count != expected {
        return Err(input_error(format!(
            "artifact contains {count} endpoint records; expected {expected}"
        )));
    }
    Ok(())
}

fn create_gpu(args: &GpuArgs) -> Result<GpuContext, CliError> {
    GpuContext::create(&gpu_options(args)?).map_err(gpu_error)
}

fn create_compute(
    args: &ComputeArgs,
    progress: &ProgressOutput,
) -> Result<ComputeContext, CliError> {
    if args.cpu_threads == Some(0) {
        return Err(input_error("--cpu-threads must be at least 1"));
    }
    match args.compute {
        ComputeArg::Cpu => {
            let incompatible = cpu_incompatible_gpu_options(&args.gpu);
            if !incompatible.is_empty() {
                return Err(input_error(format!(
                    "--compute cpu cannot be combined with {}",
                    incompatible.join(", ")
                )));
            }
            CpuContext::create(args.cpu_threads, "forced", None)
                .map(ComputeContext::Cpu)
                .map_err(gpu_error)
        }
        ComputeArg::Webgpu => {
            if args.cpu_threads.is_some() {
                return Err(input_error(
                    "--cpu-threads is not used with --compute webgpu",
                ));
            }
            create_gpu(&args.gpu).map(|gpu| ComputeContext::WebGpu(Box::new(gpu)))
        }
        ComputeArg::Auto => {
            let catalog = DeviceCatalog::enumerate(args.gpu.backend.into()).map_err(gpu_error)?;
            let hardware = catalog
                .select_hardware_report(&args.gpu.device)
                .map_err(gpu_error)?;
            if let Some(report) = hardware {
                let options = gpu_options(&args.gpu)?;
                return GpuContext::create_from_catalog(&catalog, &report.selector_id, &options)
                    .map(|gpu| ComputeContext::WebGpu(Box::new(gpu)))
                    .map_err(gpu_error);
            }

            let ignored_adapter = if args.gpu.device.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(
                    catalog
                        .select_report(&args.gpu.device)
                        .map_err(gpu_error)?
                        .name,
                )
            };
            let ignored = gpu_tuning_override_names(&args.gpu);
            if !ignored.is_empty() {
                progress.message(format!(
                    "[CPU] ignoring GPU-only options during automatic fallback: {}",
                    ignored.join(", ")
                ));
            }
            CpuContext::create(args.cpu_threads, "auto-fallback", ignored_adapter)
                .map(ComputeContext::Cpu)
                .map_err(gpu_error)
        }
    }
}

fn gpu_tuning_override_names(args: &GpuArgs) -> Vec<&'static str> {
    let mut names = Vec::new();
    if !matches!(args.shader, ShaderArg::Auto) {
        names.push("--shader");
    }
    if !args.workgroup.eq_ignore_ascii_case("auto") {
        names.push("--workgroup");
    }
    if !matches!(args.dispatch_mode, DispatchArg::Adaptive) {
        names.push("--dispatch-mode");
    }
    if (args.adaptive_target - 1.8).abs() >= f64::EPSILON {
        names.push("--adaptive-target");
    }
    if !args.dispatch_steps.eq_ignore_ascii_case("256M") {
        names.push("--dispatch-steps");
    }
    if args.retune {
        names.push("--retune");
    }
    if args.tuning_cache.is_some() {
        names.push("--tuning-cache");
    }
    names
}

fn cpu_incompatible_gpu_options(args: &GpuArgs) -> Vec<&'static str> {
    let mut names = gpu_tuning_override_names(args);
    if !args.device.eq_ignore_ascii_case("auto") {
        names.push("--device");
    }
    if !matches!(args.backend, BackendArg::Auto) {
        names.push("--backend");
    }
    names
}

fn compute_dispatch(context: &ComputeContext, args: &GpuArgs) -> Result<DispatchMode, CliError> {
    if matches!(context, ComputeContext::WebGpu(_)) {
        parse_dispatch(args)
    } else {
        Ok(DispatchMode::default())
    }
}

fn gpu_options(args: &GpuArgs) -> Result<GpuOptions, CliError> {
    Ok(GpuOptions {
        backend: args.backend.into(),
        device: args.device.clone(),
        shader: args.shader.into(),
        workgroup: parse_workgroup(&args.workgroup)?,
        dispatch: parse_dispatch(args)?,
        retune: args.retune,
        tuning_cache: args
            .tuning_cache
            .clone()
            .or_else(crate::gpu::default_tuning_cache_path),
    })
}

fn parse_workgroup(value: &str) -> Result<WorkgroupChoice, CliError> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(WorkgroupChoice::Auto);
    }
    let size = value
        .parse::<u32>()
        .map_err(|_| input_error("invalid --workgroup value"))?;
    if !matches!(size, 32 | 64 | 128 | 256 | 512 | 1024) {
        return Err(input_error(
            "--workgroup must be auto, 32, 64, 128, 256, 512, or 1024",
        ));
    }
    Ok(WorkgroupChoice::Fixed(size))
}

fn parse_dispatch(args: &GpuArgs) -> Result<DispatchMode, CliError> {
    match args.dispatch_mode {
        DispatchArg::Adaptive => {
            let ms = if (args.adaptive_target - 0.8).abs() < f64::EPSILON {
                800
            } else if (args.adaptive_target - 1.2).abs() < f64::EPSILON {
                1_200
            } else if (args.adaptive_target - 1.8).abs() < f64::EPSILON {
                1_800
            } else {
                return Err(input_error(
                    "--adaptive-target must be 0.8, 1.2, or 1.8 seconds",
                ));
            };
            Ok(DispatchMode::AdaptiveMs(ms))
        }
        DispatchArg::Fixed => Ok(DispatchMode::FixedSteps(
            match args.dispatch_steps.trim().to_ascii_lowercase().as_str() {
                "64m" => 64_000_000,
                "128m" => 128_000_000,
                "256m" => 256_000_000,
                "512m" => 512_000_000,
                "1b" => 1_000_000_000,
                _ => {
                    return Err(input_error(
                        "--dispatch-steps must be 64M, 128M, 256M, 512M, or 1B",
                    ));
                }
            },
        )),
    }
}

fn precompute_request(target: [u8; 8], dispatch: DispatchMode) -> PrecomputeRequest {
    let mut request = PrecomputeRequest::new(target);
    request.chain_len = CHAIN_LEN;
    request.table_index = TABLE_INDEX;
    request.dispatch = dispatch;
    request
}

fn create_lookup_engine(args: &LookupArgs, config: &Config) -> Result<LookupEngine, CliError> {
    match args.lookup_backend {
        LookupBackendArg::Remote => {
            if args.data_base.is_some() || args.index.is_some() {
                return Err(input_error(
                    "--data-base and --index apply only to --lookup local",
                ));
            }
            RemoteLookupClient::new(RemoteLookupConfig {
                base_url: config.remote.url.clone(),
                username: config.remote.username.clone(),
                password: config.remote.password.clone(),
                poll_interval: Duration::from_secs(2),
                request_timeout: Duration::from_secs(30 * 60),
            })
            .map(Box::new)
            .map(LookupEngine::Remote)
            .map_err(lookup_error)
        }
        LookupBackendArg::Local => {
            let data_base = args
                .data_base
                .clone()
                .ok_or_else(|| input_error("--lookup local requires --data-base"))?;
            let index = args
                .index
                .clone()
                .ok_or_else(|| input_error("--lookup local requires --index"))?;
            let mut options = LocalLookupOptions::new(data_base, index);
            if let Some(workers) = args.read_workers {
                options.read_workers = workers;
            }
            options.preload_index = args.preload_index;
            options.lock_index = args.lock_index;
            LocalTable::open(options)
                .map(Box::new)
                .map(LookupEngine::Local)
                .map_err(lookup_error)
        }
    }
}

fn write_manifest(
    run: &RunArtifacts,
    command: &str,
    parsed: Option<&ParsedTarget>,
    compute: Option<&ComputeContext>,
    outputs: &BTreeMap<String, String>,
    result: &Value,
) -> Result<(), CliError> {
    let mut manifest: RunManifest = run.new_manifest(command);
    manifest.input = parsed.map(|parsed| json!({ "target": parsed.target.to_hex(), "capture": parsed.capture.as_ref().map(|capture| json!({ "username": capture.username, "domain": capture.domain })) }));
    manifest.outputs = outputs.clone();
    manifest.result = Some(result.clone());
    if let Some(compute) = compute {
        match compute {
            ComputeContext::WebGpu(gpu) => {
                manifest.compute = Some(json!({
                    "kind": "webgpu",
                    "implementation": "wgpu",
                    "backend": gpu.adapter.backend,
                    "tuning_cache": gpu.tuning_cache
                }));
                manifest.selected_device =
                    Some(serde_json::to_value(&gpu.adapter).map_err(input_error)?);
                manifest.tuning = Some(serde_json::to_value(&gpu.selection).map_err(input_error)?);
            }
            ComputeContext::Cpu(cpu) => {
                manifest.compute = Some(serde_json::to_value(&cpu.metadata).map_err(input_error)?);
            }
        }
    }
    run.write_manifest(&manifest).map_err(input_error)
}

fn emit_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(input_error)?
    );
    Ok(())
}
fn emit_value(value: &Value, json_output: bool, human: String) -> Result<(), CliError> {
    if json_output {
        emit_json(value)
    } else {
        println!("{human}");
        Ok(())
    }
}
fn human_recovered_part(part: &RecoveredPart) -> String {
    let number = part
        .role
        .as_deref()
        .and_then(|role| role.strip_prefix("des"))
        .unwrap_or("");
    format!(
        "PT{number}: {} · K{number}: {} (index {})",
        part.plaintext, part.des_key, part.index
    )
}

fn emit_recovery(result: &RecoveryResult, json_output: bool) -> Result<(), CliError> {
    if json_output {
        return emit_json(result);
    }
    println!();
    println!("-------------------- Results --------------------");
    if result.recovered.is_empty() {
        println!("No exact plaintext chunk was recovered.");
    }
    for part in &result.recovered {
        println!("{}", human_recovered_part(part));
    }
    for hash in &result.nt_hashes {
        println!("NTLM: {hash}");
    }
    println!("Total elapsed: {}", human_duration(result.elapsed_seconds));
    if let Some(directory) = &result.artifact_dir {
        println!("Artifacts: {directory}");
    }
    Ok(())
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
fn input_error(error: impl std::fmt::Display) -> CliError {
    CliError::InputArtifact(error.to_string())
}
fn gpu_error(error: impl std::fmt::Display) -> CliError {
    CliError::Gpu(error.to_string())
}
fn lookup_error(error: impl std::fmt::Display) -> CliError {
    CliError::Lookup(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_schema_is_valid() {
        Cli::command().debug_assert();
    }
    #[test]
    fn parses_target_entrypoints() {
        assert!(
            Cli::try_parse_from([
                "ntlmrain",
                "crack",
                "--netntlmv1",
                "727B4E35F947129EA52B9CDEDAE86934BB23EF89F50FC595"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["ntlmrain", "precompute", "--des", "727B4E35F947129E"]).is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "ntlmrain",
                "verify",
                "--compute",
                "cpu",
                "--cpu-threads",
                "6",
                "--des",
                "727B4E35F947129E",
                "sample.candidates"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "ntlmrain",
                "crack",
                "--compute",
                "webgpu",
                "--des",
                "727B4E35F947129E"
            ])
            .is_ok()
        );
        for retired in ["max", "fullchain", "fullfused"] {
            assert!(
                Cli::try_parse_from([
                    "ntlmrain",
                    "crack",
                    "--shader",
                    retired,
                    "--des",
                    "727B4E35F947129E",
                ])
                .is_err(),
                "retired shader selector {retired} must be rejected",
            );
        }
    }

    #[test]
    fn cpu_gpu_option_compatibility_is_explicit() {
        let default_gpu = GpuArgs {
            device: "auto".into(),
            backend: BackendArg::Auto,
            shader: ShaderArg::Auto,
            workgroup: "auto".into(),
            dispatch_mode: DispatchArg::Adaptive,
            adaptive_target: 1.8,
            dispatch_steps: "256M".into(),
            retune: false,
            tuning_cache: None,
        };
        assert!(cpu_incompatible_gpu_options(&default_gpu).is_empty());
        let mut manual = default_gpu.clone();
        manual.shader = ShaderArg::Compact;
        manual.device = "0".into();
        assert_eq!(
            cpu_incompatible_gpu_options(&manual),
            vec!["--shader", "--device"]
        );
    }
    #[test]
    fn stable_exit_codes() {
        assert_eq!(CliError::NoMatch.exit_code(), 2);
        assert_eq!(input_error("x").exit_code(), 3);
        assert_eq!(gpu_error("x").exit_code(), 4);
        assert_eq!(lookup_error("x").exit_code(), 5);
        assert_eq!(error_exit_code(&anyhow::Error::new(CliError::NoMatch)), 2);
    }

    #[test]
    fn human_progress_formatting_is_stable() {
        assert_eq!(
            PipelineStage::Precompute.heading(),
            "==> Stage 1: precompute endpoints"
        );
        assert_eq!(PipelineStage::Lookup.heading(), "==> Stage 2: table lookup");
        assert_eq!(
            PipelineStage::Verify.heading(),
            "==> Stage 3: verify candidate chains"
        );
        assert_eq!(
            progress_line("stage", 0.5, "detail"),
            "stage [##########----------]  50.0% | detail"
        );
        assert_eq!(human_bytes(7_053_536), "6.73 MiB");
        assert_eq!(human_duration(3_661.0), "1h01m01s");
        assert_eq!(
            rate_and_eta(50, 100, Duration::from_secs(10), "items"),
            "5.00 items/s | elapsed 10s | ETA 10s"
        );
        assert_eq!(
            rate_and_elapsed(100, Duration::from_secs(10), "items"),
            "10.00 items/s | elapsed 10s"
        );
        assert_eq!(
            rate_and_eta(100, 100, Duration::from_secs(10), "items"),
            "10.00 items/s | elapsed 10s"
        );
        assert_eq!(human_rate(2_180_000_000.0), "2.18G");
        assert_eq!(parameter_label("des1"), "DES1 (CT1)");
        assert_eq!(parameter_label("des2"), "DES2 (CT2)");
        assert_eq!(parameter_label("single"), "DES");
    }

    #[test]
    fn staged_lookup_preserves_one_shared_suffix() {
        let same = vec![
            StagedFile {
                path: PathBuf::from("des1-a1b2c3d4e5f6.endpoints"),
                role: Some(Role::Des1),
            },
            StagedFile {
                path: PathBuf::from("des2-a1b2c3d4e5f6.endpoints"),
                role: Some(Role::Des2),
            },
        ];
        assert_eq!(
            shared_endpoint_suffix(&same).unwrap().as_deref(),
            Some("a1b2c3d4e5f6")
        );
        let different = vec![
            StagedFile {
                path: PathBuf::from("des1-a1b2c3d4e5f6.endpoints"),
                role: Some(Role::Des1),
            },
            StagedFile {
                path: PathBuf::from("des2-000000000000.endpoints"),
                role: Some(Role::Des2),
            },
        ];
        assert!(shared_endpoint_suffix(&different).is_err());
    }

    #[test]
    fn staged_artifacts_require_the_production_query_count() {
        assert!(require_endpoint_count(CHAIN_LEN as usize - 1).is_ok());
        assert!(require_endpoint_count(CHAIN_LEN as usize).is_err());
    }

    #[test]
    fn recovered_part_index_is_json_string_and_full_success_requires_hash() {
        let part = RecoveredPart {
            role: Some("des1".into()),
            ciphertext: "00".repeat(8),
            index: u64::MAX.to_string(),
            plaintext: "00".repeat(7),
            des_key: "01".repeat(8),
        };
        let value = serde_json::to_value(&part).unwrap();
        assert!(value["index"].is_string());

        let recovered = vec![
            RecoveredPart {
                role: Some("des1".into()),
                ciphertext: String::new(),
                index: "1".into(),
                plaintext: String::new(),
                des_key: String::new(),
            },
            RecoveredPart {
                role: Some("des2".into()),
                ciphertext: String::new(),
                index: "2".into(),
                plaintext: String::new(),
                des_key: String::new(),
            },
        ];
        assert!(!recovery_complete(
            &Target::FullResponse([0; 24]),
            2,
            &recovered,
            &[]
        ));
        assert!(recovery_complete(
            &Target::FullResponse([0; 24]),
            2,
            &recovered,
            &["00".repeat(16)]
        ));
    }

    #[test]
    fn des3_result_uses_precise_pt3_and_k3_notation() {
        let ciphertext = hex::decode("BB23EF89F50FC595").unwrap().try_into().unwrap();
        let result = pt3_recovered_part(ciphertext, [0x58, 0x6c]);
        assert_eq!(result.role.as_deref(), Some("des3"));
        assert_eq!(result.ciphertext, "BB23EF89F50FC595");
        assert_eq!(result.index, "22636");
        assert_eq!(result.plaintext, "586C");
        assert_eq!(result.des_key, "5937010101010101");
        assert_eq!(
            human_recovered_part(&result),
            "PT3: 586C · K3: 5937010101010101 (index 22636)"
        );
    }
}
