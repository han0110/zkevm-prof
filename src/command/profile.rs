//! The `profile` command.
//!
//! Resolves the guest ELF through the registry, then runs it over a fixture corpus on the chosen
//! zkVM.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Write, stderr},
    os::fd::AsFd,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{
    Parser,
    builder::{PossibleValuesParser, TypedValueParser},
};
use ere_catalog::zkVMKind;
use gag::Gag;
use rayon::prelude::*;
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    command::{now, run_url},
    fixture, registry,
    zkvm::{self, Entry, Execution, Failure, Meta, Profile, Profiler, profiler},
};

/// Digits of the ELF hash a published profile is filed under.
const ELF_SHA256_PREFIX: usize = 16;

/// A published profile read for its meta alone, which is what says whether it is this one.
#[derive(Deserialize)]
struct Document {
    meta: Meta,
}

/// What a published profile has to record for this run to be the one already in it.
struct Published<'a> {
    version: &'a str,
    zkvm: zkVMKind,
    zkvm_version: &'a str,
    elf_url: Option<&'a str>,
    elf_sha256: &'a str,
    stateless_validator_version: &'a str,
}

/// Profiles one block, turning a panic inside the backend into an error.
///
/// A guest that breaks a zkVM invariant aborts the emulator by panicking rather than by returning,
/// and rayon re-raises a worker's panic on the thread collecting the results, which would discard a
/// whole corpus over one bad block. Backends carry no state from one block to the next, so a caught
/// panic leaves nothing inconsistent behind.
fn profile_block(profiler: &dyn Profiler, stateless_input: &[u8]) -> Result<Execution> {
    match catch_unwind(AssertUnwindSafe(|| profiler.profile(stateless_input))) {
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

/// Profiles one guest on one zkVM over a fixture corpus.
///
/// Fixtures are walked recursively and only the last block of each is profiled, since that is the
/// block an EEST blockchain test exercises and the ones before it only build the state it runs
/// against.
///
/// Blocks are profiled in parallel over rayon's global pool, which by default fills every core. A
/// profiled execution holds the whole guest memory, so cap the pool with `RAYON_NUM_THREADS` on a
/// machine with many cores and little memory.
#[derive(Parser)]
pub struct ProfileCmd {
    /// zkVM to profile the guest on.
    #[arg(
        long,
        value_parser = PossibleValuesParser::new(["openvm", "sp1", "zisk"])
            .map(|value| zkVMKind::from_str(&value).expect("the value came from the list above"))
    )]
    zkvm: zkVMKind,

    /// Stateless validator whose guest is profiled.
    #[arg(long, value_parser = PossibleValuesParser::new(registry::stateless_validators()))]
    stateless_validator: String,

    /// Directory of EEST fixtures, walked recursively for `.json` files.
    ///
    /// Left out, the corpus the suite names is fetched from where the registry publishes it and
    /// cached under `fixtures`.
    #[arg(long)]
    input: Option<PathBuf>,

    /// Directory the profile is published under, at the path its guest, ELF and suite give it.
    #[arg(long, default_value = "profiles")]
    output_dir: PathBuf,

    /// Fixture corpus the profile is of, defaulting to the name of the input directory and required
    /// without one, since it is then what says which corpus to fetch.
    #[arg(long, required_unless_present = "input")]
    suite: Option<String>,

    /// Regular expression the blocks profiled have to carry, in place of the whole corpus.
    ///
    /// Matched against the name a block is keyed by, so a corpus can be cut down to a family of
    /// tests or to a span of chain without a corpus of its own.
    #[arg(long)]
    filter: Option<String>,

    /// Guest ELF to profile, in place of the downloaded one.
    ///
    /// A guest built against an SDK other than the one this crate links does not load, so profiling
    /// one that no ere-guests build carries means handing the ELF over directly.
    #[arg(long)]
    elf: Option<PathBuf>,

    /// Reprofiles a guest whose run is already published.
    ///
    /// A profile written over a corpus some of whose blocks failed is published like any other and
    /// skipped from then on, so measuring it again takes saying so.
    #[arg(long)]
    force: bool,
}

