//! Prices one execution of a guest on a zkVM.
//!
//! A guest ELF is handed to the `ere-server` image the pinned ere revision publishes, which prices
//! every input it is then given. Both the ELF and the input are opaque bytes, so nothing here knows
//! what a guest computes or what it is handed. The cost a run reports is an open map of whatever
//! kinds that zkVM charges for, and [`composition`] names those kinds in the order a chart stacks
//! them.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fmt::{self, Display, Formatter},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use ere_dockerized::{
    DockerizedzkVM, DockerizedzkVMConfig, ERE_COST_ESTIMATION_HEAP_END,
    ERE_COST_ESTIMATION_HEAP_START, Elf, Input, ProverResource, image::server_zkvm_image,
    prover::Error as ContainerError, symbol_address,
};
use serde::{Deserialize, Serialize};

pub use ere_dockerized::zkVMKind;

/// Environment variables ere decides an image reference from, whose module is private to
/// `ere-dockerized` and so named again here.
const ERE_IMAGE_REGISTRY: &str = "ERE_IMAGE_REGISTRY";
const ERE_FORCE_REBUILD_DOCKER_IMAGE: &str = "ERE_FORCE_REBUILD_DOCKER_IMAGE";

/// Registry the `ere-server` images are published to.
///
/// Named where the caller names none, since ere reads the variable to decide whether an image can be
/// pulled at all and builds one from an ere checkout where it cannot.
const PUBLISHED_REGISTRY: &str = "ghcr.io/eth-act/ere";

/// Pair ere reads a ZisK heap between where the guest carries the one ZisK marked it with before
/// v1.1.0-alpha.
const ZISK_LEGACY_HEAP: &[&str] = &["_kernel_heap_bottom", "_kernel_heap_top"];

/// Cost of one execution, keyed by the cost kinds the zkVM defines.
///
/// The kinds partition the execution, so they sum to what the whole of it cost and no key holds that
/// whole.
pub type Cost = BTreeMap<String, u64>;

/// What one execution of a guest cost, the heap it used, and what it revealed.
#[derive(Debug)]
pub struct Execution {
    pub cost: Cost,
    /// Peak bytes of heap the guest reached, absent where the zkVM read no heap or the guest left
    /// its heap untouched.
    pub peak_heap_bytes: Option<u64>,
    /// Public values the run left behind, in the order the guest revealed them.
    ///
    /// A zkVM that reserves a region carries the whole region, and the bytes past what the guest
    /// revealed are zero. A zkVM that streams them carries exactly what the guest revealed. A
    /// caller reads the prefix it expects in both cases.
    pub public_values: Vec<u8>,
}

/// Why a block was not priced.
#[derive(Debug)]
pub enum Unpriced {
    /// The zkVM reported the guest did not get through the block, which is a property of the block
    /// and holds however often it is measured again.
    Guest(anyhow::Error),
    /// The container went away under the request, which says nothing about the block. Measure it
    /// again rather than recording it as one the guest fails on.
    Container(anyhow::Error),
}

impl Unpriced {
    /// What went wrong, whichever of the two it was.
    pub fn error(&self) -> &anyhow::Error {
        match self {
            Self::Guest(error) | Self::Container(error) => error,
        }
    }

    /// Reads an error for which of the two it is.
    ///
    /// What the zkVM itself reported is the guest's, and everything else is the container, the
    /// transport, or this crate disagreeing with ere. An error of no kind read here stops a run
    /// rather than being attributed to the block it interrupted.
    fn of(error: anyhow::Error) -> Self {
        match error.downcast_ref::<ContainerError>() {
            Some(ContainerError::zkVM(_)) => Self::Guest(error),
            _ => Self::Container(error),
        }
    }
}

impl Display for Unpriced {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        Display::fmt(self.error(), formatter)
    }
}

impl Error for Unpriced {}

