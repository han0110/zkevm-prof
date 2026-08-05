//! Aggregates profiles into a report.
//!
//! Every input file contributes one series, labelled by its file stem. Series are compared over the
//! tests they all profiled, so a guest that failed on some blocks does not appear cheaper for
//! having skipped them.
//!
//! This module reduces the profiles to figures and series data. The documents live in `templates/`
//! and are checked at build time by askama, and the page builds its charts from the embedded JSON.

use std::{collections::BTreeSet, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use askama::Template;
use clap::{Parser, ValueEnum};
use serde::Serialize;

use crate::zkvm::{self, Composition, Entry, Profile};

/// One profiled block, reduced to what the report charts.
struct Block {
    total: f64,
    components: Vec<f64>,
    gas_used: u64,
    number: u64,
}

/// One guest's profile over the shared corpus.
struct Series {
    label: String,
    blocks: Vec<Block>,
    total: f64,
    components: Vec<f64>,
    gas_used: u64,
}

/// Shape the report is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Format {
    /// Single page with charts.
    Html,
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
    #[arg(long, value_enum, default_value = "html")]
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

        let mut series = profiles
            .iter()
            .map(|(label, profile)| build_series(label, profile, &shared, composition))
            .collect::<Result<Vec<_>>>()?;
        series.sort_by(|a, b| a.label.cmp(&b.label));

        let view = View::new(&series, shared.len(), composition)?;
        let page = match self.format {
            Format::Html => HtmlReport { view: &view }.render()?,
            Format::Md => MarkdownReport { view: &view }.render()?,
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
            .map(|kind| value(kind))
            .collect::<Result<Vec<f64>>>()?;
        let summed: f64 = components.iter().sum();
        // A stacked bar that does not reach its own total would misstate every share it draws. A
        // total priced as one number has no components, and so nothing to check.
        if !components.is_empty() && (summed - total).abs() > total * 1e-9 {
            bail!(
                "{label}/{name}: {:?} sum to {summed}, not the {} of {total}",
                composition.components,
                composition.total
            );
        }
        blocks.push(Block {
            total,
            components,
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
        total: blocks.iter().map(|block| block.total).sum(),
        gas_used: blocks.iter().map(|block| block.gas_used).sum(),
        components,
        blocks,
    })
}

/// Everything the templates draw.
struct View {
    blocks: usize,
    kinds: Vec<String>,
    guests: Vec<Guest>,
    /// Series the page's charts are built from, as JSON.
    chart: String,
}

struct Guest {
    label: String,
    total: String,
    mean: String,
    per_gas: String,
    relative: String,
    components: Vec<Component>,
}

struct Component {
    value: String,
    share: String,
}

/// Series data the page's chart layer reads.
#[derive(Serialize)]
struct Chart {
    kinds: Vec<String>,
    guests: Vec<ChartGuest>,
    lines: Vec<ChartLine>,
}

#[derive(Serialize)]
struct ChartGuest {
    label: String,
    components: Vec<f64>,
    total: f64,
}

#[derive(Serialize)]
struct ChartLine {
    label: String,
    /// `(gas, cost, block number)`, the third value carried for the tooltip.
    points: Vec<(f64, f64, u64)>,
}

impl View {
    fn new(series: &[Series], blocks: usize, composition: &Composition) -> Result<Self> {
        let cheapest = series
            .iter()
            .map(|guest| guest.total)
            .fold(f64::INFINITY, f64::min);

        let guests = series
            .iter()
            .map(|guest| Guest {
                label: guest.label.clone(),
                total: si(guest.total),
                mean: si(guest.total / guest.blocks.len() as f64),
                per_gas: format!("{:.1}", guest.total / guest.gas_used as f64),
                relative: format!("{:.2}x", guest.total / cheapest),
                components: guest
                    .components
                    .iter()
                    .map(|value| Component {
                        value: si(*value),
                        share: format!("{:.1}", value / guest.total * 100.0),
                    })
                    .collect(),
            })
            .collect();

        let kinds: Vec<String> = composition
            .components
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect();
        let chart = Chart {
            kinds: kinds.clone(),
            guests: series
                .iter()
                .map(|guest| ChartGuest {
                    label: guest.label.clone(),
                    components: guest.components.clone(),
                    total: guest.total,
                })
                .collect(),
            lines: series.iter().map(line).collect(),
        };

        Ok(Self {
            blocks,
            kinds,
            guests,
            chart: encode(&chart)?,
        })
    }
}

/// Serializes the chart data for the page's `<script>` block.
///
/// Escaping `</` keeps a label holding a closing tag from ending the script element early, which
/// `\/` expresses without changing the string a parser decodes.
fn encode(chart: &Chart) -> Result<String> {
    Ok(serde_json::to_string(chart)?.replace("</", "<\\/"))
}

/// Every profiled block as a `(gas, cost, number)` point, ordered along the gas axis.
fn line(series: &Series) -> ChartLine {
    let mut blocks: Vec<&Block> = series.blocks.iter().collect();
    blocks.sort_by_key(|block| block.gas_used);
    ChartLine {
        label: series.label.clone(),
        points: blocks
            .iter()
            .map(|block| (block.gas_used as f64, block.total, block.number))
            .collect(),
    }
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

#[derive(Template)]
#[template(path = "report.html")]
struct HtmlReport<'a> {
    view: &'a View,
}

#[derive(Template)]
#[template(path = "report.md")]
struct MarkdownReport<'a> {
    view: &'a View,
}

#[cfg(test)]
mod tests {
    use super::{Chart, ChartGuest, ChartLine, encode, si};

    #[test]
    fn magnitudes_carry_an_si_suffix() {
        assert_eq!(si(56_513_151_760.0), "56.51G");
        assert_eq!(si(30_100_000.0), "30.10M");
        assert_eq!(si(999.0), "999");
    }

    /// A label carrying a closing tag must not be able to end the page's script element.
    #[test]
    fn encoded_chart_data_cannot_close_its_script_element() {
        let chart = Chart {
            kinds: vec!["main".to_owned()],
            guests: vec![ChartGuest {
                label: "</script><img src=x>".to_owned(),
                components: vec![1.0],
                total: 1.0,
            }],
            lines: vec![ChartLine {
                label: "guest".to_owned(),
                points: vec![(1.0, 2.0, 3)],
            }],
        };
        let encoded = encode(&chart).unwrap();
        assert!(!encoded.contains("</"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap()["guests"][0]["label"],
            "</script><img src=x>"
        );
    }
}
