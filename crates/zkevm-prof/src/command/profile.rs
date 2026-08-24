//! The `profile` command.
//!
//! Resolves the guest ELF through the registry, then runs it over a fixture corpus on the chosen
//! zkVM.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{BufWriter, Write, stderr},
    os::fd::AsFd,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{
    Parser,
    builder::{PossibleValuesParser, TypedValueParser},
};
use gag::Gag;
use rayon::prelude::*;
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zkvm_prof::{composition, profiler, zkVMKind};

use crate::{
    command::{now, run_url},
    fixture,
    profile::{Entry, Failure, Meta, Profile},
    registry,
};

/// Digits of the ELF hash a published profile is filed under.
const ELF_SHA256_PREFIX: usize = 16;

/// How long a run goes before writing its checkpoint again, which is what an interrupted run loses.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);

/// A published profile read for its meta alone, which is what says whether it is this one.
#[derive(Deserialize)]
struct Document {
    meta: Meta,
}

/// One block as a worker hands it over, either what it cost or why the guest did not get through it.
type Measured = (String, Result<Entry, Failure>);

/// What a published profile has to record for this run to be the one already in it.
#[derive(Clone, Copy)]
struct Published<'a> {
    version: &'a str,
    zkvm: zkVMKind,
    zkvm_version: &'a str,
    elf_url: Option<&'a str>,
    elf_sha256: &'a str,
    stateless_validator_version: &'a str,
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
    /// skipped from then on, so measuring it again takes saying so. A forced run continues no
    /// checkpoint either, and measures the whole corpus again.
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

        // A run continues its own checkpoint, and a failure recorded in one is carried forward
        // rather than retried, the guest being deterministic and a block that aborted the emulator
        // aborting it again. A forced run measures the corpus again rather than continuing one
        // measured before it, so it starts from nothing and its first checkpoint replaces whatever
        // sits there.
        let checkpoint = checkpoint(&output);
        let (entries, failures) = match self.force {
            true => (BTreeMap::new(), BTreeMap::new()),
            false => match resumed(&checkpoint, already)? {
                Some(profile) => (profile.profile, profile.failures),
                None => (BTreeMap::new(), BTreeMap::new()),
            },
        };
        // Copied out before the maps move to the thread that owns them, so a worker skips a block
        // already recorded without asking that thread about it.
        let recorded: HashSet<String> = entries.keys().chain(failures.keys()).cloned().collect();
        if !recorded.is_empty() {
            eprintln!(
                "resuming from the {} blocks already recorded in {}",
                recorded.len(),
                checkpoint.display()
            );
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
        let offered = AtomicUsize::new(0);
        // Blocks this run starts from, which is what it has to add to for it to have reached one.
        let inherited = entries.len();
        // A block the guest did not get through is carried alongside the ones it did, so a profile
        // short of its corpus says which blocks it is short of and why.
        let carried = Profile {
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
                composition: composition(self.zkvm)?.iter().map(Into::into).collect(),
            },
        };
        let (sender, receiver) = mpsc::channel();
        // One thread owns the profile and the file, so a worker hands a block over and goes back to
        // profiling rather than waiting on a serialization of the whole corpus. A write that fails
        // stops that thread, which closes the channel, which is what stops the workers, and the
        // error it stopped on is the whole of what there is to report.
        let mut profile = thread::scope(|scope| {
            let writer = scope.spawn(|| gather(receiver, &checkpoint, carried));
            // A send fails only once the writer has stopped, and the writer states why it stopped,
            // so what the run comes to is read off the writer rather than off a send that could not
            // land.
            let _ = paths
                .par_iter()
                .flat_map(|path| match fixture::load(path) {
                    Ok(fixtures) => {
                        let named = fixtures
                            .into_iter()
                            .filter(|fixture| {
                                filter
                                    .as_ref()
                                    .is_none_or(|filter| filter.is_match(&fixture.test_name))
                            })
                            .collect::<Vec<_>>();
                        // Counted before the recorded blocks are dropped, so a corpus this run
                        // covered without profiling anything new reads apart from one offering no
                        // block at all.
                        offered.fetch_add(named.len(), Ordering::Relaxed);
                        named
                            .into_iter()
                            .filter(|fixture| !recorded.contains(&fixture.test_name))
                            .collect()
                    }
                    Err(error) => {
                        report(format!("{error:#}"));
                        Vec::new()
                    }
                })
                .try_for_each(|fixture| {
                    let result = profiler.profile(&fixture.stateless_input);
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
                    sender.send((fixture.test_name, outcome))
                });
            // Dropped so that the writer, which ends on a channel no sender is left on, ends.
            drop(sender);
            writer
                .join()
                .map_err(|_| anyhow!("the checkpoint writer panicked"))?
        })?;
        drop(silenced);

        if offered.load(Ordering::Relaxed) == 0 {
            match &self.filter {
                Some(filter) => bail!("no block in the corpus is named like {filter}"),
                None => bail!("the corpus holds no block"),
            }
        }
        let profiled = profile.profile.len();
        if profiled == 0 {
            // A document of failures alone is nothing to publish and nothing to continue, and left
            // there it would have every run after this one resume those failures and reach nothing.
            let _ = fs::remove_file(&checkpoint);
            bail!("every block failed to profile");
        }
        ensure!(
            done.load(Ordering::Relaxed) == 0 || profiled > inherited,
            "every block this run reached failed, so the {inherited} it inherited stay unpublished"
        );
        if !profile.failures.is_empty() {
            eprintln!(
                "profiled {profiled} of {} blocks",
                profiled + profile.failures.len()
            );
        }

        // A run ends by moving its checkpoint onto the published path, so nothing is published until
        // there is a whole run to publish, and the stamp is the run that finished it rather than the
        // one that started it.
        profile.meta.generated_at = now();
        write(&checkpoint, &profile)?;
        fs::rename(&checkpoint, &output).with_context(|| {
            format!(
                "failed to move {} to {}",
                checkpoint.display(),
                output.display()
            )
        })?;
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
    Ok(records(&document.meta, run))
}

