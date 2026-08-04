//! Per-zkVM profiling backends.
//!
//! A zkVM contributes one module implementing [`Profiler`]. The cost it reports is an open map of
//! whatever kinds that zkVM charges for, so adding a backend needs no change to the fixture
//! walking, the output format or the report.

pub mod openvm;
pub mod zisk;

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use ere_catalog::zkVMKind;
use serde::{Deserialize, Serialize};

use crate::fixture::Metadata;

/// Cost of one execution, keyed by the cost kinds the zkVM defines.
pub type Cost = BTreeMap<String, u64>;

/// How a zkVM's cost map decomposes into parts of a whole.
///
/// Each backend declares its own next to the profiler that fills the map, so the report stacks
/// whatever that zkVM charges for without knowing anything about it.
pub struct Composition {
    /// Kind holding the whole.
    pub total: &'static str,
    /// Kinds partitioning the total, in stack order.
    pub components: &'static [&'static str],
}

/// The composition of the zkVM that produced a profile.
pub fn composition(zkvm: zkVMKind) -> Result<&'static Composition> {
    match zkvm {
        zkVMKind::Zisk => Ok(&zisk::COMPOSITION),
        zkVMKind::OpenVM => bail!(
            "an {zkvm} profile carries per-AIR rows and no total, so there is nothing to compare \
             until its cost weights are calibrated"
        ),
        zkVMKind::SP1 => bail!("{zkvm} has no profiling backend"),
    }
}

/// Profiles a single guest over many inputs.
///
/// A backend is constructed once per guest ELF, so whatever it derives from the ELF (a transpiled
/// ROM, an AOT-compiled instance) is paid for once and shared across the whole corpus.
pub trait Profiler: Sync {
    fn profile(&self, stateless_input: &[u8]) -> Result<Cost>;
}

pub fn profiler(zkvm: zkVMKind, elf: &[u8]) -> Result<Box<dyn Profiler>> {
    match zkvm {
        zkVMKind::OpenVM => Ok(Box::new(openvm::OpenVMProfiler::new(elf)?)),
        zkVMKind::Zisk => Ok(Box::new(zisk::ZiskProfiler::new(elf)?)),
        zkVMKind::SP1 => bail!("{zkvm} has no profiling backend"),
    }
}

/// Profile document, as written to the output JSON.
#[derive(Deserialize, Serialize)]
pub struct Profile {
    pub profile: BTreeMap<String, Entry>,
    pub meta: Meta,
}

/// What produced a profile, which is how the report knows a cost map's shape.
#[derive(Deserialize, Serialize)]
pub struct Meta {
    pub zkvm: zkVMKind,
}

/// One profiled block.
#[derive(Deserialize, Serialize)]
pub struct Entry {
    pub cost: Cost,
    pub metadata: Metadata,
}
