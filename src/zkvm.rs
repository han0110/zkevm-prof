//! Per-zkVM profiling backends.
//!
//! A zkVM contributes one module implementing [`Profiler`]. The cost it reports is an open map of
//! whatever kinds that zkVM charges for, so adding a backend needs no change to the fixture
//! walking, the output format or the report.

pub mod openvm;
pub mod sp1;
pub mod zisk;

use std::{collections::BTreeMap, ops::Range};

use anyhow::{Context, Result, anyhow, ensure};
use elf::{ElfBytes, endian::AnyEndian};
use ere_catalog::zkVMKind;
use serde::{Deserialize, Serialize};

use crate::fixture::Metadata;

/// Cost of one execution, keyed by the cost kinds the zkVM defines.
pub type Cost = BTreeMap<String, u64>;

/// What one execution of a guest cost, and the memory it used.
#[derive(Debug)]
pub struct Execution {
    pub cost: Cost,
    /// Peak bytes of heap the guest reached, absent where the backend read no heap or the guest
    /// left its heap untouched.
    pub peak_heap_bytes: Option<u64>,
    /// Peak bytes of stack the guest reached, absent where the backend read no stack or the guest
    /// left its stack untouched.
    pub peak_stack_bytes: Option<u64>,
}

/// Address the guest ELF gives `symbol`, which a guest whose zkVM delimits a region of its memory
/// with it carries.
///
/// A missing symbol is an error rather than a region of nothing, since a guest quietly absent from
/// the chart of a region reads as one that never reached into it.
pub fn region_symbol(elf: &[u8], symbol: &str) -> Result<u64> {
    symbol_address(elf, symbol)?.with_context(|| {
        format!("the guest ELF carries no {symbol}, which delimits a region of its memory")
    })
}

/// Guest addresses a region covers, from the `start` symbol its zkVM's toolchain marks the bottom
/// with and the `end` that zkVM stops it at.
///
/// Which symbol marks a bottom is a property of the toolchain rather than of the guest, so every
/// guest built for one zkVM is delimited alike however it uses the region.
pub fn region_range(elf: &[u8], start: &str, end: u64) -> Result<Range<u64>> {
    let start = region_symbol(elf, start)?;
    ensure!(
        start < end,
        "the guest starts a region at {start:#x}, past the {end:#x} its zkVM ends it at"
    );
    Ok(start..end)
}

/// Peak bytes of a region, from the `bytes` a run left behind in it, absent when the guest left the
/// whole region zero.
///
/// Guest memory starts zeroed and neither an allocator nor a returning frame zeroes what it gives
/// up, so the bytes left non-zero are the ones the guest reached and nothing has to cooperate for
/// them to be read. Taking the span between the outermost of them rather than the distance from the
/// region's bottom reads the same whether it is filled from the bottom up or from the top down.
pub fn written_span(bytes: &[u8]) -> Option<u64> {
    let highest = bytes.iter().rposition(|byte| *byte != 0)?;
    let lowest = bytes
        .iter()
        .position(|byte| *byte != 0)
        .expect("a region holding a highest non-zero byte holds a lowest one");
    Some((highest - lowest + 1) as u64)
}

/// Address of the symbol in `elf` named `name`, absent when the ELF carries none.
///
/// A stripped guest carries no symbol table at all, which likewise reads as absent. Undefined
/// symbols are skipped, since they carry an address of zero that would pass for a resolved one.
pub fn symbol_address(elf: &[u8], name: &str) -> Result<Option<u64>> {
    let elf = ElfBytes::<AnyEndian>::minimal_parse(elf)
        .map_err(|error| anyhow!("failed to parse the guest ELF: {error}"))?;
    let Some((symbols, names)) = elf
        .symbol_table()
        .map_err(|error| anyhow!("failed to read the guest ELF symbol table: {error}"))?
    else {
        return Ok(None);
    };
    Ok(symbols
        .iter()
        .find(|symbol| {
            !symbol.is_undefined()
                && names
                    .get(symbol.st_name as usize)
                    .is_ok_and(|it| it == name)
        })
        .map(|symbol| symbol.st_value))
}

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
    fn profile(&self, stateless_input: &[u8]) -> Result<Execution>;
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
    /// Peak bytes of heap the guest reached, absent where the backend read no heap or the guest
    /// left its heap untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_heap_bytes: Option<u64>,
    /// Peak bytes of stack the guest reached, absent where the backend read no stack or the guest
    /// left its stack untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_stack_bytes: Option<u64>,
    pub metadata: Metadata,
}

#[cfg(test)]
mod tests {
    use crate::zkvm::written_span;

    /// A region filled from the bottom up and the same region filled from the top down read alike,
    /// which is what lets one reading serve a heap and a stack alike.
    #[test]
    fn the_span_between_the_outermost_bytes_is_the_peak() {
        let mut upward = [0u8; 64];
        upward[..10].copy_from_slice(&[1; 10]);
        assert_eq!(written_span(&upward), Some(10));

        let mut downward = [0u8; 64];
        downward[54..].copy_from_slice(&[1; 10]);
        assert_eq!(written_span(&downward), Some(10));
    }

    /// Zeros a guest wrote read as untouched, so the span is what the outermost non-zero bytes
    /// enclose rather than the count of them.
    #[test]
    fn the_span_covers_the_zeros_between_the_outermost_bytes() {
        let mut region = [0u8; 64];
        (region[0], region[41]) = (1, 1);
        assert_eq!(written_span(&region), Some(42));
    }

    #[test]
    fn an_untouched_region_is_no_reading_rather_than_an_empty_one() {
        assert_eq!(written_span(&[0; 64]), None);
        assert_eq!(written_span(&[]), None);
    }
}
