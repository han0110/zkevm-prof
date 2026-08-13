//! Aggregates profiles into a report.
//!
//! Every input file contributes one series, labelled by its file stem. Series are compared over the
//! tests they all profiled, so a guest that failed on some blocks does not appear cheaper for
//! having skipped them.
//!
//! One report covers one zkVM, which is what the published page loads per tab. The JSON carries raw
//! numbers and the page formats them, so the figures on the page and in the markdown come from the
//! same series without either restating the other.

use std::{
    collections::BTreeSet,
    env, fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use askama::Template;
use clap::{Parser, ValueEnum};
use ere_catalog::zkVMKind;
use serde::Serialize;

use crate::zkvm::{self, Composition, Entry, Profile};

/// One profiled block, reduced to what the report charts.
struct Block {
    total: f64,
    components: Vec<f64>,
    /// Absent for a guest whose backend cannot read the heap it used.
    peak_heap_bytes: Option<f64>,
    /// Absent for a guest whose backend cannot read the stack it used.
    peak_stack_bytes: Option<f64>,
    gas_used: u64,
    number: u64,
}

/// One guest's profile over the shared corpus.
struct Series {
    label: String,
    guest: String,
    guest_version: String,
    elf_url: Option<String>,
    blocks: Vec<Block>,
    total: f64,
    components: Vec<f64>,
}

/// Shape the report is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Format {
    /// Series data the published page loads.
    Json,
    /// Tables, for pasting into a pull request or an issue.
    Md,
}

#[derive(Parser)]
pub struct ReportCmd {
    /// Profile JSON files, one per guest. The file stem labels the series.
    #[arg(long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,

    /// Path to write the report to.
    #[arg(long)]
    output: PathBuf,

    /// Shape the report is written in.
    #[arg(long, value_enum, default_value = "json")]
    format: Format,
}

impl ReportCmd {
    pub fn run(self) -> Result<()> {
        let profiles = self
            .input
            .iter()
            .map(|path| {
                let label = path
                    .file_stem()
                    .context("input has no file name")?
                    .to_string_lossy()
                    .into_owned();
                let text = fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let profile: Profile = serde_json::from_str(&text)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok((label, profile))
            })
            .collect::<Result<Vec<_>>>()?;

        let shared = shared_tests(&profiles);
        if shared.is_empty() {
            bail!("the inputs share no profiled test");
        }
        let composition = composition(&profiles)?;
        let (zkvm, zkvm_version) = provenance(&profiles)?;

        let mut series = profiles
            .iter()
            .map(|(label, profile)| build_series(label, profile, &shared, composition))
            .collect::<Result<Vec<_>>>()?;
        series.sort_by(|a, b| a.label.cmp(&b.label));

        let page = match self.format {
            Format::Json => {
                let report = Report::new(&series, shared.len(), composition, zkvm, zkvm_version);
                serde_json::to_string_pretty(&report)?
            }
            Format::Md => {
                let view = View::new(&series, shared.len(), composition, zkvm_version);
                MarkdownReport { view: &view }.render()?
            }
        };
        fs::write(&self.output, page)
            .with_context(|| format!("failed to write {}", self.output.display()))?;
        eprintln!(
            "wrote a report over {} series and {} shared blocks to {}",
            series.len(),
            shared.len(),
            self.output.display()
        );
        Ok(())
    }
}

/// Tests every input profiled, so the series are compared over the same work.
fn shared_tests(profiles: &[(String, Profile)]) -> BTreeSet<String> {
    let mut shared: Option<BTreeSet<String>> = None;
    for (_, profile) in profiles {
        let names: BTreeSet<String> = profile.profile.keys().cloned().collect();
        shared = Some(match shared {
            Some(shared) => shared.intersection(&names).cloned().collect(),
            None => names,
        });
    }
    shared.unwrap_or_default()
}

/// The composition of the zkVM every input was profiled on.
///
/// Series are only comparable when they price the same way, so mixing zkVMs in one report is an
/// error rather than a chart of numbers in different units.
fn composition(profiles: &[(String, Profile)]) -> Result<&'static Composition> {
    let (label, first) = profiles.first().context("no inputs")?;
    for (other, profile) in &profiles[1..] {
        if profile.meta.zkvm != first.meta.zkvm {
            bail!(
                "{label} was profiled on {} but {other} on {}",
                first.meta.zkvm,
                profile.meta.zkvm
            );
        }
    }
    zkvm::composition(first.meta.zkvm)
}

