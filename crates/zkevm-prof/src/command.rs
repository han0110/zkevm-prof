//! CLI commands.

pub mod import;
pub mod index;
pub mod profile;

use std::{
    env,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// Reads a JSON document, naming the file it failed on rather than the offset alone.
pub fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

/// The `.json` files under `dir`, in name order, at `depth` where one is given.
///
/// A walk that cannot read a subtree is an error rather than a shorter listing, since a directory
/// half read reads exactly like one holding fewer files.
pub fn json_files(dir: &Path, depth: Option<usize>) -> Result<Vec<PathBuf>> {
    let mut walk = WalkDir::new(dir).sort_by_file_name();
    if let Some(depth) = depth {
        walk = walk.min_depth(depth).max_depth(depth);
    }
    walk.into_iter()
        .filter_map(|entry| match entry {
            Err(error) => Some(Err(anyhow!("failed to walk {}: {error}", dir.display()))),
            Ok(entry) => {
                let path = entry.into_path();
                let json = path
                    .extension()
                    .is_some_and(|extension| extension == "json");
                json.then_some(Ok(path))
            }
        })
        .collect()
}

/// Writes a JSON document, creating the directories its path names.
pub fn write<T: Serialize>(path: &Path, document: &T) -> Result<()> {
    let parent = path.parent().expect("the path names a file in a directory");
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    // Flushed by hand, since a buffer flushed on drop reports a short write nowhere and the document
    // would be truncated under an exit code of zero.
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, document)?;
    writer
        .flush()
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// An empty directory to write a document into, since what a path offers a run is read off the file
/// sitting at it rather than worked out from the name.
///
/// Removed first as well as last, so a run left short by a failing assertion still starts from an
/// empty tree.
#[cfg(test)]
pub fn directory(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Wall clock time a run is stamped with, in seconds since the epoch.
pub fn now() -> u64 {
    UNIX_EPOCH.elapsed().unwrap_or_default().as_secs()
}

/// Link to the workflow run that produced an output, when one produced it.
///
/// The variables are set for every GitHub Actions job, so their absence means a local run and the
/// page simply shows no link.
pub fn run_url() -> Option<String> {
    let server = env::var("GITHUB_SERVER_URL").ok()?;
    let repository = env::var("GITHUB_REPOSITORY").ok()?;
    let run = env::var("GITHUB_RUN_ID").ok()?;
    Some(format!("{server}/{repository}/actions/runs/{run}"))
}
