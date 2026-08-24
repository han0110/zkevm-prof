//! OpenVM profiling backend.
//!
//! OpenVM prices an execution from trace geometry rather than from opcode counts. Metered cost
//! execution charges every instruction the rows it adds times the width of the AIR that proves it,
//! so the total is the trace cells the app VM has to commit to. It carries no memory model and no
//! segmentation, which makes the number independent of the machine and of the prover backend.
//!
//! The execution reports that total as a single number, so the kinds come from running it once per
//! kind against an artifact priced with only that kind's AIR widths. Metered cost is a weighted sum
//! over the AIRs an execution charges and the weights are the caller's, so zeroing the rest leaves
//! exactly the cells one kind contributes.
//!
//! Peak heap comes out of the same runs. Metered cost stubs the memory model out of the pricing but
//! still runs the guest against real memory, so the state a run returns holds the heap the guest
//! wrote.

use std::ops::Range;

use anyhow::{Result, anyhow, ensure};
use openvm_sdk::{
    Sdk, StdIn,
    compiled::MeteredCostInstance,
    config::{AggregationSystemParams, AppConfig},
    openvm_circuit::arch::{execution_mode::MeteredCostCtx, instructions::riscv::RV64_MEMORY_AS},
};
use openvm_sdk_config::SdkVmConfig;
use openvm_stark_sdk::config::{MAX_APP_LOG_STACKED_HEIGHT, app_params_with_100_bits_security};
use openvm_transpiler::{elf::Elf, openvm_platform::memory::MEM_SIZE};

use crate::{Cost, Execution, Kind, Profiler, catching_panic, heap_range, peak_heap_bytes};

/// Cost kinds that partition an OpenVM execution, in stack order.
///
/// The split is between the work a guest buys by reaching for an accelerator and the work it does
/// in the base instruction set, which is the comparison worth drawing across guests running the
/// same block. Loads and stores stay under `rv64` because a guest cannot choose them away.
pub const COMPOSITION: &[Kind] = &[
    Kind {
        name: PRECOMPILE,
        note: "Precompiles",
    },
    Kind {
        name: RV64,
        note: "RISC-V instructions",
    },
];

const PRECOMPILE: &str = "precompile";
const RV64: &str = "rv64";

/// AIR name fragments the accelerator extensions own, covering Keccak, SHA-2, 256-bit integers and
/// the field expression AIRs the modular, complex and elliptic curve extensions share. Pairing
/// registers no AIR of its own and is charged through those.
const PRECOMPILE_AIRS: [&str; 5] = [
    "Keccakf",
    "Rv64IsEqualModU16",
    "Rv64VecHeap",
    "Sha2",
    "Xorin",
];

/// AIR names no instruction can add a row to, which is why they belong to no kind.
///
/// No executor is bound to any of them. The memory argument reaches its two AIRs through the page
/// hooks metered cost stubs out, the periphery hasher only through the memory argument, and the
/// lookup tables are filled from counters once the execution is over.
const UNCHARGED_AIRS: [&str; 8] = [
    "BitwiseOperationLookupAir",
    "MemoryMerkleAir",
    "PersistentBoundaryAir",
    "Poseidon2PeripheryAir",
    "ProgramAir",
    "RangeTupleCheckerAir",
    "VariableRangeCheckerAir",
    "VmConnectorAir",
];

/// Public values the VM reserves room for, in bytes. A guest revealing more than this does not run.
const NUM_PUBLIC_VALUE_BYTES: usize = 256;

/// Segmentation memory budget in bytes, which decides where segment boundaries fall.
///
/// Metered cost prices instructions without segmenting, so this does not move the reported cost. It
/// is set to what a proving machine holds so the configuration describes one machine.
const SEGMENT_MAX_MEMORY: usize = 29 << 29; // 14.5 GiB

/// Symbol the linker ends the guest image at, which `openvm_platform` starts the heap at.
///
/// The stack grows down from below the image rather than from the top of memory, so the heap runs
/// from here to the end of the address space.
const HEAP_BOTTOM: &str = "_end";

pub struct OpenVMProfiler {
    /// Priced with every AIR width, which is the cost OpenVM's own metered cost mode reports.
    whole: MeteredCostInstance<'static>,
    /// One artifact per kind, each priced with only that kind's AIR widths. Borrows the SDK's
    /// executor, which is why the SDK outlives them.
    priced: Vec<(&'static str, MeteredCostInstance<'static>)>,
    /// Shared across the artifacts, since each carries its own widths and the context only collects
    /// what a run accumulated.
    ctx: MeteredCostCtx,
    /// Guest addresses the heap covers.
    heap: Range<u64>,
}

