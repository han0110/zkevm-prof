//! The `index` command.
//!
//! A static host serves no directory listing, so the page reads the runs it can load from an index
//! written beside them. The index carries what a listing draws from, which is what lets the runs
//! list and the suites list draw without loading a profile.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
use ere_catalog::zkVMKind;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use walkdir::WalkDir;

use crate::zkvm::{Component, Meta};

/// File the index is written to, which is the one file under the directory that is not a profile.
const INDEX: &str = "index.json";

/// Lists every published profile under a directory.
#[derive(Parser)]
pub struct IndexCmd {
    /// Directory the profiles are published under.
    #[arg(long, default_value = "profiles")]
    dir: PathBuf,
}

/// A published profile read for the blocks it profiled and its meta.
#[derive(Deserialize)]
struct Document {
    /// Read for its keys alone, which are what a proving time dataset is checked against.
    profile: BTreeMap<String, IgnoredAny>,
    meta: Meta,
}

/// A published proving time dataset, one wall clock time in milliseconds per block proved.
#[derive(Deserialize)]
struct Proving {
    proving_time_ms: BTreeMap<String, f64>,
    meta: ProvingMeta,
}

/// What produced a set of proving times, which is the machine they were proved on.
#[derive(Deserialize, Serialize)]
struct ProvingMeta {
    /// What the dataset is called wherever the page offers it.
    name: String,
    /// Machines the proving ran across, every one of them the hardware below.
    machines: u32,
    hardware: Hardware,
}

/// One machine of the set that proved a dataset.
#[derive(Deserialize, Serialize)]
struct Hardware {
    cpu: String,
    ram_bytes: u64,
    os: String,
    /// Empty on a machine that proves on its CPU alone.
    #[serde(default)]
    gpus: Vec<String>,
}

/// A proving time dataset as it was read, which is the run it was proved over and what it holds.
struct Published {
    /// Profile it states the times of, which is the file its directory is named after.
    profile: PathBuf,
    /// File it sits at, without its extension, which is what the page fetches by.
    file: String,
    document: Proving,
}

/// One run as a listing reads it.
///
/// Every field is the profile's own, the publish path being composed of three of them, so the page
/// fetches by what it already holds rather than by a field carrying the path again. The workflow run
/// behind a profile is left to the profile, no listing being drawn from it.
#[derive(Serialize)]
struct Run<'a> {
    version: &'a str,
    zkvm: &'a zkVMKind,
    zkvm_version: &'a str,
    stateless_validator: &'a str,
    stateless_validator_version: &'a str,
    elf_url: Option<&'a str>,
    elf_sha256: Option<&'a str>,
    suite: &'a str,
    generated_at: u64,
    composition: &'a [Component],
    /// Proving time datasets published under the profile, empty where none were added.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    proving: Vec<Dataset<'a>>,
}

/// One proving time dataset as a listing reads it. The times are left to the file, so the page names
/// a dataset and states the machine behind it before fetching any of them.
#[derive(Serialize)]
struct Dataset<'a> {
    file: &'a str,
    meta: &'a ProvingMeta,
}

impl<'a> Run<'a> {
    fn new(meta: &'a Meta, proving: Vec<Dataset<'a>>) -> Self {
        Self {
            version: &meta.version,
            zkvm: &meta.zkvm,
            zkvm_version: &meta.zkvm_version,
            stateless_validator: &meta.stateless_validator,
            stateless_validator_version: &meta.stateless_validator_version,
            elf_url: meta.elf_url.as_deref(),
            elf_sha256: meta.elf_sha256.as_deref(),
            suite: &meta.suite,
            generated_at: meta.generated_at,
            composition: &meta.composition,
            proving,
        }
    }
}

#[derive(Serialize)]
struct Index<'a> {
    runs: Vec<Run<'a>>,
}