impl ProfileCmd {
    pub async fn run(self) -> Result<()> {
        let filter = match &self.filter {
            None => None,
            Some(pattern) => Some(
                Regex::new(pattern)
                    .with_context(|| format!("--filter {pattern:?} is not a regular expression"))?,
            ),
        };

        // Resolved before the run rather than beside the profile it labels, so a guest the registry
        // does not list fails now instead of after the whole corpus has been profiled.
        let stateless_validator_version = registry::version(self.zkvm, &self.stateless_validator)?;
        let elf_url = match &self.elf {
            Some(_) => None,
            None => registry::url(self.zkvm, &self.stateless_validator)?,
        };
        let elf = match &self.elf {
            Some(path) => {
                fs::read(path).with_context(|| format!("failed to read {}", path.display()))?
            }
            None => registry::elf(self.zkvm, &self.stateless_validator)
                .await
                .with_context(|| {
                    format!(
                        "failed to resolve the {} guest for {}",
                        self.stateless_validator, self.zkvm
                    )
                })?,
        };
        let elf_sha256 = hex::encode(Sha256::digest(&elf));
        let suite = self.suite()?;
        let output = self.output(&elf_sha256, &suite);

        // Checked before the guest is compiled, which is the expensive half of profiling one, so a
        // workflow rerun over an unchanged guest pays only for the download. Read whether or not the
        // run is forced, since what it reports about another zkVM's profile holds either way.
        let already = Published {
            version: env!("CARGO_PKG_VERSION"),
            zkvm: self.zkvm,
            zkvm_version: self.zkvm.sdk_version(),
            elf_url: elf_url.as_deref(),
            elf_sha256: &elf_sha256,
            stateless_validator_version,
        };
        if published(&output, already)? && !self.force {
            eprintln!("skipping {}, already profiled", output.display());
            return Ok(());
        }

        // Fetched and walked after the skip check and before the guest is compiled, so a rerun over
        // an unchanged guest downloads no corpus at all and a corpus that will not resolve fails now
        // rather than after the build.
        let input = match &self.input {
            Some(input) => input.clone(),
            None => fixture::fetch(&suite).await?,
        };
        let paths = fixture::find(&input)?;
        eprintln!(
            "profiling {} on {} over {} fixtures",
            self.stateless_validator,
            self.zkvm,
            paths.len()
        );
        if let Some(filter) = &filter {
            eprintln!("profiling only the blocks named like {filter}");
        }
        let profiler = profiler(self.zkvm, &elf)?;

        // Backends chatter on both streams while they run, the ZisK emulator on stdout with its
        // whole report per block and SP1 on stderr with what the guest writes. Progress goes to a
        // copy of stderr taken before the run, so dropping both streams costs nothing.
        let progress = File::from(stderr().as_fd().try_clone_to_owned()?);
        let report = |line: String| {
            (&progress)
                .write_all(format!("{line}\n").as_bytes())
                .expect("the copy of stderr is writable")
        };
        let silenced = (Gag::stdout()?, Gag::stderr()?);
        let done = AtomicUsize::new(0);
        let outcomes: Vec<(String, Result<Entry, Failure>)> = paths
            .par_iter()
            .flat_map(|path| match fixture::load(path) {
                Ok(fixtures) => fixtures
                    .into_iter()
                    .filter(|fixture| {
                        filter
                            .as_ref()
                            .is_none_or(|filter| filter.is_match(&fixture.test_name))
                    })
                    .collect(),
                Err(error) => {
                    report(format!("{error:#}"));
                    Vec::new()
                }
            })
            .map(|fixture| {
                let result = profile_block(profiler.as_ref(), &fixture.stateless_input);
                // Counts blocks rather than files, since a fixture file may hold several tests.
                let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                let outcome = match result {
                    Ok(execution) => {
                        if done.is_multiple_of(25) {
                            report(format!("[{done}] {}", fixture.test_name));
                        }
                        Ok(Entry {
                            cost: execution.cost,
                            peak_heap_bytes: execution.peak_heap_bytes,
                            metadata: fixture.metadata,
                        })
                    }
                    Err(error) => {
                        report(format!("[{done}] {}: {error:#}", fixture.test_name));
                        Err(Failure {
                            reason: format!("{error:#}"),
                            metadata: fixture.metadata,
                        })
                    }
                };
                (fixture.test_name, outcome)
            })
            .collect();
        drop(silenced);

        // A block the guest did not get through is carried alongside the ones it did, so a profile
        // short of its corpus says which blocks it is short of and why.
        let mut entries = BTreeMap::new();
        let mut failures = BTreeMap::new();
        for (test_name, outcome) in outcomes {
            match outcome {
                Ok(entry) => entries.insert(test_name, entry).map(|_| ()),
                Err(failure) => failures.insert(test_name, failure).map(|_| ()),
            };
        }

        if entries.is_empty() && failures.is_empty() {
            match &self.filter {
                Some(filter) => bail!("no block in the corpus is named like {filter}"),
                None => bail!("the corpus holds no block"),
            }
        }
        let profiled = entries.len();
        if profiled == 0 {
            bail!("every block failed to profile");
        }
        if !failures.is_empty() {
            eprintln!(
                "profiled {profiled} of {} blocks",
                profiled + failures.len()
            );
        }

        let profile = Profile {
            profile: entries,
            failures,
            meta: Meta {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                zkvm: self.zkvm,
                zkvm_version: self.zkvm.sdk_version().to_owned(),
                stateless_validator: self.stateless_validator,
                stateless_validator_version: stateless_validator_version.to_owned(),
                elf_url,
                elf_sha256: Some(elf_sha256),
                suite,
                generated_at: now(),
                run_url: run_url(),
                composition: zkvm::composition(self.zkvm)?
                    .components
                    .iter()
                    .map(Into::into)
                    .collect(),
            },
        };
        write(&output, &profile)?;
        eprintln!("wrote {profiled} blocks to {}", output.display());
        Ok(())
    }

