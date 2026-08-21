//! ZisK profiling backend.
//!
//! ZisK prices an execution as a weighted sum over the work the prover has to do, which the
//! emulator accumulates while it runs. Enabling `EmuOptions::stats` is what the `ziskemu -X` flag
//! sets, and the resulting distribution is read back from the emulator in process rather than by
//! shelling out.
//!
//! Peak heap comes out of the same run. The emulator keeps the whole guest RAM until it is dropped,
//! so the heap the ZisK linker script delimits is still there to read once the run is over.

use std::ops::Range;

use anyhow::{Result, anyhow, bail, ensure};
use zisk_common::EmuTrace;
use zisk_core::{RAM_ADDR, RAM_SIZE, ZiskRom};
use zisk_transpiler_riscv::Riscv2zisk;
use ziskemu::{Emu, EmuOptions};

use crate::zkvm::{
    Composition, Cost, Execution, Kind, Profiler, heap_range, peak_heap_bytes, symbol_address,
};

/// Cost kinds that partition a ZisK total, in stack order, and the kind holding the whole.
///
/// These name the only rows read out of the emulator's report. `VARIABLE` is skipped because it is
/// `TOTAL` less `BASE`, `FROPS` because the emulator prices those instructions into an accumulator
/// of their own that `TOTAL` never sums, and `STEPS` because it counts work rather than pricing it.
pub const COMPOSITION: Composition = Composition {
    total: "total",
    components: &[
        Kind {
            name: "base",
            note: "ROM and tables",
        },
        Kind {
            name: "precompile",
            note: "Precompiles",
        },
        Kind {
            name: "memory",
            note: "Memory",
        },
        Kind {
            name: "opcode",
            note: "ZisK instructions",
        },
        Kind {
            name: "main",
            note: "Main",
        },
    ],
};

/// The kinds [`COMPOSITION`] names, which are the ones worth reading out of the report.
fn kinds() -> impl Iterator<Item = &'static str> {
    COMPOSITION
        .components
        .iter()
        .map(|kind| kind.name)
        .chain([COMPOSITION.total])
}

/// Row the emulator's report prints `kind` under.
///
/// The report names two of them in the plural where the cost map keys them in the singular, which is
/// how the other backends key the same kinds.
fn row(kind: &'static str) -> &'static str {
    match kind {
        "opcode" => "opcodes",
        "precompile" => "precompiles",
        kind => kind,
    }
}

/// Symbol pairs a ZisK guest delimits its heap with, bottom then top, in the order they are looked
/// for.
///
/// The ZisK linker script names them `_heap_bottom` and `_heap_top`, while the Nethermind guest
/// links a script of its own carrying the names ZisK used before v1.1.0-alpha, where the stack sat
/// between the guest image and the heap rather than in a section of its own.
const HEAP_SYMBOLS: [(&str, &str); 2] = [
    ("_heap_bottom", "_heap_top"),
    ("_kernel_heap_bottom", "_kernel_heap_top"),
];

/// The symbol `elf` marks the bottom of its heap with, and the address it stops the heap at.
///
/// The pair a guest carries follows from the linker script it was built with, so the first pair it
/// carries any of is the one it is delimited by.
fn heap_delimiters(elf: &[u8]) -> Result<(&'static str, u64)> {
    for (bottom, top) in HEAP_SYMBOLS {
        if let Some(address) = symbol_address(elf, top)? {
            return Ok((bottom, address));
        }
    }
    bail!(
        "the guest ELF carries none of {}, which delimit its heap",
        HEAP_SYMBOLS.map(|(_, top)| top).join(", ")
    )
}

pub struct ZiskProfiler {
    rom: ZiskRom,
    /// Guest addresses the heap covers.
    heap: Range<u64>,
}

impl ZiskProfiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        let rom = Riscv2zisk::new(elf)
            .run()
            .map_err(|error| anyhow!("failed to transpile the ELF into a ZisK ROM: {error}"))?;
        let (heap_bottom, heap_top) = heap_delimiters(elf)?;
        let heap = heap_range(elf, heap_bottom, heap_top)?;
        // `Mem::read_slice` panics rather than failing on a range it holds no one section for, so
        // the RAM section bounds what the emulator can be asked for.
        ensure!(
            RAM_ADDR <= heap.start && heap.end <= RAM_ADDR + RAM_SIZE,
            "the guest puts its heap at {:#x}..{:#x}, outside the RAM the emulator holds",
            heap.start,
            heap.end
        );
        Ok(Self { rom, heap })
    }
}

