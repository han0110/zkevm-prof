//! The `import` command.
//!
//! A proving harness records how long each block took in a layout of its own. This reads one such
//! run and files its times under the profile they are read against, so a corpus's times sit beside
//! that corpus's costs and beside nothing else.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail, ensure};
use clap::{Parser, builder::PossibleValuesParser};
use serde::Deserialize;
use zkvm_prof::zkVMKind;

use crate::{
    command::{json_files, read, write},
    profile::{Entry, Profile},
    proving::{Hardware, Proving, ProvingMeta},
};

/// File a provoor run holds every block it proved in, which is what names a directory one.
const BLOCK_LOGS: &str = "result.block-logs.json";

/// File a cluster run records the machine in, which is what names a directory a metrics tree.
const HARDWARE: &str = "hardware.json";

/// Depth a metrics tree holds a block at, one directory for the guest and one for the SDK.
const METRIC_DEPTH: usize = 3;

/// Depth a profile sits at under the directory it is published in, one directory for the guest and
/// one for the ELF.
const PROFILE_DEPTH: usize = 3;

/// Root a corpus names its EEST fixtures under, which a provoor test name drops.
const FIXTURE_ROOT: &str = "tests/";

/// Machines times can be imported for, keyed by what `--id` names one by, which is also the file a
/// dataset is written to.
///
/// The machine is stated here rather than read off the run, since a run records the host that wrote
/// it while a dataset is proved across the whole set. A machine that proves on its CPU alone holds
/// no card.
fn machines() -> BTreeMap<&'static str, ProvingMeta> {
    BTreeMap::from([(
        "ef-cluster-4x4",
        ProvingMeta {
            name: "EF Cluster (4x4)".to_owned(),
            machines: 4,
            hardware: Hardware {
                cpu: "AMD Ryzen Threadripper PRO 9975WX 32-Cores".to_owned(),
                ram_bytes: 125 * 1024 * 1024 * 1024,
                os: "ubuntu 24.04".to_owned(),
                gpus: vec!["NVIDIA GeForce RTX 5090".to_owned(); 4],
            },
        },
    )])
}

/// Files the proving times of one run under the profile they are read against.
///
/// A run is either a provoor result directory, which holds every block it proved in one log, or a
/// cluster metrics tree, which holds one file per block under the guest and the SDK it was proved
/// from. Both report the cluster's own latency for one proof, so the two carry one measurement under
/// two layouts.
#[derive(Parser)]
pub struct ImportCmd {
    /// Directory the proving run wrote its results to.
    #[arg(long)]
    proving_results: PathBuf,

    /// Fixture corpus the run proved, which names the profile the times are read against.
    #[arg(long)]
    suite: String,

    /// SHA-256 of the ELF the run proved, which names the profile the times are read against.
    #[arg(long)]
    elf_sha256: String,

    /// Directory the profiles are published under.
    #[arg(long, default_value = "profiles")]
    profile_dir: PathBuf,

    /// Machine the times were measured on, which is what the page states beside them.
    #[arg(long, value_parser = PossibleValuesParser::new(machines().into_keys()))]
    id: String,
}

/// One proved block as a run records it, under the name that run knows it by.
struct Proved {
    name: String,
    block_number: u64,
    gas_used: u64,
    proving_time_ms: u64,
    /// Whether the proof revealed what the fixture expects, a block that proved something else
    /// being no time of that fixture.
    output_matched: bool,
}

/// One block of a provoor run, which reports the cluster's own latency alongside its own.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockLog {
    block: LoggedBlock,
    cluster_reported_proving_time_ms: u64,
    output_matched: bool,
}

#[derive(Deserialize)]
struct LoggedBlock {
    number: u64,
    gas_used: u64,
}

/// One block of a cluster metrics tree.
#[derive(Deserialize)]
struct Metric {
    metadata: MetricMetadata,
    proving: MetricProving,
}

#[derive(Deserialize)]
struct MetricMetadata {
    block_number: u64,
    block_used_gas: u64,
    /// Fixture the block was taken from, which an EEST corpus names as the corpus itself does and a
    /// chain corpus names by the block number alone.
    original_test_name: String,
}

