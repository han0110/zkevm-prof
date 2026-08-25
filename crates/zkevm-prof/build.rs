//! Resolves the ere-guests version the crate is built against, so the guest download follows the
//! Cargo pin rather than a constant kept in step with it by hand.
//!
//! A tag pin publishes the guests as release assets, while every other pin leaves them as the build
//! artifacts of the commit it resolves to, and those are two different downloads, so the tag is
//! passed on only when the pin carries one.

use cargo_metadata::MetadataCommand;

/// ere-guests package whose pin names the version the guests are downloaded from.
const ERE_GUESTS_PACKAGE: &str = "stateless-validator-catalog";

fn main() {
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=../../Cargo.lock");

    let metadata = MetadataCommand::new()
        .exec()
        .expect("failed to read the cargo metadata of this package");
    let source = metadata
        .packages
        .iter()
        .find(|package| package.name == ERE_GUESTS_PACKAGE)
        .and_then(|package| package.source.as_ref())
        .unwrap_or_else(|| panic!("{ERE_GUESTS_PACKAGE} must be a git dependency"));

    // A resolved git source names the commit it locks to, and carries the tag, branch or revision
    // it was pinned by as the query of the repository URL. Only a tag doubles as the name a release
    // publishes its assets under, so any other pin is followed by commit.
    let (locator, commit) = source
        .repr
        .split_once('#')
        .unwrap_or_else(|| panic!("{ERE_GUESTS_PACKAGE} must be a git dependency"));
    if let Some(tag) = locator
        .split_once('?')
        .and_then(|(_, query)| query.strip_prefix("tag="))
    {
        println!("cargo::rustc-env=ERE_GUESTS_TAG={tag}");
    }
    println!("cargo::rustc-env=ERE_GUESTS_COMMIT={commit}");
}