/// Gathers what the workers measured into `profile` and writes it to `path` as the run goes,
/// returning it once the last worker has handed its block over.
///
/// The blocks are taken as they land and written no more often than [`CHECKPOINT_INTERVAL`], since a
/// checkpoint is the whole profile and writing one per block would serialize a corpus of thousands
/// of blocks thousands of times over. Waiting no longer than the interval for a block bounds how
/// long the ones already in hand go unwritten, which is what a corpus ending on a block of forty
/// minutes would otherwise leave to that block. The last of them are written as the channel closes,
/// since a worker that panics ends the run by an unwind no line after it is reached from.
fn gather(blocks: Receiver<Measured>, path: &Path, mut profile: Profile) -> Result<Profile> {
    let mut written = Instant::now();
    let mut pending = false;
    loop {
        match blocks.recv_timeout(CHECKPOINT_INTERVAL) {
            Ok((test_name, Ok(entry))) => {
                profile.profile.insert(test_name, entry);
                pending = true;
            }
            Ok((test_name, Err(failure))) => {
                profile.failures.insert(test_name, failure);
                pending = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if pending && written.elapsed() >= CHECKPOINT_INTERVAL {
            write(path, &profile)?;
            written = Instant::now();
            pending = false;
        }
    }
    if pending {
        write(path, &profile)?;
    }
    Ok(profile)
}

/// Where a run keeps the profile it is still writing, beside the one it publishes.
///
/// A checkpoint is what an interrupted run leaves and what the next one continues, so it sits off
/// the published path rather than on it and the profile a forced run is measuring over stays whole
/// until there is a whole run to replace it with. It carries no `.json` extension either, which is
/// what keeps a corpus half profiled out of the index and out of a report.
fn checkpoint(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .expect("the path names a file in a directory");
    output.with_file_name(format!(".{}.checkpoint", name.to_string_lossy()))
}

/// The checkpoint at `path`, absent where none sits there or where another run wrote it, whose
/// blocks this run's first checkpoint then replaces.
///
/// One that cannot be read reads as absent too, and is reported rather than raised. A checkpoint is
/// blocks a run need not measure again rather than blocks it depends on, so a corpus profiled from
/// the start is always an answer, where a run that refused to start would leave a machine killed
/// mid-write with nothing to do but find the file and delete it.
fn resumed(path: &Path, run: Published) -> Result<Option<Profile>> {
    if !path.exists() {
        return Ok(None);
    }
    let held = fs::read_to_string(path)
        .map_err(|error| anyhow!("{error}"))
        .and_then(|text| Ok(serde_json::from_str::<Profile>(&text)?));
    match held {
        Ok(profile) => Ok(records(&profile.meta, run).then_some(profile)),
        Err(error) => {
            eprintln!(
                "profiling from the start, the checkpoint {} does not read: {error:#}",
                path.display()
            );
            Ok(None)
        }
    }
}

/// Whether `meta` is the record of `run`, which is what makes a profile one this run continues
/// rather than one it replaces.
fn records(meta: &Meta, run: Published) -> bool {
    meta.version == run.version
        && meta.zkvm_version == run.zkvm_version
        && meta.elf_sha256.as_deref() == Some(run.elf_sha256)
        && meta.elf_url.as_deref() == run.elf_url
        && meta.stateless_validator_version == run.stateless_validator_version
}

/// Writes a profile, creating the directories its path names.
///
/// Written beside the target and renamed onto it, so a run killed mid-write leaves the checkpoint
/// before it whole rather than a truncated file the next run reads instead.
fn write(path: &Path, profile: &Profile) -> Result<()> {
    let parent = path.parent().expect("the path names a file in a directory");
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let mut named = path.as_os_str().to_owned();
    named.push(".partial");
    let partial = PathBuf::from(named);
    let file = File::create(&partial)
        .with_context(|| format!("failed to create {}", partial.display()))?;
    // Flushed by hand, since a buffer flushed on drop reports a short write nowhere and the profile
    // would be truncated under an exit code of zero.
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, profile)?;
    writer
        .flush()
        .with_context(|| format!("failed to write {}", partial.display()))?;
    fs::rename(&partial, path)
        .with_context(|| format!("failed to move {} to {}", partial.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use zkvm_prof::zkVMKind;

    use crate::{
        command::profile::{Published, checkpoint, published, resumed, write},
        profile::Profile,
    };

    /// A directory to write a profile into, since what a path offers a run is read off the file
    /// sitting at it rather than worked out from the name.
    fn directory(name: &str) -> PathBuf {
        let root = env::temp_dir().join(name);
        // Removed first as well as last, so a run left short by a failing assertion still starts
        // from an empty tree.
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// A profile of the harness version and ELF named, carrying one block.
    fn document(version: &str, elf_sha256: &str) -> String {
        format!(
            r#"{{"profile":{{"block_1":{{"cost":{{"total":7}},
            "metadata":{{"gas_used":21000,"block_number":1}}}}}},
            "meta":{{"version":"{version}","zkvm":"zisk","zkvm_version":"v1.1.0-alpha",
            "stateless_validator":"reth","stateless_validator_version":"f804dc1",
            "elf_sha256":"{elf_sha256}"}}}}"#
        )
    }

    /// A profile of this run, which is what a checkpoint has to be for this run to continue it.
    fn ours(elf_sha256: &str) -> String {
        document(env!("CARGO_PKG_VERSION"), elf_sha256)
    }

    fn run() -> Published<'static> {
        Published {
            version: env!("CARGO_PKG_VERSION"),
            zkvm: zkVMKind::Zisk,
            zkvm_version: "v1.1.0-alpha",
            elf_url: None,
            elf_sha256: "37c2fecb7645bb92",
            stateless_validator_version: "f804dc1",
        }
    }

    /// A run continues the checkpoint of its own build measured by its own harness and no other,
    /// since the blocks of either say nothing about this run however far the one that wrote them
    /// got. One that does not read reads as absent too, a corpus profiled from the start being an
    /// answer where a run refusing to start is not.
    #[test]
    fn a_checkpoint_is_continued_only_by_the_run_that_wrote_it() {
        let root = directory("zkevm-prof-continues-its-own-checkpoint");
        let path = checkpoint(&root.join("mainnet-25580000-1000.json"));
        assert!(resumed(&path, run()).unwrap().is_none());

        fs::write(&path, ours("37c2fecb7645bb92")).unwrap();
        let profile = resumed(&path, run()).unwrap().unwrap();
        assert!(profile.profile.contains_key("block_1"));

        fs::write(&path, ours("0e5355489a37a840")).unwrap();
        assert!(resumed(&path, run()).unwrap().is_none());

        fs::write(&path, document("0.0.0", "37c2fecb7645bb92")).unwrap();
        assert!(resumed(&path, run()).unwrap().is_none());

        fs::write(&path, "{\"profile\":{").unwrap();
        assert!(resumed(&path, run()).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    /// A checkpoint sits off the published path and carries no `.json`, so an interrupted run
    /// leaves the profile it was measuring over whole and the index none the wiser.
    #[test]
    fn a_checkpoint_is_no_published_profile() {
        let output = PathBuf::from("profiles/reth/37c2fecb7645bb92/mainnet-25580000-1000.json");
        let path = checkpoint(&output);
        assert_eq!(path.parent(), output.parent());
        assert_ne!(path, output);
        assert_ne!(path.extension().unwrap(), "json");
    }

    /// A published profile of this run is skipped and one of another build is not, which is what
    /// keeps a rerun over an unchanged guest from measuring it again.
    #[test]
    fn a_published_profile_is_read_for_the_build_it_measured() {
        let root = directory("zkevm-prof-reads-the-published-profile");
        let path = root.join("mainnet-25580000-1000.json");
        assert!(!published(&path, run()).unwrap());

        fs::write(&path, ours("37c2fecb7645bb92")).unwrap();
        assert!(published(&path, run()).unwrap());

        fs::write(&path, ours("0e5355489a37a840")).unwrap();
        assert!(!published(&path, run()).unwrap());

        // The harness stands for everything a cost depends on that the rest of the meta does not
        // name, so a run of another one is measured again rather than skipped.
        fs::write(&path, document("0.0.0", "37c2fecb7645bb92")).unwrap();
        assert!(!published(&path, run()).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    /// A checkpoint lands whole or not at all, which is what leaves an interrupted run a file the
    /// next one can read.
    #[test]
    fn a_written_profile_leaves_nothing_beside_it() {
        let root = directory("zkevm-prof-writes-a-whole-profile");
        let path = root.join("reth/37c2fecb7645bb92/mainnet-25580000-1000.json");
        let profile: Profile = serde_json::from_str(&ours("37c2fecb7645bb92")).unwrap();
        write(&path, &profile).unwrap();

        let written: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(written, ["mainnet-25580000-1000.json"]);
        let read: Profile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(read.profile.contains_key("block_1"));
        let _ = fs::remove_dir_all(&root);
    }
}