/// One kind a cost splits into, in the form a document carries it, so a reader of that document
/// needs nothing else to stack a cost map it has never seen.
#[derive(Deserialize, Serialize)]
pub struct Component {
    /// Key the cost map records the kind under.
    pub name: String,
    /// What the kind covers, in one phrase a reader takes in at a glance.
    pub note: String,
}

/// The kinds the zkVM splits a cost into, in the order a chart stacks them.
///
/// ere names the keys and this names what each covers, so a run is checked against the list rather
/// than described by it, and a kind ere adds stops a run instead of reaching a chart unlabelled.
pub fn composition(zkvm: zkVMKind) -> Vec<Component> {
    let kinds: &[(&str, &str)] = match zkvm {
        zkVMKind::OpenVM => &[
            ("precompile", "Precompiles"),
            ("rv64", "RISC-V instructions"),
            ("system", "Memory and tables"),
        ],
        zkVMKind::SP1 => &[
            ("syscall", "Precompiles"),
            ("system", "Memory and tables"),
            ("opcode", "RISC-V instructions"),
        ],
        zkVMKind::Zisk => &[
            ("base", "ROM and tables"),
            ("precompile", "Precompiles"),
            ("memory", "Memory"),
            ("opcode", "ZisK instructions"),
            ("main", "Main"),
        ],
    };
    kinds
        .iter()
        .map(|(name, note)| Component {
            name: (*name).to_owned(),
            note: (*note).to_owned(),
        })
        .collect()
}

/// Prices a single guest over many inputs.
///
/// One container serves the whole corpus, so whatever the zkVM derives from the ELF is paid for once
/// and every input after the first is one call into a warm process.
pub struct Profiler {
    zkvm: DockerizedzkVM,
    kinds: Vec<Component>,
}

impl Profiler {
    /// Starts the published `ere-server` image for `zkvm` and hands it the guest.
    ///
    /// One guest per zkVM per host at a time. The container is named after its zkVM and binds one
    /// port, so a second one takes the name and the port from the first, whose profiler then goes on
    /// measuring the guest that replaced it.
    pub fn new(zkvm: zkVMKind, elf: &[u8]) -> Result<Self> {
        prepare(zkvm, elf)?;
        let kinds = composition(zkvm);
        standing(zkvm)?;

        let image = server_zkvm_image(zkvm, false);
        pull(&image)?;
        let zkvm = DockerizedzkVM::new(
            zkvm,
            Elf(elf.to_vec()),
            ProverResource::Cpu,
            DockerizedzkVMConfig::default(),
        )
        .with_context(|| format!("failed to start {image}"))?;

        Ok(Self { zkvm, kinds })
    }

    /// Prices one execution of the guest over `input`.
    ///
    /// A guest that breaks a zkVM invariant panics inside the container, which reports it as the
    /// error it is, so one bad input costs a caller that input and no more. A container that goes
    /// away under the request takes down every block running in it, which is why the two are told
    /// apart rather than both reported against the block.
    pub fn profile(&self, input: &[u8]) -> Result<Execution, Unpriced> {
        self.priced(input).map_err(Unpriced::of)
    }

    fn priced(&self, input: &[u8]) -> Result<Execution> {
        let (public_values, mut estimation) = self
            .zkvm
            .execute_estimated_cost(&Input::new().with_stdin(input.to_vec()))?;
        let unnamed: Vec<&str> = estimation
            .cost
            .keys()
            .map(String::as_str)
            .filter(|key| !self.kinds.iter().any(|kind| kind.name == *key))
            .collect();
        ensure!(
            unnamed.is_empty(),
            "the run priced {}, which the kinds it splits into do not name",
            unnamed.join(" and ")
        );
        // A kind the zkVM charged nothing for is left out of the map it returns, and a reader that
        // stacks the declared kinds reads a missing key as no figure rather than as no cost.
        for kind in &self.kinds {
            estimation.cost.entry(kind.name.clone()).or_insert(0);
        }
        Ok(Execution {
            cost: estimation.cost,
            peak_heap_bytes: estimation.peak_heap_bytes,
            public_values: public_values.into(),
        })
    }
}