#[derive(Deserialize)]
struct MetricProving {
    /// Absent where the proof did not land, the harness then recording why in its place.
    success: Option<MetricSuccess>,
}

#[derive(Deserialize)]
struct MetricSuccess {
    output_matched: bool,
    proving_time_ms: u64,
}

/// A profile as a run is matched against it, which is the blocks it holds under the names and the
/// numbers a proving harness knows them by.
struct Profiled<'a> {
    /// The corpus's own names, which is what an EEST harness records.
    by_name: &'a BTreeMap<String, Entry>,
    /// The block numbers, which is what a chain harness records in place of a name. A corpus whose
    /// blocks share a number contributes none, an EEST fixture always being block one.
    by_number: HashMap<u64, (&'a str, &'a Entry)>,
}

impl<'a> Profiled<'a> {
    fn new(entries: &'a BTreeMap<String, Entry>) -> Self {
        let mut numbered: HashMap<u64, Option<(&str, &Entry)>> = HashMap::new();
        for (name, entry) in entries {
            numbered
                .entry(entry.metadata.block_number)
                .and_modify(|held| *held = None)
                .or_insert(Some((name, entry)));
        }
        Self {
            by_name: entries,
            by_number: numbered
                .into_iter()
                .filter_map(|(number, found)| Some((number, found?)))
                .collect(),
        }
    }

    /// The block a run's `name` and `number` stand for, absent where the corpus holds none.
    ///
    /// An EEST harness names a fixture as the corpus does, provoor after the root the fixtures sit
    /// under is put back, while a chain harness names a block by its number, which is why a corpus
    /// of one block per number is read by number too.
    fn find(&self, name: &str, number: u64) -> Option<(&'a str, &'a Entry)> {
        let named = |name: &str| {
            self.by_name
                .get_key_value(name)
                .map(|(name, entry)| (name.as_str(), entry))
        };
        named(name)
            .or_else(|| named(&format!("{FIXTURE_ROOT}{name}")))
            .or_else(|| self.by_number.get(&number).copied())
    }
}

impl ImportCmd {
    pub fn run(self) -> Result<()> {
        let meta = machines()
            .remove(self.id.as_str())
            .expect("clap parsed the id from the machines above");
        let path = profile(&self.profile_dir, &self.elf_sha256, &self.suite)?;
        let profile: Profile = read(&path)?;
        let profiled = Profiled::new(&profile.profile);

        let proved = match self.proving_results.join(BLOCK_LOGS).is_file() {
            true => provoor(&self.proving_results)?,
            false => {
                ensure!(
                    self.proving_results.join(HARDWARE).is_file(),
                    "{} holds neither {BLOCK_LOGS} nor {HARDWARE}, so it is no proving run",
                    self.proving_results.display()
                );
                zkevm_metrics(&self.proving_results, profile.meta.zkvm)?
            }
        };

        let mut proving_time_ms = BTreeMap::new();
        let mut unmatched = 0;
        for block in proved {
            let Some((name, entry)) = profiled.find(&block.name, block.block_number) else {
                continue;
            };
            if !block.output_matched {
                unmatched += 1;
                continue;
            }
            ensure!(
                entry.metadata.gas_used == block.gas_used,
                "{name} proved {} gas against the {} it was profiled at",
                block.gas_used,
                entry.metadata.gas_used
            );
            ensure!(
                block.proving_time_ms > 0,
                "{name} carries a proving time of zero"
            );
            proving_time_ms.insert(name.to_owned(), block.proving_time_ms);
        }
        ensure!(
            !proving_time_ms.is_empty(),
            "{} proved no block {} was profiled over",
            self.proving_results.display(),
            self.suite
        );
        if unmatched > 0 {
            eprintln!(
                "left out {unmatched} blocks that proved an output the corpus does not expect"
            );
        }

        // The directory a dataset sits in names the profile it states the times of, so it is the
        // corpus beside the profile of the same name rather than a name of its own.
        let output = path
            .parent()
            .expect("the profile sits under the guest and the ELF")
            .join(&self.suite)
            .join(format!("{}.json", self.id));
        let document = Proving {
            proving_time_ms,
            meta,
        };
        write(&output, &document)?;
        eprintln!(
            "wrote {} of {} blocks to {}",
            document.proving_time_ms.len(),
            profile.profile.len(),
            output.display()
        );
        Ok(())
    }
}

