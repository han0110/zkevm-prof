//! Prices one execution of a guest on a zkVM.
//!
//! A zkVM contributes one module implementing [`Profiler`], which takes a guest ELF once and runs it
//! over any number of inputs. Both are opaque bytes, so nothing here knows what a guest computes or
//! what it is handed. The cost a run reports is an open map of whatever kinds that zkVM charges for,
//! and [`composition`] names those kinds in the order a chart stacks them.

pub mod openvm;
pub mod sp1;
pub mod zisk;

use std::{
    collections::BTreeMap,
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use elf::{ElfBytes, endian::AnyEndian};
use serde::{Deserialize, Serialize};

pub use ere_catalog::zkVMKind;

/// Cost of one execution, keyed by the cost kinds the zkVM defines.
///
/// The kinds partition the execution, so they sum to what the whole of it cost and no key holds that
/// whole. A backend that prices an execution one way and splits it another checks the two against
/// each other before returning, which is what makes the sum the zkVM's own figure.
pub type Cost = BTreeMap<String, u64>;

/// What one execution of a guest cost, and the heap it used.
#[derive(Debug)]
pub struct Execution {
    pub cost: Cost,
    /// Peak bytes of heap the guest reached, absent where the backend read no heap or the guest
    /// left its heap untouched.
    pub peak_heap_bytes: Option<u64>,
}

/// Address the guest ELF gives `symbol`, which a guest whose zkVM delimits its heap with it carries.
///
/// A missing symbol is an error rather than a heap of nothing, since a guest quietly absent from the
/// heap chart reads as one that allocates nothing at all.
pub fn heap_symbol(elf: &[u8], symbol: &str) -> Result<u64> {
    symbol_address(elf, symbol)?
        .with_context(|| format!("the guest ELF carries no {symbol}, which delimits its heap"))
}

/// Guest addresses the heap covers, from the `start` symbol its zkVM's toolchain marks the bottom
/// with and the `end` that zkVM stops it at.
///
/// Which symbol marks the bottom is a property of the toolchain that linked the guest rather than of
/// how the guest allocates, so one pair of delimiters serves every allocator.
pub fn heap_range(elf: &[u8], start: &str, end: u64) -> Result<Range<u64>> {
    let start = heap_symbol(elf, start)?;
    ensure!(
        start < end,
        "the guest starts its heap at {start:#x}, past the {end:#x} its zkVM ends it at"
    );
    Ok(start..end)
}

/// Peak heap in bytes, from the heap `bytes` a run left behind, absent when the guest left the whole
/// heap zero.
///
/// Guest memory starts zeroed and an allocator hands memory back without zeroing it, so the bytes
/// left non-zero are the ones the guest reached and no allocator has to cooperate for them to be
/// read. Taking the span between the outermost of them rather than the distance from the heap's
/// bottom reads the same whether memory is handed out from the bottom up or from the top down.
pub fn peak_heap_bytes(bytes: &[u8]) -> Option<u64> {
    let highest = bytes.iter().rposition(|byte| *byte != 0)?;
    let lowest = bytes
        .iter()
        .position(|byte| *byte != 0)
        .expect("a heap holding a highest non-zero byte holds a lowest one");
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

/// One kind a cost splits into, with the note a report prints under the chart.
pub struct Kind {
    /// Key the cost map records the kind under.
    pub name: &'static str,
    /// What the kind covers, in one phrase a reader takes in at a glance.
    pub note: &'static str,
}

/// One kind in the form a document carries it, so a reader of that document needs nothing else to
/// stack a cost map it has never seen.
#[derive(Deserialize, Serialize)]
pub struct Component {
    pub name: String,
    pub note: String,
}

impl From<&Kind> for Component {
    fn from(kind: &Kind) -> Self {
        Self {
            name: kind.name.to_owned(),
            note: kind.note.to_owned(),
        }
    }
}

/// The kinds the zkVM that produced a profile splits a cost into, in stack order.
///
/// Each backend declares its own next to the profiler that fills the map, so a caller stacks
/// whatever that zkVM charges for without knowing anything about it.
pub fn composition(zkvm: zkVMKind) -> Result<&'static [Kind]> {
    match zkvm {
        zkVMKind::OpenVM => Ok(openvm::COMPOSITION),
        zkVMKind::SP1 => Ok(sp1::COMPOSITION),
        zkVMKind::Zisk => Ok(zisk::COMPOSITION),
    }
}

/// Profiles a single guest over many inputs.
///
/// A backend is constructed once per guest ELF, so whatever it derives from the ELF (a transpiled
/// ROM, a compiled shared library) is paid for once and shared across the whole corpus.
pub trait Profiler: Sync {
    /// Prices one execution of the guest over `input`.
    ///
    /// A guest that breaks a zkVM invariant panics rather than returning, and a backend catches that
    /// and reports it as the error it is, so one bad input costs a caller that input and no more.
    fn profile(&self, input: &[u8]) -> Result<Execution>;
}

/// Runs `profile`, turning a panic inside a backend into an error.
///
/// A guest that breaks a zkVM invariant aborts the emulator by panicking rather than by returning,
/// which would otherwise discard a whole corpus over one bad input. A backend carries no state from
/// one input to the next, so a caught panic leaves nothing inconsistent behind.
fn catching_panic(profile: impl FnOnce() -> Result<Execution>) -> Result<Execution> {
    match catch_unwind(AssertUnwindSafe(profile)) {
        Ok(result) => result,
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("the backend panicked");
            bail!("{}", message.trim())
        }
    }
}

pub fn profiler(zkvm: zkVMKind, elf: &[u8]) -> Result<Box<dyn Profiler>> {
    match zkvm {
        zkVMKind::OpenVM => Ok(Box::new(openvm::OpenVMProfiler::new(elf)?)),
        zkVMKind::SP1 => Ok(Box::new(sp1::SP1Profiler::new(elf)?)),
        zkVMKind::Zisk => Ok(Box::new(zisk::ZiskProfiler::new(elf)?)),
    }
}

#[cfg(test)]
mod tests {
    use crate::{catching_panic, peak_heap_bytes};

    /// One input that aborts the emulator must not discard the corpus around it.
    #[test]
    fn a_panicking_backend_becomes_an_error() {
        let error = catching_panic(|| panic!("the guest hit an unaligned operand")).unwrap_err();
        assert!(error.to_string().contains("unaligned operand"));
    }

    /// A heap handed out from the bottom up and the same heap handed out from the top down read
    /// alike, which is what lets one reading serve allocators that grow in either direction.
    #[test]
    fn the_span_between_the_outermost_bytes_is_the_peak() {
        let mut upward = [0u8; 64];
        upward[..10].copy_from_slice(&[1; 10]);
        assert_eq!(peak_heap_bytes(&upward), Some(10));

        let mut downward = [0u8; 64];
        downward[54..].copy_from_slice(&[1; 10]);
        assert_eq!(peak_heap_bytes(&downward), Some(10));
    }

    /// Zeros a guest wrote read as untouched, so the span is what the outermost non-zero bytes
    /// enclose rather than the count of them.
    #[test]
    fn the_span_covers_the_zeros_between_the_outermost_bytes() {
        let mut heap = [0u8; 64];
        (heap[0], heap[41]) = (1, 1);
        assert_eq!(peak_heap_bytes(&heap), Some(42));
    }

    #[test]
    fn an_untouched_heap_is_no_reading_rather_than_an_empty_one() {
        assert_eq!(peak_heap_bytes(&[0; 64]), None);
        assert_eq!(peak_heap_bytes(&[]), None);
    }
}