impl OpenVMProfiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        // Resolved before the ELF is decoded, since decoding keeps the memory image and drops the
        // symbol table the heap is delimited in.
        let heap = heap_range(elf, HEAP_BOTTOM, MEM_SIZE as u64)?;
        let elf = Elf::decode(elf, MEM_SIZE as u32)
            .map_err(|error| anyhow!("failed to decode the guest ELF: {error}"))?;
        // The SDK holds a transpiler behind `Rc`, so it cannot be shared across threads and cannot
        // be stored beside the artifacts that borrow it. Leaking it gives those borrows the 'static
        // lifetime the profiler needs, and one guest is profiled per process.
        let sdk: &'static Sdk = Box::leak(Box::new(
            Sdk::new(app_config(), AggregationSystemParams::default())
                .map_err(|error| anyhow!("failed to build the OpenVM SDK: {error}"))?,
        ));
        let prover = sdk
            .app_prover(elf)
            .map_err(|error| anyhow!("failed to transpile the guest for OpenVM: {error}"))?;
        let vm = prover.vm();
        let exe = prover.exe();
        let ctx = vm.build_metered_cost_ctx();
        let executor_idx_to_air_idx = vm.executor_idx_to_air_idx();
        let kinds: Vec<Option<&'static str>> = vm.air_names().map(kind).collect();

        let masked: Vec<(&'static str, Vec<usize>)> = COMPOSITION
            .iter()
            .map(|component| {
                let mask = ctx
                    .widths
                    .iter()
                    .zip(&kinds)
                    .map(|(&width, &kind)| {
                        if kind == Some(component.name) {
                            width
                        } else {
                            0
                        }
                    })
                    .collect();
                (component.name, mask)
            })
            .collect();
        // Every charged AIR lands in one kind and every uncharged one in none, which is what the
        // per block sum against the whole cost then holds the uncharged ones to.
        ensure!(
            kinds.iter().enumerate().all(|(air, kind)| {
                let priced: usize = masked.iter().map(|(_, mask)| mask[air]).sum();
                priced == kind.map_or(0, |_| ctx.widths[air])
            }),
            "the kinds do not partition the charged AIR widths"
        );

        // Generates a C translation of the program and builds it into a shared library, which is
        // the expensive half of profiling a guest and is paid once per artifact here.
        let compile = |label: &str, widths: &[usize]| {
            sdk.executor()
                .metered_cost_instance_with_debug_map(&exe, &executor_idx_to_air_idx, widths, None)
                .map_err(|error| {
                    anyhow!("failed to compile the guest for {label} metered cost: {error}")
                })
        };
        let priced = masked
            .iter()
            .map(|(kind, mask)| Ok((*kind, compile(kind, mask)?)))
            .collect::<Result<Vec<_>>>()?;
        let whole = compile("total", &ctx.widths)?;

        Ok(Self {
            whole,
            priced,
            ctx,
            heap,
        })
    }

    /// Runs `instance`, returning its cost and, when `read_heap` is set, the peak heap the run left
    /// behind.
    ///
    /// Every artifact runs the same program and differs only in the widths it charges, so they all
    /// leave the same heap and reading it once is reading it for all of them.
    fn execute(
        &self,
        instance: &MeteredCostInstance<'static>,
        stdin: &StdIn,
        read_heap: bool,
    ) -> Result<(u64, Option<u64>)> {
        // `Sdk::execute_metered_cost` wraps this call to also read the guest's public values, which
        // a cost profile does not report. Going through the artifact keeps the SDK off the worker
        // threads, since only the artifact is shareable.
        let (ctx, state) = instance
            .execute_metered_cost(stdin.clone(), self.ctx.clone())
            .map_err(|error| anyhow!("metered cost execution failed: {error}"))?;
        if !read_heap {
            return Ok((ctx.cost, None));
        }
        // The compiled artifact runs against the state's own buffer, so the memory it hands back is
        // the one the guest wrote and a guest byte address is a byte offset into the RISC-V address
        // space.
        let heap = state
            .memory
            .checked_u8_slice(
                RV64_MEMORY_AS,
                self.heap.start,
                self.heap.end - self.heap.start,
            )
            .map_err(|error| anyhow!("failed to read the guest's heap: {error}"))?;
        Ok((ctx.cost, peak_heap_bytes(heap)))
    }

    /// Runs the guest once per kind and once more for the whole, which is what the kinds are then
    /// checked against.
    fn run(&self, input: &[u8]) -> Result<Execution> {
        let mut stdin = StdIn::default();
        stdin.write_bytes(input);

        let mut cost = Cost::new();
        for (kind, instance) in &self.priced {
            cost.insert((*kind).to_owned(), self.execute(instance, &stdin, false)?.0);
        }

        // The kinds leave out the AIRs no instruction can charge, so kinds that fall short of the
        // whole cost mean one of them took a row after all.
        let (total, peak_heap_bytes) = self.execute(&self.whole, &stdin, true)?;
        let summed: u64 = cost.values().sum();
        ensure!(
            summed == total,
            "the kinds sum to {summed}, not the total of {total}"
        );

        Ok(Execution {
            cost,
            peak_heap_bytes,
        })
    }
}

