//! The `index` command.
//!
//! A static host serves no directory listing, so the page reads the runs it can load from an index
//! written beside them. The index carries what a listing draws from, which is what lets the runs
//! list and the suites list draw without loading a profile.

use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use ere_catalog::zkVMKind;
use serde::{Deserialize, Serialize};
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

/// A published profile read for its meta alone.
#[derive(Deserialize)]
struct Document {
    meta: Meta,
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
}

impl<'a> Run<'a> {
    fn new(meta: &'a Meta) -> Self {
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
        }
    }
}

#[derive(Serialize)]
struct Index<'a> {
    runs: Vec<Run<'a>>,
}

impl IndexCmd {
    pub fn run(self) -> Result<()> {
        let mut published: Vec<(String, Document)> = profiles(&self.dir)?
            .iter()
            .map(|path| {
                let text = fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let profile = serde_json::from_str(&text)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok((identifier(&self.dir, path), profile))
            })
            .collect::<Result<Vec<_>>>()?;
        // Newest first, which is the order the page lists runs in.
        published.sort_by(|left, right| {
            right
                .1
                .meta
                .generated_at
                .cmp(&left.1.meta.generated_at)
                .then_with(|| left.0.cmp(&right.0))
        });

        let index = Index {
            runs: published
                .iter()
                .map(|(_, profile)| Run::new(&profile.meta))
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

/// Every profile under `dir`, which is every JSON file in it but the index itself.
///
/// A subtree that cannot be walked is an error rather than a gap, since an index quietly missing the
/// runs under it reads exactly like one whose runs were never published.
fn profiles(dir: &Path) -> Result<Vec<PathBuf>> {
    // The index itself is the one path excluded, rather than the name it carries, so a corpus called
    // index keeps its profile listed.
    let index = dir.join(INDEX);
    WalkDir::new(dir)
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
        .collect()
}

/// A run's identifier, which is where it sits under the directory the page fetches from.
fn identifier(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .expect("the path was walked from the directory")
        .with_extension("")
        .to_string_lossy()
        .into_owned()
}
