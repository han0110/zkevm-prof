//! OpenVM profiling backend.
//!
//! OpenVM prices an execution from trace geometry rather than from opcode counts. Metered execution
//! reports, per segment, the height every AIR reaches, and those heights are the whole measurement.
//! Padded rows follow from rounding each height to a power of two, cells from multiplying by the
//! AIR's main width, and a weighted cost from applying per-AIR weights. Width, interaction count
//! and constraint count are fixed attributes of an AIR rather than of a run, so the AIR name and
//! its height are enough to derive the rest offline.
//!
//! # What is missing
//!
//! Turning heights into a single cost needs per-AIR weights calibrated against measured proving
//! time, and that calibration is not ready. Until it is, the backend reports the heights it
//! measured and no total, so nothing here can be mistaken for a cost comparable to ZisK's.

use std::mem;

use anyhow::{Result, anyhow};
use openvm_circuit::arch::execution_mode::MeteredCtx;
use openvm_sdk::{
    F, Sdk, StdIn,
    config::{AggregationSystemParams, AppConfig},
    prover::AppProver,
};
use openvm_sdk_config::{SdkVmConfig, SdkVmCpuBuilder};
use openvm_stark_sdk::config::{
    MAX_APP_LOG_STACKED_HEIGHT, app_params_with_100_bits_security,
    baby_bear_poseidon2::BabyBearPoseidon2CpuEngine,
};
use openvm_transpiler::{elf::Elf, openvm_platform::memory::MEM_SIZE};

use crate::zkvm::{Cost, Profiler};

/// Public values the benchmarked stateless validator guests expose.
const NUM_PUBLIC_VALUES: usize = 256;
/// Segmentation memory budget in bytes, which decides where segment boundaries fall
const SEGMENT_MEMORY: usize = 29 << 29; // 14.5 GiB

type Prover = AppProver<BabyBearPoseidon2CpuEngine, SdkVmCpuBuilder>;

/// Metered execution instance.
///
/// With the `aot` feature `metered_interpreter` hands back the AOT instance, which compiles the
/// program to x86 assembly once and runs it natively. Off x86_64 the feature is not enabled and the
/// same call hands back the interpreted instance, which reports the same trace heights more slowly.
///
/// Either instance is `Send + Sync` and `execute_metered` takes `&self`, building all of its
/// mutable state per call, so one instance serves every thread. The assembly the AOT instance
/// compiles bakes in the address of its own `pre_compute_insns` heap buffer, which is why the
/// instance is built once and shared rather than rebuilt per block.
#[cfg(target_arch = "x86_64")]
type MeteredInstance<'a> = openvm_circuit::arch::AotInstance<'a, F, MeteredCtx>;
#[cfg(not(target_arch = "x86_64"))]
type MeteredInstance<'a> = openvm_circuit::arch::InterpretedInstance<'a, F, MeteredCtx>;

/// Keeps the prover that [`OpenVMProfiler::instance`] borrows into alive without exposing it.
///
/// An `AppProver` owns proving-side chips that are not `Sync`, but this value is never read after
/// construction and no reference to it is ever handed out, so sharing a profiler across threads
/// never shares the prover.
struct KeepAlive(#[allow(dead_code)] Box<Prover>);

// SAFETY: the wrapped prover is unreachable after construction, so `&KeepAlive` carries no access
// to it and sharing one across threads shares nothing. What the instance borrows into it is shared,
// but openvm asserts the metered instance is itself `Send + Sync`.
unsafe impl Sync for KeepAlive {}

pub struct OpenVMProfiler {
    /// Borrows into the kept-alive prover, which makes this self-referential. Declared first so the
    /// borrow is dropped before its referent.
    instance: MeteredInstance<'static>,
    #[allow(dead_code)]
    prover: KeepAlive,
    /// Built once from the proving key and cloned per block, which is all `execute_metered` needs.
    ctx: MeteredCtx,
    /// AIR names indexed by air id, which is how metered execution reports heights.
    air_names: Vec<String>,
}

impl OpenVMProfiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        let elf = Elf::decode(elf, MEM_SIZE as u32)
            .map_err(|error| anyhow!("failed to decode the guest ELF: {error}"))?;
        let sdk = Sdk::new(app_config(), AggregationSystemParams::default())
            .map_err(|error| anyhow!("failed to build the OpenVM SDK: {error}"))?;
        let air_names = sdk
            .app_pk()
            .app_vm_pk
            .vm_pk
            .per_air
            .iter()
            .map(|per_air| per_air.air_name.clone())
            .collect();

        let prover = Box::new(
            sdk.app_prover(elf)
                .map_err(|error| anyhow!("failed to build the app prover: {error}"))?,
        );
        let exe = prover.exe();
        let ctx = prover
            .vm()
            .build_metered_ctx(&exe)
            .with_max_memory(SEGMENT_MEMORY);
        let instance = prover
            .vm()
            .metered_interpreter(&exe)
            .map_err(|error| anyhow!("failed to build the metered instance: {error}"))?;

        // SAFETY: `instance` borrows only into `*prover`. The prover is boxed, so its heap storage
        // keeps a fixed address when the returned profiler is moved, and the `prover` field is
        // dropped after `instance` by field order. The borrow therefore never dangles and never
        // outlives its referent, so extending it to 'static for co-storage is sound.
        let instance: MeteredInstance<'static> = unsafe { mem::transmute(instance) };

        Ok(Self {
            instance,
            prover: KeepAlive(prover),
            ctx,
            air_names,
        })
    }
}

impl Profiler for OpenVMProfiler {
    fn profile(&self, stateless_input: &[u8]) -> Result<Cost> {
        let mut stdin = StdIn::default();
        stdin.write_bytes(stateless_input);
        let (segments, _) = self
            .instance
            .execute_metered(stdin, self.ctx.clone())
            .map_err(|error| anyhow!("metered execution failed: {error}"))?;

        // An AIR's height is bumped once per row it fills, so summing over segments gives the rows
        // the whole execution filled. Padding, cells and any weighted cost follow from these and
        // the AIR's fixed width, so nothing else needs recording.
        let mut rows = vec![0u64; self.air_names.len()];
        for segment in &segments {
            for (air_id, &height) in segment.trace_heights.iter().enumerate() {
                rows[air_id] += u64::from(height);
            }
        }

        Ok(self
            .air_names
            .iter()
            .zip(rows)
            .filter(|(_, rows)| *rows > 0)
            .map(|(name, rows)| (name.clone(), rows))
            .collect())
    }
}

/// The SDK configuration the benchmarked guests are built and proven against.
fn app_config() -> AppConfig<SdkVmConfig> {
    let mut vm_config = SdkVmConfig::standard();
    vm_config.system.config = vm_config
        .system
        .config
        .with_public_values(NUM_PUBLIC_VALUES);
    AppConfig::new(
        vm_config.optimize(),
        app_params_with_100_bits_security(MAX_APP_LOG_STACKED_HEIGHT),
    )
}
