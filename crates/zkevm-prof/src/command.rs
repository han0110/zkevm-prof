//! CLI commands.

pub mod import;
pub mod index;
pub mod profile;

use std::{
    env, fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Reads a JSON document, naming the file it failed on rather than the offset alone.
pub fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

/// Wall clock time a run is stamped with, in seconds since the epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
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