/// The zkVM and SDK version the report is of.
///
/// A tab shows one SDK version, so inputs built against different ones would put costs from two
/// circuits under one heading.
fn provenance(profiles: &[(String, Profile)]) -> Result<(zkVMKind, &str)> {
    let (label, first) = profiles.first().context("no inputs")?;
    for (other, profile) in &profiles[1..] {
        if profile.meta.zkvm_version != first.meta.zkvm_version {
            bail!(
                "{label} was profiled against {} but {other} against {}",
                first.meta.zkvm_version,
                profile.meta.zkvm_version
            );
        }
    }
    Ok((first.meta.zkvm, &first.meta.zkvm_version))
}

/// Link to the workflow run that produced the report, when one produced it.
///
/// The variables are set for every GitHub Actions job, so their absence means a local run and the
/// page simply shows no link.
fn run_url() -> Option<String> {
    let server = env::var("GITHUB_SERVER_URL").ok()?;
    let repository = env::var("GITHUB_REPOSITORY").ok()?;
    let run = env::var("GITHUB_RUN_ID").ok()?;
    Some(format!("{server}/{repository}/actions/runs/{run}"))
}

fn build_series(
    label: &str,
    profile: &Profile,
    shared: &BTreeSet<String>,
    composition: &Composition,
) -> Result<Series> {
    let mut blocks = Vec::with_capacity(shared.len());
    for name in shared {
        let entry: &Entry = &profile.profile[name];
        let value = |kind: &str| -> Result<f64> {
            entry
                .cost
                .get(kind)
                .map(|cost| *cost as f64)
                .with_context(|| format!("{label}/{name} has no {kind} cost"))
        };
        let total = value(composition.total)?;
        let components = composition
            .components
            .iter()
            .map(|kind| value(kind.name))
            .collect::<Result<Vec<f64>>>()?;
        let summed: f64 = components.iter().sum();
        // A stacked bar that does not reach its own total would misstate every share it draws. A
        // total priced as one number has no components, and so nothing to check.
        if !components.is_empty() && (summed - total).abs() > total * 1e-9 {
            bail!(
                "{label}/{name}: components sum to {summed}, not the {} of {total}",
                composition.total
            );
        }
        blocks.push(Block {
            total,
            components,
            peak_heap_bytes: entry.peak_heap_bytes.map(|peak| peak as f64),
            peak_stack_bytes: entry.peak_stack_bytes.map(|peak| peak as f64),
            gas_used: entry.metadata.gas_used,
            number: entry.metadata.block_number,
        });
    }

    let mut components = vec![0.0; composition.components.len()];
    for block in &blocks {
        for (sum, value) in components.iter_mut().zip(&block.components) {
            *sum += value;
        }
    }
    Ok(Series {
        label: label.to_owned(),
        guest: profile.meta.stateless_validator.clone(),
        guest_version: profile.meta.stateless_validator_version.clone(),
        elf_url: profile.meta.elf_url.clone(),
        total: blocks.iter().map(|block| block.total).sum(),
        components,
        blocks,
    })
}

/// Everything the published page draws, in the units the profiles were recorded in.
#[derive(Serialize)]
struct Report {
    zkvm: String,
    zkvm_version: String,
    /// Seconds since the epoch, which the page renders in the reader's own time zone.
    generated_at: u64,
    run_url: Option<String>,
    blocks: usize,
    kinds: Vec<String>,
    /// What each kind covers, in the order the kinds are listed.
    notes: Vec<String>,
    guests: Vec<ReportGuest>,
    cost_lines: Vec<ReportLine>,
    /// Peak heap per block, carrying only the guests whose backend can read it. Empty drops the
    /// section from the page, as an empty composition drops the composition chart.
    heap_lines: Vec<ReportLine>,
    /// Peak stack per block, carried the same way.
    stack_lines: Vec<ReportLine>,
}

#[derive(Serialize)]
struct ReportGuest {
    label: String,
    guest: String,
    guest_version: String,
    elf_url: Option<String>,
    total: f64,
    relative: f64,
    /// Absent for a guest whose backend cannot read the heap, which leaves its cell empty.
    peak_heap_bytes: Option<f64>,
    peak_heap_relative: Option<f64>,
    /// Absent for a guest whose backend cannot read the stack, which leaves its cell empty.
    peak_stack_bytes: Option<f64>,
    peak_stack_relative: Option<f64>,
    components: Vec<f64>,
}

#[derive(Serialize)]
struct ReportLine {
    label: String,
    /// `(gas, value, block number)`, the third value carried for the tooltip.
    points: Vec<(f64, f64, u64)>,
}

