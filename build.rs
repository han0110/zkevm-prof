//! Resolves the ere-guests version the crate is built against, so the guest download follows the
//! Cargo pin rather than a constant kept in step with it by hand.
//!
//! A tag pin publishes the guests as release assets, while every other pin leaves them as the build
//! artifacts of the commit it resolves to, and those are two different downloads, so the tag is
//! passed on only when the pin carries one.
//!
//! It also mirrors the target test `sp1-core-executor` gates its native JIT on, since the SP1
//! backend can only read guest memory where that JIT is the executor.

use std::env;

use cargo_metadata::MetadataCommand;

/// ere-guests package whose pin names the version the guests are downloaded from.
const ERE_GUESTS_PACKAGE: &str = "stateless-validator-catalog";

fn main() {
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=Cargo.lock");
    println!("cargo::rustc-check-cfg=cfg(sp1_jit_memory)");

    // `sp1-core-executor` compiles its native JIT for this target, and only that JIT keeps guest
    // memory in a file the SP1 backend can read the heap out of. Its own test also excludes two of
    // its features, which a dependent cannot see, so enabling either fails this crate's build
    // rather than quietly changing what it measures.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
    {
        println!("cargo::rustc-cfg=sp1_jit_memory");
    }

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
