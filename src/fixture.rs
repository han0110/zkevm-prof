//! Fetches and loads EEST blockchain test fixtures.
//!
//! A fixture file holds one test keyed by its name, and a test holds a chain of blocks. Only the
//! last block is profiled, since that is the block an EEST blockchain test is written to exercise
//! and the ones before it only build up the state it runs against.
//!
//! `suite-registry.json` says where each corpus is published, so a run names a suite and the corpus
//! is fetched into a directory of its own rather than put there by whatever invoked the profiler.

use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// Directory a fetched corpus is cached under, holding one directory per suite.
const FIXTURES_DIR: &str = "fixtures";

/// Environment variable holding the token a release download authenticates with.
///
/// A release asset is served to anyone, so this only lifts the rate limit an anonymous caller is
/// held to and is left unset without consequence.
const GITHUB_TOKEN: &str = "GITHUB_TOKEN";

static SUITES: LazyLock<HashMap<String, Suite>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../suite-registry.json"))
        .expect("suite-registry.json is well formed")
});

/// One corpus as the registry lists it, read for where it is published. The blocks it also lists
/// are what the page draws a suite from and are no part of fetching one.
#[derive(Deserialize)]
struct Suite {
    fixtures: Fixtures,
}

/// Where a corpus is published and where its blocks sit inside the archive.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    /// Assets holding the archive, concatenated in the order they are listed. A release caps one
    /// asset at 2 GiB while leaving their number free, so a corpus past that is published in parts.
    urls: Vec<String>,
    /// Path inside the archive holding the corpus, extracted so that its contents land directly in
    /// the suite's own directory. One archive can therefore carry several suites.
    subdir: String,
}

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

/// Where the corpus a suite names sits, fetching it first unless it is already cached.
///
/// The archive streams from the release straight into tar, so the only copy that reaches the disk is
/// the extracted corpus, which matters where the corpus alone fills most of what a runner has free.
/// Extraction goes to a directory renamed into place at the end, so a download that is interrupted
/// leaves nothing behind for the next run to take for a corpus it already holds.
pub async fn fetch(suite: &str) -> Result<PathBuf> {
    let target = Path::new(FIXTURES_DIR).join(suite);
    if target.exists() {
        eprintln!("using the corpus at {}", target.display());
        return Ok(target);
    }
    let fixtures = &SUITES
        .get(suite)
        .with_context(|| format!("suite-registry.json lists no {suite} corpus"))?
        .fixtures;

    let partial = Path::new(FIXTURES_DIR).join(format!(".{suite}.partial"));
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial)
        .with_context(|| format!("failed to create {}", partial.display()))?;
    let extracted = extract(&partial, fixtures).await;
    if extracted.is_err() {
        // Best effort, since the download has already failed and what is reported is that failure.
        let _ = fs::remove_dir_all(&partial);
    }
    extracted?;
    fs::rename(&partial, &target).with_context(|| {
        format!(
            "failed to move {} to {}",
            partial.display(),
            target.display()
        )
    })?;
    eprintln!("fetched the {suite} corpus into {}", target.display());
    Ok(target)
}

/// Streams the archive into a tar that extracts `subdir` alone into `into`.
async fn extract(into: &Path, fixtures: &Fixtures) -> Result<()> {
    let first = fixtures.urls.first().context("the corpus names no asset")?;
    // The leading components are stripped so what the subdir holds lands directly in the suite's
    // directory, which is the whole of what the profiler then walks.
    let depth = fixtures.subdir.split('/').count();
    let mut tar = Command::new("tar")
        .arg("--extract")
        .arg(compression(first)?)
        .arg(format!("--strip-components={depth}"))
        .arg("--directory")
        .arg(into)
        .arg(&fixtures.subdir)
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to run tar, which extracts a corpus")?;
    let mut stdin = tar
        .stdin
        .take()
        .expect("tar was spawned with a piped stdin");

    let client = client()?;
    for url in &fixtures.urls {
        eprintln!("downloading {url}");
        let mut response = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to request {url}"))?
            .error_for_status()
            .with_context(|| format!("failed to download {url}"))?;
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("failed to read {url}"))?
        {
            stdin
                .write_all(&chunk)
                .context("failed to write the archive into tar")?;
        }
    }
    drop(stdin);

    let status = tar.wait().context("failed to wait for tar")?;
    ensure!(
        status.success(),
        "tar exited with {status} extracting {} from the corpus",
        fixtures.subdir
    );
    Ok(())
}

/// The tar option for however the asset at `url` is compressed.
fn compression(url: &str) -> Result<&'static str> {
    // tar reads the compression off the archive only when it opens one itself, so a stream has to
    // be told, which the asset name already says.
    match url {
        _ if url.contains(".tar.zst") => Ok("--zstd"),
        _ if url.contains(".tar.gz") => Ok("--gzip"),
        _ => bail!("{url} names no compression tar can read a stream of"),
    }
}

fn client() -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    if let Ok(token) = env::var(GITHUB_TOKEN) {
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .with_context(|| format!("{GITHUB_TOKEN} holds no usable token"))?;
        header.set_sensitive(true);
        headers.insert(AUTHORIZATION, header);
    }
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .build()?)
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