impl Report {
    fn new(
        series: &[Series],
        blocks: usize,
        composition: &Composition,
        zkvm: zkVMKind,
        zkvm_version: &str,
    ) -> Self {
        let cheapest = series
            .iter()
            .map(|guest| guest.total)
            .fold(f64::INFINITY, f64::min);
        Self {
            zkvm: zkvm.to_string(),
            zkvm_version: zkvm_version.to_owned(),
            generated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or_default(),
            run_url: run_url(),
            blocks,
            kinds: composition
                .components
                .iter()
                .map(|kind| kind.name.to_owned())
                .collect(),
            notes: composition
                .components
                .iter()
                .map(|kind| kind.note.to_owned())
                .collect(),
            guests: series
                .iter()
                .zip(peaks(series, |block| block.peak_heap_bytes))
                .zip(peaks(series, |block| block.peak_stack_bytes))
                .map(
                    |((guest, (heap, heap_relative)), (stack, stack_relative))| ReportGuest {
                        label: guest.label.clone(),
                        guest: guest.guest.clone(),
                        guest_version: guest.guest_version.clone(),
                        elf_url: guest.elf_url.clone(),
                        total: guest.total,
                        relative: guest.total / cheapest,
                        peak_heap_bytes: heap,
                        peak_heap_relative: heap_relative,
                        peak_stack_bytes: stack,
                        peak_stack_relative: stack_relative,
                        components: guest.components.clone(),
                    },
                )
                .collect(),
            cost_lines: series.iter().map(cost_line).collect(),
            heap_lines: series
                .iter()
                .filter_map(|guest| peak_line(guest, |block| block.peak_heap_bytes))
                .collect(),
            stack_lines: series
                .iter()
                .filter_map(|guest| peak_line(guest, |block| block.peak_stack_bytes))
                .collect(),
        }
    }
}

/// Every profiled block as a `(gas, cost, number)` point, ordered along the gas axis.
fn cost_line(series: &Series) -> ReportLine {
    let mut blocks: Vec<&Block> = series.blocks.iter().collect();
    blocks.sort_by_key(|block| block.gas_used);
    ReportLine {
        label: series.label.clone(),
        points: blocks
            .iter()
            .map(|block| (block.gas_used as f64, block.total, block.number))
            .collect(),
    }
}

/// The same for the peak `of` reads, or nothing when no block in the series carries one.
fn peak_line(series: &Series, of: fn(&Block) -> Option<f64>) -> Option<ReportLine> {
    let mut blocks: Vec<&Block> = series.blocks.iter().collect();
    blocks.sort_by_key(|block| block.gas_used);
    let points: Vec<(f64, f64, u64)> = blocks
        .iter()
        .filter_map(|block| Some((block.gas_used as f64, of(block)?, block.number)))
        .collect();
    (!points.is_empty()).then(|| ReportLine {
        label: series.label.clone(),
        points,
    })
}

/// The same figures as the page, formatted for the markdown tables.
struct View {
    blocks: usize,
    zkvm_version: String,
    kinds: Vec<Legend>,
    guests: Vec<Guest>,
    heap: Vec<Peak>,
    stack: Vec<Peak>,
}

/// A kind's column header and the note printed under the table.
struct Legend {
    name: String,
    note: String,
}

struct Guest {
    label: String,
    version: String,
    total: String,
    relative: String,
    components: Vec<Component>,
}

struct Component {
    value: String,
    share: String,
}

/// One guest's mean peak in a region, carried only for the guests whose profile recorded one, as
/// `heap_lines` and `stack_lines` carry them for the page.
struct Peak {
    label: String,
    version: String,
    peak: String,
    relative: String,
}

impl View {
    fn new(
        series: &[Series],
        blocks: usize,
        composition: &Composition,
        zkvm_version: &str,
    ) -> Self {
        let cheapest = series
            .iter()
            .map(|guest| guest.total)
            .fold(f64::INFINITY, f64::min);
        Self {
            blocks,
            zkvm_version: zkvm_version.to_owned(),
            kinds: composition
                .components
                .iter()
                .map(|kind| Legend {
                    name: kind.name.to_owned(),
                    note: kind.note.to_owned(),
                })
                .collect(),
            guests: series
                .iter()
                .map(|guest| Guest {
                    label: guest.label.clone(),
                    version: guest.guest_version.clone(),
                    // Per block, as the page shows it and as the mean peak heap beside it already
                    // reads. The ratio is between the totals, which is the same ratio either way.
                    total: si(guest.total / blocks as f64),
                    relative: format!("{:.2}x", guest.total / cheapest),
                    components: guest
                        .components
                        .iter()
                        .map(|value| Component {
                            value: si(*value / blocks as f64),
                            share: format!("{:.1}", value / guest.total * 100.0),
                        })
                        .collect(),
                })
                .collect(),
            heap: peak_rows(series, |block| block.peak_heap_bytes),
            stack: peak_rows(series, |block| block.peak_stack_bytes),
        }
    }
}

