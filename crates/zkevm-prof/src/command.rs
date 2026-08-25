//! CLI commands.

pub mod index;
pub mod profile;

use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};

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
