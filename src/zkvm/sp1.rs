//! SP1 profiling backend.
//!
//! SP1 prices an execution as gas, which weighs the trace cells the AIRs an execution touches have
//! to commit to against the constraints those AIRs evaluate. A minimal trace of the run is charged
//! one chunk at a time, the same path the SDK takes when it reports gas. Gas divides that weighted
//! sum by 191 with a rounding step per chunk, so the profile records the sum, which stays exact.
//!
//! The sum splits by the accumulators SP1 charges through rather than by the two weightings, since
//! every AIR carries both weightings and only the accumulators track what the program did. Gas
//! accounts a chunk boundary as a shard boundary, so it grows as chunks get smaller, and the
//! cadence gas was calibrated against is pinned rather than the smaller one that only bounds the
//! executor's own memory.
//!
//! Peak heap comes out of the same run, where the executor makes it harder to reach than the other
//! backends do. Guest memory is a lazily committed mapping of 2 TiB served one word at a time, so a
//! heap spanning 110 GiB cannot be read as a slice. It is backed by a memfd instead, and the pages
//! that file holds are the pages the guest reached, which the kernel reports by seeking between
//! data and holes. Reaching that file means driving the transpiler the executor wraps, which is why
//! the reading is confined to the targets that executor compiles a native JIT for.
//!
//! Where that heap ends is read off the run rather than assumed. The ceiling `sp1-zkvm` builds with
//! reaches neither the guest ELF nor the executor, so a guest built against another one would be
//! measured against a heap it does not have. The runtime reserves a region for the input directly
//! above the heap and reads the input into its base, which makes the topmost written region of a run
//! that input and the heap everything below it.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use enum_map::EnumMap;
use sp1_core_executor::{
    ExecutionReport, GAS_TRACE_CHUNK_THRESHOLD, GasEstimatingVMEnum, Program, RiscvAirId,
    SP1CoreOpts, TraceChunkRaw, get_complexity_mapping, rv64im_costs,
};

use crate::zkvm::{Composition, Cost, Execution, Kind, Profiler};

#[cfg(sp1_jit_memory)]
use {
    crate::zkvm::heap_symbol,
    anyhow::{Context, ensure},
    std::ops::Range,
};

/// Symbol the linker ends the guest image at, which `sp1_zkvm` starts the heap at.
///
/// The stack occupies the addresses below the image rather than the top of memory, so the heap runs
/// from here to the region the runtime reserves for its input.
#[cfg(sp1_jit_memory)]
const HEAP_BOTTOM: &str = "_end";

/// Host page, which is the granularity guest memory reports data at and so the granularity the heap
/// is separated from the input region above it at.
#[cfg(sp1_jit_memory)]
const HOST_PAGE: u64 = 4096;

/// Bytes of the input matched against the region they are expected to occupy, which is what holds a
/// guest to the layout this reading splits the heap on.
#[cfg(sp1_jit_memory)]
const ANCHOR: usize = 64;

/// Cost kinds that partition an SP1 total, in stack order, and the kind holding the whole.
pub const COMPOSITION: Composition = Composition {
    total: "total",
    components: &[
        Kind {
            name: SYSCALL,
            note: "Precompiles",
        },
        Kind {
            name: SYSTEM,
            note: "Memory and tables",
        },
        Kind {
            name: OPCODE,
            note: "RISC-V instructions",
        },
    ],
};

/// Instructions the program runs.
const OPCODE: &str = "opcode";

/// Precompiles the program calls, charged in place or deferred to a later shard.
const SYSCALL: &str = "syscall";

/// Chips the machine runs on the program's behalf rather than at its request.
const SYSTEM: &str = "system";

/// Weight gas gives a trace cell relative to a constraint.
const TRACE_AREA_WEIGHT: u64 = 3;

/// Nonce the execution commits to, which reaches the public values rather than the cost.
const PROOF_NONCE: [u32; 4] = [0; 4];

