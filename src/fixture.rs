//! Loads EEST blockchain test fixtures.
//!
//! A fixture file holds one test keyed by its name, and a test holds a chain of blocks. Only the
//! last block is profiled, since that is the block an EEST blockchain test is written to exercise
//! and the ones before it only build up the state it runs against.

use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// One block to profile, with the metadata carried alongside its cost in the output.
pub struct Fixture {
    /// Name of the test the block was taken from.
    pub test_name: String,
    /// Schema-prefixed SSZ stateless input the guest reads.
    pub stateless_input: Vec<u8>,
    pub metadata: Metadata,
}

/// Block facts a cost is interpreted against.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Metadata {
    pub gas_used: u64,
    pub block_number: u64,
}

#[derive(Deserialize)]
struct Test {
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Block {
    stateless_input_bytes: String,
    block_header: BlockHeader,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockHeader {
    number: String,
    gas_used: String,
}

/// Collects every `.json` file under `dir`, recursively, in a stable order.
pub fn find(dir: &Path) -> Result<Vec<PathBuf>> {
    let paths = WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|entry| {
            let path = entry.ok()?.into_path();
            path.extension()
                .is_some_and(|extension| extension == "json")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        bail!("no .json fixtures found under {}", dir.display());
    }
    Ok(paths)
}

/// Loads the last block of every test in `path`.
///
/// A fixture file normally holds a single test, but the format allows several and each contributes
/// its own entry so the output stays keyed by test name rather than by file.
pub fn load(path: &Path) -> Result<Vec<Fixture>> {
    let read = || -> Result<Vec<Fixture>> {
        let file = BufReader::new(File::open(path)?);
        let tests: serde_json::Map<String, serde_json::Value> = serde_json::from_reader(file)?;
        tests
            .into_iter()
            .map(|(test_name, value)| {
                let test: Test = serde_json::from_value(value)
                    .with_context(|| format!("test {test_name} is not an EEST blockchain test"))?;
                let block = test
                    .blocks
                    .last()
                    .with_context(|| format!("test {test_name} has no blocks"))?;
                Ok(Fixture {
                    stateless_input: decode_hex(&block.stateless_input_bytes)
                        .context("statelessInputBytes")?,
                    metadata: Metadata {
                        gas_used: decode_quantity(&block.block_header.gas_used)
                            .context("blockHeader.gasUsed")?,
                        block_number: decode_quantity(&block.block_header.number)
                            .context("blockHeader.number")?,
                    },
                    test_name,
                })
            })
            .collect()
    };
    read().with_context(|| format!("failed to load fixture {}", path.display()))
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    Ok(hex::decode(value.strip_prefix("0x").unwrap_or(value))?)
}

fn decode_quantity(value: &str) -> Result<u64> {
    let digits = value.strip_prefix("0x").unwrap_or(value);
    Ok(u64::from_str_radix(digits, 16)?)
}
