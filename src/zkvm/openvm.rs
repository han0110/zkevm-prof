//! OpenVM profiling backend.
//!
//! OpenVM prices an execution from trace geometry rather than from opcode counts. Metered cost
//! execution charges every instruction the rows it adds times the width of the AIR that proves it,
//! so the total is the trace cells the app VM has to commit to. It carries no memory model and no
//! segmentation, which makes the number independent of the machine and of the prover backend.

use anyhow::{Result, anyhow};
use openvm_sdk::{
    CompiledExeMeteredCost, Sdk, StdIn,
    config::{AggregationSystemParams, AppConfig},
};
use openvm_sdk_config::SdkVmConfig;
use openvm_stark_sdk::config::{MAX_APP_LOG_STACKED_HEIGHT, app_params_with_100_bits_security};
use openvm_transpiler::{elf::Elf, openvm_platform::memory::MEM_SIZE};

use crate::zkvm::{Composition, Cost, Profiler};

/// Trace cells the execution charges, which is the whole measurement.
///
/// Metered cost yields one number rather than a distribution, so the total has no parts to stack.
pub const COMPOSITION: Composition = Composition {
    total: "cost",
    components: &[],
};

/// Public values the benchmarked stateless validator guests expose, in bytes.
const NUM_PUBLIC_VALUE_BYTES: usize = 256;

/// Segmentation memory budget in bytes, which decides where segment boundaries fall.
///
/// Metered cost prices instructions without segmenting, so this does not move the reported cost. It
/// is set to what the proving cluster deploys so the configuration describes one machine.
const SEGMENT_MAX_MEMORY: usize = 29 << 29; // 14.5 GiB

pub struct OpenVMProfiler {
    /// Borrows the SDK's config, which is why the SDK outlives it.
    compiled: CompiledExeMeteredCost<'static>,
}

impl OpenVMProfiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        let elf = Elf::decode(elf, MEM_SIZE as u32)
            .map_err(|error| anyhow!("failed to decode the guest ELF: {error}"))?;
        // The SDK holds a transpiler behind `Rc`, so it cannot be shared across threads and cannot
        // be stored beside the artifact that borrows it. Leaking it gives that borrow the 'static
        // lifetime the profiler needs, and one guest is profiled per process.
        let sdk: &'static Sdk = Box::leak(Box::new(
            Sdk::new(app_config(), AggregationSystemParams::default())
                .map_err(|error| anyhow!("failed to build the OpenVM SDK: {error}"))?,
        ));
        // Generates a C translation of the program and builds it into a shared library, which is
        // the expensive half of profiling a guest and is paid once here.
        let compiled = sdk
            .compile_metered_cost(elf)
            .map_err(|error| anyhow!("failed to compile the guest for metered cost: {error}"))?;

        Ok(Self { compiled })
    }
}

impl Profiler for OpenVMProfiler {
    fn profile(&self, stateless_input: &[u8]) -> Result<Cost> {
        let mut stdin = StdIn::default();
        stdin.write_bytes(stateless_input);
        // `Sdk::execute_metered_cost` wraps this call to also read the guest's public values, which
        // a cost profile does not report. Going through the artifact keeps the SDK off the worker
        // threads, since only the artifact is shareable.
        let (ctx, _) = self
            .compiled
            .instance
            .execute_metered_cost(stdin, self.compiled.ctx.clone())
            .map_err(|error| anyhow!("metered cost execution failed: {error}"))?;

        Ok(Cost::from([(COMPOSITION.total.to_owned(), ctx.cost)]))
    }
}

/// The SDK configuration the benchmarked guests are built and proven against.
fn app_config() -> AppConfig<SdkVmConfig> {
    let mut vm_config = SdkVmConfig::standard();
    vm_config.system.config = vm_config
        .system
        .config
        .with_public_values_bytes(NUM_PUBLIC_VALUE_BYTES);
    vm_config.system.config.segmentation_max_memory = SEGMENT_MAX_MEMORY;
    AppConfig::new(
        vm_config.optimize(),
        app_params_with_100_bits_security(MAX_APP_LOG_STACKED_HEIGHT),
    )
}