/// What the chunks of one execution charged, before the parts are turned into a cost map.
#[derive(Default)]
struct Charges {
    cost: u64,
    syscall: u64,
    system: u64,
    exit_code: u64,
}

pub struct SP1Profiler {
    program: Arc<Program>,
    opts: SP1CoreOpts,
    /// What one row of each AIR costs, which is the gas formula's two weightings already blended.
    weights: EnumMap<RiscvAirId, u64>,
    /// Guest address the heap starts at. Where it ends is read off each run instead.
    #[cfg(sp1_jit_memory)]
    heap_bottom: u64,
}

impl SP1Profiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        #[cfg(sp1_jit_memory)]
        let heap_bottom = heap_symbol(elf, HEAP_BOTTOM)?;
        let program = Program::from(elf)
            .map_err(|error| anyhow!("failed to disassemble the guest ELF: {error}"))?;
        Ok(Self {
            program: Arc::new(program),
            opts: SP1CoreOpts::default(),
            weights: weights(),
            #[cfg(sp1_jit_memory)]
            heap_bottom,
        })
    }

    /// Charges every row at the weight of the AIR that proves it.
    fn charge(&self, rows: impl Iterator<Item = (RiscvAirId, u64)>) -> u64 {
        rows.map(|(air, count)| self.weights[air] * count).sum()
    }

    /// Charges one chunk of the minimal trace into `charges`.
    fn charge_chunk(&self, chunk: &TraceChunkRaw, charges: &mut Charges) -> Result<()> {
        let mut vm = GasEstimatingVMEnum::new(
            chunk,
            Arc::clone(&self.program),
            PROOF_NONCE,
            self.opts.clone(),
        );
        let report: ExecutionReport = vm
            .execute()
            .map_err(|error| anyhow!("gas estimation failed: {error}"))?;
        let counts = match &vm {
            GasEstimatingVMEnum::Supervisor(vm) => &vm.gas_calculator,
            GasEstimatingVMEnum::User(vm) => &vm.gas_calculator,
        };

        let (complexity, trace_area) = vm.costs();
        charges.cost += TRACE_AREA_WEIGHT * trace_area + complexity;
        // Whether a precompile is charged in place or deferred to a later shard follows the
        // retained event presets, which moves rows between the two maps without repricing them.
        let untrusted = self.program.enable_untrusted_programs;
        charges.syscall += self.charge(
            counts
                .syscall_counts
                .iter()
                .chain(counts.deferred_syscall_counts.iter())
                .filter_map(|(code, count)| Some((code.as_air_id_flag(untrusted)?, *count))),
        );
        charges.system += self.charge(
            counts
                .system_chips_counts
                .iter()
                .map(|(air, count)| (air, *count)),
        );
        // Only the chunk the guest halts in carries an exit code, which is how SP1 folds them.
        charges.exit_code |= report.exit_code;
        Ok(())
    }

    /// Runs the guest over the whole minimal trace, charging every chunk and reading the heap the
    /// run left behind.
    ///
    /// Driving the transpiler is what the executor does, with the JIT it builds kept behind a
    /// private field. Holding it here instead reaches the memory the guest ran against, and leaves
    /// the charging untouched, since the chunks and the program are the same either way.
    #[cfg(sp1_jit_memory)]
    fn run(&self, stateless_input: &[u8]) -> Result<(Charges, Option<u64>)> {
        use std::os::fd::AsRawFd;

        use memmap2::MmapMut;
        use sp1_core_executor::{HALT_PC, MinimalTranspiler};
        use sp1_jit::{JitFunction, memory::AnonymousMemory, trace_capacity};
        use sp1_primitives::consts::MAX_JIT_LOG_ADDR;

        let transpiler = MinimalTranspiler::new(
            1usize << MAX_JIT_LOG_ADDR,
            false,
            Some(GAS_TRACE_CHUNK_THRESHOLD),
        );
        let mut jit: JitFunction<AnonymousMemory> = transpiler.transpile(&self.program);
        jit.with_initial_memory_image(self.program.memory_image.clone());
        // SP1 hands the guest one input entry per read and these guests read once, so the stateless
        // input goes in whole and carries no framing.
        jit.push_input(stateless_input.to_vec());

        let capacity = trace_capacity(Some(GAS_TRACE_CHUNK_THRESHOLD));
        let mut charges = Charges::default();
        while jit.pc != HALT_PC {
            let mut trace = MmapMut::map_anon(capacity)?;
            // Safety: the buffer holds what the transpiler was built to write, and the chunk
            // reading it back is the same reading the executor makes of its own.
            let chunk = unsafe {
                jit.call(trace.as_mut_ptr());
                TraceChunkRaw::new(trace.make_read_only()?)
            };
            self.charge_chunk(&chunk, &mut charges)?;
        }

        let peak_heap_bytes =
            peak_heap_bytes(jit.memory.as_raw_fd(), self.heap_bottom, stateless_input)?;
        Ok((charges, peak_heap_bytes))
    }

    /// Runs the guest over the whole minimal trace, charging every chunk.
    ///
    /// Guest memory stays behind the executor here, so no heap is read.
    #[cfg(not(sp1_jit_memory))]
    fn run(&self, stateless_input: &[u8]) -> Result<(Charges, Option<u64>)> {
        use sp1_core_executor::MinimalExecutorEnum;

        let mut executor = MinimalExecutorEnum::new(
            Arc::clone(&self.program),
            false,
            Some(GAS_TRACE_CHUNK_THRESHOLD),
        );
        // SP1 hands the guest one input entry per read and these guests read once, so the stateless
        // input goes in whole and carries no framing.
        executor.with_input(stateless_input);

        let mut charges = Charges::default();
        while let Some(chunk) = executor
            .try_execute_chunk()
            .map_err(|error| anyhow!("execution failed: {error}"))?
        {
            self.charge_chunk(&chunk, &mut charges)?;
        }
        Ok((charges, None))
    }
}