/// The profile of `elf_sha256` over `suite` published under `dir`.
///
/// The guest is searched for rather than named, an ELF being built for one guest and its hash
/// therefore saying which. A directory naming the leading digits of the hash is what a profile is
/// filed under, so the hash a run states carries it whole and the directory matches a prefix of it.
fn profile(dir: &Path, elf_sha256: &str, suite: &str) -> Result<PathBuf> {
    let named = format!("{suite}.json");
    let found: Vec<PathBuf> = json_files(dir, Some(PROFILE_DEPTH))?
        .into_iter()
        .filter(|path| {
            path.ends_with(&named)
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|elf| elf_sha256.starts_with(&*elf.to_string_lossy()))
        })
        .collect();
    match found.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!(
            "{} publishes no profile of {elf_sha256} over {suite}",
            dir.display()
        ),
        found => bail!(
            "{} publishes {} profiles of {elf_sha256} over {suite}",
            dir.display(),
            found.len()
        ),
    }
}

/// The blocks a provoor run proved, which it holds in one log keyed by test name.
fn provoor(dir: &Path) -> Result<Vec<Proved>> {
    let logs: BTreeMap<String, BlockLog> = read(&dir.join(BLOCK_LOGS))?;
    Ok(logs
        .into_iter()
        .map(|(name, log)| Proved {
            name,
            block_number: log.block.number,
            gas_used: log.block.gas_used,
            proving_time_ms: log.cluster_reported_proving_time_ms,
            output_matched: log.output_matched,
        })
        .collect())
}

