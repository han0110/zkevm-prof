//! SP1 profiling backend.
//!
//! SP1 prices an execution as gas, which weighs the trace cells the AIRs an execution touches have
//! to commit to against the constraints those AIRs evaluate. A minimal trace of the run is charged
//! one chunk at a time, the same path the SDK takes when it reports gas. Gas divides that weighted
//! sum by 191 with a rounding step per chunk, so the profile records the sum, which stays exact.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use sp1_core_executor::{
    GAS_TRACE_CHUNK_THRESHOLD, GasEstimatingVMEnum, MinimalExecutorEnum, Program, SP1CoreOpts,
};

use crate::zkvm::{Composition, Cost, Profiler};

/// Cost kinds that partition an SP1 total, in stack order, and the kind holding the whole.
///
/// Each component carries the weight gas gives it rather than its raw sum, so the two add up to the
/// total exactly.
pub const COMPOSITION: Composition = Composition {
    total: "cost",
    components: &[COMPLEXITY, TRACE_AREA],
};

/// Constraints the AIRs evaluate.
const COMPLEXITY: &str = "complexity";

/// Trace cells the AIRs commit to.
const TRACE_AREA: &str = "trace_area";

/// Weight gas gives a trace cell relative to a constraint.
const TRACE_AREA_WEIGHT: u64 = 3;

/// Nonce the execution commits to, which reaches the public values rather than the cost.
const PROOF_NONCE: [u32; 4] = [0; 4];

pub struct SP1Profiler {
    program: Arc<Program>,
    opts: SP1CoreOpts,
}

impl SP1Profiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        let program = Program::from(elf)
            .map_err(|error| anyhow!("failed to disassemble the guest ELF: {error}"))?;
        Ok(Self {
            program: Arc::new(program),
            opts: SP1CoreOpts::default(),
        })
    }
}

impl Profiler for SP1Profiler {
    fn profile(&self, stateless_input: &[u8]) -> Result<Cost> {
        // Gas accounts a chunk boundary as a shard boundary, so it grows as chunks get smaller. The
        // cadence gas was calibrated against is pinned here rather than the smaller one that only
        // bounds the executor's own memory.
        let mut executor = MinimalExecutorEnum::new(
            Arc::clone(&self.program),
            false,
            Some(GAS_TRACE_CHUNK_THRESHOLD),
        );
        // SP1 hands the guest one input entry per read and these guests read once, so the stateless
        // input goes in whole and carries no framing.
        executor.with_input(stateless_input);

        let (mut complexity, mut trace_area, mut exit_code) = (0, 0, 0);
        while let Some(chunk) = executor
            .try_execute_chunk()
            .map_err(|error| anyhow!("execution failed: {error}"))?
        {
            let mut vm = GasEstimatingVMEnum::new(
                &chunk,
                Arc::clone(&self.program),
                PROOF_NONCE,
                self.opts.clone(),
            );
            let report = vm
                .execute()
                .map_err(|error| anyhow!("gas estimation failed: {error}"))?;
            let (chunk_complexity, chunk_trace_area) = vm.costs();
            complexity += chunk_complexity;
            trace_area += chunk_trace_area;
            // Only the chunk the guest halts in carries an exit code, which is how SP1 folds them.
            exit_code |= report.exit_code;
        }
        if exit_code != 0 {
            bail!("the guest exited with {exit_code}");
        }

        let trace_area = TRACE_AREA_WEIGHT * trace_area;
        Ok(Cost::from([
            (COMPLEXITY.to_owned(), complexity),
            (TRACE_AREA.to_owned(), trace_area),
            (COMPOSITION.total.to_owned(), complexity + trace_area),
        ]))
    }
}