/// Peak heap in bytes, from the pages the file behind guest memory holds over the heap, absent when
/// it holds none.
///
/// A page the guest never reached was never written and so is a hole the file skips, which makes
/// the outermost data it holds the outermost the guest reached. The topmost written region is the
/// input the runtime reserves space for above the heap, so the heap is every region below it and
/// nothing an allocator writes to cap its pool is counted, that landing in the page the input region
/// opens with rather than in a region of its own.
#[cfg(sp1_jit_memory)]
fn peak_heap_bytes(memory: i32, heap_bottom: u64, stateless_input: &[u8]) -> Result<Option<u64>> {
    // The file holds two host words per guest word, the clock it was last touched at and the guest's
    // own value, so a guest address reaches it doubled.
    let seek = |offset: u64, whence: i32| -> Result<Option<u64>> {
        // Safety: the descriptor belongs to the mapping the caller still holds, and seeking a file
        // that has no data left past `offset` reports ENXIO rather than failing the process.
        let found = unsafe { libc::lseek(memory, offset as i64, whence) };
        match found {
            0.. => Ok(Some(found as u64)),
            _ if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) => Ok(None),
            _ => Err(std::io::Error::last_os_error()).context("failed to seek the guest memory"),
        }
    };

    let mut written: Vec<Range<u64>> = Vec::new();
    let mut position = 2 * heap_bottom;
    while let Some(data) = seek(position, libc::SEEK_DATA)? {
        let hole = seek(data, libc::SEEK_HOLE)?
            .expect("a file holding data at an offset holds a hole past it");
        written.push(data..hole);
        position = hole;
    }

    let input = written
        .pop()
        .context("the guest wrote nothing above its image, so it read no input")?;
    // The runtime opens its input region on a page and the input has to fit in what the region
    // holds, which leaves only the pages between its start and its end less that length to look on.
    let padded = (stateless_input.len() as u64).next_multiple_of(8);
    let last = input
        .end
        .checked_sub(2 * padded)
        .filter(|last| *last >= input.start)
        .context("the topmost region the guest wrote is shorter than the input it was handed")?;
    let anchor = &stateless_input[..ANCHOR.min(stateless_input.len())];
    let base = (input.start..=last)
        .step_by(HOST_PAGE as usize)
        .map(|at| head(memory, at).map(|head| (at, head)))
        .find_map(|read| match read {
            Ok((at, head)) => (head[..anchor.len()] == *anchor).then_some(Ok(at)),
            Err(error) => Some(Err(error)),
        })
        .transpose()?
        .context(
            "the guest holds no input where the runtime reserves room for it, so the heap this \
             reading takes to lie below cannot be told apart from it",
        )?;
    // Only what an allocator writes to cap its pool separates the two, and it is excluded with the
    // page it lands on. More than that between them would be heap this reading then drops.
    ensure!(
        base - input.start <= HOST_PAGE,
        "the guest wrote {} bytes below its input, more than the page an allocator caps its pool \
         with, so the heap does not end where this reading splits it",
        (base - input.start) / 2
    );

    let (Some(first), Some(last)) = (written.first(), written.last()) else {
        return Ok(None);
    };
    Ok(Some((last.end - 1) / 2 - first.start / 2 + 1))
}