/// The rows a peak table prints, dropping the guests whose profile recorded none.
fn peak_rows(series: &[Series], of: fn(&Block) -> Option<f64>) -> Vec<Peak> {
    series
        .iter()
        .zip(peaks(series, of))
        .filter_map(|(guest, (peak, relative))| {
            Some(Peak {
                label: guest.label.clone(),
                version: guest.guest_version.clone(),
                peak: bytes(peak?),
                relative: format!("{:.2}x", relative?),
            })
        })
        .collect()
}

/// The mean of the peak `of` reads over the blocks the series recorded one for, or nothing when it
/// recorded none.
///
/// A mean rather than the largest, since one figure per guest is there to compare how much memory
/// the corpus takes them rather than to size a machine for their worst block.
fn mean_peak(series: &Series, of: fn(&Block) -> Option<f64>) -> Option<f64> {
    let (sum, count) = series
        .blocks
        .iter()
        .filter_map(of)
        .fold((0.0, 0u32), |(sum, count), peak| (sum + peak, count + 1));
    (count > 0).then(|| sum / f64::from(count))
}

/// Each series' mean peak beside its ratio to the smallest, both absent for a series that recorded
/// no peak.
///
/// Ratios are taken between the means rather than averaged per block, which keeps them reading the
/// same whichever guest they are stated against.
fn peaks(series: &[Series], of: fn(&Block) -> Option<f64>) -> Vec<(Option<f64>, Option<f64>)> {
    let peaks: Vec<Option<f64>> = series.iter().map(|guest| mean_peak(guest, of)).collect();
    let smallest = peaks
        .iter()
        .flatten()
        .fold(f64::INFINITY, |smallest, peak| smallest.min(*peak));
    peaks
        .iter()
        .map(|peak| (*peak, peak.map(|peak| peak / smallest)))
        .collect()
}

/// Formats a magnitude with an SI suffix, which keeps costs past 10^10 readable.
fn si(value: f64) -> String {
    for (limit, suffix) in [(1e12, 'T'), (1e9, 'G'), (1e6, 'M'), (1e3, 'k')] {
        if value.abs() >= limit {
            return format!("{:.2}{suffix}", value / limit);
        }
    }
    format!("{value:.0}")
}

/// Formats a byte count in binary units, which is how a memory figure reads and how the page prints
/// the same number.
fn bytes(value: f64) -> String {
    for (limit, unit) in [(1u64 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")] {
        if value.abs() >= limit as f64 {
            return format!("{:.2} {unit}", value / limit as f64);
        }
    }
    format!("{value:.0} B")
}

#[derive(Template)]
#[template(path = "report.md")]
struct MarkdownReport<'a> {
    view: &'a View,
}

#[cfg(test)]
mod tests {
    use crate::command::report::{Block, Series, bytes, peaks, si};

    /// A series of two blocks carrying the given heap and stack peaks, with the fields the ranking
    /// does not read left empty.
    fn series(label: &str, blocks: [(f64, f64); 2]) -> Series {
        Series {
            label: label.to_owned(),
            guest: label.to_owned(),
            guest_version: String::new(),
            elf_url: None,
            blocks: blocks
                .iter()
                .map(|(heap, stack)| Block {
                    total: 0.0,
                    components: Vec::new(),
                    peak_heap_bytes: Some(*heap),
                    peak_stack_bytes: Some(*stack),
                    gas_used: 0,
                    number: 0,
                })
                .collect(),
            total: 0.0,
            components: Vec::new(),
        }
    }

    /// Heap and stack are ranked by one implementation, so each has to come out against the smallest
    /// of the region it reads rather than of whichever region ran first.
    #[test]
    fn each_region_is_ranked_against_its_own_smallest() {
        let series = [
            series("a", [(400.0, 30.0), (600.0, 50.0)]),
            series("b", [(100.0, 60.0), (300.0, 140.0)]),
        ];
        assert_eq!(
            peaks(&series, |block| block.peak_heap_bytes),
            [(Some(500.0), Some(2.5)), (Some(200.0), Some(1.0))]
        );
        assert_eq!(
            peaks(&series, |block| block.peak_stack_bytes),
            [(Some(40.0), Some(1.0)), (Some(100.0), Some(2.5))]
        );
    }

    #[test]
    fn magnitudes_carry_an_si_suffix() {
        assert_eq!(si(56_513_151_760.0), "56.51G");
        assert_eq!(si(30_100_000.0), "30.10M");
        assert_eq!(si(999.0), "999");
    }

    /// A memory figure reads in binary units, so the same count that prints as 57.53M of cost prints
    /// as the mebibytes it actually occupies.
    #[test]
    fn byte_counts_carry_a_binary_unit() {
        assert_eq!(bytes(57_531_938.0), "54.87 MiB");
        assert_eq!(bytes(1_073_741_824.0), "1.00 GiB");
        assert_eq!(bytes(2048.0), "2.00 KiB");
        assert_eq!(bytes(999.0), "999 B");
    }
}