/// Symbols each zkVM's toolchain delimits a guest heap with, in the order a guest is searched for
/// them, every symbol of a set having to resolve for ere to read that heap.
///
/// OpenVM and SP1 mark the bottom alone, their zkVM fixing the top. ZisK marks both, and renamed the
/// pair in v1.1.0-alpha, so a guest linking a script of its own carries the older one.
fn heap_symbols(zkvm: zkVMKind) -> &'static [&'static [&'static str]] {
    match zkvm {
        zkVMKind::OpenVM | zkVMKind::SP1 => &[&["_end"]],
        zkVMKind::Zisk => &[&["_heap_bottom", "_heap_top"], ZISK_LEGACY_HEAP],
    }
}

/// Names the registry to pull from, takes away what would make ere build instead, and names the
/// symbols delimiting a heap that ere does not read by default.
///
/// ere reads all four out of the environment and passes the last two into the container. The
/// environment is process wide, which one guest per run is what makes safe.
///
/// A guest marking the bottom of its heap with no symbol ere looks for is an error rather than a heap
/// of nothing, since a guest quietly absent from the heap chart reads as one that allocates nothing
/// at all.
fn prepare(zkvm: zkVMKind, elf: &[u8]) -> Result<()> {
    let sets = heap_symbols(zkvm);
    let carried = sets
        .iter()
        .find(|set| {
            set.iter()
                .all(|symbol| symbol_address(elf, symbol).is_some())
        })
        .with_context(|| {
            let sets: Vec<String> = sets.iter().map(|set| set.join(" and ")).collect();
            format!(
                "the guest ELF delimits its heap with neither {}",
                sets.join(" nor ")
            )
        })?;

    // SAFETY: reached once, from the one call that builds a profiler, before any block runs. The
    // runtime that reached it holds worker threads, and they sit between the download that precedes
    // this and the container that follows it, reading no environment of their own.
    unsafe {
        if env::var_os(ERE_IMAGE_REGISTRY).is_none_or(|registry| registry.is_empty()) {
            env::set_var(ERE_IMAGE_REGISTRY, PUBLISHED_REGISTRY);
        }
        // ere reads this one for its presence rather than its value, and builds every image from an
        // ere checkout wherever it is set, however the pull went.
        env::remove_var(ERE_FORCE_REBUILD_DOCKER_IMAGE);
        if *carried == ZISK_LEGACY_HEAP {
            env::set_var(ERE_COST_ESTIMATION_HEAP_START, ZISK_LEGACY_HEAP[0]);
            env::set_var(ERE_COST_ESTIMATION_HEAP_END, ZISK_LEGACY_HEAP[1]);
        }
    }
    Ok(())
}

/// Stops where a container of this zkVM is already running, which is another guest being profiled.
///
/// ere names a container after its zkVM and takes that name from whatever holds it, so the run that
/// arrives second would otherwise silently price its blocks against the guest the first is measuring.
/// A container a dead run left behind is not running and is ere's to remove, so only a live one stops
/// anything.
fn standing(zkvm: zkVMKind) -> Result<()> {
    let name = format!("ere-server-{zkvm}");
    let output = Command::new("docker")
        .args([
            "container",
            "inspect",
            "--format",
            "{{.State.Running}}",
            &name,
        ])
        .output()
        .context("failed to run docker, which is what prices a guest here")?;
    ensure!(
        output.stdout.trim_ascii() != b"true",
        "the container {name} is running, holding another guest. Run `docker rm -f {name}`."
    );
    Ok(())
}

/// Pulls `image`, so that ere finds it in place rather than building one.
///
/// ere downgrades a failed pull to a build from an ere checkout, which is hours of SDK compilation
/// rather than the published image a profile is supposed to be measured on.
fn pull(image: &str) -> Result<()> {
    let status = Command::new("docker")
        .args(["pull", "--quiet", image])
        .status()
        .context("failed to run docker, which is what prices a guest here")?;
    ensure!(status.success(), "failed to pull {image}");
    Ok(())
}