impl IndexCmd {
    pub fn run(self) -> Result<()> {
        let (profiles, provings) = published(&self.dir)?;
        let datasets = provings
            .iter()
            .map(|path| {
                Ok(Published {
                    profile: proved_over(path).expect("the path was walked as a dataset"),
                    file: stem(path),
                    document: read(path)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut runs: Vec<(String, &PathBuf, Document)> = profiles
            .iter()
            .map(|path| Ok((identifier(&self.dir, path), path, read(path)?)))
            .collect::<Result<Vec<_>>>()?;
        // Newest first, which is the order the page lists runs in.
        runs.sort_by(|left, right| {
            right
                .2
                .meta
                .generated_at
                .cmp(&left.2.meta.generated_at)
                .then_with(|| left.0.cmp(&right.0))
        });

        // The directory a dataset sits in names the run it was proved over, so times that name none
        // of that run's blocks are times filed under the wrong corpus rather than times to list.
        for (dataset, path) in datasets.iter().zip(&provings) {
            let (_, _, run) = runs
                .iter()
                .find(|(_, profile, _)| **profile == dataset.profile)
                .expect("the dataset was paired with a profile that was walked");
            ensure!(
                dataset
                    .document
                    .proving_time_ms
                    .keys()
                    .any(|block| run.profile.contains_key(block)),
                "the times in {} name no block {} profiled",
                path.display(),
                dataset.profile.display()
            );
        }

        let index = Index {
            runs: runs
                .iter()
                .map(|(_, profile, document)| {
                    let proving = datasets
                        .iter()
                        .filter(|dataset| dataset.profile == **profile)
                        .map(|dataset| Dataset {
                            file: &dataset.file,
                            meta: &dataset.document.meta,
                        })
                        .collect();
                    Run::new(&document.meta, proving)
                })
                .collect(),
        };
        let path = self.dir.join(INDEX);
        let file =
            File::create(&path).with_context(|| format!("failed to create {}", path.display()))?;
        // Flushed by hand, since a buffer flushed on drop reports a short write nowhere and the
        // index would be truncated under an exit code of zero.
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &index)?;
        writer
            .flush()
            .with_context(|| format!("failed to write {}", path.display()))?;
        eprintln!("indexed {} runs into {}", index.runs.len(), path.display());
        Ok(())
    }
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

/// Every JSON file under `dir` but the index itself, split into the profiles and the proving time
/// datasets published under them.
///
/// A subtree that cannot be walked is an error rather than a gap, since an index quietly missing the
/// runs under it reads exactly like one whose runs were never published.
fn published(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    // The index itself is the one path excluded, rather than the name it carries, so a corpus called
    // index keeps its profile listed.
    let index = dir.join(INDEX);
    let paths = WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|entry| match entry {
            Err(error) => Some(Err(anyhow!("failed to walk {}: {error}", dir.display()))),
            Ok(entry) => {
                let path = entry.into_path();
                let json = path
                    .extension()
                    .is_some_and(|extension| extension == "json");
                (json && path != index).then_some(Ok(path))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(paths
        .into_iter()
        .partition(|path| proved_over(path).is_none()))
}

/// The profile a path states proving times for, which is the file its own directory is named after,
/// so `reth/<elf>/eest-v0.6.2-100m/ef-cluster-4x4.json` states the times of the run published at
/// `reth/<elf>/eest-v0.6.2-100m.json`. A path whose directory names no profile is a profile itself.
///
/// One directory per corpus, since a dataset is read against the costs of the corpus it was proved
/// over and against nothing else, and the path says which that is rather than leaving it to be
/// worked out from the blocks inside.
///
/// The suffix is appended rather than set as an extension, a corpus named `eest-v0.6.2-100m` holding
/// dots that setting one would cut the name back to.
fn proved_over(path: &Path) -> Option<PathBuf> {
    let mut named = path.parent()?.as_os_str().to_owned();
    named.push(".json");
    let profile = PathBuf::from(named);
    profile.is_file().then_some(profile)
}

/// A file's name without its extension, which is what the page fetches a dataset by.
fn stem(path: &Path) -> String {
    path.file_stem()
        .expect("the path was walked as a file")
        .to_string_lossy()
        .into_owned()
}

/// A run's identifier, which is where it sits under the directory the page fetches from.
fn identifier(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .expect("the path was walked from the directory")
        .with_extension("")
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use crate::command::index::proved_over;

    /// A layout to walk, since what tells a dataset from a profile is whether the directory it sits
    /// in names a profile beside it, which is a question about files rather than about names.
    fn tree(name: &str) -> std::path::PathBuf {
        let root = env::temp_dir().join(name);
        // Removed first as well as last, so a run left short by a failing assertion still starts
        // from an empty tree.
        let _ = fs::remove_dir_all(&root);
        let elf = root.join("reth/0e5355489a37a840");
        fs::create_dir_all(elf.join("eest-v0.6.2-100m")).unwrap();
        fs::create_dir_all(elf.join("proving")).unwrap();
        fs::write(elf.join("eest-v0.6.2-100m.json"), "{}").unwrap();
        fs::write(elf.join("eest-v0.6.2-100m/ef-cluster-4x4.json"), "{}").unwrap();
        fs::write(elf.join("proving/ef-cluster-4x4.json"), "{}").unwrap();
        elf
    }

    /// A dataset states the times of the profile its own directory is named after, which is what
    /// ties one corpus's times to that corpus's costs and to no other.
    #[test]
    fn a_dataset_names_the_profile_its_directory_is_named_after() {
        let elf = tree("zkevm-prof-dataset-names-its-profile");
        let profile = elf.join("eest-v0.6.2-100m.json");
        assert_eq!(
            proved_over(&elf.join("eest-v0.6.2-100m/ef-cluster-4x4.json")),
            Some(profile.clone())
        );
        // A profile's own directory names the ELF rather than a profile, so it states nobody's
        // times and is read as a profile itself.
        assert_eq!(proved_over(&profile), None);
        let _ = fs::remove_dir_all(elf.parent().unwrap().parent().unwrap());
    }

    /// A file under a directory naming no profile is no dataset, which is what keeps the layout from
    /// resting on a directory name a publisher could pick freely.
    #[test]
    fn a_file_under_a_directory_naming_no_profile_is_no_dataset() {
        let elf = tree("zkevm-prof-dataset-needs-a-profile");
        assert_eq!(proved_over(&elf.join("proving/ef-cluster-4x4.json")), None);
        let _ = fs::remove_dir_all(elf.parent().unwrap().parent().unwrap());
    }
}
