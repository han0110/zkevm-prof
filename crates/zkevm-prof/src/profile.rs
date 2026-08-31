//! The profile document, as published.
//!
//! A run records what each block cost, the blocks the guest did not get through, and the meta
//! naming the guest, the corpus and the harness the figures came from.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zkvm_prof::{Component, Cost, zkVMKind};

use crate::fixture::Metadata;

/// Profile document, as written to the output JSON.
#[derive(Deserialize, Serialize)]
pub struct Profile {
    pub profile: BTreeMap<String, Entry>,
    /// Blocks the guest failed on, which are what the profile is short of the corpus by. Empty for
    /// a run that profiled every block it was given, and absent from a profile written before
    /// failures were recorded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failures: BTreeMap<String, Failure>,
    pub meta: Meta,
}

/// What produced a profile, which is how the report knows a cost map's shape.
#[derive(Deserialize, Serialize)]
pub struct Meta {
    /// Harness that measured the run, which stands for everything a cost depends on that the rest of
    /// this does not name, the ere revision the cost was estimated by included.
    pub version: String,
    pub zkvm: zkVMKind,
    /// zkVM SDK the guest was built against, as the ere catalog names it.
    pub zkvm_version: String,
    /// Stateless validator the profile is of.
    pub stateless_validator: String,
    /// Version of that guest, as the registry resolves it.
    pub stateless_validator_version: String,
    /// Where the profiled ELF is published, absent when it was handed over directly or built as an
    /// artifact the GitHub API serves under no stable URL.
    #[serde(default)]
    pub elf_url: Option<String>,
    /// SHA-256 of the profiled ELF, absent for a profile written before the hash was recorded.
    #[serde(default)]
    pub elf_sha256: Option<String>,
    /// Fixture corpus the guest was profiled over, as `suite-registry.json` names it.
    #[serde(default)]
    pub suite: String,
    /// Wall clock time a run is stamped with, in seconds since the epoch.
    #[serde(default)]
    pub generated_at: u64,
    /// Link to the workflow run that produced the profile, absent for a local run.
    #[serde(default)]
    pub run_url: Option<String>,
    /// Kinds the cost map decomposes into, in stack order.
    #[serde(default)]
    pub composition: Vec<Component>,
}

/// One profiled block.
#[derive(Deserialize, Serialize)]
pub struct Entry {
    pub cost: Cost,
    /// Peak bytes of heap the guest reached, absent where the zkVM read no heap or the guest left
    /// its heap untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_heap_bytes: Option<u64>,
    pub metadata: Metadata,
}

/// One block the guest did not get through, carried alongside the ones it did so that a profile
/// short of its corpus says which blocks it is short of and why.
#[derive(Deserialize, Serialize)]
pub struct Failure {
    /// What the container reported, which is the guest's own error or the panic that aborted the
    /// emulator.
    pub reason: String,
    pub metadata: Metadata,
}

#[cfg(test)]
mod tests {
    use crate::profile::Profile;

    /// Every profile published before failures were recorded carries no `failures` key, and the
    /// page still has to be able to load one.
    #[test]
    fn a_profile_without_failures_parses() {
        let document = r#"{"profile":{},"meta":{"version":"0.1.0","zkvm":"zisk",
            "zkvm_version":"v1.0.0-alpha","stateless_validator":"reth",
            "stateless_validator_version":"f804dc1"}}"#;
        let profile: Profile = serde_json::from_str(document).unwrap();
        assert!(profile.failures.is_empty());
        // A run that failed on nothing writes no key either, so the two read alike.
        let written = serde_json::to_string(&profile).unwrap();
        assert!(!written.contains("failures"));
    }
}
