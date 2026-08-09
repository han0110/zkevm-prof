//! Registry of the guest ELFs that can be profiled.
//!
//! `elf-registry.json` lists, per zkVM, the stateless validators a guest ELF is published for. An
//! entry naming a version and a URL is downloaded from that URL and known by that version, and an
//! entry naming neither is downloaded from the ere-guests build this crate pins its catalog to and
//! versioned by that catalog. Adding a guest is adding an entry.

use std::{
    collections::{BTreeSet, HashMap},
    env,
    sync::LazyLock,
};

use anyhow::{Context, Result, ensure};
use ere_catalog::zkVMKind;
use serde::Deserialize;
use stateless_validator_catalog::StatelessValidatorKind;
use stateless_validator_downloader::Downloader;

/// ere-guests release the guests it builds are published in, absent unless the pin carries a tag.
///
/// The build script fills this and the commit below from the Cargo pin of the catalog this crate
/// links, so the guests and the catalog naming their version come from the same ere-guests build.
const ERE_GUESTS_TAG: Option<&str> = option_env!("ERE_GUESTS_TAG");

/// ere-guests commit the guests it builds are built by.
const ERE_GUESTS_COMMIT: &str = env!("ERE_GUESTS_COMMIT");

/// Repository ere-guests publishes the guests it builds from.
const ERE_GUESTS_REPOSITORY: &str = "https://github.com/eth-act/ere-guests";

/// Environment variable holding the token the artifact download authenticates with.
///
/// The GitHub artifact API rejects anonymous reads, so unlike a release asset an artifact cannot be
/// fetched without a token.
const GITHUB_TOKEN: &str = "GITHUB_TOKEN";

static REGISTRY: LazyLock<HashMap<zkVMKind, Vec<Elf>>> = LazyLock::new(|| {
    let registry: HashMap<zkVMKind, Vec<Elf>> =
        serde_json::from_str(include_str!("../elf-registry.json"))
            .expect("elf-registry.json is well formed");
    assert!(
        registry
            .values()
            .flatten()
            .all(|elf| elf.version.is_some() == elf.url.is_some()),
        "an entry names a version and a URL together or neither"
    );
    registry
});

/// One guest ELF, as the registry lists it.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Elf {
    /// Stateless validator the guest implements.
    stateless_validator: String,
    /// Version the guest is known by, absent when the ere-guests catalog versions it.
    version: Option<String>,
    /// Release asset the ELF is downloaded from, absent when ere-guests builds it.
    url: Option<String>,
}

impl Elf {
    /// Kind the ere-guests catalog knows the guest by.
    fn stateless_validator(&self) -> Result<StatelessValidatorKind> {
        self.stateless_validator.parse().with_context(|| {
            format!(
                "{} is not in the ere-guests catalog, so its entry has to name a version and a URL",
                self.stateless_validator
            )
        })
    }
}

/// Returns the stateless validators the registry lists an ELF for, on any zkVM.
pub fn stateless_validators() -> Vec<&'static str> {
    BTreeSet::from_iter(
        REGISTRY
            .values()
            .flatten()
            .map(|elf| elf.stateless_validator.as_str()),
    )
    .into_iter()
    .collect()
}

/// Returns the version of the `stateless_validator` guest for `zkvm`, which the report shows beside
/// its cost.
pub fn version(zkvm: zkVMKind, stateless_validator: &str) -> Result<&'static str> {
    let elf = find(zkvm, stateless_validator)?;
    match &elf.version {
        Some(version) => Ok(version),
        None => Ok(elf.stateless_validator()?.version()),
    }
}

/// Returns where the ELF of the `stateless_validator` guest for `zkvm` is published, absent when the
/// pin resolves to a build artifact, which the GitHub API serves under no stable URL.
///
/// A release names its assets the way ere-guests does, so the two sources address an ELF alike.
pub fn url(zkvm: zkVMKind, stateless_validator: &str) -> Result<Option<String>> {
    let elf = find(zkvm, stateless_validator)?;
    match &elf.url {
        Some(url) => Ok(Some(url.clone())),
        None => Ok(ERE_GUESTS_TAG.map(|tag| {
            format!(
                "{ERE_GUESTS_REPOSITORY}/releases/download/{tag}\
                 /stateless-validator-{stateless_validator}-{zkvm}-{}.elf",
                zkvm.sdk_version()
            )
        })),
    }
}

/// Returns the ELF of the `stateless_validator` guest for `zkvm`.
pub async fn elf(zkvm: zkVMKind, stateless_validator: &str) -> Result<Vec<u8>> {
    let elf = find(zkvm, stateless_validator)?;
    match &elf.url {
        Some(url) => download(url).await,
        None => {
            let downloader = match ERE_GUESTS_TAG {
                Some(tag) => Downloader::from_tag(tag).await?,
                None => {
                    let github_token = env::var(GITHUB_TOKEN).with_context(|| {
                        format!(
                            "{GITHUB_TOKEN} must hold a token that can read ere-guests build artifacts"
                        )
                    })?;
                    Downloader::from_commit(ERE_GUESTS_COMMIT, &github_token).await?
                }
            };
            Ok(downloader
                .download(elf.stateless_validator()?, zkvm)
                .await?
                .elf)
        }
    }
}

fn find(zkvm: zkVMKind, stateless_validator: &str) -> Result<&'static Elf> {
    REGISTRY
        .get(&zkvm)
        .into_iter()
        .flatten()
        .find(|elf| elf.stateless_validator == stateless_validator)
        .with_context(|| {
            format!("elf-registry.json lists no {stateless_validator} guest for {zkvm}")
        })
}

async fn download(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("failed to request {url}"))?;
    let status = response.status();
    ensure!(status.is_success(), "{url} returned {status}");
    Ok(response.bytes().await?.to_vec())
}

#[cfg(test)]
mod tests {
    use crate::registry::{REGISTRY, version};

    /// An entry left to ere-guests has to name a guest the catalog carries, since that catalog is
    /// what versions it and what the download asks for.
    #[test]
    fn every_entry_resolves_a_version() {
        REGISTRY
            .iter()
            .flat_map(|(&zkvm, elfs)| elfs.iter().map(move |elf| (zkvm, elf)))
            .for_each(|(zkvm, elf)| {
                version(zkvm, &elf.stateless_validator).unwrap_or_else(|error| panic!("{error:#}"));
            });
    }
}