    /// The corpus a profile is of, which names the directory holding it unless it is stated.
    fn suite(&self) -> Result<String> {
        if let Some(suite) = &self.suite {
            return Ok(suite.clone());
        }
        let input = self
            .input
            .as_ref()
            .expect("clap requires a suite wherever there is no input to name one");
        Ok(input
            .file_name()
            .with_context(|| format!("{} names no suite", input.display()))?
            .to_string_lossy()
            .into_owned())
    }

    /// Where a profile is published, which is the guest, the ELF and the corpus it covers.
    ///
    /// The zkVM is left out because an ELF is built for one, so two zkVMs never share a hash. The
    /// harness the run was measured by is recorded in the profile rather than named here, so a
    /// release supersedes the run it re-measures instead of filing a second one beside it.
    fn output(&self, elf_sha256: &str, suite: &str) -> PathBuf {
        self.output_dir
            .join(&self.stateless_validator)
            .join(&elf_sha256[..ELF_SHA256_PREFIX])
            .join(format!("{suite}.json"))
    }
}

/// Whether `path` already carries this run, which is this ELF profiled by this harness on this zkVM
/// version.
///
/// The path pins the guest, the ELF and the corpus, so what is left to check is the harness that
/// measured it, the SDK that harness linked, the full ELF hash it was filed under, where that ELF
/// came from and which version of the guest it is. A profile of another zkVM at this path would mean
/// two zkVMs shared an ELF hash. The path is laid out on the assumption that cannot happen, so it is
/// an error rather than a run to replace.
fn published(path: &Path, run: Published) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read the published {}", path.display()))?;
    let document: Document = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse the published {}", path.display()))?;
    ensure!(
        document.meta.zkvm == run.zkvm,
        "{} carries a {} profile, not the {} one this run writes",
        path.display(),
        document.meta.zkvm,
        run.zkvm
    );
    Ok(document.meta.version == run.version
        && document.meta.zkvm_version == run.zkvm_version
        && document.meta.elf_sha256.as_deref() == Some(run.elf_sha256)
        && document.meta.elf_url.as_deref() == run.elf_url
        && document.meta.stateless_validator_version == run.stateless_validator_version)
}

/// Writes a profile, creating the directories its path names.
fn write(path: &Path, profile: &Profile) -> Result<()> {
    let parent = path.parent().expect("the path names a file in a directory");
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    // Flushed by hand, since a buffer flushed on drop reports a short write nowhere and the profile
    // would be truncated under an exit code of zero.
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, profile)?;
    writer
        .flush()
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Execution, Profiler, Result, profile_block};

    struct Panicking;

    impl Profiler for Panicking {
        fn profile(&self, _: &[u8]) -> Result<Execution> {
            panic!("the guest hit an unaligned operand")
        }
    }

    /// One block that aborts the emulator must not discard the corpus around it.
    #[test]
    fn a_panicking_backend_becomes_an_error() {
        let error = profile_block(&Panicking, &[]).unwrap_err();
        assert!(error.to_string().contains("unaligned operand"));
    }
}
