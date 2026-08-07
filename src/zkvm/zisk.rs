//! ZisK profiling backend.
//!
//! ZisK prices an execution as a weighted sum over the work the prover has to do, which the
//! emulator accumulates while it runs. Enabling `EmuOptions::stats` is what the `ziskemu -X` flag
//! sets, and the resulting distribution is read back from the emulator in process rather than by
//! shelling out.

use anyhow::{Result, anyhow, bail, ensure};
use zisk_common::EmuTrace;
use zisk_core::{Riscv2zisk, ZiskRom};
use ziskemu::{Emu, EmuOptions};

use crate::zkvm::{Composition, Cost, Kind, Profiler};

/// Cost kinds that partition a ZisK total, in stack order, and the kind holding the whole.
///
/// These are also the only rows read out of the emulator's report. `VARIABLE` is skipped because it
/// is `TOTAL` less `BASE`, `FROPS` because it re-counts opcodes already priced under `OPCODES`, and
/// `STEPS` because it counts work rather than pricing it.
pub const COMPOSITION: Composition = Composition {
    total: "total",
    components: &[
        Kind {
            name: "base",
            note: "ROM and tables",
        },
        Kind {
            name: "main",
            note: "Main AIR",
        },
        Kind {
            name: "memory",
            note: "Memory",
        },
        Kind {
            name: "opcodes",
            note: "ZisK instructions",
        },
        Kind {
            name: "precompiles",
            note: "precompiles",
        },
    ],
};

/// The kinds [`COMPOSITION`] names, which are the report labels worth reading.
fn kinds() -> impl Iterator<Item = &'static str> {
    COMPOSITION
        .components
        .iter()
        .map(|kind| kind.name)
        .chain([COMPOSITION.total])
}

pub struct ZiskProfiler {
    rom: ZiskRom,
}

impl ZiskProfiler {
    pub fn new(elf: &[u8]) -> Result<Self> {
        let rom = Riscv2zisk::new(elf)
            .run()
            .map_err(|error| anyhow!("failed to transpile the ELF into a ZisK ROM: {error}"))?;
        Ok(Self { rom })
    }
}

impl Profiler for ZiskProfiler {
    fn profile(&self, stateless_input: &[u8]) -> Result<Cost> {
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
        parse(&emu.ctx.stats.report(&self.rom))
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
            kinds().find(|kind| label.eq_ignore_ascii_case(kind)),
            value.parse::<u64>(),
        ) else {
            continue;
        };
        cost.entry(kind.to_owned()).or_insert(value);
    }

    let missing: Vec<&str> = kinds().filter(|kind| !cost.contains_key(*kind)).collect();
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
        assert_eq!(cost["opcodes"], 1378944808);
        assert_eq!(cost["precompiles"], 1947067046);
        assert_eq!(cost["memory"], 766637533);
        assert_eq!(cost["base"], 293601280);
        assert_eq!(cost["total"], 10185334731);
        // Rows that are derived, re-counted or not costs at all stay out.
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