/// The guest's own bytes at the file offset `at`, which has to be word aligned.
///
/// Every guest word reaches the file as the clock it was last touched at followed by the word's own
/// value, so the guest's bytes are the second half of each sixteen byte record.
#[cfg(sp1_jit_memory)]
fn head(memory: i32, at: u64) -> Result<[u8; ANCHOR]> {
    let mut records = [0u8; 2 * ANCHOR];
    // Safety: the descriptor belongs to the mapping the caller still holds, and the buffer holds
    // every byte the read is given room to write.
    let read = unsafe {
        libc::pread(
            memory,
            records.as_mut_ptr().cast(),
            records.len(),
            at as i64,
        )
    };
    ensure!(
        read == records.len() as isize,
        "failed to read the guest's input region"
    );
    let mut head = [0u8; ANCHOR];
    let (records, _) = records.as_chunks::<16>();
    for (bytes, record) in head.chunks_mut(8).zip(records) {
        bytes.copy_from_slice(&record[8..]);
    }
    Ok(head)
}

impl Profiler for SP1Profiler {
    fn profile(&self, stateless_input: &[u8]) -> Result<Execution> {
        let (charges, peak_heap_bytes) = self.run(stateless_input)?;
        if charges.exit_code != 0 {
            bail!("the guest exited with {}", charges.exit_code);
        }

        // Instructions are what the total has left once the precompiles and the machine's own chips
        // are taken out, which keeps the parts summing to SP1's own figure rather than to a second
        // reading of the same weights.
        let cost = charges.cost;
        let opcode = cost
            .checked_sub(charges.syscall + charges.system)
            .ok_or_else(|| {
                anyhow!(
                    "the priced chips exceed the {} of {cost}",
                    COMPOSITION.total
                )
            })?;

        Ok(Execution {
            cost: Cost::from([
                (OPCODE.to_owned(), opcode),
                (SYSCALL.to_owned(), charges.syscall),
                (SYSTEM.to_owned(), charges.system),
                (COMPOSITION.total.to_owned(), cost),
            ]),
            peak_heap_bytes,
        })
    }
}

/// What one row of each AIR costs, which is its trace cells weighed against its constraints.
///
/// Both tables come from the executor, which is what prices the total, so the parts cannot drift
/// from the whole. SP1 keeps them apart because gas blends them only at the end, and blending them
/// per AIR instead leaves the same total while letting it split by what the program did.
fn weights() -> EnumMap<RiscvAirId, u64> {
    let complexity = get_complexity_mapping();
    let mut cells: EnumMap<RiscvAirId, u64> = EnumMap::default();
    for (air, cost) in rv64im_costs() {
        cells[air] = cost as u64;
    }
    EnumMap::from_fn(|air| TRACE_AREA_WEIGHT * cells[air] + complexity[air])
}
