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

use crate::{fixture::Metadata, registry::HeapSymbols};

/// Cost of one execution, keyed by the cost kinds the zkVM defines.
pub type Cost = BTreeMap<String, u64>;

/// What one execution of a guest cost, and the heap it used.
#[derive(Debug)]
pub struct Execution {
    pub cost: Cost,
    /// Peak bytes the guest's heap allocator handed out, absent when its entry declares no heap or
    /// the cursor that entry names never moved.
    pub peak_heap_bytes: Option<u64>,
}

/// Where a guest keeps its allocator's cursor, resolved against the ELF that declared it.
///
/// A backend does one thing with this. Once a run is over it reads the word at `cursor` out of the
/// guest memory its zkVM exposes and hands the value to [`HeapCursor::peak`]. What that word means
/// is the registry's claim about the guest rather than anything a backend knows, so a guest handing
/// heap out from a cursor that only moves up reports its high-water mark, and a guest that keeps no
/// such word declares no heap and reports nothing.
pub struct HeapCursor {
    /// Guest address of the word holding the next free heap address.
    pub cursor: u64,
    /// Guest address the heap starts at, which the allocator initializes the cursor to.
    pub base: u64,
}

impl HeapCursor {
    /// Resolves the symbols an entry declares against the guest they were declared for.
    ///
    /// A declared symbol the ELF does not carry is an error rather than a heap of nothing, since a
    /// guest quietly missing from the heap chart reads as one that allocates nothing at all.
    pub fn resolve(elf: &[u8], symbols: &HeapSymbols) -> Result<Self> {
        let address = |symbol: &str| {
            symbol_address(elf, symbol)?.with_context(|| {
                format!(
                    "the guest ELF carries no {symbol}, which the registry declares its heap in"
                )
            })
        };
        Ok(Self {
            cursor: address(&symbols.cursor)?,
            base: address(&symbols.base)?,
        })
    }

    /// Fails when the word at `cursor` falls outside `memory`, the guest addresses the backend holds
    /// memory for.
    ///
    /// Backends check this once at construction rather than per block, since a cursor out of range
    /// is a registry error that will not fix itself, and the memory behind these reads is a raw
    /// buffer in more than one of them.
    pub fn ensure_readable(&self, memory: Range<u64>) -> Result<()> {
        let word = self.cursor..self.cursor + size_of::<u64>() as u64;
        ensure!(
            memory.start <= word.start && word.end <= memory.end,
            "the declared cursor resolves to {:#x}, which is outside the guest memory the backend reads",
            self.cursor
        );
        Ok(())
    }

    /// Peak heap in bytes, from the cursor's final `value`, absent when the cursor never moved.
    ///
    /// A guest carrying the symbol but allocating through something else leaves the cursor where it
    /// started, which is no reading rather than an empty heap.
    pub fn peak(&self, value: u64) -> Option<u64> {
        (value > self.base).then(|| value - self.base)
    }
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

pub fn profiler(
    zkvm: zkVMKind,
    elf: &[u8],
    heap: Option<&HeapSymbols>,
) -> Result<Box<dyn Profiler>> {
    match zkvm {
        zkVMKind::OpenVM => Ok(Box::new(openvm::OpenVMProfiler::new(elf, heap)?)),
        zkVMKind::SP1 => Ok(Box::new(sp1::SP1Profiler::new(elf, heap)?)),
        zkVMKind::Zisk => Ok(Box::new(zisk::ZiskProfiler::new(elf, heap)?)),
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
    /// Peak bytes the guest's heap allocator handed out, absent when its entry declares no heap or
    /// the cursor that entry names never moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_heap_bytes: Option<u64>,
    pub metadata: Metadata,
}

#[cfg(test)]
mod tests {
    use crate::zkvm::HeapCursor;

    #[test]
    fn only_a_cursor_that_moved_is_a_peak() {
        let heap = HeapCursor {
            cursor: 0xa00301f0,
            base: 0xa0430318,
        };
        assert_eq!(heap.peak(0xa0430318 + 4096), Some(4096));
        // A guest allocating through another allocator leaves the cursor where ziskos started it,
        // which is no reading rather than an empty heap.
        assert_eq!(heap.peak(0xa0430318), None);
        assert_eq!(heap.peak(0), None);
        assert_eq!(heap.peak(0xa0430317), None);
    }

    /// The whole word has to be inside the memory, not just the address it starts at.
    #[test]
    fn a_cursor_is_readable_only_with_its_whole_word() {
        let cursor = |cursor| HeapCursor { cursor, base: 0 };
        let memory = 0xa0000000..0xc0000000;
        assert!(cursor(0xa0000000).ensure_readable(memory.clone()).is_ok());
        assert!(cursor(0xbffffff8).ensure_readable(memory.clone()).is_ok());
        assert!(cursor(0xbffffff9).ensure_readable(memory.clone()).is_err());
        assert!(cursor(0x9fffffff).ensure_readable(memory).is_err());
    }
}
