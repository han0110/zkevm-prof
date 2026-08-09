//! Per-zkVM profiling backends.
//!
//! A zkVM contributes one module implementing [`Profiler`]. The cost it reports is an open map of
//! whatever kinds that zkVM charges for, so adding a backend needs no change to the fixture
//! walking, the output format or the report.

pub mod openvm;
pub mod sp1;
pub mod zisk;

use std::collections::BTreeMap;

use anyhow::Result;
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
    /// Kinds partitioning the total, in stack order. Empty when the zkVM prices an execution as one
    /// number, which leaves the total nothing to be split into.
    pub components: &'static [Kind],
}

/// One kind a total splits into, with the note the report prints under the chart.
pub struct Kind {
    /// Key the cost map records the kind under.
    pub name: &'static str,
    /// What the kind covers, in one phrase a reader takes in at a glance.
    pub note: &'static str,
}

/// The composition of the zkVM that produced a profile.
pub fn composition(zkvm: zkVMKind) -> Result<&'static Composition> {
    match zkvm {
        zkVMKind::OpenVM => Ok(&openvm::COMPOSITION),
        zkVMKind::SP1 => Ok(&sp1::COMPOSITION),
        zkVMKind::Zisk => Ok(&zisk::COMPOSITION),
    }
}

/// Profiles a single guest over many inputs.
///
/// A backend is constructed once per guest ELF, so whatever it derives from the ELF (a transpiled
/// ROM, a compiled shared library) is paid for once and shared across the whole corpus.
pub trait Profiler: Sync {
    fn profile(&self, stateless_input: &[u8]) -> Result<Cost>;
}

pub fn profiler(zkvm: zkVMKind, elf: &[u8]) -> Result<Box<dyn Profiler>> {
    match zkvm {
        zkVMKind::OpenVM => Ok(Box::new(openvm::OpenVMProfiler::new(elf)?)),
        zkVMKind::SP1 => Ok(Box::new(sp1::SP1Profiler::new(elf)?)),
        zkVMKind::Zisk => Ok(Box::new(zisk::ZiskProfiler::new(elf)?)),
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
    /// zkVM SDK the guest was built against, as the ere catalog names it.
    pub zkvm_version: String,
    /// Stateless validator the profile is of.
    pub stateless_validator: String,
    /// Version of that guest, as the registry resolves it.
    pub stateless_validator_version: String,
    /// Where the profiled ELF is published, absent when it was handed over directly or built as an
    /// artifact the GitHub API serves under no stable URL.
    #[serde(default)]
    pub elf_url: Option<String>,
}

/// One profiled block.
#[derive(Deserialize, Serialize)]
pub struct Entry {
    pub cost: Cost,
    pub metadata: Metadata,
}
