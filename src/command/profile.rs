//! The `profile` command.
//!
//! Resolves the guest ELF through the registry, then runs it over a fixture corpus on the chosen
//! zkVM.

use std::{
    fs::{self, File},
    io::{BufWriter, Write, stderr},
    os::fd::AsFd,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::{Context, Result, bail};
use clap::{
    Parser,
    builder::{PossibleValuesParser, TypedValueParser},
};
use ere_catalog::zkVMKind;
use gag::Gag;
use rayon::prelude::*;

use crate::{
    fixture, registry,
    zkvm::{Entry, Execution, Meta, Profile, Profiler, profiler},
};

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
    #[arg(long)]
    input: PathBuf,

    /// Path to write the profile JSON to.
    #[arg(long)]
    output: PathBuf,

    /// Guest ELF to profile, in place of the downloaded one.
    ///
    /// A guest built against an SDK other than the one this crate links does not load, so profiling
    /// one that no ere-guests build carries means handing the ELF over directly.
    #[arg(long)]
    elf: Option<PathBuf>,
}

impl ProfileCmd {
    pub async fn run(self) -> Result<()> {
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
        let profiler = profiler(self.zkvm, &elf)?;

        let paths = fixture::find(&self.input)?;
        eprintln!(
            "profiling {} on {} over {} fixtures",
            self.stateless_validator,
            self.zkvm,
            paths.len()
        );

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
        let entries: Vec<(String, Entry)> = paths
            .par_iter()
            .flat_map(|path| match fixture::load(path) {
                Ok(fixtures) => fixtures,
                Err(error) => {
                    report(format!("{error:#}"));
                    Vec::new()
                }
            })
            .filter_map(|fixture| {
                let result = profile_block(profiler.as_ref(), &fixture.stateless_input);
                // Counts blocks rather than files, since a fixture file may hold several tests.
                let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                match result {
                    Ok(execution) => {
                        if done.is_multiple_of(25) {
                            report(format!("[{done}] {}", fixture.test_name));
                        }
                        Some((
                            fixture.test_name,
                            Entry {
                                cost: execution.cost,
                                peak_heap_bytes: execution.peak_heap_bytes,
                                peak_stack_bytes: execution.peak_stack_bytes,
                                metadata: fixture.metadata,
                            },
                        ))
                    }
                    Err(error) => {
                        report(format!("[{done}] {}: {error:#}", fixture.test_name));
                        None
                    }
                }
            })
            .collect();
        drop(silenced);

        let attempted = done.into_inner();
        let profiled = entries.len();
        if profiled == 0 {
            bail!("every block failed to profile");
        }
        if profiled < attempted {
            eprintln!(
                "profiled {profiled} of {attempted} blocks, {} failed",
                attempted - profiled
            );
        }

        let profile = Profile {
            profile: entries.into_iter().collect(),
            meta: Meta {
                zkvm: self.zkvm,
                zkvm_version: self.zkvm.sdk_version().to_owned(),
                stateless_validator: self.stateless_validator,
                stateless_validator_version: stateless_validator_version.to_owned(),
                elf_url,
            },
        };
        let file = File::create(&self.output)
            .with_context(|| format!("failed to create {}", self.output.display()))?;
        serde_json::to_writer_pretty(BufWriter::new(file), &profile)?;
        eprintln!("wrote {profiled} profiles to {}", self.output.display());
        Ok(())
    }
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
