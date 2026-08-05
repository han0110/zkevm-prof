//! The `profile` command.
//!
//! Resolves the guest ELF, then runs it over a fixture corpus on the chosen zkVM. Guests built by
//! ere-guests are fetched from the artifacts of one commit's build; Nethermind is not part of that
//! catalog, so its guest is published from a Nethermind fork and fetched from there instead.

use std::{
    env,
    fs::{self, File},
    io::BufWriter,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{
    Parser, ValueEnum,
    builder::{PossibleValuesParser, TypedValueParser},
};
use ere_catalog::zkVMKind;
use gag::Gag;
use rayon::prelude::*;
use stateless_validator_catalog::StatelessValidatorKind;
use stateless_validator_downloader::Downloader;

use crate::{
    fixture,
    zkvm::{Cost, Entry, Meta, Profile, Profiler, profiler},
};

/// ere-guests commit the profiled guests are built by.
///
/// Guests come from the build artifacts of this commit rather than from a release, since no release
/// yet carries the OpenVM `v2.1.0-preview` guests this crate is pinned against.
const ERE_GUESTS_COMMIT: &str = "c0e111032878843b496715d4b4903c7cd0ad2043";

/// Environment variable holding the token the artifact download authenticates with.
///
/// The GitHub artifact API rejects anonymous reads, so unlike a release asset an artifact cannot be
/// fetched without a token.
const GITHUB_TOKEN: &str = "GITHUB_TOKEN";

/// Release the Nethermind guest is fetched from, which is also the version it is known by.
///
/// Nethermind has no ere-guests release, so its guest is published from a fork, built by the same
/// `make build` recipe the Nethermind release workflow runs. Assets follow the ere-guests naming so
/// a fork-released guest and a catalog one resolve alike.
const NETHERMIND_TAG: &str = "glamsterdam-devnet-7";

/// Stateless validator whose guest is profiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum StatelessValidator {
    Ethrex,
    Nethermind,
    Reth,
    Zesu,
}

impl StatelessValidator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ethrex => "ethrex",
            Self::Nethermind => "nethermind",
            Self::Reth => "reth",
            Self::Zesu => "zesu",
        }
    }

    /// Version of the guest, which the report shows beside its cost.
    ///
    /// Nethermind is outside the ere-guests catalog, so it is versioned by the fork release it is
    /// published under rather than by a catalog entry.
    pub fn version(self) -> &'static str {
        match StatelessValidatorKind::try_from(self) {
            Ok(kind) => kind.version(),
            Err(_) => NETHERMIND_TAG,
        }
    }
}

impl TryFrom<StatelessValidator> for StatelessValidatorKind {
    type Error = anyhow::Error;

    fn try_from(validator: StatelessValidator) -> Result<Self> {
        match validator {
            StatelessValidator::Ethrex => Ok(Self::Ethrex),
            StatelessValidator::Reth => Ok(Self::Reth),
            StatelessValidator::Zesu => Ok(Self::Zesu),
            StatelessValidator::Nethermind => Err(anyhow!(
                "{} is not in the ere-guests catalog",
                validator.as_str()
            )),
        }
    }
}

/// Returns the ELF of `stateless_validator` compiled for `zkvm`.
pub async fn elf(stateless_validator: StatelessValidator, zkvm: zkVMKind) -> Result<Vec<u8>> {
    match stateless_validator {
        StatelessValidator::Nethermind => {
            download(&format!(
                "https://github.com/han0110/nethermind/releases/download/{NETHERMIND_TAG}\
                 /stateless-validator-{}-{zkvm}-{}.elf",
                stateless_validator.as_str(),
                zkvm.sdk_version()
            ))
            .await
        }
        _ => {
            let github_token = env::var(GITHUB_TOKEN).with_context(|| {
                format!("{GITHUB_TOKEN} must hold a token that can read ere-guests build artifacts")
            })?;
            Ok(Downloader::from_commit(ERE_GUESTS_COMMIT, &github_token)
                .await?
                .download(stateless_validator.try_into()?, zkvm)
                .await?
                .elf)
        }
    }
}

/// Profiles one block, turning a panic inside the backend into an error.
///
/// A guest that breaks a zkVM invariant aborts the emulator by panicking rather than by returning,
/// and rayon re-raises a worker's panic on the thread collecting the results, which would discard a
/// whole corpus over one bad block. Backends carry no state from one block to the next, so a caught
/// panic leaves nothing inconsistent behind.
fn profile_block(profiler: &dyn Profiler, stateless_input: &[u8]) -> Result<Cost> {
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

async fn download(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("failed to request {url}"))?;
    let status = response.status();
    ensure!(status.is_success(), "{url} returned {status}");
    Ok(response.bytes().await?.to_vec())
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
        value_parser = PossibleValuesParser::new(["openvm", "zisk"])
            .map(|value| zkVMKind::from_str(&value).expect("the value came from the list above"))
    )]
    zkvm: zkVMKind,

    /// Stateless validator whose guest is profiled.
    #[arg(long, value_enum)]
    stateless_validator: StatelessValidator,

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
        let elf = match &self.elf {
            Some(path) => {
                fs::read(path).with_context(|| format!("failed to read {}", path.display()))?
            }
            None => elf(self.stateless_validator, self.zkvm)
                .await
                .with_context(|| {
                    format!(
                        "failed to resolve the {} guest for {}",
                        self.stateless_validator.as_str(),
                        self.zkvm
                    )
                })?,
        };
        let profiler = profiler(self.zkvm, &elf)?;

        let paths = fixture::find(&self.input)?;
        eprintln!(
            "profiling {} on {} over {} fixtures",
            self.stateless_validator.as_str(),
            self.zkvm,
            paths.len()
        );

        // Backends chatter on stdout while they run; the ZisK emulator prints its whole report on
        // every block. Progress goes to stderr, so dropping stdout for the run costs nothing.
        let silenced = Gag::stdout()?;
        let done = AtomicUsize::new(0);
        let entries: Vec<(String, Entry)> = paths
            .par_iter()
            .flat_map(|path| match fixture::load(path) {
                Ok(fixtures) => fixtures,
                Err(error) => {
                    eprintln!("{error:#}");
                    Vec::new()
                }
            })
            .filter_map(|fixture| {
                let result = profile_block(profiler.as_ref(), &fixture.stateless_input);
                // Counts blocks rather than files, since a fixture file may hold several tests.
                let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                match result {
                    Ok(cost) => {
                        if done.is_multiple_of(25) {
                            eprintln!("[{done}] {}", fixture.test_name);
                        }
                        Some((
                            fixture.test_name,
                            Entry {
                                cost,
                                metadata: fixture.metadata,
                            },
                        ))
                    }
                    Err(error) => {
                        eprintln!("[{done}] {}: {error:#}", fixture.test_name);
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
                guest: self.stateless_validator.as_str().to_owned(),
                guest_version: self.stateless_validator.version().to_owned(),
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
    use super::{Cost, Profiler, Result, profile_block};

    struct Panicking;

    impl Profiler for Panicking {
        fn profile(&self, _: &[u8]) -> Result<Cost> {
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