impl Profiler for OpenVMProfiler {
    fn profile(&self, input: &[u8]) -> Result<Execution> {
        catching_panic(|| self.run(input))
    }
}

/// The kind an AIR is priced under, from the name the proving key records it as, or `None` for the
/// AIRs metered cost never charges.
///
/// Whatever an accelerator does not claim is base instruction execution, which is where the loads,
/// the stores and the hint stream land.
fn kind(air_name: &str) -> Option<&'static str> {
    if UNCHARGED_AIRS
        .iter()
        .any(|fragment| air_name.contains(fragment))
    {
        None
    } else if PRECOMPILE_AIRS
        .iter()
        .any(|fragment| air_name.contains(fragment))
    {
        Some(PRECOMPILE)
    } else {
        Some(RV64)
    }
}

/// The SDK configuration a guest is priced against.
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

#[cfg(test)]
mod tests {
    use super::{COMPOSITION, PRECOMPILE, RV64, kind};

    /// The AIR names [`app_config`] keygens to, one per line, as the proving key records them.
    /// Regenerated whenever the pinned OpenVM changes the AIR set.
    const AIR_NAMES: &str = include_str!("testdata/openvm-air-names.txt");

    /// A fragment that stops matching moves real cost between kinds without failing anything else,
    /// so the split each fragment set makes is pinned here. The last count is the AIRs left to no
    /// kind, which a profiled block then holds to zero.
    #[test]
    fn the_kinds_split_the_air_set_as_expected() {
        let counts = AIR_NAMES.lines().fold([0; 3], |mut counts, air_name| {
            let index = match kind(air_name) {
                Some(kind) => COMPOSITION
                    .iter()
                    .position(|component| component.name == kind)
                    .expect("every kind is a component"),
                None => COMPOSITION.len(),
            };
            counts[index] += 1;
            counts
        });
        assert_eq!(counts, [51, 39, 8]);
    }

    #[test]
    fn kinds_follow_the_extension_an_air_belongs_to() {
        assert_eq!(kind("KeccakfPermAir"), Some(PRECOMPILE));
        assert_eq!(kind("Sha2BlockHasherVmAir<Sha256Config>"), Some(PRECOMPILE));
        assert_eq!(
            kind("VmAirWrapper<Rv64VecHeapAdapterAir<1, 12, 12>, FieldExpressionCoreAir>"),
            Some(PRECOMPILE)
        );
        // A 256-bit add reads its operands from the heap, where the base one reads registers.
        assert_eq!(
            kind(
                "VmAirWrapper<Rv64VecHeapU16AdapterAir<2, 4, 4>, 2, 4, 4, 4, 16, 16>, AddSubCoreAir<16, 16, true>"
            ),
            Some(PRECOMPILE)
        );
        assert_eq!(
            kind("VmAirWrapper<Rv64BaseAluRegU16AdapterAir, AddSubCoreAir<4, 16, true>"),
            Some(RV64)
        );
        assert_eq!(
            kind("VmAirWrapper<Rv64LoadMultiByteAdapterAir, LoadCoreAir<8, 5>"),
            Some(RV64)
        );
        assert_eq!(kind("Rv64HintStoreAir"), Some(RV64));
        assert_eq!(kind("PhantomAir"), Some(RV64));
        assert_eq!(kind("MemoryMerkleAir<8>"), None);
        assert_eq!(kind("Poseidon2PeripheryAir<BabyBearParameters>, 1>"), None);
        assert_eq!(kind("VariableRangeCheckerAir"), None);
    }
}