impl Profiler for ZiskProfiler {
    fn profile(&self, stateless_input: &[u8]) -> Result<Execution> {
        let options = EmuOptions {
            stats: true,
            ..Default::default()
        };
        let mut emu = Emu::new(&self.rom);
        // The emulator prints this same report to stdout, which the caller silences.
        emu.run(
            frame(stateless_input),
            &options,
            None::<Box<dyn Fn(EmuTrace)>>,
        );
        if !emu.terminated() {
            bail!("emulation did not reach the end of the program");
        }
        emu.ctx.stats.set_use_thousands_sep(false);
        let cost = parse(&emu.ctx.stats.report(&self.rom))?;
        // The emulator holds the whole RAM in one buffer, so the heap comes back as a slice of it.
        let heap = emu
            .ctx
            .inst_ctx
            .mem
            .read_slice(self.heap.start, self.heap.end - self.heap.start);
        Ok(Execution {
            cost,
            peak_heap_bytes: peak_heap_bytes(heap),
        })
    }
}

/// Wraps `input` the way ZisK reads it: a little-endian length, the bytes, then zero padding to a
/// multiple of eight.
fn frame(input: &[u8]) -> Vec<u8> {
    let padded = input.len().next_multiple_of(8);
    let mut framed = Vec::with_capacity(size_of::<u64>() + padded);
    framed.extend_from_slice(&(input.len() as u64).to_le_bytes());
    framed.extend_from_slice(input);
    framed.resize(size_of::<u64>() + padded, 0);
    framed
}

/// Reads the cost distribution out of the emulator's report.
///
/// Each kind is taken from its first occurrence, which is the summary block at the top; the
/// sections below it open with headers that reuse the same labels.
fn parse(report: &str) -> Result<Cost> {
    let mut cost = Cost::new();
    for line in report.lines() {
        let mut fields = line.split_whitespace();
        let (Some(label), Some(value)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Some(kind), Ok(value)) = (
            kinds().find(|kind| label.eq_ignore_ascii_case(row(kind))),
            value.parse::<u64>(),
        ) else {
            continue;
        };
        cost.entry(kind.to_owned()).or_insert(value);
    }

    let missing: Vec<&str> = kinds()
        .filter(|kind| !cost.contains_key(*kind))
        .map(row)
        .collect();
    if !missing.is_empty() {
        bail!("the emulator report is missing {}", missing.join(", "));
    }

    // ZisK computes the total as exactly this sum, so parts that fall short of it mean the summary
    // block was misread rather than that the emulator disagrees with itself.
    let summed: u64 = COMPOSITION
        .components
        .iter()
        .map(|kind| cost[kind.name])
        .sum();
    let total = cost[COMPOSITION.total];
    ensure!(
        summed == total,
        "the report's components sum to {summed}, not the {} of {total}",
        COMPOSITION.total
    );

    Ok(cost)
}

#[cfg(test)]
mod tests {
    use super::{frame, parse};

    /// A real `Stats::report` capture, taken through the same calls [`ZiskProfiler::profile`]
    /// makes: the Nethermind ZisK guest run on mainnet block 25580033, with thousands separators
    /// turned off.
    const REPORT: &str = include_str!("testdata/ziskemu-report.txt");

    #[test]
    fn input_is_length_prefixed_and_padded_to_eight() {
        assert_eq!(frame(&[]), [0; 8]);
        assert_eq!(
            frame(&[0xaa, 0xbb, 0xcc]),
            [3, 0, 0, 0, 0, 0, 0, 0, 0xaa, 0xbb, 0xcc, 0, 0, 0, 0, 0]
        );
        assert_eq!(frame(&[1; 8]).len(), 16);
        assert_eq!(frame(&[1; 9]).len(), 24);
    }

    #[test]
    fn cost_is_read_from_the_report() {
        let cost = parse(REPORT).unwrap();
        assert_eq!(cost["main"], 5799084064);
        assert_eq!(cost["opcode"], 1378944808);
        assert_eq!(cost["precompile"], 1947067046);
        assert_eq!(cost["memory"], 766637533);
        assert_eq!(cost["base"], 293601280);
        assert_eq!(cost["total"], 10185334731);
        // Rows that are derived, priced apart from the total, or not costs at all stay out.
        for skipped in ["variable", "frops", "steps"] {
            assert!(!cost.contains_key(skipped), "{skipped} was recorded");
        }
    }

    /// Parts that fall short of the total mean the summary block was misread.
    #[test]
    fn a_report_that_does_not_sum_is_an_error() {
        let broken = REPORT.replacen("293601280", "293601281", 1);
        assert!(parse(&broken).is_err());
    }

    #[test]
    fn a_truncated_report_is_an_error() {
        assert!(parse("STEPS 1\nMAIN 2\n").is_err());
    }
}