/// The blocks a cluster run proved, one file each under the guest and the SDK it was proved from.
///
/// A block whose proof did not land carries no time and is left out, the harness recording why it
/// did not in place of the timing.
///
/// The zkVM the SDK directory names is what says the times and the profile's costs came from one
/// zkVM, which no block name or gas figure states, one corpus proving the same blocks on every
/// zkVM. The versions either directory carries are left alone, a run and a profile labelling one
/// build differently.
fn zkevm_metrics(dir: &Path, zkvm: zkVMKind) -> Result<Vec<Proved>> {
    let proved_on = format!("{zkvm}-");
    let paths = json_files(dir, Some(METRIC_DEPTH))?;
    for path in &paths {
        let sdk = path
            .parent()
            .and_then(Path::file_name)
            .expect("the metric sits under the guest and the SDK")
            .to_string_lossy();
        ensure!(
            sdk.starts_with(&proved_on),
            "{} was proved on {sdk}, not the {zkvm} the profile prices",
            path.display()
        );
    }
    paths
        .into_iter()
        .map(|path| {
            let metric: Metric = read(&path)?;
            Ok(metric.proving.success.map(|success| Proved {
                name: metric.metadata.original_test_name,
                block_number: metric.metadata.block_number,
                gas_used: metric.metadata.block_used_gas,
                proving_time_ms: success.proving_time_ms,
                output_matched: success.output_matched,
            }))
        })
        .filter_map(Result::transpose)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use zkvm_prof::zkVMKind;

    use crate::{
        command::{
            directory,
            import::{Profiled, profile, zkevm_metrics},
        },
        profile::Entry,
    };

    /// A tree of published profiles to search, since what tells one profile from another is the ELF
    /// directory it sits under rather than anything in its name.
    fn tree(name: &str) -> PathBuf {
        let root = directory(name);
        for elf in ["reth/0e5355489a37a840", "ethrex/6b31e1f6ecdafbfc"] {
            let elf = root.join(elf);
            fs::create_dir_all(&elf).unwrap();
            fs::write(elf.join("eest-v0.6.2-100m.json"), "{}").unwrap();
            fs::write(elf.join("mainnet-25580000-1000.json"), "{}").unwrap();
        }
        root
    }

    fn entries(blocks: &[(&str, u64)]) -> BTreeMap<String, Entry> {
        blocks
            .iter()
            .map(|(name, block_number)| {
                let entry = format!(
                    r#"{{"cost":{{"total":7}},
                    "metadata":{{"gas_used":21000,"block_number":{block_number}}}}}"#
                );
                ((*name).to_owned(), serde_json::from_str(&entry).unwrap())
            })
            .collect()
    }

    /// An ELF belongs to one guest, so the hash a run states is what finds the profile and the guest
    /// need not be named alongside it.
    #[test]
    fn a_profile_is_found_by_the_hash_of_the_elf_it_measured() {
        let root = tree("zkevm-prof-finds-a-profile-by-its-elf");
        let elf = "0e5355489a37a8402cf3988f78700cbecaaadba9fcc4e210d58387cc9f782e98";
        assert_eq!(
            profile(&root, elf, "eest-v0.6.2-100m").unwrap(),
            root.join("reth/0e5355489a37a840/eest-v0.6.2-100m.json")
        );
        // A corpus the ELF was never profiled over is no profile to file times under, and neither is
        // a corpus profiled for another ELF alone.
        assert!(profile(&root, elf, "eest-v0.6.2").is_err());
        assert!(profile(&root, "6b31e1f6ecdafbfc", "eest-v0.6.2-100m").is_ok());
        assert!(profile(&root, "6b31e1f6", "eest-v0.6.2-100m").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    /// An EEST harness names a fixture as the corpus does, provoor dropping the root the fixtures
    /// sit under, so both reach the block the corpus profiled.
    #[test]
    fn an_eest_block_is_found_by_the_name_its_corpus_gives_it() {
        let name = "tests/benchmark/compute/test_modexp.py::test_modexp[fork_Amsterdam]";
        let entries = entries(&[
            (name, 1),
            ("tests/benchmark/compute/test_stack.py::test_push", 1),
        ]);
        let profiled = Profiled::new(&entries);
        assert_eq!(profiled.find(name, 1).unwrap().0, name);
        assert_eq!(
            profiled
                .find(name.strip_prefix("tests/").unwrap(), 1)
                .unwrap()
                .0,
            name
        );
        // Every EEST fixture is block one, so a corpus of them is read by name alone and a name it
        // does not hold reaches nothing.
        assert!(
            profiled
                .find("tests/benchmark/compute/test_add.py::test_add", 1)
                .is_none()
        );
    }

    /// A chain harness names a block by its number, which reaches the fixture the corpus names by
    /// that number and its hash.
    #[test]
    fn a_chain_block_is_found_by_its_number() {
        let first = "witness-generator-spec-cli::block_25580000_24c8fa4d";
        let entries = entries(&[
            (first, 25580000),
            (
                "witness-generator-spec-cli::block_25580001_b4635b0d",
                25580001,
            ),
        ]);
        let profiled = Profiled::new(&entries);
        assert_eq!(
            profiled.find("mainnet_25580000", 25580000).unwrap().0,
            first
        );
        assert!(profiled.find("mainnet_25581000", 25581000).is_none());
    }

    /// One corpus proves the same blocks on every zkVM, so neither a block name nor a gas figure
    /// tells one run from another and the SDK the run was proved on is what does.
    #[test]
    fn a_run_of_another_zkvm_is_no_dataset() {
        let root = directory("zkevm-prof-reads-the-sdk-a-run-was-proved-on");
        let sdk = root.join("reth-f804dc1/zisk-v1.1.0-alpha");
        fs::create_dir_all(&sdk).unwrap();
        fs::write(
            sdk.join("eest__mainnet_25580000__block0.json"),
            r#"{"metadata":{"block_number":25580000,"block_used_gas":26211834,
            "original_test_name":"mainnet_25580000"},
            "proving":{"success":{"output_matched":true,"proving_time_ms":3203}}}"#,
        )
        .unwrap();

        let proved = zkevm_metrics(&root, zkVMKind::Zisk).unwrap();
        assert_eq!(proved.len(), 1);
        assert_eq!(proved[0].proving_time_ms, 3203);
        assert!(zkevm_metrics(&root, zkVMKind::OpenVM).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
